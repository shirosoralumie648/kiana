//! bordered-loader.ts。
//!
//! 供扩展 UI 使用的带边框加载组件。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/bordered-loader.ts`。
//!
//! # TUI / Theme 适配
//!
//! * TS 的 `TUI` 参数换成 `Option<RequestRenderCallback>`；`tui.requestRender()` 由回调复现。
//! * TS 的 `setInterval` 由宿主定时器驱动（[`BorderedLoader::set_timer`] 转发给底层 Loader）。
//! * TS 的 `Theme` 是类实例、可直接被闭包捕获；Rust 的 [`Theme`] 不实现 `Clone`，因此构造
//!   函数接收 `Arc<Theme>`。所有仓库内调用点传的都是**全局 `theme` Proxy**（`theme.fg()`
//!   每次访问都重新解析当前全局主题），因此着色闭包必须在 render 时重新读取；构造时通过
//!   [`Arc::ptr_eq`] 与当前全局主题比较来区分两种语义：
//!   - 传入的正是当前全局主题 → 每次着色读全局主题（主题切换后立即生效）；
//!   - 传入的是另一个具体主题实例 → 快照（与 TS 直接传某个 `Theme` 实例时的行为一致）。
//!     这样既复现了真实调用点的行为，也保留了 spec §2.12 描述的快照语义。
//! * TS 的 `onAbort` setter → [`BorderedLoader::set_on_abort`]；`signal` getter →
//!   [`BorderedLoader::signal`]，返回 [`CancellationToken`]（ARCHITECTURE §4.2）。
//! * TS 通过类继承扩展 `Container`；Rust 用组合 + 代理组件，并把 `container_children`
//!   指向内部容器（焦点遍历）。只有可取消时才把按键转给 `CancellableLoader`。
//!
//! 子组件顺序（**逐字**）：`DynamicBorder(borderColor)` → loader →
//! （可取消时）`Spacer(1)` + `Text(keyHint("tui.select.cancel", "cancel"), 1, 0)` →
//! `Spacer(1)` → `DynamicBorder(borderColor)`。可取消时共 6 个子项、不可取消时 4 个；
//! 测试用短消息在 80 列下分别锁定 7 行与 5 行的渲染高度（消息换行会改变行数）。
//!
//! # 调用方与生命周期
//!
//! 仓库内的调用点是 `kiana-interactive`：`tui_renderer::create_bordered_loader` 传入全局主题
//! 与 `request_render` 构造它，`bug_report` 与 `session_share` 在等待异步操作时把它挂到编辑区
//! 位置。调用方通常先取 [`BorderedLoader::signal`] 的克隆交给异步任务做 `select!`，操作结束
//! （或用户按取消键）后调用 [`BorderedLoader::dispose`] 并归还编辑器。
//!
//! # 内部结构与约束
//!
//! * 组合代替继承：本类型持有一个 [`Container`]，子项如上；`SharedLoader` 代理让容器拥有
//!   loader 的同时，本类型仍能调用它的 `set_timer`/`dispose`/`signal`。
//! * 可取消与否在构造时分派为 `CancellableLoader` 或 `Loader`（TS 是运行时判型）。
//!   键盘输入是否被处理由本类型自己的 `handle_input` 按 `cancellable` 判断：不可取消时
//!   直接忽略所有输入（`SharedLoader::handles_input` 也只在可取消时报告 `true`，对应 TS
//!   里只有 `CancellableLoader` 定义了 `handleInput`）。因此不可取消模式下 Esc 不会取消
//!   任何东西；本组件也从不对该令牌调用 `cancel()`（持有克隆的调用方仍可自行取消）。
//! * [`BorderedLoader::dispose`] **不取消**取消令牌、也不触发 `on_abort`：它只停止动画
//!   （见 `CancellableLoader::dispose` 的文档）。需要中止异步任务时，调用方必须自己
//!   `cancel()` 取得的那份令牌。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_theme::{Theme, ThemeColor};
use kiana_tui::components::cancellable_loader::CancellableLoader;
use kiana_tui::components::loader::{Loader, RequestRenderCallback};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::{StyleFn, Text};
use kiana_tui::tui::{
    Component, Container, MouseFocusTarget, Timer, TuiMouseEvent, TuiMouseEventResult,
};
use tokio_util::sync::CancellationToken;

use crate::dynamic_border::DynamicBorder;
use crate::keybinding_hints::key_hint;

/// 构造时传入的主题句柄：区分 TS 的全局 `theme` Proxy 与显式主题快照。
///
/// TS 的 `theme` Proxy 每次属性访问都从 `globalThis` 取当前主题，因此在 `BorderedLoader`
/// 挂载期间 `initTheme()`（含系统明暗切换）会立刻反映到边框/ spinner /消息颜色上；而直接
/// 传入某个 `Theme` 实例时颜色是快照。Rust 没有 Proxy，构造时用 [`Arc::ptr_eq`] 判断传入的
/// `Arc<Theme>` 是否就是当前全局主题来复现这两种语义。
#[derive(Clone)]
enum ThemeHandle {
    /// 传入的是当前全局主题（`kiana_theme::THEME`）：每次着色都重新读取全局主题。
    Global,
    /// 显式传入的具体主题实例：保持快照。
    Snapshot(Arc<Theme>),
}

impl ThemeHandle {
    /// 按构造时的全局主题判定语义（全局 → 动态；其它实例 → 快照）。
    fn resolve(theme: Arc<Theme>) -> Self {
        match kiana_theme::current_theme() {
            Some(current) if Arc::ptr_eq(&current, &theme) => ThemeHandle::Global,
            _ => ThemeHandle::Snapshot(theme),
        }
    }

    /// TS `theme.fg(token, text)`。
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        match self {
            ThemeHandle::Global => kiana_theme::THEME
                .fg(color, text)
                .expect("theme must be initialized before rendering (TS throws here)"),
            ThemeHandle::Snapshot(theme) => theme.fg(color, text),
        }
    }
}

/// `BorderedLoader` 的选项（TS `{ cancellable?: boolean }`，默认 `true`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderedLoaderOptions {
    /// 是否可取消（默认 `true`）。
    pub cancellable: bool,
}

/// 默认值：`cancellable: true`（TS `options?.cancellable ?? true`）。
impl Default for BorderedLoaderOptions {
    fn default() -> Self {
        BorderedLoaderOptions { cancellable: true }
    }
}

/// 底层 loader（TS 里是 `CancellableLoader | Loader` 的运行时判型）。
enum LoaderKind {
    Cancellable(CancellableLoader),
    Plain(Loader),
}

impl LoaderKind {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self {
            LoaderKind::Cancellable(loader) => loader.render(width),
            LoaderKind::Plain(loader) => loader.render(width),
        }
    }

    fn invalidate(&mut self) {
        match self {
            LoaderKind::Cancellable(loader) => loader.invalidate(),
            LoaderKind::Plain(loader) => loader.invalidate(),
        }
    }

    fn handle_input(&mut self, data: &str) {
        match self {
            LoaderKind::Cancellable(loader) => loader.handle_input(data),
            LoaderKind::Plain(_) => {}
        }
    }

    /// TS `dispose()`：优先 `loader.dispose()`，否则 `loader.stop()`。
    fn dispose(&mut self) {
        match self {
            LoaderKind::Cancellable(loader) => loader.dispose(),
            LoaderKind::Plain(loader) => loader.stop(),
        }
    }

    fn set_timer(&mut self, timer: Box<dyn Timer>) {
        match self {
            LoaderKind::Cancellable(loader) => loader.set_timer(timer),
            LoaderKind::Plain(loader) => loader.set_timer(timer),
        }
    }

    /// 可取消分支返回 loader 的共享令牌；`Plain` 分支只返回一个新建的占位令牌。
    /// [`BorderedLoader::signal`] 在非可取消模式下不会调用这里，而是返回字段 `plain_signal`
    /// 的克隆，保证同一组件多次取到的 signal 是同一个从未取消的令牌。
    fn signal(&self) -> CancellationToken {
        match self {
            LoaderKind::Cancellable(loader) => loader.signal(),
            LoaderKind::Plain(_) => CancellationToken::new(),
        }
    }
}

/// loader 的共享代理：容器持有该子项，`BorderedLoader` 仍能访问被包装的 loader。
///
/// `handles_input()` 只在包着 `CancellableLoader` 时返回 `true`，对应 TS 里只有
/// `CancellableLoader`（而非 `Loader`）定义了 `handleInput`；其余方法逐项转发。
struct SharedLoader(Rc<RefCell<LoaderKind>>);

impl Component for SharedLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }

    fn handles_input(&self) -> bool {
        matches!(&*self.0.borrow(), LoaderKind::Cancellable(_))
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 带边框的加载组件（TS `class BorderedLoader extends Container`）。
///
/// 字段分工：`container` 决定渲染与鼠标路由；`loader` 与容器里的 `SharedLoader` 子项共享
/// 同一个底层 loader（`Rc<RefCell<...>>`）；`cancellable` 是构造时定下的模式，之后不变；
/// `plain_signal` 只服务非可取消模式，从不被取消。
pub struct BorderedLoader {
    container: Container,
    loader: Rc<RefCell<LoaderKind>>,
    cancellable: bool,
    /// 非可取消时返回的 signal（永不 cancel；TS `new AbortController().signal`）。
    plain_signal: CancellationToken,
}

impl BorderedLoader {
    /// TS `constructor(tui, theme, message, options?)`。
    ///
    /// `options = None` 等价于默认值（可取消）。构造时先把 `theme` 解析成 `ThemeHandle`，
    /// 据此生成三个着色闭包（边框 / spinner / 消息），再按 `cancellable` 选择底层 loader；
    /// 这些闭包与 message 此后固定，改消息需要重建组件。
    pub fn new(
        request_render: Option<RequestRenderCallback>,
        theme: Arc<Theme>,
        message: &str,
        options: Option<BorderedLoaderOptions>,
    ) -> Self {
        let cancellable = options.unwrap_or_default().cancellable;

        let theme = ThemeHandle::resolve(theme);
        let border_color: StyleFn = {
            let theme = theme.clone();
            Arc::new(move |text: &str| theme.fg(ThemeColor::Border, text))
        };
        let spinner_color: StyleFn = {
            let theme = theme.clone();
            Arc::new(move |text: &str| theme.fg(ThemeColor::Accent, text))
        };
        let message_color: StyleFn = {
            let theme = theme.clone();
            Arc::new(move |text: &str| theme.fg(ThemeColor::Muted, text))
        };

        let loader_kind = if cancellable {
            LoaderKind::Cancellable(CancellableLoader::new(
                request_render,
                spinner_color,
                message_color,
                message,
                None,
            ))
        } else {
            LoaderKind::Plain(Loader::new(
                request_render,
                spinner_color,
                message_color,
                message,
                None,
            ))
        };
        let loader = Rc::new(RefCell::new(loader_kind));

        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::new(Arc::clone(&border_color))));
        container.add_child(Box::new(SharedLoader(Rc::clone(&loader))));
        if cancellable {
            container.add_child(Box::new(Spacer::with_lines(1.0)));
            container.add_child(Box::new(Text::with_padding(
                key_hint("tui.select.cancel", "cancel"),
                1,
                0,
            )));
        }
        container.add_child(Box::new(Spacer::with_lines(1.0)));
        container.add_child(Box::new(DynamicBorder::new(border_color)));

        BorderedLoader {
            container,
            loader,
            cancellable,
            plain_signal: CancellationToken::new(),
        }
    }

    /// 是否可取消（TS 私有字段）。
    pub fn is_cancellable(&self) -> bool {
        self.cancellable
    }

    /// TS `get signal()`：可取消时返回 loader 的 signal；否则返回永不 abort 的 signal。
    ///
    /// 两种情况返回的都是令牌**克隆**（与组件共享取消状态），调用方可以放心保存、跨
    /// `await` 持有。可取消模式下组件自身只在 `CancellableLoader` 收到取消键时取消令牌；
    /// 任何持有克隆的代码也可以主动 `cancel()`（克隆共享同一个取消状态）。非可取消模式下
    /// 本组件从不对该令牌调用 `cancel()`，因此它不会因组件内部行为而取消。
    pub fn signal(&self) -> CancellationToken {
        if self.cancellable {
            self.loader.borrow().signal()
        } else {
            self.plain_signal.clone()
        }
    }

    /// TS `set onAbort(fn)`：只在可取消时透传（否则是空操作）。
    ///
    /// 是**替换**而不是追加：再次调用会丢弃上一个回调，`None` 清除。回调在取消键处理期间
    /// 同步执行，此时令牌已经处于取消状态（见 `CancellableLoader` 的取消语义）。
    pub fn set_on_abort(&mut self, callback: Option<Box<dyn FnMut()>>) {
        if !self.cancellable {
            return;
        }
        if let LoaderKind::Cancellable(loader) = &mut *self.loader.borrow_mut() {
            loader.on_abort = callback;
        }
    }

    /// 安装/替换宿主定时器（转发给底层 Loader；TS 的 `setInterval` 由宿主驱动）。
    ///
    /// 排程规则由底层 loader 决定（可取消模式经 `CancellableLoader` 转发到 `Loader`）：
    /// 安装后若动画已启动且帧数大于 1 会立即排程；测试里默认帧间隔为 80ms。替换的旧定时器
    /// 不会被本组件显式取消，其未触发的回调是否清理取决于 `Timer` 实现。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        self.loader.borrow_mut().set_timer(timer);
    }

    /// TS `dispose()`：可取消时 `dispose()`，否则 `stop()`。
    ///
    /// 两者最终都只是停止动画，**不取消 `signal()` 的令牌**、也不调用 `on_abort`；重复调用
    /// 安全（底层 `stop()` 幂等），dispose 之后仍可渲染。
    pub fn dispose(&mut self) {
        self.loader.borrow_mut().dispose();
    }
}

impl Component for BorderedLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    /// TS `handleInput(data)`：只在可取消时转给 `CancellableLoader`，否则忽略所有输入。
    fn handle_input(&mut self, data: &str) {
        if self.cancellable {
            self.loader.borrow_mut().handle_input(data);
        }
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let result = self.container.handle_mouse(event);
        // TS `Container.handleMouse`：转发型容器定义了 `handleInput` 时，子控件请求焦点会把
        // 焦点留给自己（`BorderedLoader` 定义了 `handleInput`）。
        //
        // 内层 `Container` 的同类修正只看 `Container::handles_input()`（恒为 false，见
        // kiana-tui 的 `tui.rs`），不会因为外层包装定义了 `handleInput` 而生效；因此这里
        // 显式把 `focus` 结果的归属改成本组件。
        result.map(|mut result| {
            if result.focus == Some(true) {
                let address = self as *const BorderedLoader as *const () as usize;
                result.focus_target = Some(MouseFocusTarget::Address(address));
            }
            result
        })
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }
}
