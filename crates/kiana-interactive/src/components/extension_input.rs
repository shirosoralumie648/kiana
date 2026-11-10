//! components/extension-input.ts。
//!
//! 扩展 `ui.input()` 的简单文本输入对话框。
//!
//! ## 与 TS 的表示差异
//!
//! * `opts.tui` 在 TS 里只供 `CountdownTimer` 请求重绘；Rust 侧收窄为
//!   `Option<RequestRenderCallback>`。
//! * `_placeholder` 在 TS 源码里完全未使用；Rust 侧保留该参数并忽略（不传给 `Input`）。
//!
//! ## 被谁调用
//!
//! 只有一个宿主路径：扩展 `ui.input()`（`interactive_mode/extension_ui` 的
//! `begin_extension_input`）。宿主把提交/取消包成 `finish_dialog_token(token, …)`，并给
//! 这个对话框注册 dispose（`DialogRuntime`）；组件被包进 `Rc<RefCell<>>` 由对话框栈共享。
//!
//! ## 设计原因
//!
//! * 倒计时需要**原地改写标题**（`{title} (3s)`），而组件树里已经放了一份标题
//!   `Text`。于是标题也走共享句柄：`SharedText` 代理挂在容器里，`CountdownTimer` 的
//!   `on_tick` 回调直接写同一份 `Rc<RefCell<Text>>`。取消回调同理共享
//!   （`Rc<RefCell<Box<dyn FnMut()>>>`），因为构造闭包与 `CountdownTimer` 的到期回调
//!   都要调用它。
//! * 倒计时是可选的：`timeout` 缺失或 `<= 0`、或没有 `tui` 重绘句柄时都不构造
//!   `CountdownTimer`，组件退化为普通输入框（`match` 的 `_` 分支）。
//!
//! ## 执行流程（`handle_input`）
//!
//! 1. `tui.select.confirm` 或 `\n` → 取 `Input` 当前值调 `on_submit` 并返回（不再转发给
//!    输入框，因此回车不会作为换行进入文本）；
//! 2. `tui.select.cancel` → 调共享的 `on_cancel`；
//! 3. 其余按键交给内部 `Input`。
//!
//! ## 注意事项
//!
//! * `on_submit`/`on_cancel` 触发后宿主会摘除整个对话框；组件内的回调各自是独立的
//!   `FnMut`，这里不负责幂等（宿主侧 `finish_dialog_token` 保证只 resolve 一次）。
//! * `dispose` 只取消倒计时，对输入框与回调无影响；幂等，可安全多次调用。
//! * `set_timer` 只有在构造时真的创建了倒计时才有效果；未启用超时的实例调用它是
//!   no-op（`countdown` 为 `None`）。

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use kiana_theme::ThemeColor;
use kiana_tui::components::input::Input;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{Component, Container, Focusable, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::countdown_timer::CountdownTimer;
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_hint;

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `ExtensionInputOptions`。
#[derive(Clone, Default)]
pub struct ExtensionInputOptions {
    /// TS `tui`：CountdownTimer 的重绘句柄。
    pub tui: Option<RequestRenderCallback>,
    /// 超时（毫秒）；`0`/`None` 不启用倒计时。
    pub timeout: Option<f64>,
    /// 初始值。
    pub initial_value: Option<String>,
    /// 标题下方的说明。
    pub description: Option<String>,
}

/// 共享 [`Text`] 的代理（倒计时需要原地改写标题）。
///
/// 容器树持代理、`CountdownTimer` 的 `on_tick` 持 `Rc<RefCell<Text>>`，两条路径写的是
/// 同一份文本；`handles_input` 恒为 false。
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

/// 共享 [`Input`] 的代理。
struct InputProxy(Rc<RefCell<Input>>);

impl Component for InputProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for InputProxy {
    fn focused(&self) -> bool {
        self.0.borrow().focused()
    }

    fn set_focused(&mut self, focused: bool) {
        self.0.borrow_mut().set_focused(focused);
    }
}

/// 扩展输入对话框（TS `class ExtensionInputComponent extends Container implements Focusable`）。
///
/// 布局：上下边框之间依次是标题（可被倒计时改写）、可选说明、单行 [`Input`]、按键提示。
/// `on_cancel` 放在 `Rc<RefCell<>>` 里是因为它被两个所有者共享：组件自身（Escape 分发）
/// 与 `CountdownTimer` 的到期回调（超时视为取消）。
pub struct ExtensionInputComponent {
    container: Container,
    input: Rc<RefCell<Input>>,
    on_submit: Box<dyn FnMut(&str)>,
    on_cancel: Rc<RefCell<Box<dyn FnMut()>>>,
    title_text: Rc<RefCell<Text>>,
    base_title: String,
    countdown: Option<CountdownTimer>,
    focused: bool,
}

impl ExtensionInputComponent {
    /// TS `constructor(title, _placeholder, onSubmit, onCancel, opts?)`。
    ///
    /// `opts.timeout`/`opts.tui` 组合决定是否创建倒计时（见 [`ExtensionInputOptions`]）：
    /// 创建时立即 `on_tick(ceil(timeout/1000))` 一次，所以启用超时的实例刚构造完标题就
    /// 已带 `(Ns)` 后缀（测试 `extension_input_timeout_updates_the_title` 锁定）。
    /// `initial_value` 非空才写入 `Input`。
    pub fn new(
        title: &str,
        _placeholder: Option<&str>,
        on_submit: impl FnMut(&str) + 'static,
        on_cancel: impl FnMut() + 'static,
        opts: ExtensionInputOptions,
    ) -> Self {
        let base_title = title.to_string();
        let on_cancel: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(Box::new(on_cancel)));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Spacer::new()));

        let title_text = Rc::new(RefCell::new(Text::with_padding(
            fg(ThemeColor::Accent, title),
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
                            &format!("{base_title} ({seconds}s)"),
                        ));
                    }),
                    Box::new(move || {
                        (cancel.borrow_mut())();
                    }),
                ))
            }
            _ => None,
        };

        let input = Rc::new(RefCell::new(Input::new(Default::default())));
        if let Some(initial_value) = &opts.initial_value
            && !initial_value.is_empty()
        {
            input.borrow_mut().set_value(initial_value.clone());
        }
        container.add_child(Box::new(InputProxy(Rc::clone(&input))));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            format!(
                "{}  {}",
                key_hint("tui.select.confirm", "submit"),
                key_hint("tui.select.cancel", "cancel")
            ),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(DynamicBorder::default()));

        ExtensionInputComponent {
            container,
            input,
            on_submit: Box::new(on_submit),
            on_cancel,
            title_text,
            base_title,
            countdown,
            focused: false,
        }
    }

    /// 当前标题文本（已着色；测试/宿主查询用）。
    pub fn title_text(&self) -> String {
        self.title_text.borrow().text().to_string()
    }

    /// 基础标题（不含倒计时后缀）。
    pub fn base_title(&self) -> &str {
        &self.base_title
    }

    /// 安装宿主定时器（倒计时用；TS 的 `setInterval` 由宿主注入）。
    ///
    /// TS 在构造时直接 `setInterval`；Rust 的 [`CountdownTimer`] 不自带驱动，需要宿主
    /// 之后注入。没有倒计时的实例调用它是 no-op。
    pub fn set_timer(&mut self, timer: Box<dyn kiana_tui::tui::Timer>) {
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.set_timer(timer);
        }
    }

    /// TS `dispose()`：取消倒计时；幂等。
    ///
    /// 不触发 `on_expire`（即不会把「超时」误报成取消回调之后的又一次取消）。
    pub fn dispose(&mut self) {
        if let Some(countdown) = self.countdown.as_mut() {
            countdown.dispose();
        }
    }
}

impl Component for ExtensionInputComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, key_data: &str) {
        // 分发顺序：确认 → 取消 → 其余交给 Input。先拦截确认/取消键，避免它们被当作
        // 文本插入；确认分支在回调前就把值取成 owned String，避免回调内部再借 `input`
        // 造成 RefCell 冲突。
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.confirm") || key_data == "\n" {
            let value = self.input.borrow().get_value().to_string();
            (self.on_submit)(&value);
        } else if kb.matches(key_data, "tui.select.cancel") {
            (self.on_cancel.borrow_mut())();
        } else {
            self.input.borrow_mut().handle_input(key_data);
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    /// TS `Container.handleMouse`：按行路由到输入框。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
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

impl Focusable for ExtensionInputComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.input.borrow_mut().set_focused(focused);
    }
}
