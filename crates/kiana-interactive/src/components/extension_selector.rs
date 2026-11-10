//! components/extension-selector.ts。
//!
//! 扩展 `ui.select()` 的字符串选项列表（up/down **clamp**，不环绕）。
//!
//! ## 与 TS 的表示差异
//!
//! * `opts.tui` 收窄为 `Option<RequestRenderCallback>`（CountdownTimer 的重绘句柄）。
//! * 不实现 `Focusable`：本组件只负责渲染选项与分发按键，焦点由宿主统一管理。
//!
//! ## 被谁调用
//!
//! 三个宿主路径：扩展 `ui.select()`（`interactive_mode/extension_ui` 的
//! `begin_extension_selector`）、扩展 `ui.confirm()` 的布尔选择（同一函数，布尔映射在
//! 宿主 `resolve_responder` 里完成）、以及登录流程的临时选项提示
//! （`interactive_mode/commands/auth.rs`）与 `/bug` 的交付方式选择（`bug_report.rs`）。
//! 交互模式一侧的对话框都注册了 dispose（取消倒计时）。
//!
//! ## 设计原因
//!
//! * 选项行不是一次性画死的静态内容：每按一次 ↑/↓ 都要重画高亮，所以行所在的
//!   `Container` 也放进 `Rc<RefCell<>>`（容器树里挂 `ListProxy` 代理），
//!   `update_list` 每次清空重建全部行。选项数量是扩展给的小列表，整表重建足够。
//! * 与 `extension_input` 相同的共享动机：倒计时改写标题、超时触发取消，因此
//!   `title_text` 与 `on_cancel` 都是 `Rc` 共享句柄。
//! * `selected_index` 用 `isize`：空列表（`options.len() == 0`）时按 ↓ 会得到
//!   `min(-1, 1) = -1`（TS 的 `Math.min(this.options.length - 1, ...)` 同样产生 -1），
//!   有符号类型才能容纳这个值。
//!
//! ## 执行流程（`handle_input`）
//!
//! 1. `app.tools.expand` → 调用宿主注入的 `on_toggle_tools_expanded`（保持 Ctrl+O 在
//!    对话框打开时仍能展开工具输出）；未注入则静默忽略；
//! 2. `tui.select.up` / `k` → 下标减 1，`max(0, …)` 下限截断，**不环绕**；
//! 3. `tui.select.down` / `j` → 下标加 1，`min(len-1, …)` 上限截断；两种移动都会
//!    `update_list` 重画；
//! 4. `tui.select.confirm` / `\n` → 仅当选中项存在**且非空串**时才调 `on_select`
//!    （JS 空串为假，TS 的 `if (sel)` 过滤了空选项；Rust 侧用 `filter` 显式复现，
//!    否则确认空选项会意外关闭对话框）；
//! 5. `tui.select.cancel` → 调共享的 `on_cancel`。
//!
//! ## 注意事项
//!
//! * 上下键的截断是 `max`/`min`，不是取模；本组件没有「从末尾绕回开头」的行为
//!   （测试 `extension_selector_clamps_and_never_wraps` 锁定）。
//! * 倒计时与 `extension_input` 同一套规则：`timeout > 0.0` 且有 `tui` 句柄才创建，
//!   构造时立即 tick 一次（标题马上带 `(Ns)`）；`dispose` 幂等，之后 `set_timer`
//!   也不再恢复倒计时（见 `kiana-ui-kit` 的 `CountdownTimer` 文档）。
//! * 标题额外套了 `bold`；倒计时改写时同样保持粗体。

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use kiana_theme::ThemeColor;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::countdown_timer::CountdownTimer;
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::{key_hint, raw_key_hint};

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

/// `ExtensionSelectorOptions`。
///
/// `timeout` 与 `tui` 必须同时给出才会启用倒计时；只给其一与都给 `None` 等价。
#[derive(Default)]
pub struct ExtensionSelectorOptions {
    /// TS `tui`：CountdownTimer 的重绘句柄。
    pub tui: Option<RequestRenderCallback>,
    /// 超时（毫秒）；`0`/`None` 不启用倒计时。
    pub timeout: Option<f64>,
    /// `app.tools.expand` 的处理器。
    pub on_toggle_tools_expanded: Option<Box<dyn FnMut()>>,
    /// 标题下方的说明。
    pub description: Option<String>,
}

/// 共享 [`Text`] 的代理（倒计时需要原地改写标题）。
struct SharedText(Rc<RefCell<Text>>);

impl Component for SharedText {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 扩展选择器（TS `class ExtensionSelectorComponent extends Container`）。
///
/// 布局：上下边框之间是标题、可选说明、选项列表（`ListProxy`）、按键提示。选中行前缀
/// `→ ` 并用 Accent 色，未选中行前缀两个空格并用 Text 色。
pub struct ExtensionSelectorComponent {
    container: Container,
    list_container: Rc<RefCell<Container>>,
    options: Vec<String>,
    selected_index: isize,
    on_select: Box<dyn FnMut(&str)>,
    on_cancel: Rc<RefCell<Box<dyn FnMut()>>>,
    title_text: Rc<RefCell<Text>>,
    base_title: String,
    countdown: Option<CountdownTimer>,
    on_toggle_tools_expanded: Option<Box<dyn FnMut()>>,
}

impl ExtensionSelectorComponent {
    /// TS `constructor(title, options, onSelect, onCancel, opts?)`。
    ///
    /// 构造末尾调用一次 `update_list` 画出初始高亮（`selected_index == 0`；空列表时
    /// 列表区为空）。启用倒计时时构造即 tick 一次，标题立即带剩余秒数。
    pub fn new(
        title: &str,
        options: Vec<String>,
        on_select: impl FnMut(&str) + 'static,
        on_cancel: impl FnMut() + 'static,
        opts: ExtensionSelectorOptions,
    ) -> Self {
        let base_title = title.to_string();
        let on_cancel: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(Box::new(on_cancel)));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));

        let title_text = Rc::new(RefCell::new(Text::with_padding(
            fg(ThemeColor::Accent, &bold(title)),
            1,
            0,
        )));
        container.add_child(Box::new(SharedText(Rc::clone(&title_text))));
        if let Some(description) = opts
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
        {
            container.add_child(Box::new(Spacer::new()));
            container.add_child(Box::new(Text::with_padding(
                fg(ThemeColor::Text, description),
                1,
                0,
            )));
        }
        container.add_child(Box::new(Spacer::new()));

        let countdown = match (opts.timeout, &opts.tui) {
            (Some(timeout), Some(request_render)) if timeout > 0.0 => {
                let title_text = Rc::clone(&title_text);
                let base_title = base_title.clone();
                let cancel = Rc::clone(&on_cancel);
                Some(CountdownTimer::new(
                    timeout,
                    Some(Rc::clone(request_render)),
                    Box::new(move |seconds: f64| {
                        title_text.borrow_mut().set_text(fg(
                            ThemeColor::Accent,
                            &bold(&format!("{base_title} ({seconds}s)")),
                        ));
                    }),
                    Box::new(move || {
                        (cancel.borrow_mut())();
                    }),
                ))
            }
            _ => None,
        };

        let list_container = Rc::new(RefCell::new(Container::new()));
        container.add_child(Box::new(ListProxy(Rc::clone(&list_container))));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            raw_key_hint("↑↓", "navigate")
                + "  "
                + &key_hint("tui.select.confirm", "select")
                + "  "
                + &key_hint("tui.select.cancel", "cancel"),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));

        let mut selector = ExtensionSelectorComponent {
            container,
            list_container,
            options,
            selected_index: 0,
            on_select: Box::new(on_select),
            on_cancel,
            title_text,
            base_title,
            countdown,
            on_toggle_tools_expanded: opts.on_toggle_tools_expanded,
        };
        selector.update_list();
        selector
    }

    /// 当前选项（只读）。
    pub fn options(&self) -> &[String] {
        &self.options
    }

    /// 当前选中下标（TS `selectedIndex`；空列表按下 ↓ 会变成 -1）。
    ///
    /// `isize` 允许 -1（见模块文档「设计原因」）；`-1` 时 `update_list` 不高亮任何行，
    /// 确认键也不触发回调（`usize::try_from` 失败）。
    pub fn selected_index(&self) -> isize {
        self.selected_index
    }

    /// 当前标题文本（已着色；测试/宿主查询用）。
    pub fn title_text(&self) -> String {
        self.title_text.borrow().text().to_string()
    }

    /// 基础标题（不含倒计时后缀）。
    pub fn base_title(&self) -> &str {
        &self.base_title
    }

    /// 安装宿主定时器（倒计时用）。
    ///
    /// 没有启用倒计时的实例是 no-op；`dispose` 之后再注入也不会恢复（见
    /// `kiana-ui-kit` 的 `CountdownTimer::set_timer`）。
    pub fn set_timer(&mut self, timer: Box<dyn kiana_tui::tui::Timer>) {
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.set_timer(timer);
        }
    }

    fn update_list(&mut self) {
        // 整表重建而非局部改样式：Container 的子项是按值持有的 Box，就地改一行需要
        // downcast + 重设文本；行数少，clear + 重建更简单（TS `updateList()` 同样是整表
        // 重建，见 spec/ca-interactive-components.md §2.54.4）。
        let mut list = self.list_container.borrow_mut();
        list.clear();
        for (index, option) in self.options.iter().enumerate() {
            let is_selected = index as isize == self.selected_index;
            let text = if is_selected {
                fg(ThemeColor::Accent, "→ ") + &fg(ThemeColor::Accent, option)
            } else {
                format!("  {}", fg(ThemeColor::Text, option))
            };
            list.add_child(Box::new(Text::with_padding(text, 1, 0)));
        }
    }

    /// TS `dispose()`：取消倒计时；幂等。
    pub fn dispose(&mut self) {
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.dispose();
        }
    }
}

/// 列表容器的代理（`Rc<RefCell<Container>>` 以支持原地重建列表行）。
///
/// `update_list` 在组件侧持有共享句柄重建行；容器树里只挂这个只转发 `render` /
/// `invalidate` 的代理。
struct ListProxy(Rc<RefCell<Container>>);

impl Component for ListProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

impl Component for ExtensionSelectorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        // 分发顺序固定：展开键 → 移动 → 确认 → 取消。移动分支截断而非环绕，
        // 每次都重画列表；确认分支对空串/越界下标是 no-op。
        let kb = get_keybindings();
        if kb.matches(key_data, "app.tools.expand") {
            if let Some(handler) = self.on_toggle_tools_expanded.as_mut() {
                handler();
            }
        } else if kb.matches(key_data, "tui.select.up") || key_data == "k" {
            self.selected_index = isize::max(0, self.selected_index - 1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") || key_data == "j" {
            self.selected_index =
                isize::min(self.options.len() as isize - 1, self.selected_index + 1);
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            // TS `const sel = this.options[this.selectedIndex]; if (sel) onSelectCallback(sel)`：
            // 空串在 JS 里为假，确认空选项不触发回调（选择器保持打开）。
            let selected = usize::try_from(self.selected_index)
                .ok()
                .and_then(|index| self.options.get(index))
                .filter(|selected| !selected.is_empty())
                .cloned();
            if let Some(selected) = selected {
                (self.on_select)(&selected);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel.borrow_mut())();
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}
