//! `extensions/mcp/ui.ts`：`/mcp` 管理器视图（连接过程中可重建的菜单、只读状态页、登录页）。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/ui.ts`。
//!
//! # 与 TS 的表示差异
//!
//! - TS 的 `TUI` 参数换成 [`RequestRenderCallback`]（与 `kiana_ui_kit::auth_url` 相同的适配）：
//!   TUI 适配层只把 `requestRender()` 取出，组件不持有 TUI 本身。
//! - TS 的 Promise 由 [`futures::channel::oneshot`] 复刻；`menu`/`redirectUrl` 的
//!   `inputHandler` 存在视图的共享状态里，`handleInput` 触发它。
//! - `Theme` 在 Rust 里不是 `Clone`，而 `CustomFactory` 只把 `&Theme` 传进来、组件又必须
//!   `'static`：视图在构造时把用到的 `Theme::fg`/`Theme::bold` ANSI 序列快照成
//!   `ThemeSnapshot`（`pub(crate)`，不对外导出），因此 `show_mcp_manager` 使用宿主传入的
//!   主题（而不是全局当前主题）。
//! - 登录页的 URL 复制通过可注入的 [`UrlCopier`] 实现（TS 用 `vi.mock` 替换
//!   `copyToClipboard`）。缺省实现仍是 [`AuthUrlComponent::copy`]。
//! - `show_mcp_manager` 的 `manage` future 允许返回 `Result<(), String>`：TS 的
//!   `Promise<void>` 只有 reject 一条错误通道，Rust 用 `Err(message)` 表示同一路径
//!   （`ctx.ui.notify(message, "error")` 后关闭视图）。
//!
//! # 为什么存在
//!
//! `/mcp` 的状态机、连接生命周期与全部文案都在 `mcp/index.rs`；本文件只把状态渲染成
//! TUI 组件（可重建的菜单、只读状态页、登录页），不维护服务器状态、不读写配置。
//! `index.rs` 的菜单与登录流程只调用 [`McpUi`] 的三个方法（`show_tools`/`choose_exposure`
//! 接收 `&dyn McpUi`，主循环持有具体的 [`McpManagerView`]），表现层的契约集中在 trait 上。
//!
//! # 谁在使用
//!
//! - `mcp/index.rs` 的 `show_manager`（TUI 模式下无参数的 `/mcp`）调用
//!   [`show_mcp_manager`] 挂载本视图，随后 `manage_loop` 反复 await [`McpUi::menu`]。
//! - 同文件的 `sign_in_with_ui` 在登录流程里调用 [`McpUi::status`] 与
//!   [`McpUi::redirect_url`]；`show_tools` / `choose_exposure` 只用 [`McpUi::menu`]。
//! - 集成测试 `tests/mcp_manager_view.rs`（状态页取消键）与 `tests/auth_url_copy.rs`
//!   （登录页复制授权 URL）直接构造 [`McpManagerView`]。
//!
//! # 一次典型流程
//!
//! 1. [`show_mcp_manager`] 把 `manage` 存进 `Mutex<Option<_>>` 并注册 `CustomFactory`；
//!    宿主调用工厂时取走 `manage`、同步捕获主题快照，返回一个 `'static` future。
//! 2. 该 future 构造 [`McpManagerView`]（初始为 “MCP servers” + “Loading…”），用
//!    `spawn_local` 运行 `manage(view)`，最后把组件作为 future 的输出交给宿主；`manage`
//!    返回后调用 `done`。
//! 3. `manage` 调 [`McpUi::menu`]：先同步渲染一次，再订阅服务器变化；选择/取消经 `oneshot`
//!    解析为 `Some(value)` / `None`。订阅通知会重建菜单并尽量保留上次选中项。
//! 4. 登录流程用 [`McpUi::status`] 显示进行中页面，用 [`McpUi::redirect_url`] 显示授权 URL、
//!    等待粘贴回调 URL 或被 `signal` 中止。
//!
//! # 注意事项
//!
//! - 可变状态都在 `Rc<RefCell<_>>` 里，只能在单线程 UI 的 `LocalSet` 上使用；
//!   [`McpManagerView`] 的 `Clone` 共享同一份状态，克隆可以交给本线程的 `spawn_local` 任务。
//! - 会回调到视图自身或用户闭包的地方，`RefCell` 借用都先释放：输入处理器可能回调
//!   `set_content`（内部 `borrow_mut`），在持有借用时调用它会 panic。
//! - `menu`/`redirect_url` 的 future 由 `oneshot` 完成；接收端把 `Ok(None)`（取消）与
//!   `Err`（发送端随菜单状态一起被释放）都折叠为 `None`，调用方无法区分二者。
//! - 框架文字（边框、标题、details/error/empty/status）使用构造时捕获的主题快照；但
//!   `SelectList` 的配色来自全局 `get_select_list_theme()`，`key_hint` 的键位文本与配色也
//!   读 `kiana_tui`/`kiana_theme` 的全局表。宿主主题/键位与全局值不一致时，列表与提示会
//!   显示全局值。
//! - 面向用户的文案（“Loading…”、“Nothing to show.”、登录页说明与键位提示标签）是当前稳定契约；
//!   修改会同时改变 `/mcp` 的输出与测试断言。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use futures::channel::oneshot;
use kiana_config::keybindings::KeybindingsManager;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::types::ExtensionCommandContextApi;
use kiana_ext_api::ui::{CustomFactory, NotifyKind};
use kiana_theme::ThemeColor;
use kiana_theme::theme::{Theme, get_select_list_theme};

use crate::theme_snapshot::ThemeSnapshot;
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::text::{truncate_to_width, visible_width};
use kiana_tui::tui::{Component, Container, Focusable, Tui, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::auth_url::AuthUrlComponent;
use kiana_ui_kit::dynamic_border::DynamicBorder;
use kiana_ui_kit::keybinding_hints::key_hint;
use tokio_util::sync::CancellationToken;

/// 输入处理器（TS `inputHandler`）。
///
/// 只在 UI 线程、由 [`McpManagerView`] 的 `Component::handle_input` 调用；参数是一条完整的
/// 按键序列（与 `KeybindingsManager::matches` 的约定一致，不做前缀匹配）。调用方必须先把它
/// 从 `RefCell` 里克隆出来并释放借用，处理器内部才能安全地换页（`set_content` 需要
/// `borrow_mut`）。
pub type InputHandler = Rc<dyn Fn(&str)>;

/// 输入目标（TS `inputTarget` 的 `Focusable`）。
///
/// 当前页面里接受键盘输入的组件（登录页是输入框）。`set_content` 换页时先给旧目标解除
/// 焦点，再按视图当前的焦点状态设置新目标，因此焦点不会随页面切换丢失。
pub type InputTarget = Rc<RefCell<Input>>;

/// 菜单（TS `McpMenu`）。
///
/// 由 [`McpMenuBuilder`] 在每次渲染前现算；本结构只携带文本与条目，不持有组件。渲染时
/// `details` 用 muted、`error` 用 error 色、`empty` 用 muted；条目被克隆进一个新的
/// [`SelectList`]，因此重建菜单不会继承旧列表的内部状态，只按 `selected` 恢复选中项。
#[derive(Debug, Clone, Default)]
pub struct McpMenu {
    /// 标题（由 `frame` 用 accent 色 + 粗体渲染）。
    pub title: String,
    /// 标题下方的说明（muted；可含换行，`Text` 会按宽度再换行）。
    pub details: Option<String>,
    /// 说明下方、以 error 颜色显示的文本。
    pub error: Option<String>,
    /// 条目；为空时显示 `empty`，且只有取消键有效。
    pub items: Vec<SelectItem>,
    /// 没有条目时显示。
    pub empty: Option<String>,
    /// 首次打开时选中的条目 value；重建菜单时若已有上一次的选中值，则以那一个为准。
    pub selected: Option<String>,
    /// 确认键的动作文案（键位提示用）。
    pub confirm_label: String,
    /// 取消键的动作文案（键位提示用）。
    pub cancel_label: String,
}

/// 菜单构建器（TS `build: () => McpMenu`）。每次订阅回调都会重新调用。
///
/// 在 UI 线程同步调用：首次渲染一次，之后每次订阅通知一次。实现里不能 `await`，也不要
/// 假设它只被调用一次——两次调用之间服务器状态可能已经改变。
pub type McpMenuBuilder = Rc<dyn Fn() -> McpMenu>;

/// 订阅钩子（TS `subscribe?: (listener) => () => void`）。
///
/// `listener` 在每次变化时重建菜单；返回值取消订阅。`menu` 在首次渲染之后才注册监听，
/// 并在收到结果时调用一次退订（退订函数随后从共享状态中取走）。监听器可能被同步调用
/// （包括注册过程中），已 settle 的菜单会忽略之后的调用。
pub type McpMenuSubscribeFn = Rc<dyn Fn(Rc<dyn Fn()>) -> Rc<dyn Fn()>>;

/// 登录页 URL 的复制实现（测试注入点）。`Err` 的 `Display` 必须逐字等于 TS 的
/// `error.message`；成功时 `AuthUrlComponent` 显示 `Copied URL to clipboard`。
///
/// 由 `start_copy` 在 `spawn_local` 任务里调用，参数是授权 URL 的副本；future 完成前视图
/// 仍会渲染 URL（实现拿不到视图，也不会阻塞借用）。`None` 时退回
/// `kiana_core::clipboard::copy_to_clipboard`。
pub type UrlCopier = Rc<dyn Fn(String) -> LocalBoxFuture<'static, Result<(), String>>>;

/// `/mcp` 的 UI 抽象（TS `McpUi`）。
///
/// `menu` 与 `redirect_url` 是“渲染 + 等待用户”的本地异步接口：返回的 [`LocalBoxFuture`]
/// 不是 `Send`，必须在 UI 线程的 `LocalSet` 上等待（实现用 `Rc` 与 `spawn_local`）；
/// `status` 只渲染不等待（同步方法）。目前只有
/// [`McpManagerView`] 实现本 trait；非 TUI 路径直接使用 `ctx.ui()`，不经过它。
pub trait McpUi {
    /// 显示菜单，返回选中条目的 value；取消时 `None`。`subscribe` 在每次变化时重建菜单并保留选中项。
    ///
    /// 首次渲染在返回 future 之前同步完成，订阅在首次渲染之后注册。future 在用户选择/取消
    /// 时由 `oneshot` 完成；接收端把发送端丢失也折叠为 `None`。
    fn menu(
        &self,
        build: McpMenuBuilder,
        subscribe: Option<McpMenuSubscribeFn>,
    ) -> LocalBoxFuture<'static, Option<String>>;

    /// 显示操作进行中的消息。有 `on_cancel` 时取消键调用它。
    ///
    /// 没有 `on_cancel` 时不显示 footer、也不安装输入处理器（按键被忽略）；有它时 footer
    /// 显示固定标签 `cancel`。本方法不等待结果，页面会一直保留到下一次换页；它也不自行
    /// 取消任何 token——取消语义完全由传入的闭包决定。
    fn status(&self, title: &str, message: &str, on_cancel: Option<Rc<dyn Fn()>>);

    /// 显示授权 URL 并等待粘贴的重定向 URL。取消或 `signal` 中止时返回 `None`。
    ///
    /// 返回值是输入框内容经 JS 语义 `trim` 后的非空字符串；内容为空时按确认键不会结束
    /// 页面。确认/取消/复制分别匹配 `tui.select.confirm`/`tui.select.cancel`/
    /// `app.message.copy`，其余按键进入输入框。`signal` 已取消时立即返回 `None`，不安装
    /// 页面。
    fn redirect_url(
        &self,
        title: &str,
        authorization_url: &str,
        signal: CancellationToken,
    ) -> LocalBoxFuture<'static, Option<String>>;
}

/// `body` 里共享同一个 `AuthUrlComponent` 的代理组件。
///
/// 组件始终挂载（复制期间也不取出，见 [`McpManagerView::start_copy`]）；`Option` 只是
/// 与 [`RedirectInput`] 相同的共享槽形态。
///
/// 需要共享槽是因为 `Container` 只保存 `Box<dyn Component>`，没有可用来回收具体组件的
/// 句柄；而复制完成的任务必须拿到同一个组件改写提示行。`handles_input` 返回 `false`
/// 复刻 TS `AuthUrlComponent` 没有 `handleInput` 的事实。
struct SharedAuthUrl(Rc<RefCell<Option<AuthUrlComponent>>>);

impl Component for SharedAuthUrl {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self.0.borrow_mut().as_mut() {
            Some(component) => component.render(width),
            None => Vec::new(),
        }
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0
            .borrow_mut()
            .as_mut()
            .and_then(|component| component.handle_mouse(event))
    }

    fn invalidate(&mut self) {
        if let Some(component) = self.0.borrow_mut().as_mut() {
            component.invalidate();
        }
    }
}

/// `body` 里共享同一个 `SelectList` 的代理组件。
///
/// 与 [`SharedAuthUrl`] 同理：列表被放进 `Container` 后，选择回调与输入处理器仍需要同一个
/// `Rc<RefCell<SelectList>>`（`Container` 只保存 `Box<dyn Component>`，无法取回具体组件）。
struct SharedSelectList(Rc<RefCell<SelectList>>);

impl Component for SharedSelectList {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.0.borrow_mut().handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// `body` 里共享同一个 `Input` 的代理组件。
///
/// 登录页的按键处理器与 `InputTarget` 都持有同一个 `Rc<RefCell<Input>>`；本代理负责把
/// `Component`/`Focusable` 的调用转发给内部输入框，并在 [`Component::as_focusable`]
/// 返回自身，让宿主的焦点遍历可以穿过代理。
struct SharedInput(Rc<RefCell<Input>>);

impl Component for SharedInput {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        Component::handle_input(&mut *self.0.borrow_mut(), data);
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for SharedInput {
    fn focused(&self) -> bool {
        Focusable::focused(&*self.0.borrow())
    }

    fn set_focused(&mut self, focused: bool) {
        Focusable::set_focused(&mut *self.0.borrow_mut(), focused);
    }
}

/// `frame(theme, title, body, footer?)`：上下边框 + accent 粗体标题 + 可选 dim footer。
///
/// 子项顺序就是渲染顺序：上边框 → 标题（左右各留 1 列）→ `body` 原样接上 →（有 footer
/// 时）1 空行 + dim footer → 下边框。两个边框的着色闭包各持有一份 `ThemeSnapshot` 克隆，
/// 因为 `DynamicBorder::from_fn` 要求 `'static + Send + Sync` 的函数，不能借用视图。
fn frame(
    theme: &ThemeSnapshot,
    title: &str,
    body: Vec<Box<dyn Component>>,
    footer: Option<&str>,
) -> Container {
    let mut container = Container::new();
    let top_theme = theme.clone();
    container.add_child(Box::new(DynamicBorder::from_fn(move |text: &str| {
        top_theme.fg(ThemeColor::Accent, text)
    })));
    container.add_child(Box::new(Text::with_padding(
        theme.fg(ThemeColor::Accent, &theme.bold(title)),
        1,
        0,
    )));
    for child in body {
        container.add_child(child);
    }
    if let Some(footer) = footer {
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            theme.fg(ThemeColor::Dim, footer),
            1,
            0,
        )));
    }
    let bottom_theme = theme.clone();
    container.add_child(Box::new(DynamicBorder::from_fn(move |text: &str| {
        bottom_theme.fg(ThemeColor::Accent, text)
    })));
    container
}

/// `MAX_VISIBLE_ITEMS`。
///
/// 列表滚动窗口的可见行数上限（TS 同名常量，12）；只影响窗口大小，不裁剪 `items`，
/// 所有条目仍可被方向键选中。
const MAX_VISIBLE_ITEMS: usize = 12;

/// 当前内容与输入路由。
///
/// `content` 是整棵页面树；`input_handler`/`input_target` 只有在当前页面需要按键时才存在。
/// 三者必须一起替换，`set_content` 负责其中的焦点交接。
struct ViewState {
    content: Container,
    input_handler: Option<InputHandler>,
    input_target: Option<InputTarget>,
    focused: bool,
}

/// [`McpManagerView`] 的共享状态。
///
/// 字段在构造后不再替换；页面内容只在 `state: RefCell<ViewState>` 内部替换。克隆视图只
/// 复制外层的 `Rc`，因此 `request_render`/主题快照/键位表在所有克隆间共享。
struct ViewInner {
    request_render: RequestRenderCallback,
    theme: ThemeSnapshot,
    keybindings: Arc<KeybindingsManager>,
    state: RefCell<ViewState>,
    url_copier: Option<UrlCopier>,
}

/// 一个仍在等待结果、或已重新渲染的菜单。
///
/// `sender` 与 `unsubscribe` 都用 `Option` + `take`：`finish_menu` 是幂等的，第二次调用
/// 看不到它们；`selected` 跨越多次重建保留，重建时优先于新 `McpMenu` 的初始值。
struct ActiveMenu {
    settled: bool,
    sender: Option<oneshot::Sender<Option<String>>>,
    unsubscribe: Option<Rc<dyn Fn()>>,
    build: McpMenuBuilder,
    selected: Option<String>,
}

/// 登录页等待中的重定向 URL 输入。
///
/// `done` 是内部取消令牌：页面因提交/取消而结束时由 `finish_redirect` 取消它，让
/// `redirect_url` 里同时等待 `signal` 的任务退出。
struct RedirectInput {
    settled: bool,
    sender: Option<oneshot::Sender<Option<String>>>,
    done: CancellationToken,
}

/// `McpManagerView`：`/mcp` 的组件（TS 类同名）。
///
/// 同时实现 [`McpUi`]（菜单/状态页/登录页）、`Component`（渲染与按键路由）与
/// `Focusable`（焦点转发）。`Clone` 只复制外层的 `Rc`，因此克隆共享同一份状态；只能在
/// 单线程 UI 上使用：内部有 `Rc<dyn Fn>`、`Rc<RefCell<Input>>` 与 `spawn_local` 任务。
#[derive(Clone)]
pub struct McpManagerView {
    inner: Rc<ViewInner>,
}

impl McpManagerView {
    /// TS `new McpManagerView(tui, theme, keybindings)`。
    ///
    /// `request_render` 是 `tui.requestRender()` 的等价物；`url_copier` 是测试注入点，
    /// `None` 时使用 `kiana_core::clipboard::copy_to_clipboard`（[`AuthUrlComponent::copy`]
    /// 的实现）。
    ///
    /// 构造时立即捕获 `theme` 的快照（组件必须 `'static`，不能借用 `&Theme`）；初始内容
    /// 是 “MCP servers” 边框加 “Loading…”。此后每次 `set_content` 与每次 `handle_input`
    /// 都会调用一次 `request_render`。仓库内的生产路径由 `show_mcp_manager` 使用
    /// `from_snapshot`，`new` 目前只被集成测试直接调用。
    pub fn new(
        request_render: RequestRenderCallback,
        theme: Arc<Theme>,
        keybindings: Arc<KeybindingsManager>,
        url_copier: Option<UrlCopier>,
    ) -> Self {
        Self::from_snapshot(
            request_render,
            ThemeSnapshot::capture(&theme),
            keybindings,
            url_copier,
        )
    }

    /// 测试入口：用借用的 `&Theme` 构造视图（生产路径由 `show_mcp_manager` 直接调
    /// `from_snapshot`）。
    ///
    /// `#[cfg(test)]`：只有本文件的
    /// `manager_view_uses_the_theme_passed_to_the_factory` 使用它，用来验证视图采用宿主传入
    /// 的主题而不是全局当前主题；快照仍然在构造时捕获。
    #[cfg(test)]
    fn from_theme(
        request_render: RequestRenderCallback,
        theme: &Theme,
        keybindings: Arc<KeybindingsManager>,
        url_copier: Option<UrlCopier>,
    ) -> Self {
        Self::from_snapshot(
            request_render,
            ThemeSnapshot::capture(theme),
            keybindings,
            url_copier,
        )
    }

    /// 用已捕获的主题快照构造视图；初始页面固定为 “MCP servers” + “Loading…”。
    ///
    /// 三个构造入口（[`McpManagerView::new`]、测试用 `from_theme`、[`show_mcp_manager`]
    /// 的工厂）都汇到这里，保证初始内容与焦点状态一致。
    fn from_snapshot(
        request_render: RequestRenderCallback,
        theme: ThemeSnapshot,
        keybindings: Arc<KeybindingsManager>,
        url_copier: Option<UrlCopier>,
    ) -> Self {
        let content = frame(
            &theme,
            "MCP servers",
            vec![Box::new(Text::with_padding(
                theme.fg(ThemeColor::Muted, "Loading…"),
                1,
                1,
            ))],
            None,
        );
        McpManagerView {
            inner: Rc::new(ViewInner {
                request_render,
                theme,
                keybindings,
                state: RefCell::new(ViewState {
                    content,
                    input_handler: None,
                    input_target: None,
                    focused: false,
                }),
                url_copier,
            }),
        }
    }

    /// `setContent(content, inputHandler?, inputTarget?)`。
    ///
    /// 顺序：给旧 `input_target` 解除焦点 → 替换内容/处理器/目标 → 按 `focused` 给新目标
    /// 设置焦点 → 请求重绘。先显式 `drop(state)` 再调用 `request_render`，避免外部回调
    /// 重入视图时再次借用同一个 `RefCell`。旧页面在这里被丢弃。
    fn set_content(
        &self,
        content: Container,
        input_handler: Option<InputHandler>,
        input_target: Option<InputTarget>,
    ) {
        let mut state = self.inner.state.borrow_mut();
        if let Some(target) = state.input_target.take() {
            Focusable::set_focused(&mut *target.borrow_mut(), false);
        }
        state.content = content;
        state.input_handler = input_handler;
        state.input_target = input_target;
        if let Some(target) = &state.input_target {
            Focusable::set_focused(&mut *target.borrow_mut(), state.focused);
        }
        drop(state);
        (self.inner.request_render)();
    }

    /// 重建菜单组件树（TS `render()`）。
    ///
    /// 阶段：
    /// 1. 先克隆 `build` 再调用，避免在借用 `ActiveMenu` 时执行外部构建器；
    /// 2. 选中值优先取上一次渲染记录的 `selected`，否则用本次 `McpMenu::selected`；
    /// 3. 依次拼 details（muted）、error（error 色）、空行、列表或 empty 文案；
    /// 4. 安装回调：确认 → `finish_menu(Some(value))`、取消 → `finish_menu(None)`、
    ///    选中变化 → 记录 `selected`；
    /// 5. `set_content` 换页并请求重绘。
    ///
    /// 条目为空时只安装取消键（没有可选项，确认键不产生结果），且不设置 `input_target`。
    /// 每次重建都创建新的 `SelectList`，列表内部状态不保留；选中项按 value 在条目中查找，
    /// 找不到（条目被删）时回落到下标 0。
    fn render_menu(&self, menu: &Rc<RefCell<ActiveMenu>>) {
        // 先克隆 builder 再调用：build 可能去读服务器状态，不能持有 ActiveMenu 的借用。
        let built = {
            let build = Rc::clone(&menu.borrow().build);
            build()
        };
        // 重建时优先保留上一次的选中值；只有首次渲染才用 builder 给的默认值。
        let wanted = menu
            .borrow()
            .selected
            .clone()
            .or_else(|| built.selected.clone());
        let theme = self.inner.theme.clone();
        let mut body: Vec<Box<dyn Component>> = Vec::new();
        if let Some(details) = &built.details {
            body.push(Box::new(Text::with_padding(
                theme.fg(ThemeColor::Muted, details),
                1,
                0,
            )));
        }
        if let Some(error) = &built.error {
            body.push(Box::new(Text::with_padding(
                theme.fg(ThemeColor::Error, error),
                1,
                0,
            )));
        }
        body.push(Box::new(Spacer::new()));
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", &built.confirm_label),
            key_hint("tui.select.cancel", &built.cancel_label)
        );
        // 空列表：只显示 empty 文案与取消提示，确认键没有可确认的对象。
        if built.items.is_empty() {
            let empty = built
                .empty
                .clone()
                .unwrap_or_else(|| "Nothing to show.".to_string());
            body.push(Box::new(Text::with_padding(
                theme.fg(ThemeColor::Muted, &empty),
                1,
                0,
            )));
            let menu_state = Rc::clone(menu);
            let keybindings = Arc::clone(&self.inner.keybindings);
            let handler: Rc<dyn Fn(&str)> = Rc::new(move |data: &str| {
                if keybindings.matches(data, "tui.select.cancel") {
                    finish_menu(&menu_state, None);
                }
            });
            let content = frame(
                &theme,
                &built.title,
                body,
                Some(&key_hint("tui.select.cancel", &built.cancel_label)),
            );
            self.set_content(content, Some(handler), None);
            return;
        }
        // 视口高度 = min(条目数, 12)；SelectList 自己按选中下标滚动，不裁剪 items。
        let mut list = SelectList::new(
            built.items.clone(),
            built.items.len().min(MAX_VISIBLE_ITEMS),
            get_select_list_theme(),
            SelectListLayoutOptions::default(),
        );
        // 按 value 恢复选中项；wanted 指向已被删除的条目时保持默认下标 0。
        if let Some(index) = built
            .items
            .iter()
            .position(|item| Some(&item.value) == wanted.as_ref())
        {
            list.set_selected_index(index as isize);
        }
        // 回写当前选中值（可能是回落后的第一个条目），供下一次重建恢复。
        if let Some(item) = list.get_selected_item() {
            menu.borrow_mut().selected = Some(item.value.clone());
        }
        let list = Rc::new(RefCell::new(list));
        {
            let menu_select = Rc::clone(menu);
            list.borrow_mut().on_select = Some(Box::new(move |item: &SelectItem| {
                finish_menu(&menu_select, Some(item.value.clone()));
            }));
            let menu_cancel = Rc::clone(menu);
            list.borrow_mut().on_cancel = Some(Box::new(move || {
                finish_menu(&menu_cancel, None);
            }));
            let menu_change = Rc::clone(menu);
            list.borrow_mut().on_selection_change = Some(Box::new(move |item: &SelectItem| {
                menu_change.borrow_mut().selected = Some(item.value.clone());
            }));
        }
        // 共享代理：列表进 body 后，输入处理器仍要拿着同一个 Rc 转发按键。
        body.push(Box::new(SharedSelectList(Rc::clone(&list))));
        let handler: Rc<dyn Fn(&str)> = Rc::new(move |data: &str| {
            list.borrow_mut().handle_input(data);
        });
        let content = frame(&theme, &built.title, body, Some(&footer));
        self.set_content(content, Some(handler), None);
    }

    /// 在后台执行 URL 复制（TS `void link.copy()`）。
    ///
    /// 组件始终留在 [`SharedAuthUrl`] 里：复制在锁外等待，完成后才短暂借出组件写入提示，
    /// 因此复制期间的任何渲染仍然包含授权 URL（TS 保持组件挂载，只改提示）。
    ///
    /// 实现要点：先借用一次取出 `url` 副本（借用不跨 `await`），再由 `spawn_local` 等待
    /// 注入的 copier；`link` 被移进任务，任务结束前组件不会被释放。copier 为 `None` 时走
    /// `kiana_core::clipboard::copy_to_clipboard`。
    fn start_copy(&self, link: Rc<RefCell<Option<AuthUrlComponent>>>) {
        let copier = self.inner.url_copier.clone();
        tokio::task::spawn_local(async move {
            // 只取 URL 的副本：RefCell 借用在这里结束，后面的 await 不会再持有它。
            let url = match link.borrow().as_ref() {
                Some(component) => component.url.clone(),
                None => return,
            };
            match copier {
                Some(copier) => apply_copy_result(&link, copier(url).await),
                None => {
                    apply_copy_result(&link, kiana_core::clipboard::copy_to_clipboard(&url).await)
                }
            }
        });
    }
}

/// 把复制结果写进提示行（`AuthUrlComponent::copy_with` 的提示逻辑）。
///
/// 传给 `copy_with` 的 future 总是立即可就绪（复制已经在锁外完成），因此这里短暂持有
/// `RefCell` 借用、不会跨真正让出的 await，渲染不会看到组件被取走。
///
/// 不变量：`result` 必须已经就绪。`block_on` 在这里只轮询一次就返回；若将来把真正的异步
/// 复制搬进来，它会阻塞 `LocalSet` 的 UI 线程，依赖同一线程推进的任务（包括渲染）都无法
/// 继续，必须先 `await` 再借用组件。
fn apply_copy_result<E: std::fmt::Display>(
    link: &Rc<RefCell<Option<AuthUrlComponent>>>,
    result: Result<(), E>,
) {
    let mut guard = link.borrow_mut();
    if let Some(component) = guard.as_mut() {
        futures::executor::block_on(component.copy_with(|_| std::future::ready(result)));
    }
}

/// 结束菜单（TS `finish(value)`）。幂等；先取消订阅再 resolve。
///
/// 先把 `unsubscribe` 与 `sender` 从共享状态里 `take` 出来并释放借用，再执行退订：退订若
/// 同步触发监听器，监听器会看到 `settled == true` 而直接返回。发送端缺失（已经结束）或
/// 接收端已被丢弃时忽略错误——结果无人接收等价于取消。
fn finish_menu(menu: &Rc<RefCell<ActiveMenu>>, value: Option<String>) {
    let (unsubscribe, sender) = {
        let mut state = menu.borrow_mut();
        if state.settled {
            return;
        }
        state.settled = true;
        (state.unsubscribe.take(), state.sender.take())
    };
    if let Some(unsubscribe) = unsubscribe {
        unsubscribe();
    }
    if let Some(sender) = sender {
        let _ = sender.send(value);
    }
}

/// 结束登录页（TS `finish(value)`）。
///
/// 与 `finish_menu` 一样幂等且先释放借用；额外取消 `done`，让 `redirect_url` 里同时等待
/// `signal` 与 `done` 的任务退出——否则该任务会一直活到 `signal` 被取消为止。
fn finish_redirect(state: &Rc<RefCell<RedirectInput>>, value: Option<String>) {
    let (done, sender) = {
        let mut state = state.borrow_mut();
        if state.settled {
            return;
        }
        state.settled = true;
        (state.done.clone(), state.sender.take())
    };
    done.cancel();
    if let Some(sender) = sender {
        let _ = sender.send(value);
    }
}

impl McpUi for McpManagerView {
    fn menu(
        &self,
        build: McpMenuBuilder,
        subscribe: Option<McpMenuSubscribeFn>,
    ) -> LocalBoxFuture<'static, Option<String>> {
        // 每个菜单一份独立状态与 oneshot；选择/取消时 sender 被消费一次。
        let (sender, receiver) = oneshot::channel();
        let menu = Rc::new(RefCell::new(ActiveMenu {
            settled: false,
            sender: Some(sender),
            unsubscribe: None,
            build,
            selected: None,
        }));
        // 先同步渲染一次；订阅只负责之后的变化。
        self.render_menu(&menu);
        if let Some(subscribe) = subscribe {
            let view = self.clone();
            let menu_state = Rc::clone(&menu);
            let listener: Rc<dyn Fn()> = Rc::new(move || {
                // settle 后忽略迟到的通知：内容可能已被下一屏替换。
                if !menu_state.borrow().settled {
                    view.render_menu(&menu_state);
                }
            });
            let unsubscribe = subscribe(listener);
            // 退订函数存回菜单状态，由 finish_menu 恰好调用一次。
            menu.borrow_mut().unsubscribe = Some(unsubscribe);
        }
        // Ok(Some) 选择、Ok(None) 取消、Err 发送端随状态释放——后两者都折叠为 None。
        Box::pin(async move { receiver.await.ok().flatten() })
    }

    fn status(&self, title: &str, message: &str, on_cancel: Option<Rc<dyn Fn()>>) {
        // 状态页只有一段 muted 文案；没有 on_cancel 时连 footer 都不显示。
        let theme = self.inner.theme.clone();
        let body = vec![
            Box::new(Spacer::new()) as Box<dyn Component>,
            Box::new(Text::with_padding(
                theme.fg(ThemeColor::Muted, message),
                1,
                0,
            )),
        ];
        let Some(on_cancel) = on_cancel else {
            self.set_content(frame(&theme, title, body, None), None, None);
            return;
        };
        // 匹配用视图持有的 KeybindingsManager；提示文本由 key_hint 另行读取全局键位表。
        let keybindings = Arc::clone(&self.inner.keybindings);
        let handler: Rc<dyn Fn(&str)> = Rc::new(move |data: &str| {
            if keybindings.matches(data, "tui.select.cancel") {
                on_cancel();
            }
        });
        self.set_content(
            frame(
                &theme,
                title,
                body,
                Some(&key_hint("tui.select.cancel", "cancel")),
            ),
            Some(handler),
            None,
        );
    }

    fn redirect_url(
        &self,
        title: &str,
        authorization_url: &str,
        signal: CancellationToken,
    ) -> LocalBoxFuture<'static, Option<String>> {
        let (sender, receiver) = oneshot::channel();
        let state = Rc::new(RefCell::new(RedirectInput {
            settled: false,
            sender: Some(sender),
            done: CancellationToken::new(),
        }));
        // TS 的 `signal.aborted` 分支：立即结束，而且不安装页面。
        if signal.is_cancelled() {
            finish_redirect(&state, None);
            return Box::pin(async { None });
        }
        // `signal.addEventListener("abort", onAbort, { once: true })`；`done` 在 finish 时清掉任务。
        // 页面正常结束（提交/取消）会 cancel done，watcher 随之退出；signal 中止时由 watcher
        // 负责结束页面，两条路径都汇入幂等的 finish_redirect。
        {
            let state = Rc::clone(&state);
            let done = state.borrow().done.clone();
            tokio::task::spawn_local(async move {
                tokio::select! {
                    () = signal.cancelled() => {}
                    () = done.cancelled() => {}
                }
                finish_redirect(&state, None);
            });
        }
        let theme = self.inner.theme.clone();
        let input = Rc::new(RefCell::new(Input::new(InputOptions::default())));
        // 授权 URL 组件用共享代理挂进 body；复制任务通过同一个 link 句柄改写提示行。
        let link = Rc::new(RefCell::new(Some(AuthUrlComponent::new(
            Some(Rc::clone(&self.inner.request_render)),
            authorization_url,
        ))));
        let body: Vec<Box<dyn Component>> = vec![
            Box::new(Spacer::new()),
            Box::new(Text::with_padding(
                theme.fg(
                    ThemeColor::Muted,
                    "Approve access in your browser. If it did not open, visit:",
                ),
                1,
                0,
            )),
            Box::new(SharedAuthUrl(Rc::clone(&link))),
            Box::new(Spacer::new()),
            Box::new(Text::with_padding(
                theme.fg(
                    ThemeColor::Muted,
                    "If the browser runs on another machine, paste the URL it was redirected to:",
                ),
                1,
                0,
            )),
            Box::new(SharedInput(Rc::clone(&input))),
        ];
        let keybindings = Arc::clone(&self.inner.keybindings);
        let handler_state = Rc::clone(&state);
        let handler_input = Rc::clone(&input);
        let handler_link = Rc::clone(&link);
        let view = self.clone();
        let handler: Rc<dyn Fn(&str)> = Rc::new(move |data: &str| {
            // 确认：JS 语义 trim 后非空才结束（TS 的 `if (value)`）；空内容继续等待。
            if keybindings.matches(data, "tui.select.confirm") {
                let value =
                    kiana_json::js_string::js_trim(handler_input.borrow().get_value()).to_string();
                if !value.is_empty() {
                    finish_redirect(&handler_state, Some(value));
                }
                return;
            }
            // 取消：与 signal 中止同一条结束路径，返回 None。
            if keybindings.matches(data, "tui.select.cancel") {
                finish_redirect(&handler_state, None);
                return;
            }
            // 复制不结束页面：start_copy 在后台等待剪贴板，然后改写 URL 组件的提示行。
            if keybindings.matches(data, "app.message.copy") {
                view.start_copy(Rc::clone(&handler_link));
                return;
            }
            // 其余按键（可打印字符与编辑键）交给输入框。
            Component::handle_input(&mut *handler_input.borrow_mut(), data);
        });
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "submit"),
            key_hint("tui.select.cancel", "cancel")
        );
        self.set_content(
            frame(&theme, title, body, Some(&footer)),
            Some(handler),
            Some(input),
        );
        Box::pin(async move { receiver.await.ok().flatten() })
    }
}

impl Component for McpManagerView {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 先渲染并释放 state 借用，再逐行裁剪；超宽行直接硬裁（省略号为空串，TS 同），
        // 保证返回的每一行都不超过宿主给的可视宽度。
        let lines = {
            let mut state = self.inner.state.borrow_mut();
            state.content.render(width)
        };
        lines
            .into_iter()
            .map(|line| {
                if visible_width(&line) > width {
                    truncate_to_width(&line, width, "", false)
                } else {
                    line
                }
            })
            .collect()
    }

    fn handle_input(&mut self, data: &str) {
        // 只借用一次并克隆出处理器后立刻释放：处理器内部可能调用 set_content
        // （需要 `borrow_mut`），持有借用调用它会触发 RefCell panic。
        let handler = self.inner.state.borrow().input_handler.clone();
        if let Some(handler) = handler {
            handler(data);
        }
        // 处理器之后仍请求一次重绘：不是所有处理器都会换页（换页时 set_content 已经请求过一次）。
        (self.inner.request_render)();
    }

    fn invalidate(&mut self) {
        // 丢弃当前页面的渲染缓存；宿主在主题切换或需要全量重绘时调用。
        self.inner.state.borrow_mut().content.invalidate();
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        // 让宿主的焦点遍历能停在视图上；实际焦点由 Focusable 实现转发给 input_target。
        Some(self)
    }
}

impl Focusable for McpManagerView {
    fn focused(&self) -> bool {
        self.inner.state.borrow().focused
    }

    fn set_focused(&mut self, focused: bool) {
        // 记录视图焦点并同步给当前输入目标；`set_content` 装上新目标后按这个标志恢复焦点。
        let target = {
            let mut state = self.inner.state.borrow_mut();
            state.focused = focused;
            state.input_target.clone()
        };
        if let Some(target) = target {
            Focusable::set_focused(&mut *target.borrow_mut(), focused);
        }
    }
}

/// `tui.requestRender()`：`Arc<dyn Tui>` 的成员是 `&mut self`，而工厂只给 `Arc<dyn Tui>`。
/// 运行时是单线程 UI 线程，这里短暂取一次可变引用；`Weak` 避免组件与 TUI 形成 `Arc` 环。
///
/// `Weak::upgrade` 成功后在本次调用期间持有强引用，因此解引用时对象仍然存活。强转指针并
/// 解引用依赖两条运行时约定：回调只在 UI 线程调用，且调用期间没有其它 `&mut dyn Tui`
/// 存在（类型系统不保证）。`request_render(false)` 在本仓库的 `Tui` 实现里只更新重绘标志
/// （见 `TuiBase` / `TuiMainScreen` / `TuiAltScreen` 的 override），不会同步渲染或重入组件。
fn render_requester_for_tui(tui: Arc<dyn Tui>) -> RequestRenderCallback {
    let weak = Arc::downgrade(&tui);
    Rc::new(move || {
        let Some(tui) = weak.upgrade() else {
            return;
        };
        let pointer = Arc::as_ptr(&tui) as *mut dyn Tui;
        // SAFETY: 仅在单线程 UI 线程上调用；升级得到的 `tui` 强引用让指针在本次调用期间
        // 有效；`request_render(false)` 只更新重绘标志，不重入本组件。
        unsafe {
            (*pointer).request_render(false);
        }
    })
}

/// `showMcpManager(ctx, manage)`：在管理器视图里运行 `manage`，结束后关闭视图。
///
/// `manage` 必须是 `Send`（`CustomFactory` 的要求）；它只能捕获可跨线程的数据，并在
/// 视图工厂于 `LocalSet` 上被轮询后通过线程本地表取回扩展的本地状态。
///
/// # 执行顺序
///
/// 1. `manage` 被放进 `Arc<Mutex<Option<_>>>`：它是 `FnOnce`，而宿主要求工厂实现 `Fn`，
///    因此只能在工厂体里 `take` 一次（锁中毒时取回内部值，不会因一次 panic 永久失败）。
/// 2. 宿主调用工厂时：取走 `manage`、把 `Arc<dyn Tui>` 转成
///    `render_requester_for_tui`、同步捕获主题快照（`&Theme` 不能进入 `'static` future）。
/// 3. 工厂返回的 future 构造 [`McpManagerView`]（初始 Loading 页）、`spawn_local` 运行
///    `manage(view)`，最后把组件作为 future 的输出交给宿主；`Err(message)` 先
///    `notify(..., Error)`，随后无论成功失败都调用 `done(())`。
/// 4. 本函数等待 `ctx.ui().custom(factory, None)`；正常情况下 `done` 解析宿主侧的
///    responder，因此返回时管理器流程已经结束（宿主销毁时也可能以默认值解析剩余
///    responder，见 `CustomDoneFn` 的文档）。无 UI 时 `custom` 不会调用工厂，`manage`
///    不会运行。
///
/// 若工厂被重复调用，第二次 `take` 得到 `None`，会立即 `done(())` 并返回初始视图。
pub async fn show_mcp_manager<F, Fut>(ctx: Arc<dyn ExtensionCommandContextApi>, manage: F)
where
    F: FnOnce(McpManagerView) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + 'static,
{
    let slot = Arc::new(std::sync::Mutex::new(Some(manage)));
    let caller_ctx = Arc::clone(&ctx);
    let factory: CustomFactory = Arc::new(move |tui, theme, keybindings, done| {
        // FnOnce 只能取一次；工厂被重复调用时会得到 None。
        let manage = slot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let ctx = Arc::clone(&ctx);
        let request_render = render_requester_for_tui(tui);
        // 返回的 future 是 `'static`，不能借用 `&Theme`：在同步工厂体内捕获快照，
        // 这样管理器视图使用的是宿主传入的主题（TS: `new McpManagerView(tui, theme, …)`）。
        let theme = ThemeSnapshot::capture(theme);
        Box::pin(async move {
            let view = McpManagerView::from_snapshot(request_render, theme, keybindings, None);
            match manage {
                Some(manage) => {
                    // 组件作为工厂 future 的输出返回给宿主；manage 在 LocalSet 上异步运行，
                    // 不阻塞工厂 future。
                    let task_view = view.clone();
                    let error_ctx = Arc::clone(&ctx);
                    tokio::task::spawn_local(async move {
                        if let Err(message) = manage(task_view).await {
                            error_ctx.ui().notify(&message, Some(NotifyKind::Error));
                        }
                        // 无论成功还是已通知错误，都要 done 才能解析 custom 的等待。
                        done(Box::new(()));
                    });
                }
                // 工厂重复调用：没有 manage 可运行，立即结束。
                None => done(Box::new(())),
            }
            Box::new(view) as Box<dyn Component>
        })
    });
    let _ = caller_ctx.ui().custom(factory, None).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_theme::theme::{get_theme_by_name, init_theme};

    fn keybindings() -> Arc<KeybindingsManager> {
        Arc::new(KeybindingsManager::new(Default::default(), None))
    }

    /// 快照必须与 `Theme::fg`/`Theme::bold` 逐字节一致（空串与探针含 NUL 的情况也要覆盖）。
    #[test]
    fn theme_snapshot_matches_theme_fg_and_bold() {
        init_theme(Some("dark"), false);
        let theme = get_theme_by_name("dark").expect("builtin dark theme");
        let snapshot = ThemeSnapshot::capture(&theme);
        for color in ThemeColor::ALL {
            for text in ["", "x", "MCP servers", "a\u{0}b"] {
                assert_eq!(snapshot.fg(color, text), theme.fg(color, text));
            }
        }
        for text in ["", "x", "MCP servers"] {
            assert_eq!(snapshot.bold(text), theme.bold(text));
        }
    }

    /// `/mcp` 管理器必须使用宿主传给 `CustomFactory` 的主题，而不是全局当前主题。
    #[test]
    fn manager_view_uses_the_theme_passed_to_the_factory() {
        init_theme(Some("dark"), false);
        let light = get_theme_by_name("light").expect("builtin light theme");
        let dark = get_theme_by_name("dark").expect("builtin dark theme");
        let mut view = McpManagerView::from_theme(Rc::new(|| {}), &light, keybindings(), None);
        let rendered = view.render(80).join("\n");
        assert!(
            rendered.contains(&light.fg(ThemeColor::Accent, &light.bold("MCP servers"))),
            "manager view must color the title with the passed theme"
        );
        assert!(
            !rendered.contains(&dark.fg(ThemeColor::Accent, &dark.bold("MCP servers"))),
            "manager view must not fall back to the global theme"
        );
        assert!(
            rendered.contains(&light.fg(ThemeColor::Muted, "Loading…")),
            "manager view must color the body with the passed theme"
        );
    }
}
