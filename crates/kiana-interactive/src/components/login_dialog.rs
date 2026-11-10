//! components/login-dialog.ts。
//!
//! OAuth / API-key 登录对话框：显示授权 URL、设备码确认行、手动输入与提示；
//! 取消时 abort 整个登录流程。
//!
//! ## 与 TS 的表示差异
//!
//! * TS 的 `TUI` 只用到 `requestRender()`；Rust 侧收窄为
//!   [`RequestRenderCallback`]（`Rc<dyn Fn()>`，与 `kiana-ui-kit::AuthUrlComponent` 同一约定）。
//! * TS 的 `AbortController` → [`CancellationToken`]（ARCHITECTURE §4.2）；`signal` 返回
//!   票据的克隆。
//! * TS 的 `Promise<string>`（`showManualInput` / `showPrompt`）→
//!   [`futures::channel::oneshot::Receiver`]，取消时以 [`LoginCancelledError`]
//!   （消息逐字 `Login cancelled`）结束。被后续提示顶替的旧 receiver 永远保持 pending
//!   （TS 覆盖 `inputResolver`/`inputRejecter` 后旧 Promise 永不 settle）。
//! * `copyToClipboard` / `openBrowser` 通过可注入闭包实现（TS 用 `vi.mock`）。设备码的
//!   异步复制用 [`LoginSpawn`] 投递（默认 `tokio::task::spawn_local`，与交互模式单线程
//!   `LocalSet` 一致；测试注入 `block_on` 或自行驱动）。
//!
//! ## 边界：只负责显示与收集输入
//!
//! 组件不发起网络请求，也不知道 provider 细节。上游是
//! `interactive_mode/commands/auth.rs` 的 `spawn_auth_ui_loop`：它把 runtime 推送的
//! `AuthEvent` 翻译成 `show_auth` / `show_device_code` / `show_progress` 等调用；用户
//! 提交的文本经 `show_manual_input` / `show_prompt` 返回的 receiver 交回
//! `await_dialog_receiver`。因此这些 `show_*` 方法都可以在 UI 线程上同步调用。
//!
//! ## 内容区结构与代理组件
//!
//! 上下两条 `DynamicBorder` 与标题固定不变，中间是可整体替换的 `content_container`。
//! 需要跨方法保留状态的子组件（输入框、授权 URL、设备码提示行）都以
//! `Rc<RefCell<...>>` 共享，再包成 [`Component`] 代理挂进容器：这样 `show_*` 换掉内容后，
//! 已经启动的异步任务仍能写回原来那个实例，即使它已不在渲染树里。
//!
//! ## 交互状态与失败模式
//!
//! * 设备码确认是**一次性**动作：`activate_device_code` 取走 `device_code_action`，之后再按
//!   确认键会落入输入框分支。
//! * 取消（`tui.select.cancel`）写入 [`CancellationToken`]、以 [`LoginCancelledError`] 结束
//!   当前 pending 输入，并以 `(false, Some("Login cancelled"))` 调用完成回调。`cancel` 没有
//!   幂等保护，重复按键会重复回调。
//! * 被新提示顶替的旧输入 receiver 不会收到错误，而是永远 pending（实现见
//!   `superseded_inputs` 与 [`Drop`] 实现）：调用方必须靠自己的取消 signal 退出。

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::future::Future;
use std::rc::Rc;

use futures::channel::oneshot;
use kiana_ai::auth::oauth_compat::OAuthDeviceCodeInfo;
use kiana_ai::auth::types::AuthInfoLink;
use kiana_theme::ThemeColor;
use kiana_tui::components::input::Input;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::components::truncated_text::TruncatedText;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::tui::{Component, Container, Focusable, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::auth_url::AuthUrlComponent;
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_hint;
use tokio_util::sync::CancellationToken;

/// 剪贴板复制（TS `copyToClipboard`）：`Err` 的 `String` 是 `error.message`。
///
/// 返回 `Pin<Box<dyn Future>>` 而不是 `async fn`，是为了让闭包可以被复制进异步任务并保持
/// 与 TS Promise 返回签名一致。默认实现调用
/// `kiana_core::clipboard::copy_to_clipboard`，把错误转成字符串。
pub type LoginClipboardCopy =
    Rc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>>;

/// 打开浏览器（TS `openBrowser`）：真实实现永不失败；`Err` 对应测试注入的抛错。
///
/// 组件在 `show_auth` 与设备码确认两处调用，且都忽略返回值：启动浏览器失败不会中断
/// 登录，用户仍可手动复制 URL。
pub type LoginOpenBrowser = Rc<dyn Fn(&str) -> Result<(), String>>;

/// 投递异步任务（TS 里是 `void copyToClipboard(...).then(...)` 的微任务）。
///
/// 默认实现 `tokio::task::spawn_local`；必须在单线程 `LocalSet` 上调用。
pub type LoginSpawn = Rc<dyn Fn(std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>)>;

/// 登录完成回调（TS `(success: boolean, message?: string) => void`）。
///
/// 组件只在取消路径调用它，参数固定为 `(false, Some("Login cancelled"))`；成功路径由
/// 调用方在登录流程返回后自行收尾（见 `auth.rs` 的 `create_login_dialog_with_complete`）。
pub type LoginCompleteCallback = Box<dyn FnMut(bool, Option<String>)>;

/// 输入被取消（TS `new Error("Login cancelled")`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginCancelledError;

impl fmt::Display for LoginCancelledError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Login cancelled")
    }
}

impl std::error::Error for LoginCancelledError {}

/// `showManualInput` / `showPrompt` 的返回类型。
///
/// `Err(LoginCancelledError)` 只在对话框取消时产生（`cancel` 发送）；若 sender 被更新的
/// 提示顶替，receiver 收不到任何消息而永久 pending，调用方不能只等它。
pub type LoginPromptResult = Result<String, LoginCancelledError>;

/// 按全局主题着色（TS `theme.fg(token, str)`）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `theme.bold(text)`。
fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// [`LoginClipboardCopy`] 的默认实现：`kiana_core::clipboard::copy_to_clipboard`，
/// 错误转成 `error.to_string()`。
fn default_clipboard_copy() -> LoginClipboardCopy {
    Rc::new(|text: String| {
        Box::pin(async move {
            kiana_core::clipboard::copy_to_clipboard(&text)
                .await
                .map_err(|error| error.to_string())
        })
    })
}

/// [`LoginOpenBrowser`] 的默认实现：`kiana_core::clipboard::open_browser`，恒返回 `Ok`。
fn default_open_browser() -> LoginOpenBrowser {
    Rc::new(|url: &str| {
        kiana_core::clipboard::open_browser(url);
        Ok(())
    })
}

/// [`LoginSpawn`] 的默认实现：`tokio::task::spawn_local`（须在 `LocalSet` 上调用）。
fn default_spawn() -> LoginSpawn {
    Rc::new(|future| {
        tokio::task::spawn_local(future);
    })
}

/// 异步复制任务可观察的对话框标志（TS 的 `this.focused` / `signal.aborted`）。
struct LoginShared {
    focused: Cell<bool>,
    aborted: Cell<bool>,
}

/// 设备码状态行的共享文本（TS 原地替换 `hint.children`）。
struct HintState {
    text: RefCell<String>,
    present: Cell<bool>,
}

impl HintState {
    fn new(text: String) -> Self {
        HintState {
            text: RefCell::new(text),
            present: Cell::new(true),
        }
    }
}

/// 设备码提示行的容器代理（渲染 `TruncatedText(text, 1, 0)`）。
struct HintProxy(Rc<HintState>);

impl Component for HintProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        let text = self.0.text.borrow().clone();
        TruncatedText::with_padding(text, 1, 0).render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// 共享 [`Input`] 的容器代理。
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

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
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

/// 动态内容区的容器代理（外部持有 `Rc<RefCell<Container>>` 以增删子项）。
struct ContentProxy(Rc<RefCell<Container>>);

impl Component for ContentProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        // 借用无法逃逸；焦点遍历走 `for_each_container_child`。
        None
    }

    fn for_each_container_child(&self, f: &mut dyn FnMut(&dyn Component) -> bool) {
        let content = self.0.borrow();
        for child in &content.children {
            if !f(child.as_ref()) {
                break;
            }
        }
    }

    fn for_each_container_child_mut(
        &mut self,
        f: &mut dyn FnMut(&mut (dyn Component + 'static)) -> bool,
    ) {
        let mut content = self.0.borrow_mut();
        for child in content.children.iter_mut() {
            if !f(child.as_mut()) {
                break;
            }
        }
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// 共享的 [`AuthUrlComponent`]（复制结果写回同一个实例，即使它已从内容区移除）。
type SharedAuthUrl = Rc<RefCell<AuthUrlComponent>>;

/// 授权 URL 槽位的容器代理（`showAuth` 创建、`showDeviceCode`/`showDetails` 清空）。
struct AuthUrlProxy(Rc<RefCell<Option<SharedAuthUrl>>>);

impl Component for AuthUrlProxy {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self.0.borrow().as_ref() {
            Some(auth_url) => auth_url.borrow_mut().render(width),
            None => Vec::new(),
        }
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// 设备码确认动作。
struct DeviceCodeAction {
    info: OAuthDeviceCodeInfo,
    hint: Rc<HintState>,
}

/// 登录对话框（TS `class LoginDialogComponent extends Container implements Focusable`）。
///
/// 宿主把它挂到编辑器槽位并设为聚焦（`auth.rs::mount_login_dialog`）。设备码确认、
/// 取消与提交键都由 [`Component::handle_input`] 统一分派；输入框只是内容区的一个子组件，
/// 因此焦点状态要同时写到 `shared` 与实际 [`Input`]。
pub struct LoginDialogComponent {
    container: Container,
    content_container: Rc<RefCell<Container>>,
    input: Rc<RefCell<Input>>,
    request_render: RequestRenderCallback,
    /// 当前展示的登录 URL（`app.message.copy` 复制它；设备码时清空）。
    auth_url: Rc<RefCell<Option<SharedAuthUrl>>>,
    cancel_token: CancellationToken,
    pending_input: Option<oneshot::Sender<LoginPromptResult>>,
    /// 被新提示顶替的 sender：TS 覆盖 `inputResolver` / `inputRejecter` 后旧 Promise 永不
    /// settle；保留旧 sender（从不发送）让旧 receiver 永远保持 pending。
    superseded_inputs: Vec<oneshot::Sender<LoginPromptResult>>,
    device_code_action: Option<DeviceCodeAction>,
    device_code_hint: Option<Rc<HintState>>,
    on_complete: LoginCompleteCallback,
    shared: Rc<LoginShared>,
    clipboard_copy: LoginClipboardCopy,
    open_browser: LoginOpenBrowser,
    spawn: LoginSpawn,
}

impl LoginDialogComponent {
    /// TS `constructor(tui, providerId, onComplete, providerNameOverride?, titleOverride?)`。
    ///
    /// `provider_name_override` 为空串时回退到 `provider_id`；`title_override` 为 `None`
    /// 时使用 `Login to <providerName>`。构造只搭好固定外壳（边框、标题、代理内容区），
    /// 不触发渲染；后续 `show_*` 才改变内容。
    pub fn new(
        request_render: RequestRenderCallback,
        provider_id: &str,
        on_complete: LoginCompleteCallback,
        provider_name_override: Option<&str>,
        title_override: Option<&str>,
    ) -> Self {
        // `providerNameOverride || providerId`：空串同样回退。
        let provider_name = match provider_name_override {
            Some(override_name) if !override_name.is_empty() => override_name.to_string(),
            _ => provider_id.to_string(),
        };
        // `titleOverride ?? `Login to ${providerName}``：只有 None 才回退。
        let title = title_override
            .map(str::to_string)
            .unwrap_or_else(|| format!("Login to {provider_name}"));

        let content_container = Rc::new(RefCell::new(Container::new()));
        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::default()));
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Accent, &bold(&title)),
            1,
            0,
        )));
        container.add_child(Box::new(ContentProxy(Rc::clone(&content_container))));
        container.add_child(Box::new(DynamicBorder::default()));

        LoginDialogComponent {
            container,
            content_container,
            input: Rc::new(RefCell::new(Input::new(Default::default()))),
            request_render,
            auth_url: Rc::new(RefCell::new(None)),
            cancel_token: CancellationToken::new(),
            pending_input: None,
            superseded_inputs: Vec::new(),
            device_code_action: None,
            device_code_hint: None,
            on_complete,
            shared: Rc::new(LoginShared {
                focused: Cell::new(false),
                aborted: Cell::new(false),
            }),
            clipboard_copy: default_clipboard_copy(),
            open_browser: default_open_browser(),
            spawn: default_spawn(),
        }
    }

    /// 注入剪贴板实现（测试接缝；TS 用 `vi.mock`）。
    pub fn set_clipboard_copy(&mut self, clipboard_copy: LoginClipboardCopy) {
        self.clipboard_copy = clipboard_copy;
    }

    /// 注入浏览器启动器（测试接缝）。
    pub fn set_open_browser(&mut self, open_browser: LoginOpenBrowser) {
        self.open_browser = open_browser;
    }

    /// 注入异步任务投递（测试接缝；默认 `spawn_local`）。
    pub fn set_spawn(&mut self, spawn: LoginSpawn) {
        self.spawn = spawn;
    }

    /// TS `get signal()`（同一个令牌，abort 状态全程共享）。
    ///
    /// 返回的是克隆；`cancel` 对原令牌调用 `cancel`，所有克隆同时进入取消状态，runtime
    /// 侧因此能感知用户按 Esc（`auth.rs` 用 `AbortScope::from(signal)` 包装它）。
    pub fn signal(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// 当前是否已取消（`signal.aborted`）。
    pub fn is_aborted(&self) -> bool {
        self.shared.aborted.get()
    }

    fn replace_input_with_submitted_text(&mut self, value: &str) {
        // TS：`children.map(child => child === this.input ? new Text(`> ${value}`, 0, 0) : child)`。
        let input = Rc::clone(&self.input);
        let mut content = self.content_container.borrow_mut();
        // 只替换仍挂在内容区里的同一个 `Input` 实例。TS 比较 `child === this.input`；
        // Rust 子项是代理组件，因此先 downcast 成 `InputProxy` 再用 `Rc::ptr_eq` 比较。
        for child in content.children.iter_mut() {
            let matches_input = child
                .as_any()
                .and_then(|any| any.downcast_ref::<InputProxy>())
                .is_some_and(|proxy| Rc::ptr_eq(&proxy.0, &input));
            if matches_input {
                *child = Box::new(Text::with_padding(format!("> {value}"), 0, 0));
            }
        }
    }

    fn clear_device_code_action(&mut self) {
        // 先置 `present = false` 再移除：已投递的异步复制任务通过该标志判断是否还能写文本，
        // 避免对话框已切换/关闭后仍改共享状态。随后按实例匹配移除提示行。
        if let Some(hint) = self.device_code_hint.take() {
            hint.present.set(false);
            let mut content = self.content_container.borrow_mut();
            let position = content.children.iter().position(|child| {
                child
                    .as_any()
                    .and_then(|any| any.downcast_ref::<HintProxy>())
                    .is_some_and(|proxy| Rc::ptr_eq(&proxy.0, &hint))
            });
            if let Some(index) = position {
                content.remove_child_at(index);
            }
        }
        self.device_code_action = None;
    }

    fn cancel(&mut self) {
        // 顺序有语义：先撤下设备码动作并置 aborted，再发取消，最后结束待答输入与回调。
        // 注意没有幂等保护：重复按取消会再次执行整个序列（包括完成回调）。
        self.clear_device_code_action();
        self.shared.aborted.set(true);
        self.cancel_token.cancel();
        if let Some(sender) = self.pending_input.take() {
            let _ = sender.send(Err(LoginCancelledError));
        }
        (self.on_complete)(false, Some("Login cancelled".to_string()));
    }

    /// Called by onAuth callback - show URL and optional instructions。
    ///
    /// 会**清空**内容区并重建：一个 `Spacer`、授权 URL 组件、可选的说明文本。新 URL 也写入
    /// `auth_url` 槽位，供 `app.message.copy` 使用；同时尽力 `open_browser(url)`（返回值
    /// 被忽略）。非空 `instructions` 才显示提示行。
    pub fn show_auth(&mut self, url: &str, instructions: Option<&str>) {
        self.clear_device_code_action();
        self.content_container.borrow_mut().clear();
        self.content_container
            .borrow_mut()
            .add_child(Box::new(Spacer::new()));
        *self.auth_url.borrow_mut() = Some(Rc::new(RefCell::new(AuthUrlComponent::new(
            Some(Rc::clone(&self.request_render)),
            url,
        ))));
        let auth_url = Rc::clone(&self.auth_url);
        self.content_container
            .borrow_mut()
            .add_child(Box::new(AuthUrlProxy(auth_url)));
        if let Some(instructions) = instructions.filter(|instructions| !instructions.is_empty()) {
            self.content_container
                .borrow_mut()
                .add_child(Box::new(Spacer::new()));
            self.content_container
                .borrow_mut()
                .add_child(Box::new(Text::with_padding(
                    fg(ThemeColor::Warning, instructions),
                    1,
                    0,
                )));
        }
        // TS：`openBrowser(url)`（不 await；真实实现从不抛错）。
        let _ = (self.open_browser)(url);
        (self.request_render)();
    }

    /// Called by onDeviceCode callback - show URL and user code。
    ///
    /// 先清空 `auth_url`，保证复制键不会复制上一步的授权 URL。`*_on_confirm` 为 `None`
    /// 时按 `true` 补齐。内容区替换为：验证 URL（OSC 8 超链接）、点击提示、空行、用户码，
    /// 以及（两者至少一个开启时）确认提示行。此时**不**打开浏览器、不复制，必须等用户按
    /// 确认键（`activate_device_code`）；该动作只保留一次。
    pub fn show_device_code(&mut self, info: OAuthDeviceCodeInfo) {
        // 先清引用，防止 copy 键复制旧 URL。
        *self.auth_url.borrow_mut() = None;
        // 与 TS 的 `?? true` 对齐：只在字段为 `None` 时补默认值，显式 `false` 保留。
        let info = OAuthDeviceCodeInfo {
            open_browser_on_confirm: Some(info.open_browser_on_confirm.unwrap_or(true)),
            copy_code_on_confirm: Some(info.copy_code_on_confirm.unwrap_or(true)),
            ..info
        };
        self.clear_device_code_action();
        self.content_container.borrow_mut().clear();
        self.content_container
            .borrow_mut()
            .add_child(Box::new(Spacer::new()));
        let linked_url = format!(
            "\x1b]8;;{uri}\x07{uri}\x1b]8;;\x07",
            uri = info.verification_uri
        );
        self.content_container
            .borrow_mut()
            .add_child(Box::new(Text::with_padding(
                fg(ThemeColor::Accent, &linked_url),
                1,
                0,
            )));

        let click_hint = if cfg!(target_os = "macos") {
            "Cmd+click to open"
        } else {
            "Ctrl+click to open"
        };
        let hyperlink = format!(
            "\x1b]8;;{uri}\x07{click_hint}\x1b]8;;\x07",
            uri = info.verification_uri
        );
        self.content_container
            .borrow_mut()
            .add_child(Box::new(Text::with_padding(
                fg(ThemeColor::Dim, &hyperlink),
                1,
                0,
            )));
        self.content_container
            .borrow_mut()
            .add_child(Box::new(Spacer::new()));
        self.content_container
            .borrow_mut()
            .add_child(Box::new(Text::with_padding(
                fg(
                    ThemeColor::Warning,
                    &format!("Enter code: {}", info.user_code),
                ),
                1,
                0,
            )));

        let open_browser_on_confirm = info.open_browser_on_confirm.unwrap_or(true);
        let copy_code_on_confirm = info.copy_code_on_confirm.unwrap_or(true);
        // 已取消或两个动作都关闭时不注册确认动作：此时提示行没有可执行操作。
        if !self.shared.aborted.get() && (open_browser_on_confirm || copy_code_on_confirm) {
            let action_label = if open_browser_on_confirm {
                if copy_code_on_confirm {
                    "to open browser and copy code"
                } else {
                    "to open browser"
                }
            } else {
                "to copy code"
            };
            // 单行提示同时承载进度/结果文本（窄终端里也复用同一行）。
            let hint = Rc::new(HintState::new(format!(
                "({})",
                key_hint("tui.select.confirm", action_label)
            )));
            self.content_container
                .borrow_mut()
                .add_child(Box::new(HintProxy(Rc::clone(&hint))));
            self.device_code_hint = Some(Rc::clone(&hint));
            self.device_code_action = Some(DeviceCodeAction { info, hint });
        }
        (self.request_render)();
    }

    /// TS 私有 `setDeviceCodeStatus(hint, message)`。
    fn set_device_code_status(
        shared: &LoginShared,
        hint: &HintState,
        message: &str,
        request_render: &RequestRenderCallback,
    ) {
        // 三重守卫：已取消、对话框失焦、提示行已被移除时都不写文本也不触发渲染。异步复制
        // 可能在对话框关闭之后才完成，这个检查避免它把结果写到不该再显示的 UI 上。
        if shared.aborted.get() || !shared.focused.get() || !hint.present.get() {
            return;
        }
        *hint.text.borrow_mut() = fg(ThemeColor::Dim, message);
        request_render();
    }

    /// TS 私有 `activateDeviceCode(info, hint)`：同步改状态，异步复制、尽力打开浏览器。
    fn activate_device_code(&mut self, info: &OAuthDeviceCodeInfo, hint: &Rc<HintState>) {
        let copy_code_on_confirm = info.copy_code_on_confirm.unwrap_or(true);
        let open_browser_on_confirm = info.open_browser_on_confirm.unwrap_or(true);
        // 先同步更新提示文本，让按键后立即有反馈；复制任务完成后才覆盖为结果文本。
        Self::set_device_code_status(
            &self.shared,
            hint,
            if copy_code_on_confirm {
                "Copying code..."
            } else {
                "Browser open requested"
            },
            &self.request_render,
        );

        if copy_code_on_confirm {
            let copier = Rc::clone(&self.clipboard_copy);
            let hint = Rc::clone(hint);
            let shared = Rc::clone(&self.shared);
            let request_render = Rc::clone(&self.request_render);
            let code = info.user_code.clone();
            let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> =
                Box::pin(async move {
                    let message = match copier(code).await {
                        Ok(()) => "Code copied to clipboard",
                        Err(_) => "Copy code manually",
                    };
                    Self::set_device_code_status(&shared, &hint, message, &request_render);
                });
            (self.spawn)(future);
        }

        if open_browser_on_confirm {
            // `new URL()` + protocol 白名单；解析失败/启动器失败都不得中断登录。
            // 只放行 http(s)，其它 scheme（或非法 URL）直接跳过打开动作。
            if let Ok(url) = url::Url::parse(&info.verification_uri)
                && (url.scheme() == "https" || url.scheme() == "http")
            {
                let _ = (self.open_browser)(url.as_str());
            }
        }
    }

    /// Show input for manual code/URL entry (for callback server providers)。
    ///
    /// 与其他 `show_*` 不同，这里**追加**内容而不是替换：先清空输入框文本，再追加提示、
    /// 输入框与取消提示。返回的 receiver 对应一次新的待答提示；若之前还有未回答的提示，
    /// 它的 receiver 会永久 pending（见 [`LoginPromptResult`]）。
    pub fn show_manual_input(&mut self, prompt: &str) -> oneshot::Receiver<LoginPromptResult> {
        self.clear_device_code_action();
        self.input.borrow_mut().set_value("");
        self.push_content(Box::new(Spacer::new()));
        self.push_content(Box::new(Text::with_padding(
            fg(ThemeColor::Dim, prompt),
            1,
            0,
        )));
        self.push_input();
        self.push_content(Box::new(Text::with_padding(
            format!("({})", key_hint("tui.select.cancel", "to cancel")),
            1,
            0,
        )));
        (self.request_render)();
        self.pending_receiver()
    }

    /// Called by onPrompt callback - show prompt and wait for input。
    ///
    /// 注意：**不**清空内容，追加到已有内容（保留 `showAuth` 的 URL）。
    ///
    /// 空 `placeholder` 不显示示例行。返回 receiver 的语义与
    /// [`show_manual_input`](LoginDialogComponent::show_manual_input) 相同：每次调用顶替
    /// 上一个待答提示。
    pub fn show_prompt(
        &mut self,
        message: &str,
        placeholder: Option<&str>,
    ) -> oneshot::Receiver<LoginPromptResult> {
        self.clear_device_code_action();
        self.push_content(Box::new(Spacer::new()));
        self.push_content(Box::new(Text::with_padding(
            fg(ThemeColor::Text, message),
            1,
            0,
        )));
        if let Some(placeholder) = placeholder.filter(|placeholder| !placeholder.is_empty()) {
            self.push_content(Box::new(Text::with_padding(
                fg(ThemeColor::Dim, &format!("e.g., {placeholder}")),
                1,
                0,
            )));
        }
        self.push_input();
        self.push_content(Box::new(Text::with_padding(
            format!(
                "({} {})",
                key_hint("tui.select.cancel", "to cancel,"),
                key_hint("tui.select.confirm", "to submit")
            ),
            1,
            0,
        )));

        self.input.borrow_mut().set_value("");
        (self.request_render)();
        self.pending_receiver()
    }

    fn push_content(&mut self, component: Box<dyn Component>) {
        self.content_container.borrow_mut().add_child(component);
    }

    fn push_input(&mut self) {
        let input = Rc::clone(&self.input);
        self.content_container
            .borrow_mut()
            .add_child(Box::new(InputProxy(input)));
    }

    fn pending_receiver(&mut self) -> oneshot::Receiver<LoginPromptResult> {
        let (sender, receiver) = oneshot::channel();
        // TS 直接覆盖 `inputResolver` / `inputRejecter`：被顶替的 Promise 永远不会 settle。
        // 保留旧 sender（不发送、不丢弃），旧 receiver 因此保持 pending 而不是 Canceled。
        if let Some(previous) = self.pending_input.replace(sender) {
            self.superseded_inputs.push(previous);
        }
        receiver
    }

    /// Show informational text before another login step。
    ///
    /// 与 `show_auth` 一样会**清空**内容区，并同时清空 `auth_url` 槽位；`lines` 逐行渲染，
    /// 不保留之前的 URL / 输入框。
    pub fn show_details(&mut self, lines: &[&str]) {
        *self.auth_url.borrow_mut() = None;
        self.clear_device_code_action();
        self.content_container.borrow_mut().clear();
        self.push_content(Box::new(Spacer::new()));
        for line in lines {
            self.push_content(Box::new(Text::with_padding(*line, 1, 0)));
        }
        (self.request_render)();
    }

    /// Show provider-owned information and links without starting an auth callback flow。
    ///
    /// 与 `show_details` 不同，这里**追加**内容（保留已有内容区），链接渲染成 OSC 8
    /// 超链接；`label` 为空或缺失时只显示 URL。`show_close_hint` 为真时追加取消键提示。
    /// 不修改 `auth_url`，因此之前的授权 URL 仍可被复制键取到。
    pub fn show_info(&mut self, message: &str, links: &[AuthInfoLink], show_close_hint: bool) {
        self.push_content(Box::new(Spacer::new()));
        self.push_content(Box::new(Text::with_padding(
            fg(ThemeColor::Text, message),
            1,
            0,
        )));
        for link in links {
            let text = match link.label.as_deref().filter(|label| !label.is_empty()) {
                Some(label) => format!("{label}: {}", link.url),
                None => link.url.clone(),
            };
            let hyperlink = format!("\x1b]8;;{url}\x07{text}\x1b]8;;\x07", url = link.url);
            self.push_content(Box::new(Text::with_padding(
                fg(ThemeColor::Accent, &hyperlink),
                1,
                0,
            )));
        }
        if show_close_hint {
            self.push_content(Box::new(Spacer::new()));
            self.push_content(Box::new(Text::with_padding(
                format!("({})", key_hint("tui.select.cancel", "to close")),
                1,
                0,
            )));
        }
        (self.request_render)();
    }

    /// Show waiting message (for polling flows like GitHub Copilot)。
    ///
    /// 追加一行等待文本与一行取消提示；不清空内容，便于在设备码信息下方显示。
    pub fn show_waiting(&mut self, message: &str) {
        self.push_content(Box::new(Spacer::new()));
        self.push_content(Box::new(Text::with_padding(
            fg(ThemeColor::Dim, message),
            1,
            0,
        )));
        self.push_content(Box::new(Text::with_padding(
            format!("({})", key_hint("tui.select.cancel", "to cancel")),
            1,
            0,
        )));
        (self.request_render)();
    }

    /// Called by onProgress callback。
    ///
    /// 只追加一行 Dim 文本；不替换、不清空已有内容。
    pub fn show_progress(&mut self, message: &str) {
        self.push_content(Box::new(Text::with_padding(
            fg(ThemeColor::Dim, message),
            1,
            0,
        )));
        (self.request_render)();
    }

    fn start_auth_url_copy(&mut self) {
        // TS `void this.authUrl.copy()`：URL 与提示目标在**按键时**绑定。`AuthUrlComponent.url`
        // 不可变，`copy()` 的 `this` 也是按键时那个实例；因此后续 `showDeviceCode` /
        // `showDetails` / 新的 `showAuth` 替换内容区都不会改变本次复制，复制结果只会写到
        // 那个（可能已脱离内容区的）实例上。
        let auth_url = match self.auth_url.borrow().clone() {
            Some(auth_url) => auth_url,
            None => return,
        };
        let url = auth_url.borrow().url.clone();
        // TS 同步调用 `copyToClipboard(this.url)`（首个 await 前）；这里同步构造复制 future。
        let clipboard_copy = Rc::clone(&self.clipboard_copy);
        let copy = clipboard_copy(url);
        let future: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> =
            Box::pin(async move {
                let result = copy.await;
                let mut auth_url = auth_url.borrow_mut();
                // 结果已就绪：`copy_with` 的单次 poll 即完成（不嵌套 executor）。
                let copy = auth_url.copy_with(|_| std::future::ready(result));
                let mut copy = std::pin::pin!(copy);
                let waker = std::task::Waker::noop();
                let mut context = std::task::Context::from_waker(waker);
                let _ = copy.as_mut().poll(&mut context);
            });
        (self.spawn)(future);
    }
}

impl Component for LoginDialogComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();

        // 优先级：取消 > 复制授权 URL > 设备码确认 > 输入提交 > 转发给 Input。
        // 每一支命中后立即 return，保证一次按键只产生一个副作用。
        if kb.matches(data, "tui.select.cancel") {
            self.cancel();
            return;
        }
        if self.auth_url.borrow().is_some() && kb.matches(data, "app.message.copy") {
            self.start_auth_url_copy();
            return;
        }

        // 设备码确认仅在动作仍在、没有待答输入、未取消且对话框聚焦时生效；否则按键继续
        // 走下面的输入分支。
        let device_code_action_pending = self.device_code_action.is_some()
            && self.pending_input.is_none()
            && !self.shared.aborted.get()
            && self.shared.focused.get()
            && kb.matches(data, "tui.select.confirm");
        if device_code_action_pending {
            let action = self.device_code_action.take().expect("checked above");
            self.activate_device_code(&action.info, &action.hint);
            (self.request_render)();
            return;
        }

        // TS 里 `Input.onSubmit` 在输入框消费提交键；这里复刻同一分支。
        // 只有存在待答提示时才应答；提交后把输入框替换成 `> value` 文本，pending_input
        // 变回 `None`，后续按键才会重新交给 Input。
        if self.pending_input.is_some() && (kb.matches(data, "tui.input.submit") || data == "\n") {
            let value = self.input.borrow().get_value().to_string();
            self.replace_input_with_submitted_text(&value);
            if let Some(sender) = self.pending_input.take() {
                let _ = sender.send(Ok(value));
            }
            return;
        }

        self.input.borrow_mut().handle_input(data);
    }

    fn handles_input(&self) -> bool {
        true
    }

    /// TS `Container.handleMouse`：按行路由到内容区（授权 URL / Input）。
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

impl Focusable for LoginDialogComponent {
    fn focused(&self) -> bool {
        self.shared.focused.get()
    }

    fn set_focused(&mut self, focused: bool) {
        // `this._focused = value; this.input.focused = value;`
        self.shared.focused.set(focused);
        self.input.borrow_mut().set_focused(focused);
    }
}

impl Drop for LoginDialogComponent {
    fn drop(&mut self) {
        // 被顶替的提示在 TS 里永不 settle（Promise 丢失了 resolver/rejecter）；即使对话框
        // 销毁，receiver 也必须保持 pending，因此故意不丢弃这些 sender。
        for sender in std::mem::take(&mut self.superseded_inputs) {
            std::mem::forget(sender);
        }
    }
}

// 只锁定错误消息文案；对话框行为的完整覆盖在 `tests/components_login_dialog.rs`
//（含 auth-url-copy 等 TS 测试的移植，用注入的剪贴板/浏览器/spawn 驱动）。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_error_message_is_exact() {
        assert_eq!(LoginCancelledError.to_string(), "Login cancelled");
    }
}
