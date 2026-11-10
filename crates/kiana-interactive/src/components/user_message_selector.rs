//! components/user-message-selector.ts。
//!
//! 从用户消息 fork：展示用户消息列表（最多 10 条、上下环绕）并回调选中条目的 entry id。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/user-message-selector.ts`。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 的 `UserMessageList` 是模块私有类；Rust 侧公开（`getMessageList()` 需要返回它）。
//! * 空列表的 `setTimeout(() => onCancel(), 100)` 改为「首次渲染/输入时若已过 100ms 再触发」。
//! * `UserMessageList.text.replace(/\n/g, " ").trim()` 里的 trim 用 JS 空白集合。
//!
//! ## 这个组件解决什么问题
//!
//! `/fork` 需要让用户选一条历史用户消息作为分叉点。选择器把宿主已经投影好的纯文本消息
//! （[`UserMessageItem::text`]）排成一个可上下移动的列表，确认时只把选中条目的
//! **entry id** 回调出去；真正的会话分叉由调用方完成（见
//! `interactive_mode::commands::session::InteractiveMode::show_user_message_selector`）。
//! 组件本身不访问 session，也不解析消息内容。
//!
//! ## 数据结构与执行流程
//!
//! - [`UserMessageList`]：无渲染缓存的纯状态机。持有消息数组、当前下标（`isize`，空列表
//!   时为 0）和固定的 `max_visible = 10`；两个回调是公开字段，宿主可在构造后替换。
//! - [`UserMessageSelectorComponent`]：用 `Container` 按固定顺序拼出
//!   Spacer / 标题 / 说明 / Spacer / 上边框 / Spacer / 消息列表 / Spacer / 下边框，
//!   并记录消息列表在 `children` 中的下标，供
//!   [`UserMessageSelectorComponent::get_message_list`] 向下转型取回。
//! - 渲染：列表把可视窗口裁到最多 10 条，每条消息固定产出 3 行（正文、`Message i of N`、
//!   空行）；窗口没有覆盖全部消息时再追一行 `(i/N)` 滚动指示。
//! - 输入：`tui.select.up` / `down` 环绕移动，`confirm` 回调选中 id，`cancel` 回调取消。
//!
//! ## 约束与注意事项
//!
//! - 回调是同步的 `FnMut`，在 `handle_input` 或 `render` 的调用栈内执行。回调里要发起
//!   异步工作（例如分叉会话）必须自行 `spawn_local`，不能 `await`。
//! - 空列表的自动取消是**惰性**的：构造时记下截止时间，直到下一次 `render` 或
//!   `handle_input` 才检查，不会像 TS 的 `setTimeout` 那样在后台自行触发。调用方若既不
//!   渲染也不送输入，`on_cancel` 不会发生。当前生产调用方在构造前已对空列表早退，所以
//!   这条路径主要出现在直接构造组件的测试里。
//! - `render` 可能产生副作用：它先渲染，再在截止时间已过时调用一次 `on_cancel`。

use std::time::{Duration, Instant};

use kiana_theme::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::text::{is_js_whitespace, truncate_to_width};
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::dynamic_border::DynamicBorder;

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 无参数回调（TS `() => void`）。
///
/// 在 UI 线程同步调用；内部若需要异步操作，调用方自行 `spawn_local`。
pub type VoidCallback = Box<dyn FnMut()>;
/// 单字符串参数回调（TS `(value: string) => void`）。
///
/// 参数是选中条目的 entry id（不是消息文本）。同样同步调用。
pub type StringCallback = Box<dyn FnMut(&str)>;

/// 一条用户消息（TS `UserMessageItem`）。
///
/// `text` 是宿主**已经投影好的纯文本**，不是 `kiana-ai` 的消息结构；本组件不会拼接
/// content 块，也不会解析 Markdown。渲染时只把换行替换成空格并做截断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessageItem {
    /// 会话里的条目 id（确认时回调出去的值）。
    pub id: String,
    /// 消息文本（宿主投影后的纯文本）。
    pub text: String,
    /// 可选时间戳（TS 里未使用）。
    pub timestamp: Option<String>,
}

/// 用户消息列表组件（TS `UserMessageList`）。
///
/// 一个不缓存的纯渲染 + 键位状态机：每次 `render` 都重新计算可视窗口与每一行。
/// `max_visible` 固定为 10，没有 setter。
///
/// 不变量：
/// - `messages` 非空时，`selected_index` 指向 `0..messages.len()` 中的一个下标；
/// - `messages` 为空时 `selected_index` 初始为 0（越界访问由
///   [`UserMessageList::selected_message`] 的负下标检查兜底）。
pub struct UserMessageList {
    messages: Vec<UserMessageItem>,
    selected_index: isize,
    max_visible: usize,
    /// 选中回调（entry id）。
    pub on_select: Option<StringCallback>,
    /// 取消回调。
    pub on_cancel: Option<VoidCallback>,
}

impl UserMessageList {
    /// `new(messages, initialSelectedId?)`：默认选中 `initialSelectedId`，否则最新一条。
    ///
    /// 查找按 `id` 做线性扫描；`initial_selected_id` 为 `None` 或没有匹配项时，选中
    /// `max(0, len - 1)`，即最后一条（宿主按时间顺序传入，所以是最新消息）。
    /// 空列表时下标为 0，但任何取消息的方法都返回 `None`。
    pub fn new(messages: Vec<UserMessageItem>, initial_selected_id: Option<&str>) -> Self {
        let initial_index = initial_selected_id
            .and_then(|id| messages.iter().position(|message| message.id == id))
            .map(|index| index as isize);
        let selected_index = initial_index.unwrap_or_else(|| (messages.len() as isize - 1).max(0));
        UserMessageList {
            messages,
            selected_index,
            max_visible: 10,
            on_select: None,
            on_cancel: None,
        }
    }

    /// 当前选中的消息（越界时为 `None`）。
    ///
    /// 负下标（空列表上按过一次 `up` 后可能出现）直接返回 `None`；正下标越界由
    /// `Vec::get` 返回 `None`。调用方不需要自己判空。
    pub fn selected_message(&self) -> Option<&UserMessageItem> {
        if self.selected_index < 0 {
            return None;
        }
        self.messages.get(self.selected_index as usize)
    }

    /// 当前选中下标。
    ///
    /// 类型是 `isize` 而不是 `usize`：空列表时初始值为 0，但向上移动会先变成 -1，
    /// 用有符号类型避免下溢。
    pub fn selected_index(&self) -> isize {
        self.selected_index
    }

    /// `invalidate()`（空实现）。
    ///
    /// 本组件不缓存任何渲染结果，每次 `render` 都从字段重新计算，所以没有可失效的内容。
    pub fn invalidate(&mut self) {}

    /// `render(width)`。
    ///
    /// 输出结构（与 TS 逐行对齐）：
    /// - 空列表：只有一行 `  No user messages found`（muted）；
    /// - 非空：先按选中项居中计算 `max_visible` 大小的窗口，再为窗口内每条消息输出
    ///   3 行——光标 + 截断后的正文、`  Message i of N`、空行。`i` 是**全局**序号；
    /// - 窗口未覆盖全部消息时，末尾追加 `  (selected+1/N)`。
    ///
    /// 宽度处理：正文预留 2 列给光标（`› ` 或两个空格），因此截断宽度是
    /// `width.saturating_sub(2)`；`width < 2` 时截断宽度为 0，正文变为空串。元数据行
    /// 不做截断，宽度可能超出调用方给的 `width`（元数据行语义上不应被截断）。
    pub fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        // 空列表没有可滚动的条目，也不访问 selected_index。
        if self.messages.is_empty() {
            lines.push(fg(ThemeColor::Muted, "  No user messages found"));
            return lines;
        }

        let length = self.messages.len();
        // 窗口起点：让选中项尽量居中（max_visible/2 条消息在它前面），同时保证起点不
        // 超过“最后 max_visible 条”的起点，也不小于 0。length < max_visible 时
        // `length - max_visible` 为负，随后被 max(0) 夹到 0。
        let start_index = (self.selected_index - (self.max_visible as isize) / 2)
            .min(length as isize - self.max_visible as isize)
            .max(0) as usize;
        let end_index = (start_index + self.max_visible).min(length);

        for index in start_index..end_index {
            let message = &self.messages[index];
            let is_selected = index as isize == self.selected_index;

            // 单行化：只把 `\n` 换成空格，再按 JS 的空白集合去掉首尾空白（不能用 Rust
            // 的 `trim`，两者对 U+0085/U+FEFF 等码点的判定不同）。
            let normalized_message = message
                .text
                .replace('\n', " ")
                .trim_matches(is_js_whitespace)
                .to_string();

            // 选中项用 accent 色的 `› ` 光标并加粗正文；未选中项用两个空格保持对齐。
            let cursor = if is_selected {
                fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };
            let max_msg_width = width.saturating_sub(2);
            let truncated_msg = truncate_to_width(&normalized_message, max_msg_width, "...", false);
            let message_line = if is_selected {
                format!("{cursor}{}", bold(&truncated_msg))
            } else {
                format!("{cursor}{truncated_msg}")
            };
            lines.push(message_line);

            // 序号是全局下标 + 1，而不是窗口内序号。
            let position = index + 1;
            lines.push(fg(
                ThemeColor::Muted,
                &format!("  Message {position} of {length}"),
            ));
            lines.push(String::new());
        }

        // 只有窗口被裁剪时才显示滚动指示（首尾任一未覆盖）。
        if start_index > 0 || end_index < length {
            lines.push(fg(
                ThemeColor::Muted,
                &format!("  ({}/{})", self.selected_index + 1, length),
            ));
        }

        lines
    }

    /// `handleInput(keyData)`。
    ///
    /// 每次调用都从全局 keybindings 读取当前绑定（用户可在运行时改键），只识别四个动作：
    ///
    /// - `tui.select.up`：下标 -1，在 0 处环绕到 `len - 1`；
    /// - `tui.select.down`：下标 +1，在 `len - 1` 处环绕到 0；
    /// - `tui.select.confirm`：把选中条目的 `id` 克隆后交给 `on_select`（先克隆是为了在
    ///   回调运行期间不持有 `self.messages` 的借用）；
    /// - `tui.select.cancel`：调用 `on_cancel`。
    ///
    /// 回调都是 `Option`，未安装时对应动作静默忽略。空列表时 `len == 0`：`up` 会把下标
    /// 变成 -1、`down` 变成 1，但 `selected_message` 返回 `None`，且空列表不渲染条目，
    /// 所以不会产生越界访问；选择器层的 100ms 自动取消通常会先处理这种状态。
    pub fn handle_input(&mut self, key_data: &str) {
        let kb = kiana_tui::keybindings::get_keybindings();
        let length = self.messages.len() as isize;
        if kb.matches(key_data, "tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                length - 1
            } else {
                self.selected_index - 1
            };
        } else if kb.matches(key_data, "tui.select.down") {
            self.selected_index = if self.selected_index == length - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if kb.matches(key_data, "tui.select.confirm") {
            if let Some(selected) = self.selected_message() {
                let id = selected.id.clone();
                if let Some(callback) = &mut self.on_select {
                    callback(&id);
                }
            }
        } else if kb.matches(key_data, "tui.select.cancel")
            && let Some(callback) = &mut self.on_cancel
        {
            callback();
        }
    }
}

impl Component for UserMessageList {
    // 固有方法承载 TS 的公开 API；trait 实现只做同名转发，保证组件能被放进容器树。
    fn render(&mut self, width: usize) -> Vec<String> {
        UserMessageList::render(self, width)
    }

    fn handle_input(&mut self, data: &str) {
        UserMessageList::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        UserMessageList::invalidate(self);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        // 选择器用 `downcast_mut::<UserMessageList>` 从容器子项取回自己。
        Some(self)
    }
}

/// 从用户消息 fork 的选择器（TS `UserMessageSelectorComponent`）。
///
/// 自身是一个 `Container` 的包装：固定的标题/说明/边框子项 + 中间的 [`UserMessageList`]。
/// 宿主通过 [`UserMessageSelectorComponent::get_message_list`] 取得列表并把焦点直接设在
/// 它上面；本组件的 `handle_input` 只是备用转发路径。
pub struct UserMessageSelectorComponent {
    container: Container,
    /// 消息列表在 `container.children` 中的下标；构造后必须保持有效，否则
    /// `get_message_list` 的向下转型会 panic。
    message_list_index: usize,
    /// 空列表的自动取消截止时间；`None` 表示不再检查。
    auto_cancel_at: Option<Instant>,
}

impl UserMessageSelectorComponent {
    /// `new(messages, onSelect, onCancel, initialSelectedId?)`。
    ///
    /// 按 TS 的顺序装配 9 个子项（Spacer、粗体标题、muted 说明、Spacer、上边框、Spacer、
    /// 消息列表、Spacer、下边框）。两个回调被移入列表的公开字段，之后仍可由调用方替换。
    ///
    /// 空列表时记录 `now + 100ms` 的自动取消截止时间；此时列表本身仍会被加入容器并渲染
    /// 空状态提示，直到截止时间在 `render`/`handle_input` 中被观察到。
    pub fn new(
        messages: Vec<UserMessageItem>,
        on_select: StringCallback,
        on_cancel: VoidCallback,
        initial_selected_id: Option<&str>,
    ) -> Self {
        let empty = messages.is_empty();
        // 列表先构造，回调作为公开字段装上。
        let mut message_list = UserMessageList::new(messages, initial_selected_id);
        message_list.on_select = Some(on_select);
        message_list.on_cancel = Some(on_cancel);

        // 布局顺序是稳定契约（黄金渲染逐行比对）：标题/说明各带 1 列水平 padding，边框每次 render 自适应
        // 宽度。消息列表的下标以 add_child 前的 children.len() 记录（即列表即将占据的
        // 位置），后续不再插入/删除子项。
        let mut container = Container::new();
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            bold("Fork from Message"),
            1,
            0,
        )));
        container.add_child(Box::new(Text::with_padding(
            fg(
                ThemeColor::Muted,
                "Select a user message to copy the active path up to that point into a new session",
            ),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));
        let message_list_index = container.children.len();
        container.add_child(Box::new(message_list));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));

        UserMessageSelectorComponent {
            container,
            message_list_index,
            // 只在空列表时启动惰性取消；非空列表不设截止时间。
            auto_cancel_at: if empty {
                Some(Instant::now() + Duration::from_millis(100))
            } else {
                None
            },
        }
    }

    /// `getMessageList()`。
    ///
    /// 从 `children[message_list_index]` 做 `downcast_mut` 取回列表。`Container` 只保存
    /// `Box<dyn Component>`，无法在类型层面记录具体类型，因此这里依赖构造时建立的
    /// “该下标一定是 `UserMessageList`”不变量；不变量被破坏时 `expect` 会 panic（属于
    /// 编程错误，不是运行时输入错误）。
    pub fn get_message_list(&mut self) -> &mut UserMessageList {
        self.container.children[self.message_list_index]
            .as_any_mut()
            .and_then(|component| component.downcast_mut::<UserMessageList>())
            .expect("message list child is a UserMessageList")
    }

    /// 检查空列表的惰性取消截止时间。
    ///
    /// 只有首次观察到截止时间已过时才调用 `on_cancel`，随后把 `auto_cancel_at` 置回
    /// `None`，因此重复调用不会重复回调。返回 `true` 表示本次调用触发了取消：
    /// `handle_input` 用它决定是否丢弃本次按键（TS 里定时器会先于输入取消选择器）。
    fn maybe_auto_cancel(&mut self) -> bool {
        if let Some(deadline) = self.auto_cancel_at
            && Instant::now() >= deadline
        {
            // 先清空截止时间再回调：即使回调触发重入渲染，也不会第二次调用 on_cancel。
            self.auto_cancel_at = None;
            let message_list = self.get_message_list();
            if let Some(callback) = &mut message_list.on_cancel {
                callback();
            }
            return true;
        }
        false
    }
}

impl Component for UserMessageSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let lines = self.container.render(width);
        // 惰性截止时间在渲染完成后结算，避免用户回调在行内容尚未产出时重入。
        // 注意副作用：截止时间已过时，本方法会在返回前调用一次 on_cancel；返回的
        // `lines` 仍是取消前这一帧的内容。
        self.maybe_auto_cancel();
        lines
    }

    fn handle_input(&mut self, data: &str) {
        // 先结算截止时间：已过期时 TS 的定时器会先于本次输入取消选择器，因此这一击
        // 不再转发给列表。
        if self.maybe_auto_cancel() {
            return;
        }
        self.get_message_list().handle_input(data);
    }

    fn handles_input(&self) -> bool {
        // TS 的 UserMessageSelectorComponent 继承 Container，没有 handleInput；
        // 输入直接发给 getMessageList()（宿主负责聚焦）。
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        // 让焦点/挂载遍历能看到标题、边框与消息列表；宿主正是用这个能力找到列表并设焦点
        // （调用方见 `show_user_message_selector`）。
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }
}
