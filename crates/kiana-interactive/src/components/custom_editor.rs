//! components/custom-editor.ts。
//!
//! 在 `kiana_tui::Editor` 之上加上编码代理的应用级键位、扩展快捷键、粘贴图片与
//! 「状态嵌在顶栏」能力。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/custom-editor.ts`。
//!
//! # 与 TS 的差异
//!
//! TS 用子类覆写 `protected renderTopBorder`；Rust 的 `Editor` 暴露
//! [`kiana_tui::components::editor::Editor::set_top_border_renderer`]（`Send + Sync` 回调），
//! 无法捕获 `!Send` 的状态指示器。这里改为：回调只记录 `hiddenLineCount` 并原样返回父类
//! 边框文本，`CustomEditor::render` 在拿到渲染结果后用记录的值重算第一行（字节结果与 TS 相同）。
//!
//! ## Public API for InteractiveMode
//!
//! - [`CustomEditor::new`]（[`EditorTui`], `EditorTheme`, `Arc<KeybindingsManager>`,
//!   [`CustomEditorOptions`]）
//! - 回调：`set_on_submit` / `set_on_change` / `set_on_escape` / `set_on_ctrl_d`
//!   / `set_on_paste_image` / `set_on_extension_shortcut`
//! - 动作：`set_action_handler`（同键替换）/ `set_working_status_indicator`
//! - 文本：`get_text` / `set_text` / `insert_text_at_cursor` / `get_cursor`
//!   / `get_expanded_text` / `add_to_history`
//! - 外观：`set_padding_x` / `set_autocomplete_max_visible` / `set_autocomplete_provider`
//!   / `set_border_color_fn`；`as_editor_component()` 返回 `&mut dyn EditorComponent`
//!
//! # 职责与边界
//!
//! 默认编辑器（`InteractiveMode` 构造时创建，见 `interactive_mode.rs`）就是本类型。
//! 它在通用 [`Editor`] 之上补三类“应用层”行为：
//!
//! 1. 键位分派：在编辑器自身处理之前拦截 `app.*` 应用动作、扩展注册的快捷键、
//!    粘贴图片、Escape 与 Ctrl+D（`handle_input`）；
//! 2. 状态嵌入：把工作/压缩/摘要/重试状态指示器画进上边框（`embed_working_status`）；
//! 3. 宿主接缝：回调设置、文本操作、自动补全与边框颜色，供 `InteractiveMode` 与
//!    扩展的编辑器替换逻辑使用。
//!
//! 它不理解会话/任务语义：动作做什么由调用方注入，本类型只决定“哪个键在什么条件下
//! 调用哪个回调，否则交给 [`Editor`]”。
//!
//! # `handle_input` 的分派顺序
//!
//! 顺序经过测试锁定（`tests/components_status_editor.rs`），改动会改变同名键位的优先级：
//!
//! 1. 已注册的扩展快捷键（返回 `true` 即消费）；
//! 2. `app.clipboard.pasteImage` → 粘贴图片回调；
//! 3. `app.interrupt`：自动补全未展开时先走动态 `onEscape`，再走动作表里的同名项；
//!    否则把按键交回编辑器用于取消补全；
//! 4. `app.exit`（Ctrl+D）：仅在编辑器为空时走动态 `onCtrlD`/动作表，否则继续交给
//!    编辑器处理“删除后一个字符”；
//! 5. 显式历史键（`tui.editor.historyPrevious` / `historyNext`）优先于应用动作；
//! 6. 其余应用动作按登记顺序命中第一个即停（`Vec` 保留插入顺序，同键原位替换）；
//! 7. 以上都不命中时交给 [`Editor::handle_input`]。
//!
//! # 状态嵌入的渲染路径
//!
//! 传给 [`Editor::set_top_border_renderer`] 的回调要求 `Send + Sync`，无法捕获 `!Send`
//! 的状态指示器。因此该回调只把本次渲染的 `hidden_line_count` 写入
//! `Arc<Mutex<Option<usize>>>`（字段 `top_border_hidden`）并原样返回父类边框；
//! [`Component::render`] 在拿到 `Editor::render` 的结果后再读回该值，用
//! `render_top_border_with_status` 重算首行。`Editor::render` 每次都会调用上边框回调
//! （见 `kiana-tui` 的 `render_editor`），所以“先记录、后重算”的时序在单线程渲染中成立。
//!
//! `render_top_border_with_status` 会优先尝试把状态与 `↑ N more` 滚动提示同时放进
//! 边框；放不下时先退化为 spinner，再放不下就只保留 spinner。内置指示器的
//! `render_in_border` / `render_spinner_in_border` 会把文本截到给定宽度，因此拼出的
//! 可见宽度等于传入 `width`；自定义 [`StatusIndicator`] 实现若返回更宽文本则无此保证。
//!
//! # 注意事项
//!
//! - `top_border_hidden` 的 `Mutex` 只用于满足边框回调的 `Send + Sync` 约束，实际访问
//!   都在 UI 线程；两处访问都用 `into_inner()` 忽略锁中毒。
//! - `set_working_status_indicator` 与 `set_border_color_fn` 不主动 `invalidate`，
//!   效果在下一次 `render` 可见；需要立即重绘由调用方请求。
//! - `copy_handlers_from` 只给**缺失**的回调安装转发闭包，动作表则总是覆盖；转发闭包
//!   在调用时回查 `source`，因此 source 之后替换的处理器也会被看到。
//! - `set_keybindings` 是替换句柄而不是原地 reload（见方法文档）。

use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kiana_config::keybindings::{AppKeybinding, KeybindingsManager};
use kiana_tui::components::editor::{Editor, EditorOptions, EditorTheme, EditorTui};
use kiana_tui::editor_component::{
    AutocompleteProviderObject, EditorBorderColor, EditorComponent, EditorTextCallback,
};
use kiana_tui::text::visible_width;
use kiana_tui::tui::{Component, Focusable, TuiMouseEvent, TuiMouseEventResult};

use super::status_indicator::StatusIndicator;

/// TS `onExtensionShortcut?: (data: string) => boolean`。
pub type ExtensionShortcutHandler = Box<dyn FnMut(&str) -> bool>;

/// TS `CustomEditorOptions = EditorOptions & { embedWorkingStatus?: boolean }`。
#[derive(Debug, Clone, Default)]
pub struct CustomEditorOptions {
    /// 水平内边距（TS `EditorOptions.paddingX`）。
    pub padding_x: Option<f64>,
    /// 自动补全最大可见项数（TS `EditorOptions.autocompleteMaxVisible`）。
    pub autocomplete_max_visible: Option<f64>,
    /// 是否在编辑器顶栏内嵌工作/压缩/摘要/重试状态。
    pub embed_working_status: bool,
}

/// `class CustomEditor extends Editor`。
pub struct CustomEditor {
    editor: Editor,
    keybindings: Arc<KeybindingsManager>,
    embed_working_status: bool,
    /// TS `actionHandlers: Map<AppKeybinding, () => void>`：插入顺序 + 同键替换。
    action_handlers: Vec<(AppKeybinding, Box<dyn FnMut()>)>,
    on_escape: Option<Box<dyn FnMut()>>,
    on_ctrl_d: Option<Box<dyn FnMut()>>,
    on_paste_image: Option<Box<dyn FnMut()>>,
    /// 扩展快捷键处理器：返回 true 表示已处理。
    on_extension_shortcut: Option<ExtensionShortcutHandler>,
    working_status_indicator: Option<Box<dyn StatusIndicator>>,
    /// 上一次 `renderTopBorder` 的 hiddenLineCount（由边框回调写入）。
    top_border_hidden: Arc<Mutex<Option<usize>>>,
}

impl CustomEditor {
    /// `constructor(tui, theme, keybindings, options?)`。
    pub fn new(
        editor_tui: EditorTui,
        theme: EditorTheme,
        keybindings: Arc<KeybindingsManager>,
        options: CustomEditorOptions,
    ) -> Self {
        let mut editor = Editor::new(
            editor_tui,
            theme,
            EditorOptions {
                padding_x: options.padding_x,
                autocomplete_max_visible: options.autocomplete_max_visible,
            },
        );
        let top_border_hidden = Arc::new(Mutex::new(None));
        if options.embed_working_status {
            let slot = Arc::clone(&top_border_hidden);
            editor.set_top_border_renderer(Some(Arc::new(
                move |_width, hidden_line_count, _color, base| {
                    *slot.lock().unwrap_or_else(|error| error.into_inner()) =
                        Some(hidden_line_count);
                    base.to_string()
                },
            )));
        }
        CustomEditor {
            editor,
            keybindings,
            embed_working_status: options.embed_working_status,
            action_handlers: Vec::new(),
            on_escape: None,
            on_ctrl_d: None,
            on_paste_image: None,
            on_extension_shortcut: None,
            working_status_indicator: None,
            top_border_hidden,
        }
    }

    /// 替换键位管理器（`/reload` 用）。
    ///
    /// TS 的 `KeybindingsManager` 是可变单例，`reload()` 原地生效；Rust 侧编辑器持有
    /// `Arc<KeybindingsManager>`，无法原地 reload，因此由宿主重新加载后替换句柄。
    pub fn set_keybindings(&mut self, keybindings: Arc<KeybindingsManager>) {
        self.keybindings = keybindings;
    }

    /// `setWorkingStatusIndicator(indicator)`（不调用 invalidate，下一次 render 生效）。
    pub fn set_working_status_indicator(&mut self, indicator: Option<Box<dyn StatusIndicator>>) {
        self.working_status_indicator = indicator;
    }

    /// 当前状态指示器（测试/宿主查询用）。
    pub fn working_status_indicator(&self) -> Option<&dyn StatusIndicator> {
        self.working_status_indicator.as_deref()
    }

    /// `embedWorkingStatus`（构造后不可变）。
    pub fn embed_working_status(&self) -> bool {
        self.embed_working_status
    }

    /// `onAction(action, handler)`：登记应用动作处理器（同键替换，保留插入位置）。
    pub fn set_action_handler(&mut self, action: AppKeybinding, handler: Box<dyn FnMut()>) {
        if let Some(entry) = self
            .action_handlers
            .iter_mut()
            .find(|(existing, _)| *existing == action)
        {
            entry.1 = handler;
        } else {
            self.action_handlers.push((action, handler));
        }
    }

    /// `onAction` 的 TS 同名别名。
    pub fn on_action(&mut self, action: AppKeybinding, handler: Box<dyn FnMut()>) {
        self.set_action_handler(action, handler);
    }

    /// TS `onSubmit`（与 `EditorComponent::set_on_submit` 等价）。
    pub fn set_on_submit(&mut self, callback: Option<EditorTextCallback>) {
        self.editor.on_submit = callback;
    }

    /// TS `onChange`（与 `EditorComponent::set_on_change` 等价）。
    pub fn set_on_change(&mut self, callback: Option<EditorTextCallback>) {
        self.editor.on_change = callback;
    }

    /// TS `onEscape`（动态覆盖 `app.interrupt` 的处理器）。
    pub fn set_on_escape(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_escape = handler;
    }

    /// 取出当前 `onEscape` 处理器（TS 读取 `this.defaultEditor.onEscape` 后临时替换）。
    pub fn take_on_escape(&mut self) -> Option<Box<dyn FnMut()>> {
        self.on_escape.take()
    }

    /// TS `onCtrlD`（动态覆盖 `app.exit` 的处理器）。
    pub fn set_on_ctrl_d(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_ctrl_d = handler;
    }

    /// TS `onPasteImage`。
    pub fn set_on_paste_image(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_paste_image = handler;
    }

    /// TS `onExtensionShortcut`。
    pub fn set_on_extension_shortcut(&mut self, handler: Option<ExtensionShortcutHandler>) {
        self.on_extension_shortcut = handler;
    }

    /// `setCustomEditorComponent` 的鸭子复制（`interactive-mode.ts:2727-2802`）。
    ///
    /// TS 只在目标编辑器确实是 `CustomEditor`（有 `actionHandlers` Map）时执行：缺失的
    /// `onEscape` / `onCtrlD` / `onPasteImage` / `onExtensionShortcut` 安装成转发到默认编辑器
    /// 同名回调的闭包，再把默认编辑器的每个动作处理器**覆盖**进目标的动作表（插入顺序与默认
    /// 编辑器一致）。这里的目标动作处理器同样在调用时回查 `source`，避免复制后源对象更新
    /// 而目标仍指向旧处理器。
    pub fn copy_handlers_from(&mut self, source: &Rc<std::cell::RefCell<CustomEditor>>) {
        if self.on_escape.is_none() {
            let source = Rc::clone(source);
            self.on_escape = Some(Box::new(move || {
                if let Some(handler) = source.borrow_mut().on_escape.as_mut() {
                    handler();
                }
            }));
        }
        if self.on_ctrl_d.is_none() {
            let source = Rc::clone(source);
            self.on_ctrl_d = Some(Box::new(move || {
                if let Some(handler) = source.borrow_mut().on_ctrl_d.as_mut() {
                    handler();
                }
            }));
        }
        if self.on_paste_image.is_none() {
            let source = Rc::clone(source);
            self.on_paste_image = Some(Box::new(move || {
                if let Some(handler) = source.borrow_mut().on_paste_image.as_mut() {
                    handler();
                }
            }));
        }
        if self.on_extension_shortcut.is_none() {
            let source = Rc::clone(source);
            self.on_extension_shortcut = Some(Box::new(move |data: &str| -> bool {
                source
                    .borrow_mut()
                    .on_extension_shortcut
                    .as_mut()
                    .is_some_and(|handler| handler(data))
            }));
        }
        let actions: Vec<AppKeybinding> = source
            .borrow()
            .action_handlers
            .iter()
            .map(|(action, _)| *action)
            .collect();
        for action in actions {
            let source = Rc::clone(source);
            self.set_action_handler(
                action,
                Box::new(move || {
                    if let Some(handler) = source
                        .borrow_mut()
                        .action_handlers
                        .iter_mut()
                        .find(|(existing, _)| *existing == action)
                        .map(|(_, handler)| handler)
                    {
                        handler();
                    }
                }),
            );
        }
    }

    /// 把自身当作 [`EditorComponent`] 使用（宿主替换编辑器时用）。
    pub fn as_editor_component(&mut self) -> &mut dyn EditorComponent {
        self
    }

    /// 底层 [`Editor`]。
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// 底层 [`Editor`] 的可变引用。
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// `getText()`。
    pub fn get_text(&self) -> String {
        self.editor.get_text()
    }

    /// `setText(text)`。
    pub fn set_text(&mut self, text: &str) {
        self.editor.set_text(text);
    }

    /// `insertTextAtCursor(text)`。
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        self.editor.insert_text_at_cursor(text);
    }

    /// `getCursor()`：`(line, col)`。
    pub fn get_cursor(&self) -> (usize, usize) {
        self.editor.get_cursor()
    }

    /// `getExpandedText()`。
    pub fn get_expanded_text(&self) -> String {
        self.editor.get_expanded_text()
    }

    /// `addToHistory(text)`。
    pub fn add_to_history(&mut self, text: &str) {
        self.editor.add_to_history(text);
    }

    /// `setPaddingX(padding)`。
    pub fn set_padding_x(&mut self, padding: f64) {
        self.editor.set_padding_x(padding);
    }

    /// `setAutocompleteMaxVisible(maxVisible)`。
    pub fn set_autocomplete_max_visible(&mut self, max_visible: f64) {
        self.editor.set_autocomplete_max_visible(max_visible);
    }

    /// `setAutocompleteProvider(provider)`。
    pub fn set_autocomplete_provider(
        &mut self,
        provider: Box<dyn kiana_tui::autocomplete::AutocompleteProvider>,
    ) {
        self.editor.set_autocomplete_provider(provider);
    }

    /// 安装边框着色函数（TS `borderColor = fn`）。
    pub fn set_border_color_fn(&mut self, color: Arc<dyn Fn(&str) -> String + Send + Sync>) {
        self.editor.border_color = color;
    }

    /// 读取边框着色函数。
    pub fn border_color_fn(&self) -> &Arc<dyn Fn(&str) -> String> {
        &self.editor.border_color
    }

    /// `renderTopBorder(width, hiddenLineCount)`（含状态嵌入）。
    ///
    /// 输入是本次渲染宽度、编辑器上报的隐藏行数与边框着色函数；输出整条上边框文本。
    /// 返回 `None` 表示放弃嵌入，调用方保留 `Editor::render` 生成的原边框。
    /// 宽度预算全部按“可见列数”计算，退化顺序见下面的分步注释。
    fn render_top_border_with_status(
        indicator: &mut dyn StatusIndicator,
        border_color: &dyn Fn(&str) -> String,
        width: usize,
        hidden_line_count: usize,
    ) -> Option<String> {
        if width == 0 {
            return None;
        }
        // 阶段 1：先请指示器按 (width - 5, 至少 1) 渲染完整状态。5 列留给后面的
        // 布局开销：`── ` 前缀 3 列、状态后间隔 1 列、至少 1 列 `─` 填充（与
        // 阶段 5 的 `width >= status_width + 5` 一致）；内置指示器会把文本截到
        // 给定宽度，所以这里的 status_width 可信（自定义实现不保证，见方法文档）。
        // 状态文本为空时直接返回 None，不占边框。
        let mut status = indicator.render_in_border((width.saturating_sub(5)).max(1));
        let mut status_width = visible_width(&status);
        if status_width == 0 {
            return None;
        }

        // 阶段 2：计算 "↑ N more" 滚动提示的居中位置与能否与状态并存。
        // overflow_start 是提示的左边界；can_fit_overflow 要求提示整体留在宽度内，
        // 且它与左侧状态块（`── ` + status + 1 列间隔）之间至少空 1 列。
        let overflow_label =
            (hidden_line_count > 0).then(|| format!(" ↑ {hidden_line_count} more "));
        let overflow_label_width = overflow_label
            .as_ref()
            .map(|label| visible_width(label))
            .unwrap_or(0);
        let overflow_start = width.saturating_sub(overflow_label_width) / 2;
        let can_fit_overflow = |status_width: usize| -> bool {
            overflow_label.is_some()
                && overflow_label_width + 2 <= width
                && overflow_start as i64 - (3 + status_width + 1) as i64 >= 1
        };

        // 阶段 3：放不下完整状态时先退化为 spinner（只保留动画帧、丢掉消息文本），
        // 退化后再重新检查能否与滚动提示并存。
        if overflow_label.is_some() && !can_fit_overflow(status_width) {
            status = indicator.render_spinner_in_border(width);
            status_width = visible_width(&status);
        }

        // 阶段 4：最优布局 —— 左侧 `── ` + 状态，中段用 `─` 填充到滚动提示的居中位置，
        // 提示之后再用 `─` 补满到行尾。
        if can_fit_overflow(status_width) {
            let overflow_label = overflow_label.expect("checked by can_fit_overflow");
            let left_block_width = 3 + status_width + 1;
            let fill_left = overflow_start - left_block_width;
            let fill_right = width - overflow_start - overflow_label_width;
            return Some(format!(
                "{}{}{}",
                border_color("── "),
                status,
                border_color(&format!(
                    " {}{}{}",
                    "─".repeat(fill_left),
                    overflow_label,
                    "─".repeat(fill_right)
                ))
            ));
        }

        // 阶段 5：没有滚动提示（或提示已退化掉）时，退而求其次只嵌状态：
        // `── ` + 状态 + 填充，同样要求 `width >= status_width + 5`。
        if width >= status_width + 5 {
            return Some(format!(
                "{}{}{}",
                border_color("── "),
                status,
                border_color(&format!(" {}", "─".repeat(width - status_width - 4)))
            ));
        }

        // 阶段 6：最后兜底 —— 换成 spinner（内置实现把帧截到 width）并掐着宽度
        // 拼接：前缀最多 3 列、受剩余宽度限制，末段补 `─`；最窄情况下各段可缩到 0。
        status = indicator.render_spinner_in_border(width);
        status_width = visible_width(&status);
        let prefix_width = 3.min(width.saturating_sub(status_width));
        Some(format!(
            "{}{}{}",
            border_color(&"─".repeat(prefix_width)),
            status,
            border_color(&"─".repeat(width.saturating_sub(prefix_width + status_width)))
        ))
    }
}

impl Component for CustomEditor {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.editor.render(width);
        if self.embed_working_status && width > 0 && !lines.is_empty() {
            let border_color = Arc::clone(&self.editor.border_color);
            let hidden_line_count = *self
                .top_border_hidden
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let (Some(indicator), Some(hidden_line_count)) =
                (self.working_status_indicator.as_mut(), hidden_line_count)
                && let Some(border) = Self::render_top_border_with_status(
                    indicator.as_mut(),
                    border_color.as_ref(),
                    width,
                    hidden_line_count,
                )
            {
                lines[0] = border;
            }
        }
        lines
    }

    fn handle_input(&mut self, data: &str) {
        // 1. 扩展注册的快捷键优先。
        if let Some(handler) = &mut self.on_extension_shortcut
            && handler(data)
        {
            return;
        }

        // 2. 剪贴板粘贴图片。
        if self.keybindings.matches(data, "app.clipboard.pasteImage") {
            if let Some(handler) = &mut self.on_paste_image {
                handler();
            }
            return;
        }

        // 3. Escape/interrupt —— 仅当自动补全未激活时走动态处理器。
        if self.keybindings.matches(data, "app.interrupt") {
            if !self.editor.is_showing_autocomplete() {
                if let Some(handler) = &mut self.on_escape {
                    handler();
                    return;
                }
                if let Some((_, handler)) = self
                    .action_handlers
                    .iter_mut()
                    .find(|(action, _)| *action == "app.interrupt")
                {
                    handler();
                    return;
                }
            }
            // 让父类处理 escape 以取消自动补全。
            self.editor.handle_input(data);
            return;
        }

        // 4. 退出（Ctrl+D）—— 只在编辑器为空时。
        if self.keybindings.matches(data, "app.exit") && self.editor.get_text().is_empty() {
            if let Some(handler) = &mut self.on_ctrl_d {
                handler();
                return;
            }
            if let Some((_, handler)) = self
                .action_handlers
                .iter_mut()
                .find(|(action, _)| *action == "app.exit")
            {
                handler();
            }
            return;
        }
        // `app.exit` 且文本非空：继续交给编辑器处理 delete-char-forward。

        // 5. 显式历史键优先于应用动作（例如把 Ctrl+P 绑到 historyPrevious）。
        if self.keybindings.matches(data, "tui.editor.historyPrevious")
            || self.keybindings.matches(data, "tui.editor.historyNext")
        {
            self.editor.handle_input(data);
            return;
        }

        // 6. 其余应用动作（按 Map 插入顺序）。
        for (action, handler) in self.action_handlers.iter_mut() {
            if *action != "app.interrupt"
                && *action != "app.exit"
                && self.keybindings.matches(data, action)
            {
                handler();
                return;
            }
        }

        // 7. 交给编辑器。
        self.editor.handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.editor.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        Component::invalidate(&mut self.editor);
        if let Some(indicator) = self.working_status_indicator.as_mut() {
            Component::invalidate(indicator.as_mut());
        }
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl Focusable for CustomEditor {
    fn focused(&self) -> bool {
        Focusable::focused(&self.editor)
    }

    fn set_focused(&mut self, focused: bool) {
        Focusable::set_focused(&mut self.editor, focused);
    }
}

impl EditorComponent for CustomEditor {
    fn get_text(&self) -> String {
        CustomEditor::get_text(self)
    }

    fn set_text(&mut self, text: &str) {
        CustomEditor::set_text(self, text);
    }

    fn set_on_submit(&mut self, callback: Option<EditorTextCallback>) {
        self.editor.on_submit = callback;
    }

    fn set_on_change(&mut self, callback: Option<EditorTextCallback>) {
        self.editor.on_change = callback;
    }

    fn add_to_history(&mut self, text: &str) {
        CustomEditor::add_to_history(self, text);
    }

    fn insert_text_at_cursor(&mut self, text: &str) {
        CustomEditor::insert_text_at_cursor(self, text);
    }

    fn get_expanded_text(&self) -> String {
        CustomEditor::get_expanded_text(self)
    }

    fn set_padding_x(&mut self, padding: usize) {
        CustomEditor::set_padding_x(self, padding as f64);
    }

    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        CustomEditor::set_autocomplete_max_visible(self, max_visible as f64);
    }

    fn set_autocomplete_provider(&mut self, provider: AutocompleteProviderObject) {
        if let Ok(provider) =
            provider.downcast::<Box<dyn kiana_tui::autocomplete::AutocompleteProvider>>()
        {
            CustomEditor::set_autocomplete_provider(self, *provider);
        }
    }

    fn set_border_color(&mut self, color: Option<EditorBorderColor>) {
        self.editor.border_color = match color {
            Some(color) => Arc::from(color),
            None => Arc::new(|text: &str| text.to_string()),
        };
    }

    fn border_color(&self, text: &str) -> String {
        (self.editor.border_color)(text)
    }
}
