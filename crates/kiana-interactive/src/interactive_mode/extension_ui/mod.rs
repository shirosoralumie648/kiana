//! `interactive-mode.ts` 的扩展 UI 上下文（§2.50.18 / §2.50.20）。
//!
//! # 线程模型
//!
//! [`ExtensionUiContext`] 必须 `Send + Sync`，而 `InteractiveMode` 是 `!Send`。桥接层因此
//! 只保存一个 `tokio::sync::mpsc::UnboundedSender<UiMessage>` 与同步 getter 的快照；真正的
//! 对话框 / 组件创建在 UI LocalSet 的 drain 任务里执行。
//!
//! 请求体里唯一的非 `Send` 负载是 `CustomOptions` 的静态 `OverlayOptions`
//!（`visible: Box<dyn Fn(usize, usize) -> bool>`）；它被 `SendOverlayOptions` 包装后随
//! mpsc 传递，并且只在 UI 线程上执行（TS 的 UI 是单线程的，扩展处理器本身也在该线程）。
//! 对话框状态（组件句柄、oneshot 响应端、overlay 句柄）是 UI 线程的线程局部数据，因为
//! `SharedComponent` / `OverlayHandle` 是 `!Send`。
//!
//! 同步 getter（`getEditorText` / `getToolsExpanded` / `getEditorComponent`）无法 round-trip
//! oneshot，读取 UI 线程发布的 `UiSnapshot`（`set*` 会立即更新快照，用户键入由 UI 循环
//! 在每轮工作后刷新）。
//!
//! # 这个文件解决什么问题
//!
//! 扩展 API 要求 UI 上下文是 `Send + Sync`，但真正的 UI——组件树、`Rc<RefCell<_>>` 句柄、
//! 渲染器——只能留在 `!Send` 的 LocalSet 线程上。本文件是两者之间唯一的桥：
//!
//! * 调用方拿到的是 `InteractiveUiContext`（`Arc<dyn ExtensionUiContext>`），它只持有一个
//!   `UnboundedSender<UiMessage>` 和一份共享快照；所有方法都只做“打包消息 + 发送”。
//! * 会改 UI 的操作变成 `UiRequest`，由 UI 线程上的 drain 任务（`run_ui_loop`）串行执行。
//! * 需要等待用户输入的调用（`select` / `confirm` / `input` / `editor` / `custom`）额外
//!   登记一个 oneshot `Responder`，以自增 token 为键；UI 侧组件结束时按 token 取走并
//!   resolve。
//!
//! # 模块边界
//!
//! * 本文件负责：桥接对象 `InteractiveUiContext`、对话框/overlay 的生命周期与 responder
//!   解析、编辑器槽（含自定义编辑器）、主题读写、UI drain 循环与快照发布。
//! * 子模块 `widgets.rs` 负责 widget/footer/header/状态行/终端输入监听器等“组件挂载”
//!   部分；`process_request` 对这些变体只做一行转发，细节见该文件的 `//!`。
//! * 主要调用方：`session.rs` 的 `bind_current_session_extensions` /
//!   `create_project_trust_context`、`commands/mod.rs` 的 `show_extension_editor`，以及
//!   集成测试通过 `#[doc(hidden)]` 的 `test_*` 接缝。
//!
//! # 一次对话框请求的完整路径
//!
//! 1. 调用方 `await` 上下文方法；方法分配 token、把 `Responder` 放进线程局部
//!    `UI_STATE.responders`、把请求送进通道，然后 await oneshot。
//! 2. `run_ui_loop` 收到 `UiMessage::Request` 后升级 `Weak<InteractiveModeInner>`。模式已经
//!    销毁时走 `respond_request_default` 用默认值收尾；否则在 UI 线程上执行
//!    `process_request`。
//! 3. `process_request` 构造对话框组件并调用 `push_extension_dialog`：栈顶组件被挂进编辑器
//!    槽，`show_top_extension_dialog` 同时更新焦点和程序状态（OSC 7501 的 `blocked`）。
//! 4. 用户操作组件 → 组件回调调用 `finish_dialog_token`：它**同步**移除 token 对应的
//!    responder 与 `DialogRuntime`（因此重复的完成/取消是 no-op），再用 `spawn_local`
//!    延后一拍关闭 UI 并 resolve。延后是必须的：回调通常发生在组件自己的 `handle_input`
//!    调用栈里，立即清空编辑器槽会 drop 正在分发的代理对象。
//! 5. `custom()` 多一步：工厂 future 完成后再把组件放进 `UI_STATE.custom_ready`，由同一
//!    循环里的 `drain_custom_ready` 挂载（overlay 走 `show_overlay`，内联则入对话框栈）。
//!
//! # 线程局部状态
//!
//! * `UI_STATE`：`responders` / `dialogs` / `custom` / `custom_ready` 四张表，只在 UI 线程
//!   读写；token 由进程级 `NEXT_TOKEN` 分配，不同上下文不会撞号。
//! * `SNAPSHOT_REGISTRY`：所有存活上下文的快照（`Weak`），`publish_ui_snapshot` 把同一份
//!   状态写给每个上下文；下次注册/发布时顺带清理已被丢弃的上下文。
//! * `EVENT_SENDER`：最近一次 `create_extension_ui_context` 的发送端，`event_sender()` 是
//!   读取入口（当前仓库内没有调用点，由父模块的 `#![allow(dead_code)]` 兜底）。
//!
//! # 重要约束与失败模式
//!
//! * 同一上下文内请求按发送顺序处理；每次 `create_extension_ui_context` 都会新建通道和
//!   独立的 drain 任务，**跨上下文的请求没有全局顺序**。
//! * drain 任务自己持有一个 `sender` 克隆，所以外部把上下文全部 drop 后通道也不会关闭，
//!   任务会一直等待，直到所属任务/LocalSet 结束。
//! * `reset_extension_ui` 只 dispose 组件、清空栈与 `custom` 表，**不 resolve** 等待中的
//!   responder——被 reset 掉的对话框 Promise 永远挂起（测试
//!   `reset_extension_ui_leaves_pending_dialogs_unresolved` 锁定该行为）。
//! * oneshot 发送端因故被 drop 时，接收端按默认值恢复：`select` / `input` / `editor` 得到
//!   `None`，`confirm` 得到 `false`，`custom` 得到擦除后的 `()`。
//! * 主题尚未初始化时 `theme()` 不 panic，而是退回内建 `dark`（`global_theme` 的
//!   fallback）；真正会 panic 的是渲染辅助 `fg` 里的 `expect`（见其文档）。这是沿用
//!   TS“渲染前必须有主题”的假设。
//!
//! # 阅读顺序
//!
//! `InteractiveUiContext` 的 trait 方法（先看 `select` / `custom`）→ `UiRequest` 变体
//! → `process_request` → `begin_extension_selector` / `mount_custom_component`
//! → `finish_dialog_token` / `close_dialog_runtime` → `run_ui_loop` 与快照发布。

pub mod widgets;

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomDoneFn, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, FooterFactory,
    HeaderFactory, NotifyKind, OverlayOptionsOrFn, PermissionCardChoice, PermissionCardRequest,
    PermissionScopeKind, TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult, WidgetContent,
    WorkingIndicatorOptions,
};
use kiana_theme::theme::Theme;
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::tui::{Component, OverlayHandle, OverlayOptions, Tui, TuiMode};
use kiana_ui_kit::keybinding_hints::key_text;
use kiana_ui_kit::themed_text::ThemedText;

use crate::components::extension_editor::{ExtensionEditorComponent, ExtensionEditorOptions};
use crate::components::extension_input::{ExtensionInputComponent, ExtensionInputOptions};
use crate::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::components::status_indicator::StatusIndicatorKind;
use crate::program_status_reporter::BlockedStatus;
use crate::render_core::approval::{
    ApprovalCard, ApprovalDecision, ApprovalRequest, ApprovalScope,
};
use crate::render_core::i18n::Translator;
use crate::render_core::modal::{InputEvent, ModalArbiter, ModalKey, ModalResolution, Routed};
use crate::tui_renderer::SharedComponentProxy;

use self::widgets::{current_theme_arc, shared_from_boxed, tui_arc};
use super::{ActiveEditor, ExtensionDialog, InteractiveMode, SharedComponent};
use crate::tui_renderer::InteractiveTuiReference;

/// 用全局主题给 `text` 着色（渲染期辅助）。
///
/// 主题未初始化时 `THEME.fg` 返回 `Err`（`ThemeError`），这里直接 `expect`——与 TS 在同一
/// 位置抛异常对应。调用方（`show_extension_error` 的渲染闭包等）必须保证启动流程已初始化主题。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

// ---------------------------------------------------------------------------
// 全局主题（冻结签名 `theme() -> &Theme`：用进程级历史表保证引用有效）
// ---------------------------------------------------------------------------

/// 返回 `ExtensionUiContext::theme()` 所需的 `&'static Theme`。
///
/// `theme()` 的签名是借用 `&Theme`，而全局主题是会被切换的 `Arc<Theme>`；如果直接返回
/// 当前 `Arc` 的裸指针，下一次主题切换释放旧实例后引用就会悬空。因此这里把见到的每个
/// `Arc` 追加进进程级 `HISTORY`（只增不减），再返回它的裸指针，保证任何曾经返回过的
/// 引用活到进程结束。`CURRENT` 只用来跳过重复 push（`Arc::ptr_eq`）。
///
/// * 主题尚未初始化时退回内建 `dark`（`FALLBACK_THEME`）。
/// * 两把 `Mutex` 在中毒（持锁线程 panic）时用 `into_inner` 继续，避免一次渲染期 panic
///   把后续所有 `theme()` 调用也变成 panic。
/// * 代价是 `HISTORY` 随主题实例数量增长，不回收；这是返回借用而非 `Arc` 的必要成本。
fn global_theme() -> &'static Theme {
    static FALLBACK_THEME: LazyLock<Arc<Theme>> = LazyLock::new(|| {
        kiana_theme::theme::get_theme_by_name("dark")
            .expect("the builtin dark theme must always be available")
    });
    static HISTORY: LazyLock<Mutex<Vec<Arc<Theme>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    static CURRENT: LazyLock<Mutex<Option<Arc<Theme>>>> = LazyLock::new(|| Mutex::new(None));

    let theme = match kiana_theme::theme::current_theme() {
        Some(theme) => theme,
        None => Arc::clone(&FALLBACK_THEME),
    };
    let mut current = CURRENT.lock().unwrap_or_else(|error| error.into_inner());
    let same = current
        .as_ref()
        .is_some_and(|existing| Arc::ptr_eq(existing, &theme));
    if !same {
        *current = Some(Arc::clone(&theme));
        HISTORY
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(theme);
    }
    let pointer = Arc::as_ptr(current.as_ref().expect("theme just stored"));
    // SAFETY: 该 Arc 同时保存在 HISTORY（进程级、从不清理）里，因此 `Theme` 活到进程结束。
    unsafe { &*pointer }
}

// ---------------------------------------------------------------------------
// 桥接消息与线程局部状态的类型
// ---------------------------------------------------------------------------

/// UI 线程收到的消息。
#[allow(
    clippy::large_enum_variant,
    reason = "请求体本身就大（选择器选项 / 工厂）；装箱会改变所有权形态"
)]
enum UiMessage {
    /// 桥接层提交的请求（`Send`：非 `Send` 的 overlay 选项用受控包装传递）。
    Request(UiRequest),
    /// UI 线程 / 组件回调产生的事件。
    Event(UiEvent),
}

/// 组件回调产生的事件（全部 `Send`，可以跨 mpsc 传递）。
enum UiEvent {
    /// `custom()` 的 `done(result)` 被调用（`Box<dyn Any + Send>` 是唯一需要跨任务传递的结果）。
    CustomDone {
        /// 请求 token。
        token: u64,
        /// 结果。
        result: Box<dyn Any + Send>,
    },
    /// `custom()` 的工厂 future 已完成，组件暂存在线程局部表里。
    CustomReady,
}

/// 对话框的结果。
enum DialogValue {
    /// `undefined`。
    None,
    /// `boolean`（confirm）。
    Bool(bool),
    /// `string | undefined`。
    Text(Option<String>),
    /// `permission_card` 的选择结果。
    PermissionCard(PermissionCardChoice),
    /// `custom()` 的泛型结果。
    Custom(Box<dyn Any + Send>),
}

impl DialogValue {
    /// 取字符串结果；非 `Text` 变体一律 `None`（对应 TS 的 `undefined`）。
    fn text(self) -> Option<String> {
        match self {
            DialogValue::Text(value) => value,
            _ => None,
        }
    }
    /// 仅 `Bool(true)` 为真。当前没有构造 `DialogValue::Bool` 的地方：`confirm` 实际走
    /// `Text("Yes"/"No")`，由 `resolve_responder` 负责映射成布尔值。
    fn bool(self) -> bool {
        matches!(self, DialogValue::Bool(true))
    }
    /// 取 `custom()` 的擦除结果；没有自定义结果时返回擦除后的 `()`，调用方 `downcast`
    /// 会失败（与 TS 无返回值时的行为对齐）。
    fn custom(self) -> Box<dyn Any + Send> {
        match self {
            DialogValue::Custom(value) => value,
            _ => Box::new(()),
        }
    }
}

/// 等待中的 oneshot 响应端。
///
/// 以 token 为键存放在 `UI_STATE.responders`；完成路径用 `shift_remove` 取走（取走后再
/// 完成一次就是 no-op）。`reset_extension_ui` 特意**不**清理这张表，好让对应 future
/// 保持挂起（TS 的 Promise 语义）。
enum Responder {
    Select(tokio::sync::oneshot::Sender<Option<String>>),
    Confirm(tokio::sync::oneshot::Sender<bool>),
    Text(tokio::sync::oneshot::Sender<Option<String>>),
    /// per-tool 审批卡：`None` 只有在发送端被 drop 时才会出现（调用方按 Reject 处理）。
    PermissionCard(tokio::sync::oneshot::Sender<Option<PermissionCardChoice>>),
    Custom(tokio::sync::oneshot::Sender<Box<dyn Any + Send>>),
}

/// 自定义组件的完成结果（`!Send`，留在 UI 线程）。
///
/// `Failed` 分支在 `drain_custom_ready` 里等价于“以 `None` 结束对话框”；当前文件内没有
/// 构造 `Failed` 的地方（`CustomFactory` 的返回类型不是 `Result`，没有错误通道）。
enum CustomOutcome {
    Component(Box<dyn Component>),
    Failed,
}

/// 一次 `custom()` 的 UI 侧状态。
struct CustomRuntime {
    /// `done()` 是否已先于组件创建被调用。
    closed: bool,
    /// 工厂选项。
    options: Option<CustomOptions>,
    /// 打开前的编辑器文本（关闭时恢复）。
    saved_editor_text: String,
}

/// `custom()` 完成后暂存在线程局部的组件。
struct CustomReady {
    token: u64,
    outcome: CustomOutcome,
}

/// 一个对话框在 UI 线程上的状态（阻塞状态与组件在 `inner.extension_dialogs` 里）。
struct DialogRuntime {
    component: SharedComponent,
    dispose: Option<Box<dyn FnMut()>>,
    /// overlay 模式下隐藏/关闭用的句柄。
    overlay: Option<Box<dyn OverlayHandle>>,
    /// `custom()` 的编辑器原文（关闭时恢复）。
    saved_editor_text: Option<String>,
}

/// `OverlayOptions` 含 `Box<dyn Fn(usize, usize) -> bool>`（不是 `Send`）。TS 的 UI 是单线程的：
/// 该闭包由调用方创建、在 UI 线程渲染时执行；这里用受控包装跨 mpsc 传递。
struct SendOverlayOptions(OverlayOptions);
// SAFETY: 值只在 UI 线程上使用（创建线程 = 扩展线程；本移植中与 UI 线程相同）。
unsafe impl Send for SendOverlayOptions {}

/// `OverlayOptionsOrFn` 的可跨通道版本：`Static` 换成包装后的 `SendOverlayOptions`，
/// `Dynamic` 本身要求 `Send + Sync`，可以原样传递。
///
/// 转换在 `SendCustomOptions` 里逐字段完成；`mount_custom_component` 在挂载时把 `Dynamic`
/// 求值**一次**（与 TS 在 `showExtensionCustom` 里调用函数一致）。
enum SendOverlayOptionsOrFn {
    Static(SendOverlayOptions),
    Dynamic(Arc<dyn Fn() -> OverlayOptions + Send + Sync>),
}

/// `CustomOptions::on_handle` 的存储形态。
///
/// 回调在 overlay 创建后、仍在 UI 线程上同步调用；参数是 `SharedOverlayHandle`，与对话框
/// 栈里保存的句柄指向同一个底层 overlay，因此扩展后续的 `hide()`/`setHidden()` 会影响
/// 栈内状态。
type OverlayHandleCallback = Arc<dyn Fn(Box<dyn OverlayHandle>) + Send + Sync>;

/// `CustomOptions` 的可跨线程中转形态。
///
/// `CustomOptions` 本身不实现 `Send`（静态 `OverlayOptions` 含渲染闭包），而 trait 里的
/// `custom()` future 可能在线程间移动。这里只做字段搬运：`from_options` / `into_options`
/// 一一对应，不校验、不补默认值；`None`/`Some` 语义原样保留。
struct SendCustomOptions {
    overlay: Option<bool>,
    overlay_options: Option<SendOverlayOptionsOrFn>,
    on_handle: Option<OverlayHandleCallback>,
}

impl SendCustomOptions {
    /// 把不可跨线程的 `CustomOptions` 搬进可跨线程形态。只做字段搬运；`Static` 分支的
    /// `Send` 承诺由 `SendOverlayOptions` 的 `unsafe impl` 承担（见其类型文档）。
    fn from_options(options: CustomOptions) -> Self {
        SendCustomOptions {
            overlay: options.overlay,
            overlay_options: options.overlay_options.map(|options| match options {
                OverlayOptionsOrFn::Static(options) => {
                    SendOverlayOptionsOrFn::Static(SendOverlayOptions(options))
                }
                OverlayOptionsOrFn::Dynamic(function) => SendOverlayOptionsOrFn::Dynamic(function),
            }),
            on_handle: options.on_handle,
        }
    }

    /// 还原成 UI 线程使用的 `CustomOptions`，与 `from_options` 逐字段对应。
    fn into_options(self) -> CustomOptions {
        CustomOptions {
            overlay: self.overlay,
            overlay_options: self.overlay_options.map(|options| match options {
                SendOverlayOptionsOrFn::Static(options) => OverlayOptionsOrFn::Static(options.0),
                SendOverlayOptionsOrFn::Dynamic(function) => OverlayOptionsOrFn::Dynamic(function),
            }),
            on_handle: self.on_handle,
        }
    }
}

/// UI 线程上所有“按 token 索引”的簿记。
///
/// * `responders`：等待中的 oneshot 发送端（发送端放在这里，future 才能拿到结果）。
/// * `dialogs`：已挂载的对话框/overlay 运行时；`custom()` 的组件挂载后才进这张表。
/// * `custom`：`custom()` 从请求到组件就绪之间的中间状态（见 `CustomRuntime`）。
/// * `custom_ready`：工厂 future 已完成、等待在 UI 循环里挂载的组件。单独成表是因为
///   future 的完成回调不在 drain 循环的调用栈里，需要把 `!Send` 组件先暂存、再由循环取走。
///
/// 四张表都只在 UI 线程访问；`IndexMap` 的插入顺序即对话框的登记顺序，但栈的显示顺序由
/// `InteractiveModeInner::extension_dialogs`（`Vec`）决定。
struct UiThreadState {
    responders: indexmap::IndexMap<u64, Responder>,
    dialogs: indexmap::IndexMap<u64, DialogRuntime>,
    custom: indexmap::IndexMap<u64, CustomRuntime>,
    custom_ready: Vec<CustomReady>,
    /// 审批卡队列：一个对话框承载所有 `permission_card` 请求（`ModalArbiter` 保证同一
    /// 时刻只显示一张卡，并显示 `Approval i/N`）。
    approval_queue: Option<Rc<RefCell<ApprovalQueueComponent>>>,
}

impl UiThreadState {
    fn new() -> Self {
        UiThreadState {
            responders: indexmap::IndexMap::new(),
            dialogs: indexmap::IndexMap::new(),
            custom: indexmap::IndexMap::new(),
            custom_ready: Vec::new(),
            approval_queue: None,
        }
    }
}

thread_local! {
    /// 本线程（UI 线程）的对话框簿记；其它线程读取会得到一份空表，因此这些函数只允许在
    /// UI 线程调用（桥接层的 trait 方法只负责发消息，不直接碰它）。
    static UI_STATE: RefCell<UiThreadState> = RefCell::new(UiThreadState::new());
}

/// 审批队列对话框的合成 token。
///
/// 队列本身不是一次用户发起的对话框请求，但它需要在 `UI_STATE.dialogs` 里有条目，才能
/// 复用 `finish_dialog_token` 的关闭流程（dispose → 移除栈 → 恢复编辑器）。用一个
/// `NEXT_TOKEN` 永远不会分配到的值避免与真实请求冲突。
const APPROVAL_QUEUE_TOKEN: u64 = u64::MAX;

/// 切换 UI 语言时同步已存在的审批队列：`ModalArbiter` 的标题/提示走新翻译器。
///
/// 队列不存在时是 no-op；只应在 UI 线程调用（`UI_STATE` 是线程局部的）。
pub(crate) fn set_approval_queue_translator(translator: &Translator) {
    UI_STATE.with(|state| {
        let queue = state.borrow().approval_queue.clone();
        if let Some(queue) = queue {
            let mut queue = queue.borrow_mut();
            queue.arbiter.set_translator(translator.clone());
            queue.arbiter.set_approval_translator(translator);
        }
    });
}

/// 把审批决定映射成扩展 API 的选择。
fn permission_choice(decision: ApprovalDecision) -> PermissionCardChoice {
    match decision {
        ApprovalDecision::AllowOnce => PermissionCardChoice::AllowOnce,
        ApprovalDecision::AllowAlways(scope) => PermissionCardChoice::AllowAlways(match scope {
            ApprovalScope::Session => PermissionScopeKind::Session,
            ApprovalScope::Local => PermissionScopeKind::Local,
            ApprovalScope::Project => PermissionScopeKind::Project,
        }),
        ApprovalDecision::Reject { reason } => PermissionCardChoice::Reject { reason },
    }
}

/// 原始终端输入 → `ModalKey`。
///
/// 与 `ApprovalCardComponent::handle_input` 保持同一套按键（`j`/`k`、方向键、Enter、
/// Escape、单字符数字直选）；额外的 Ctrl+Enter 序列由 [`is_ctrl_enter`] 单独识别。
fn modal_key_from_input(data: &str) -> Option<ModalKey> {
    match data {
        "\x1b[A" | "k" => Some(ModalKey::Up),
        "\x1b[B" | "j" => Some(ModalKey::Down),
        "\r" | "\n" => Some(ModalKey::Enter),
        "\x1b" => Some(ModalKey::Escape),
        _ => {
            let mut chars = data.chars();
            match (chars.next(), chars.next()) {
                (Some(character), None) => Some(ModalKey::Char(character)),
                _ => None,
            }
        }
    }
}

/// 危险动作的显式确认键（Ctrl+Enter）。
///
/// `ApprovalCard` 有意让危险动作上的 Enter 返回 `Ignored`；接线层必须提供另一条绑定，
/// 否则危险动作只能被拒绝。这里接受常见终端的三种 Ctrl+Enter 编码。
fn is_ctrl_enter(data: &str) -> bool {
    matches!(
        data,
        "\x1b[27;5;13~" | "\x1b[13;5u" | "\x1b[13;5~" | "\x1b[13;5"
    )
}

/// 审批卡队列的 TUI 组件。
///
/// 所有 `permission_card` 请求共享一个对话框：卡片进入 [`ModalArbiter`]，同一时刻只有
/// 一张可见（`Approval i/N`），决定按 FIFO 顺序 resolve 到各自的 oneshot。队列清空时
/// 通过 `on_drained` 关闭这个合成对话框（关闭动作仍需延后一拍，见
/// `finish_dialog_token` 的注释）。
struct ApprovalQueueComponent {
    arbiter: ModalArbiter,
    /// 与 arbiter 队列顺序一致的请求 token（FIFO）。
    tokens: VecDeque<u64>,
    /// 产生决定：`(请求 token, 选择)`。
    on_decision: Rc<dyn Fn(u64, PermissionCardChoice)>,
    /// 队列清空：关闭承载队列的合成对话框。
    on_drained: Rc<dyn Fn()>,
}

impl ApprovalQueueComponent {
    fn new(
        translator: Translator,
        on_decision: Rc<dyn Fn(u64, PermissionCardChoice)>,
        on_drained: Rc<dyn Fn()>,
    ) -> Self {
        let mut arbiter = ModalArbiter::new();
        arbiter.set_translator(translator);
        Self {
            arbiter,
            tokens: VecDeque::new(),
            on_decision,
            on_drained,
        }
    }

    fn enqueue(&mut self, token: u64, card: ApprovalCard) {
        self.tokens.push_back(token);
        self.arbiter.enqueue_approval(card);
        debug_assert_eq!(
            self.tokens.len(),
            self.arbiter.approval_count(),
            "token queue must mirror the arbiter queue"
        );
    }

    /// 完成队首卡片。调用方保证 `token` 是当前活动卡片对应的 token。
    fn resolve(&mut self, token: u64, decision: ApprovalDecision) {
        let resolved = self.tokens.pop_front();
        debug_assert_eq!(resolved, Some(token), "approval decisions are FIFO");
        (self.on_decision)(token, permission_choice(decision));
        if self.tokens.is_empty() {
            (self.on_drained)();
        }
    }

    fn handle_key(&mut self, key: ModalKey) {
        let Some(token) = self.tokens.front().copied() else {
            return;
        };
        if let Routed::Modal(Some(ModalResolution::Approval(decision))) =
            self.arbiter.route_input(InputEvent::Key(key))
        {
            self.resolve(token, decision);
        }
    }

    /// Ctrl+Enter：显式确认（危险动作也能批准）。
    fn confirm_explicitly(&mut self) {
        let Some(token) = self.tokens.front().copied() else {
            return;
        };
        let decision = self
            .arbiter
            .active_approval_mut()
            .and_then(|card| card.confirm_dangerous());
        let Some(decision) = decision else {
            return;
        };
        if let Some(ModalResolution::Approval(decision)) = self.arbiter.resolve_active(decision) {
            self.resolve(token, decision);
        }
    }
}

impl Component for ApprovalQueueComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.arbiter.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if is_ctrl_enter(data) {
            self.confirm_explicitly();
        } else if let Some(key) = modal_key_from_input(data) {
            self.handle_key(key);
        }
    }

    fn handles_input(&self) -> bool {
        true
    }
}

/// 进程级自增 token 源。
///
/// 每个请求（对话框、custom）都拿一个全新 token 作为 `responders` / `dialogs` / `custom`
/// 的键。用单调递增的 `u64` 而不是复用槽位，保证一个迟到的完成事件不可能命中另一个请求。
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn next_token() -> u64 {
    NEXT_TOKEN.fetch_add(1, Ordering::SeqCst)
}

/// 桥接层排队的一件工作（可以包含 `!Send` 负载）。
///
/// 带 `token` 的四个变体（`Select` / `Input` / `Editor` / `CustomStart`）遵守同一约定：
/// 消息发出前，调用方已在 `UI_STATE.responders` 登记了对应类型的响应端；UI 侧完成、
/// 取消或模式销毁时按 token 取走。其余变体没有等待方，处理失败不会回传任何结果。
#[allow(
    clippy::large_enum_variant,
    reason = "请求相互排斥；装箱会改变所有权与回调捕获形态"
)]
enum UiRequest {
    Notify {
        message: String,
        kind: Option<NotifyKind>,
    },
    SetStatus {
        key: String,
        text: Option<String>,
    },
    SetWorkingMessage(Option<String>),
    SetWorkingVisible(bool),
    SetWorkingIndicator(Option<WorkingIndicatorOptions>),
    SetHiddenThinkingLabel(Option<String>),
    SetWidget {
        key: String,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    },
    SetFooter(Option<FooterFactory>),
    SetHeader(Option<HeaderFactory>),
    SetTitle(String),
    PasteToEditor(String),
    SetEditorText(String),
    AddAutocompleteProvider(AutocompleteProviderFactory),
    SetEditorComponent(Option<EditorFactory>),
    SetToolsExpanded(bool),
    /// 只把主题名写进 settings；全局主题已在桥接层切换完成（跨线程路径），
    /// UI 侧不重复应用。
    SetThemeSetting(String),
    AddTerminalInput(TerminalInputHandler),
    RemoveTerminalInput(TerminalInputHandler),
    /// 打开选择器；`blocked` 由桥接层给出。`confirm()` 复用本变体，选项固定为 Yes/No。
    Select {
        token: u64,
        title: String,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
        blocked: BlockedStatus,
    },
    /// 文本输入；本变体没有 `blocked` 字段，UI 侧固定按 `Question` 上报程序状态。
    Input {
        token: u64,
        title: String,
        placeholder: Option<String>,
        opts: Option<ExtensionUIDialogOptions>,
    },
    /// 多行编辑器；没有 `ExtensionUIDialogOptions`，因此没有超时与取消信号。
    Editor {
        token: u64,
        title: String,
        prefill: Option<String>,
    },
    /// 启动 `custom()` 工厂；`options` 已转换成可跨线程形态，组件在 UI 线程创建。
    CustomStart {
        token: u64,
        factory: CustomFactory,
        options: Option<SendCustomOptions>,
    },
    /// per-tool 审批卡；由 UI 线程转成 `ApprovalCard` 并进入 `ModalArbiter` 队列。
    PermissionCard {
        token: u64,
        request: PermissionCardRequest,
    },
}

/// 同步 getter 的快照（由 UI 线程发布）。
///
/// `*_pending` 表示 UI 线程上刚被同步 setter 写入、但对应 `UiRequest` 还没被 drain 循环
/// 处理。TS 的 setter 直接改 `this` 上的属性，同一处理器里 set → get 必须看到新值；drain
/// 循环处理完请求后重新发布快照（pending 清零），之后 getter 仍读实时状态。
#[derive(Clone, Default)]
struct UiSnapshot {
    /// 编辑器文本。UI 线程发布实时值时取 `get_expanded_text()`；pending 期间则是 setter
    /// 刚写入的原始字符串。
    editor_text: String,
    /// 工具输出是否展开。
    tools_expanded: bool,
    /// 当前扩展编辑器工厂；`None` 表示使用默认编辑器。
    editor_component: Option<EditorFactory>,
    /// `editor_text` 是否为“已写入、等待 drain”的本地值；见上文字段说明。
    editor_text_pending: bool,
    /// `tools_expanded` 是否为“已写入、等待 drain”的本地值。
    tools_expanded_pending: bool,
    /// `editor_component` 是否为“已写入、等待 drain”的本地值。
    editor_component_pending: bool,
}

// ---------------------------------------------------------------------------
// ExtensionUiContext 的 Send + Sync 实现
// ---------------------------------------------------------------------------

/// 暴露给扩展的 UI 上下文（`Arc<dyn ExtensionUiContext>` 的具体类型）。
///
/// 只包含两个 `Send + Sync` 字段：
/// * `sender`：请求/事件一律排队给 UI 线程；
/// * `snapshot`：同步 getter 在“读不到实时状态”时使用的回退值（见 `UiSnapshot`）。
///
/// 这里的方法都不直接访问 `InteractiveModeInner`；需要返回值的方法会先登记 `Responder`
/// 再发送请求。发送结果用 `let _ =` 丢弃：drain 任务自己持有一个 `sender` 克隆，所以在
/// 任务存活期间通道不会关闭，失败路径实际上不会触发；即便触发，responder 也只会留在
/// `UI_STATE` 里，不会让 future panic。
struct InteractiveUiContext {
    sender: tokio::sync::mpsc::UnboundedSender<UiMessage>,
    snapshot: Arc<RwLock<UiSnapshot>>,
}

impl InteractiveUiContext {
    fn send(&self, request: UiRequest) {
        let _ = self.sender.send(UiMessage::Request(request));
    }

    fn envelope_sender(&self) -> tokio::sync::mpsc::UnboundedSender<UiMessage> {
        self.sender.clone()
    }

    fn snapshot(&self) -> UiSnapshot {
        // TS 的 getter 是对 `this` 的实时读取。扩展回调在 UI 线程上同步调用
        // （`spawn_local`），因此优先读实时状态；跨线程调用时回退到 UI 线程发布的快照。
        //
        // 例外：同一处理器里刚调用过 setter（请求还排在 drain 队列里）时，快照里的
        // `*_pending` 值才是 TS 语义下的新值（TS setter 是同步属性写入）。
        //
        // 这里用 `try_borrow` 而不是 `borrow`：getter 可能在 `&mut InteractiveMode` 方法
        // 内部被同步调用，此时 `inner` 已被可变借用，`borrow` 会 panic；失败就走下面的
        // 快照回退，语义仍然是“最后一次发布的状态”。
        if let Some(mode) = super::session::current_mode()
            && let Ok(inner) = mode.inner.try_borrow()
        {
            let local = self
                .snapshot
                .read()
                .map(|snapshot| snapshot.clone())
                .unwrap_or_default();
            return UiSnapshot {
                editor_text: if local.editor_text_pending {
                    local.editor_text
                } else {
                    inner.editor.get_expanded_text()
                },
                tools_expanded: if local.tools_expanded_pending {
                    local.tools_expanded
                } else {
                    inner.tool_output_expanded
                },
                editor_component: if local.editor_component_pending {
                    local.editor_component
                } else {
                    inner.editor_component_factory.clone()
                },
                editor_text_pending: false,
                tools_expanded_pending: false,
                editor_component_pending: false,
            };
        }
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }
}

/// 在 UI 线程登记一个等待中的响应端。
///
/// 必须在发送请求**之前**调用：请求一旦进入队列，drain 循环随时可能取走它并（在用户
/// 操作后）立刻按 token 找 responder。条目由 `finish_dialog_token` /
/// `handle_custom_done` / `respond_request_default` 用 `shift_remove` 取走；
/// `reset_extension_ui` 有意保留它。
///
/// 注意这是**线程局部**的：只有与 UI 线程同线程的调用才能被 UI 侧看到。trait 对象本身
/// 是 `Send + Sync`，但 `select` / `confirm` / `input` / `editor` / `custom` 的 responder
/// 登记依赖调用方与扩展处理器运行在 UI 线程这一前提（见模块文档的线程模型）。
fn store_responder(token: u64, responder: Responder) {
    UI_STATE.with(|state| {
        state.borrow_mut().responders.insert(token, responder);
    });
}

/// `select` 的公共 await 端：发送请求并等待选择结果。
///
/// `receiver` 的错误（发送端被 drop）统一映射为 `None`（= TS 的 `undefined`）。
/// `_token` 目前只是调用点的对称参数，未参与逻辑；token 已由调用方用于登记 responder。
async fn await_select(
    sender: tokio::sync::mpsc::UnboundedSender<UiMessage>,
    request: UiRequest,
    _token: u64,
    receiver: tokio::sync::oneshot::Receiver<Option<String>>,
) -> Option<String> {
    let _ = sender.send(UiMessage::Request(request));
    receiver.await.unwrap_or(None)
}

impl ExtensionUiContext for InteractiveUiContext {
    fn select(
        &self,
        title: &str,
        options: &[String],
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        let title = title.to_string();
        let options = options.to_vec();
        let sender = self.envelope_sender();
        Box::pin(async move {
            // 已经取消的会话不要再开对话框：直接返回 `undefined`。若 signal 在检查之后才
            // 取消，由 `begin_extension_selector` 注册的 abort watcher 负责关闭。
            if opts
                .as_ref()
                .and_then(|opts| opts.signal.as_ref())
                .is_some_and(|signal| signal.is_cancelled())
            {
                return None;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            let token = next_token();
            store_responder(token, Responder::Select(tx));
            await_select(
                sender,
                UiRequest::Select {
                    token,
                    title: title.clone(),
                    options,
                    opts,
                    blocked: BlockedStatus {
                        kind: kiana_tui::terminal_colors::ProgramStatusKind::Question,
                        message: title,
                    },
                },
                token,
                rx,
            )
            .await
        })
    }

    fn confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool> {
        let title = format!("{title}\n{message}");
        let sender = self.envelope_sender();
        Box::pin(async move {
            // confirm 复用 select 组件：选项固定为 Yes/No，标题拼成 "title\nmessage"。
            // 与 `select` / `input` 不同，这里**不**在发送前检查 signal——发送前已取消的
            // signal 由 UI 侧 `process_request` 对 Select 的二次检查以 `None` 收尾（对话框
            // 从不挂载，映射为 false）；检查之后才取消的由 abort watcher 收尾。
            let (tx, rx) = tokio::sync::oneshot::channel();
            let token = next_token();
            store_responder(token, Responder::Confirm(tx));
            let _ = sender.send(UiMessage::Request(UiRequest::Select {
                token,
                title: title.clone(),
                options: vec!["Yes".to_string(), "No".to_string()],
                opts,
                blocked: BlockedStatus {
                    kind: kiana_tui::terminal_colors::ProgramStatusKind::Permission,
                    message: title.lines().next().unwrap_or_default().to_string(),
                },
            }));
            rx.await.unwrap_or(false)
        })
    }

    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        let title = title.to_string();
        let placeholder = placeholder.map(str::to_string);
        let sender = self.envelope_sender();
        Box::pin(async move {
            // 与 `select` 相同：已取消则直接返回 `undefined`；晚到的取消由 watcher 收尾。
            if opts
                .as_ref()
                .and_then(|opts| opts.signal.as_ref())
                .is_some_and(|signal| signal.is_cancelled())
            {
                return None;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            let token = next_token();
            store_responder(token, Responder::Text(tx));
            let _ = sender.send(UiMessage::Request(UiRequest::Input {
                token,
                title,
                placeholder,
                opts,
            }));
            rx.await.unwrap_or(None)
        })
    }

    fn permission_card(
        &self,
        request: PermissionCardRequest,
    ) -> LocalBoxFuture<'_, Option<PermissionCardChoice>> {
        let sender = self.envelope_sender();
        Box::pin(async move {
            // 与其它对话框相同的线程约定：responder 必须在 UI 线程登记（调用方经
            // `agent_session::run_on_session_thread` 进入 UI 线程）。
            let (tx, rx) = tokio::sync::oneshot::channel();
            let token = next_token();
            store_responder(token, Responder::PermissionCard(tx));
            let _ = sender.send(UiMessage::Request(UiRequest::PermissionCard {
                token,
                request,
            }));
            rx.await.unwrap_or(None)
        })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        self.send(UiRequest::Notify {
            message: message.to_string(),
            kind,
        });
    }

    fn on_terminal_input(
        &self,
        handler: TerminalInputHandler,
    ) -> kiana_core::event_bus::Unsubscribe {
        self.send(UiRequest::AddTerminalInput(Arc::clone(&handler)));
        let sender = self.envelope_sender();
        Box::new(move || {
            let _ = sender.send(UiMessage::Request(UiRequest::RemoveTerminalInput(
                Arc::clone(&handler),
            )));
        })
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.send(UiRequest::SetStatus {
            key: key.to_string(),
            text: text.map(str::to_string),
        });
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.send(UiRequest::SetWorkingMessage(message.map(str::to_string)));
    }

    fn set_working_visible(&self, visible: bool) {
        self.send(UiRequest::SetWorkingVisible(visible));
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        self.send(UiRequest::SetWorkingIndicator(options));
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.send(UiRequest::SetHiddenThinkingLabel(label.map(str::to_string)));
    }

    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        self.send(UiRequest::SetWidget {
            key: key.to_string(),
            content,
            options,
        });
    }

    fn set_footer(&self, factory: Option<FooterFactory>) {
        self.send(UiRequest::SetFooter(factory));
    }

    fn set_header(&self, factory: Option<HeaderFactory>) {
        self.send(UiRequest::SetHeader(factory));
    }

    fn set_title(&self, title: &str) {
        self.send(UiRequest::SetTitle(title.to_string()));
    }

    fn custom(
        &self,
        factory: CustomFactory,
        options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn Any + Send>> {
        let sender = self.envelope_sender();
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let token = next_token();
            store_responder(token, Responder::Custom(tx));
            let _ = sender.send(UiMessage::Request(UiRequest::CustomStart {
                token,
                factory,
                options: options.map(SendCustomOptions::from_options),
            }));
            rx.await.unwrap_or_else(|_| Box::new(()))
        })
    }

    fn paste_to_editor(&self, text: &str) {
        self.send(UiRequest::PasteToEditor(text.to_string()));
    }

    fn set_editor_text(&self, text: &str) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.editor_text = text.to_string();
            // TS 的 setText 是同步属性写入：请求被 drain 之前 getter 也要看到新值。
            snapshot.editor_text_pending = true;
        }
        self.send(UiRequest::SetEditorText(text.to_string()));
    }

    fn get_editor_text(&self) -> String {
        self.snapshot().editor_text
    }

    fn editor(&self, title: &str, prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        let title = title.to_string();
        let prefill = prefill.map(str::to_string);
        let sender = self.envelope_sender();
        Box::pin(async move {
            // `editor()` 的签名没有 opts，因此不存在 AbortSignal 取消路径（也就没有 watcher）；
            // 结果只可能来自组件提交、取消或 responder 被 drop。
            let (tx, rx) = tokio::sync::oneshot::channel();
            let token = next_token();
            store_responder(token, Responder::Text(tx));
            let _ = sender.send(UiMessage::Request(UiRequest::Editor {
                token,
                title,
                prefill,
            }));
            rx.await.unwrap_or(None)
        })
    }

    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) {
        self.send(UiRequest::AddAutocompleteProvider(factory));
    }

    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        // 与 `set_editor_text` 相同：先写本地快照并置 pending，让同一处理器里紧接着的
        // `get_editor_component` 立刻看到新值；真正的替换发生在 drain 循环。
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.editor_component = factory.clone();
            snapshot.editor_component_pending = true;
        }
        self.send(UiRequest::SetEditorComponent(factory));
    }

    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.snapshot().editor_component
    }

    fn theme(&self) -> &Theme {
        global_theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        // path 是展示/诊断用的字符串；非 UTF-8 路径会被替换成 U+FFFD（`to_string_lossy`）。
        kiana_theme::theme::get_available_themes_with_paths()
            .into_iter()
            .map(|info| ThemeInfo {
                name: info.name,
                path: info.path.map(|path| path.to_string_lossy().into_owned()),
            })
            .collect()
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        let theme = kiana_theme::theme::get_theme_by_name(name)?;
        // trait 要求返回 owned `Theme`。普通主题每次加载都是新 `Arc`，`try_unwrap` 直接
        // 成功；**已注册主题**由 `get_theme_by_name` 返回共享 `Arc`，只能按 `source_path`
        // 重新加载一份。没有源路径的注册主题因此返回 `None`（尽管按名字确实存在）。
        match Arc::try_unwrap(theme) {
            Ok(theme) => Some(theme),
            Err(theme) => {
                let path = theme.source_path()?.to_path_buf();
                let reloaded = kiana_theme::theme::load_theme_from_path(&path, None).ok()?;
                Arc::try_unwrap(reloaded).ok()
            }
        }
    }

    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult {
        // TS 的 setTheme 直接调用 `this.themeController.setThemeName/setThemeInstance`：
        // 同步切换主题（含 auto-sync 开关与 onChanged 通知），成功时才写 settings。
        if let Some(mut mode) = super::session::current_mode() {
            return mode.set_theme_from_extension(theme);
        }
        // 非 UI 线程（没有线程局部控制器）：应用全局主题并把 settings 写入排给 UI 线程。
        match theme {
            ThemeOrName::Name(name) => {
                let result = kiana_theme::theme::set_theme(&name, true);
                if result.success {
                    self.send(UiRequest::SetThemeSetting(name));
                }
                ThemeResult {
                    success: result.success,
                    error: result.error.clone(),
                }
            }
            ThemeOrName::Theme(theme) => {
                // 实例路径只替换全局主题，不写 settings（`ThemeOrName::Theme` 的契约），
                // 因此这里不排 `SetThemeSetting` 请求。
                kiana_theme::theme::set_theme_instance(Arc::new(theme));
                ThemeResult {
                    success: true,
                    error: None,
                }
            }
        }
    }

    fn get_tools_expanded(&self) -> bool {
        self.snapshot().tools_expanded
    }

    fn set_tools_expanded(&self, expanded: bool) {
        // 同上：pending 标志保证 set → get 在同一处理器里同步可见；drain 后再发布一次
        // 实时状态并清零 pending。
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.tools_expanded = expanded;
            snapshot.tools_expanded_pending = true;
        }
        self.send(UiRequest::SetToolsExpanded(expanded));
    }
}

// ---------------------------------------------------------------------------
// 线程局部唤醒句柄
// ---------------------------------------------------------------------------

thread_local! {
    /// 最近一次 `create_extension_ui_context` 创建的发送端。
    ///
    /// 组件回调（例如 `done()`）需要在不持有上下文的情况下唤醒 drain 循环；线程局部槽
    /// 让同一 UI 线程上的代码能取回通道。多次创建上下文时后写覆盖前写（只保留最新的
    /// 一个），已存在的旧上下文仍可通过自己的 `sender` 工作。
    static EVENT_SENDER: RefCell<Option<tokio::sync::mpsc::UnboundedSender<UiMessage>>> =
        const { RefCell::new(None) };
}

/// 取回线程局部的发送端。
///
/// 当前仓库内没有调用点：`CustomStart` 的 `done` 回调直接捕获创建时拿到的 `sender`，
/// 不经过这个槽；保留它是给同线程组件回调的通用入口。未创建上下文时返回 `None`。
fn event_sender() -> Option<tokio::sync::mpsc::UnboundedSender<UiMessage>> {
    EVENT_SENDER.with(|sender| sender.borrow().clone())
}

// ---------------------------------------------------------------------------
// 对话框栈 / 自定义组件
// ---------------------------------------------------------------------------

impl InteractiveMode {
    /// 关闭 `commands` 模块登记的会话/模型选择器（`active_selector_*` 槽）。
    /// 对应 TS `disposeActiveSelector()`。
    ///
    /// 这不是扩展对话框栈的一部分：选择器的 token 与 `UI_STATE.dialogs` 无关，由
    /// `commands/mod.rs` 在显示选择器时写入。扩展对话框压栈前会先调用它，避免编辑器槽
    /// 被两个来源同时占用。先清空槽位再执行 `dispose`，因此 dispose 内部重入也看不到旧
    /// token。
    pub(crate) fn dispose_active_selector(&mut self) {
        let dispose = self.inner.borrow_mut().active_selector_dispose.take();
        self.inner.borrow_mut().active_selector_token = None;
        if let Some(mut dispose) = dispose {
            dispose();
        }
    }

    /// 用当前编辑器填回编辑器槽（对话框栈清空时）。
    ///
    /// 先清空 `editor_container`，再重新挂上活动编辑器（默认或扩展编辑器）的**新代理**，
    /// 把新地址写回 `ActiveEditor` 的 `address`，最后把焦点设到该地址。代理地址每次挂载
    /// 都会变，这就是 `address` 必须同步更新的原因。
    ///
    /// 调用方包括选择器 `done()`、reload 流程，以及导出/认证命令结束后（它们此前替换过
    /// 编辑器槽）。
    pub(crate) fn restore_editor_into_slot(&mut self) {
        let container = Rc::clone(&self.inner.borrow().editor_container);
        container.borrow_mut().clear();
        let (proxy, address): (Box<dyn Component>, usize) = {
            let inner = self.inner.borrow();
            match &inner.editor {
                ActiveEditor::Default { handle, .. } => {
                    let (proxy, address) = super::DefaultEditorProxy::new(Rc::clone(handle));
                    (proxy, address)
                }
                ActiveEditor::Custom { handle, .. } => {
                    let (proxy, address) = super::EditorComponentProxy::new(Rc::clone(handle));
                    (proxy, address)
                }
            }
        };
        container.borrow_mut().add_child(proxy);
        match &mut self.inner.borrow_mut().editor {
            ActiveEditor::Default { address: slot, .. }
            | ActiveEditor::Custom { address: slot, .. } => {
                *slot = address;
            }
        }
        if let Some(address) = self.editor_slot_address() {
            self.inner.borrow_mut().ui.set_focus_to_address(address);
        }
    }

    /// 编辑器槽里最后一个子项的组件地址。
    ///
    /// 容器子项是 `SharedComponentProxy`，其地址就是焦点系统的组件身份。调用点总是在
    /// `add_child` 之后立刻调用，所以 `last()` 就是刚挂上的代理；槽为空时返回 `None`。
    fn editor_slot_address(&self) -> Option<usize> {
        let container = Rc::clone(&self.inner.borrow().editor_container);
        let container = container.borrow();
        container
            .children
            .last()
            .map(|child| child.as_ref() as *const dyn Component as *const () as usize)
    }

    /// `pushExtensionDialog(component, blocked?)`。
    ///
    /// 压栈前先 `dispose_active_selector`（打开扩展对话框会收起会话/模型选择器），再把
    /// 组件放进 `extension_dialogs` 并显示新的栈顶。
    ///
    /// `blocked` 是该对话框对程序状态（OSC 7501）的声明：`Some` 表示等待用户输入，`None`
    /// 表示不阻塞（例如内联 `custom` 组件）。
    pub(crate) fn push_extension_dialog(
        &mut self,
        component: SharedComponent,
        blocked: Option<BlockedStatus>,
    ) {
        self.dispose_active_selector();
        self.inner
            .borrow_mut()
            .extension_dialogs
            .push(ExtensionDialog { component, blocked });
        self.show_top_extension_dialog();
    }

    /// 按组件身份移除对话框（TS `removeExtensionDialog`）。
    ///
    /// 用 `Rc::ptr_eq` 而不是 token 比较：调用方（`close_dialog_runtime`）只持有组件
    /// 句柄。`Vec::remove` 之后 `index == len` 当且仅当被移除的是原来的栈顶；只有这种
    /// 情况才需要重画，移除下层对话框不影响当前显示的栈顶。
    pub(crate) fn remove_extension_dialog(&mut self, component: &SharedComponent) {
        let index = {
            let mut inner = self.inner.borrow_mut();
            let index = inner
                .extension_dialogs
                .iter()
                .position(|dialog| Rc::ptr_eq(&dialog.component, component));
            if let Some(index) = index {
                inner.extension_dialogs.remove(index);
                Some(index)
            } else {
                None
            }
        };
        // 移除后 index 等于新长度 ⇒ 被移除的是栈顶（最后一个元素）。
        if let Some(index) = index
            && index == self.inner.borrow().extension_dialogs.len()
        {
            self.show_top_extension_dialog();
        }
    }

    /// `showTopExtensionDialog()`。
    ///
    /// 把栈顶组件（栈为空时改为活动编辑器）挂进编辑器槽并设置焦点。每次都会先清空容器
    /// 再重建代理，因此 `ActiveEditor::address` 会变化。最后把栈顶的 `blocked` 写进
    /// `ProgramStatusReporter` 的 `"extension-dialog"` 槽；`None` 会删除该槽（没有对话框
    /// 时不会留下过期的阻塞状态），并请求一次渲染。
    pub(crate) fn show_top_extension_dialog(&mut self) {
        let top = self
            .inner
            .borrow()
            .extension_dialogs
            .last()
            .map(|dialog| (Rc::clone(&dialog.component), dialog.blocked.clone()));
        let container = Rc::clone(&self.inner.borrow().editor_container);
        container.borrow_mut().clear();
        let focus_address = match &top {
            Some((component, _)) => {
                container
                    .borrow_mut()
                    .add_child(Box::new(SharedComponentProxy::new(Rc::clone(component))));
                self.editor_slot_address()
            }
            None => {
                let (proxy, address): (Box<dyn Component>, usize) = {
                    let inner = self.inner.borrow();
                    match &inner.editor {
                        ActiveEditor::Default { handle, .. } => {
                            let (proxy, address) =
                                super::DefaultEditorProxy::new(Rc::clone(handle));
                            (proxy, address)
                        }
                        ActiveEditor::Custom { handle, .. } => {
                            let (proxy, address) =
                                super::EditorComponentProxy::new(Rc::clone(handle));
                            (proxy, address)
                        }
                    }
                };
                container.borrow_mut().add_child(proxy);
                match &mut self.inner.borrow_mut().editor {
                    ActiveEditor::Default { address: slot, .. }
                    | ActiveEditor::Custom { address: slot, .. } => {
                        *slot = address;
                    }
                }
                Some(address)
            }
        };
        if let Some(address) = focus_address {
            self.inner.borrow_mut().ui.set_focus_to_address(address);
        }
        self.inner.borrow_mut().program_status.set_blocked(
            "extension-dialog",
            top.as_ref().and_then(|(_, blocked)| blocked.clone()),
        );
        self.request_render();
    }

    /// 对话框创建：selector（select / confirm 共用）。
    ///
    /// * `token` 是调用方已经登记好 responder 的键；组件结束时统一发
    ///   `DialogValue::Text(Some(选项))` 或 `Text(None)`，`confirm` 的布尔映射在
    ///   `resolve_responder` 里完成。
    /// * `opts.timeout` 交给 `ExtensionSelectorComponent` 的倒计时；到期走 onCancel，
    ///   结果同样是 `None`。`opts.signal` 由 `spawn_abort_watcher` 监听。
    /// * `on_toggle_tools_expanded` 保证选择器打开时 Ctrl+O 仍能展开工具输出。
    ///
    /// 组件与 `DialogRuntime` 必须成对登记：前者进对话框栈，后者让 `finish_dialog_token`
    /// 能找到 dispose/overlay。这里先写 `UI_STATE.dialogs` 再 `push_extension_dialog`，
    /// 所以组件一旦可见，关闭路径就一定能找到对应的 runtime。
    fn begin_extension_selector(
        &mut self,
        token: u64,
        title: &str,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
        blocked: BlockedStatus,
    ) {
        let weak = self.downgrade();
        let finish_select = {
            let weak = weak.clone();
            move |value: Option<String>| {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.finish_dialog_token(token, DialogValue::Text(value));
                }
            }
        };
        let on_select = finish_select.clone();
        let on_cancel = {
            let finish = finish_select;
            move || finish(None)
        };
        let weak_toggle = weak.clone();
        let request_render = self.request_render_callback();
        let selector = Rc::new(RefCell::new(ExtensionSelectorComponent::new(
            title,
            options,
            move |option: &str| on_select(Some(option.to_string())),
            on_cancel,
            ExtensionSelectorOptions {
                tui: Some(request_render),
                timeout: opts.as_ref().and_then(|opts| opts.timeout),
                on_toggle_tools_expanded: Some(Box::new(move || {
                    if let Some(inner) = weak_toggle.upgrade() {
                        InteractiveMode { inner }.toggle_tool_output_expansion();
                    }
                })),
                description: None,
            },
        )));
        // TS 在构造时直接 setInterval；Rust 的 CountdownTimer 需要宿主注入定时器。
        // 没有这一步，timeout 只在构造时 tick 一次，永远不会到期取消。
        if opts
            .as_ref()
            .and_then(|opts| opts.timeout)
            .is_some_and(|timeout| timeout > 0.0)
        {
            selector.borrow_mut().set_timer(Box::new(
                crate::components::easter_egg_3d::LocalTimer::new(Rc::new(std::cell::Cell::new(
                    true,
                ))),
            ));
        }
        let dispose_handle = Rc::clone(&selector);
        let dispose: Option<Box<dyn FnMut()>> = Some(Box::new(move || {
            if let Ok(mut selector) = dispose_handle.try_borrow_mut() {
                selector.dispose();
            }
        }));
        let shared: SharedComponent = Rc::clone(&selector) as SharedComponent;
        UI_STATE.with(|state| {
            state.borrow_mut().dialogs.insert(
                token,
                DialogRuntime {
                    component: Rc::clone(&shared),
                    dispose,
                    overlay: None,
                    saved_editor_text: None,
                },
            );
        });
        self.spawn_abort_watcher(token, opts.as_ref().and_then(|opts| opts.signal.clone()));
        self.push_extension_dialog(shared, Some(blocked));
    }

    /// 对话框创建：文本输入（`ExtensionUiContext::input`）。
    ///
    /// 与 selector 的差异：值是用户键入的字符串；`blocked` 在这里固定成 `Question`
    /// （`UiRequest::Input` 没有 `blocked` 字段，无法从桥接层传进来）。`opts.timeout` /
    /// `opts.signal` 的处理与 selector 相同。
    fn begin_extension_input(
        &mut self,
        token: u64,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) {
        let weak = self.downgrade();
        let finish_value = {
            let weak = weak.clone();
            move |value: Option<String>| {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.finish_dialog_token(token, DialogValue::Text(value));
                }
            }
        };
        let on_submit = finish_value.clone();
        let on_cancel = {
            let finish = finish_value;
            move || finish(None)
        };
        let request_render = self.request_render_callback();
        let input = Rc::new(RefCell::new(ExtensionInputComponent::new(
            title,
            placeholder,
            move |value: &str| on_submit(Some(value.to_string())),
            on_cancel,
            ExtensionInputOptions {
                tui: Some(request_render),
                timeout: opts.as_ref().and_then(|opts| opts.timeout),
                initial_value: None,
                description: None,
            },
        )));
        // 与 selector 相同：倒计时需要宿主注入定时器才会真正走完 timeout。
        if opts
            .as_ref()
            .and_then(|opts| opts.timeout)
            .is_some_and(|timeout| timeout > 0.0)
        {
            input.borrow_mut().set_timer(Box::new(
                crate::components::easter_egg_3d::LocalTimer::new(Rc::new(std::cell::Cell::new(
                    true,
                ))),
            ));
        }
        let dispose_handle = Rc::clone(&input);
        let dispose: Option<Box<dyn FnMut()>> = Some(Box::new(move || {
            if let Ok(mut input) = dispose_handle.try_borrow_mut() {
                input.dispose();
            }
        }));
        let shared: SharedComponent = Rc::clone(&input) as SharedComponent;
        UI_STATE.with(|state| {
            state.borrow_mut().dialogs.insert(
                token,
                DialogRuntime {
                    component: Rc::clone(&shared),
                    dispose,
                    overlay: None,
                    saved_editor_text: None,
                },
            );
        });
        self.spawn_abort_watcher(token, opts.as_ref().and_then(|opts| opts.signal.clone()));
        self.push_extension_dialog(
            shared,
            Some(BlockedStatus {
                kind: kiana_tui::terminal_colors::ProgramStatusKind::Question,
                message: title.to_string(),
            }),
        );
    }

    /// 审批卡创建：`ExtensionUiContext::permission_card`。
    ///
    /// 所有审批共享一个对话框与一个 [`ModalArbiter`]：第二张卡排队等待而不是叠加第二个
    /// 对话框，标题会显示 `Approval i/N`。每张卡的决定按 FIFO resolve 到自己的 token；
    /// 队列清空后关闭这个合成对话框。取消（Escape/关闭）按 fail-closed 映射为 Reject。
    ///
    /// 危险动作：`ApprovalCard` 对 Enter 返回 `Ignored`，必须用 Ctrl+Enter
    /// （`confirm_dangerous`）才能批准，否则只能选择 Reject。
    fn begin_permission_card(&mut self, token: u64, request: PermissionCardRequest) {
        let approval_request =
            ApprovalRequest::new(request.tool_name.clone(), request.args.clone())
                .with_id(format!("permission-{token}"))
                .with_cwd(request.cwd.clone())
                .with_decision_reason(request.decision_reason.clone())
                .with_dangerous(request.dangerous)
                .with_always_scopes(
                    request
                        .always_scopes
                        .iter()
                        .map(|scope| match scope {
                            PermissionScopeKind::Session => ApprovalScope::Session,
                            PermissionScopeKind::Local => ApprovalScope::Local,
                            PermissionScopeKind::Project => ApprovalScope::Project,
                        })
                        .collect(),
                );
        let translator = self.inner.borrow().render_core_host.translator();
        let card = ApprovalCard::build_localized(&approval_request, &translator);

        // 已有队列：直接入队（卡片由 ModalArbiter 排队，不会出现第二个对话框）。
        if let Some(queue) = UI_STATE.with(|state| state.borrow().approval_queue.clone()) {
            queue.borrow_mut().enqueue(token, card);
            self.request_render();
            return;
        }

        let on_decision: Rc<dyn Fn(u64, PermissionCardChoice)> = Rc::new(|token, choice| {
            let responder =
                UI_STATE.with(|state| state.borrow_mut().responders.shift_remove(&token));
            if let Some(responder) = responder {
                resolve_responder(responder, DialogValue::PermissionCard(choice));
            }
        });
        let weak = self.downgrade();
        let on_drained: Rc<dyn Fn()> = Rc::new(move || {
            // 队列清空：关闭承载队列的合成对话框。`finish_dialog_token` 同步移除
            // responder/runtime、延后一拍做 UI 变更，因此可以从组件自己的
            // `handle_input` 调用栈里安全调用。
            if let Some(inner) = weak.upgrade() {
                UI_STATE.with(|state| state.borrow_mut().approval_queue = None);
                let mut mode = InteractiveMode { inner };
                mode.finish_dialog_token(APPROVAL_QUEUE_TOKEN, DialogValue::None);
            }
        });
        let queue = Rc::new(RefCell::new(ApprovalQueueComponent::new(
            translator.clone(),
            on_decision,
            on_drained,
        )));
        queue.borrow_mut().enqueue(token, card);
        let shared: SharedComponent = Rc::clone(&queue) as SharedComponent;
        UI_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.approval_queue = Some(Rc::clone(&queue));
            state.dialogs.insert(
                APPROVAL_QUEUE_TOKEN,
                DialogRuntime {
                    component: Rc::clone(&shared),
                    dispose: None,
                    overlay: None,
                    saved_editor_text: None,
                },
            );
        });
        self.push_extension_dialog(
            shared,
            Some(BlockedStatus {
                kind: kiana_tui::terminal_colors::ProgramStatusKind::Permission,
                message: translator.t("approval.title", &[]),
            }),
        );
    }

    /// 对话框创建：多行编辑器（`ExtensionUiContext::editor`）。
    ///
    /// 与 input/selector 的差异：
    /// * 没有 `opts`，所以没有超时和 signal，也就不注册 abort watcher；
    /// * 组件来自 `ExtensionEditorComponent`，外部编辑器命令取自 settings（组件内处理
    ///   打开 `$EDITOR`）；
    /// * 组件是 `Box<dyn Component>`，用 `shared_from_boxed` 包成共享句柄，因此
    ///   `DialogRuntime` 没有 `dispose` 回调（`dispose: None`）。
    fn begin_extension_editor(&mut self, token: u64, title: &str, prefill: Option<&str>) {
        let weak = self.downgrade();
        let finish_value = {
            let weak = weak.clone();
            move |value: Option<String>| {
                if let Some(inner) = weak.upgrade() {
                    InteractiveMode { inner }.finish_dialog_token(token, DialogValue::Text(value));
                }
            }
        };
        let on_submit = finish_value.clone();
        let on_cancel = {
            let finish = finish_value;
            move || finish(None)
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        let editor_tui =
            kiana_tui::components::editor::EditorTui::from_terminal(renderer.borrow().terminal());
        let keybindings = Rc::new((*self.inner.borrow().keybindings).clone());
        let tui_rc: Rc<RefCell<dyn Tui>> = Rc::new(RefCell::new(
            self::widgets::TuiArcAdapter::new(InteractiveTuiReference::new(Rc::clone(&renderer))),
        ));
        let component = ExtensionEditorComponent::new(
            tui_rc,
            editor_tui,
            keybindings,
            title,
            prefill,
            move |value: &str| on_submit(Some(value.to_string())),
            on_cancel,
            ExtensionEditorOptions::default(),
            Some(self.settings_manager().get_external_editor_command()),
        );
        let shared: SharedComponent = shared_from_boxed(Box::new(component));
        UI_STATE.with(|state| {
            state.borrow_mut().dialogs.insert(
                token,
                DialogRuntime {
                    component: Rc::clone(&shared),
                    dispose: None,
                    overlay: None,
                    saved_editor_text: None,
                },
            );
        });
        self.push_extension_dialog(
            shared,
            Some(BlockedStatus {
                kind: kiana_tui::terminal_colors::ProgramStatusKind::Question,
                message: title.to_string(),
            }),
        );
    }

    /// 监听 `AbortSignal`：取消时以 `DialogValue::None` 结束对话框并返回默认值。
    ///
    /// 每个请求一个 `spawn_local` 任务，只捕获 `Weak<InteractiveModeInner>` 与 signal；
    /// 模式已销毁时 `upgrade` 失败直接返回（UI 已不存在，没有可关闭的组件）。
    ///
    /// 对话框正常结束时该任务**不会**被主动中止：token 已从 responders/dialogs 移除，
    /// 之后再触发 `finish_dialog_token` 只是 no-op；任务会等到 signal 取消或运行时结束。
    fn spawn_abort_watcher(&self, token: u64, signal: Option<tokio_util::sync::CancellationToken>) {
        let Some(signal) = signal else {
            return;
        };
        let weak = self.downgrade();
        tokio::task::spawn_local(async move {
            signal.cancelled().await;
            if let Some(inner) = weak.upgrade() {
                InteractiveMode { inner }.finish_dialog_token(token, DialogValue::None);
            }
        });
    }

    /// 结束一个对话框：移除组件、恢复编辑器槽并 resolve oneshot。
    ///
    /// UI 变更**延后一个任务**执行：`finish` 常在组件自身的 `handle_input` 调用栈里被触发，
    /// 立即 `clear()` 编辑器槽会 drop 正在执行输入分发的代理对象（Rust 里是悬垂引用）。
    /// TS 的 GC 引用没有这个问题；延后一拍可观察顺序不变。
    ///
    /// token → responder / `DialogRuntime` 的取出发生在当前同步调用里（早于
    /// `spawn_local`）：signal 取消与用户提交同时发生时，只有先到的一次能取到条目，
    /// 后到的一次不会重复 resolve。
    fn finish_dialog_token(&mut self, token: u64, value: DialogValue) {
        let responder = UI_STATE.with(|state| state.borrow_mut().responders.shift_remove(&token));
        let runtime = UI_STATE.with(|state| state.borrow_mut().dialogs.shift_remove(&token));
        let weak = self.downgrade();
        tokio::task::spawn_local(async move {
            let Some(inner) = weak.upgrade() else {
                if let Some(responder) = responder {
                    resolve_responder_default(responder);
                }
                return;
            };
            let mut mode = InteractiveMode { inner };
            mode.close_dialog_runtime(runtime);
            if let Some(responder) = responder {
                resolve_responder(responder, value);
            }
            mode.request_render();
        });
    }

    /// 关闭一个已登记的对话框（overlay 或编辑器槽），恢复编辑器原文。
    ///
    /// 顺序固定为：先跑 `dispose` 回调（selector/input 用它停掉倒计时），再按形态收尾——
    /// overlay 只 `hide()`（组件不在编辑器槽里，也不恢复编辑器文本）；内联对话框从栈中
    /// 移除，并在 `saved_editor_text` 存在时写回编辑器。最后统一调用组件的 `dispose()`。
    fn close_dialog_runtime(&mut self, runtime: Option<DialogRuntime>) {
        let Some(mut runtime) = runtime else {
            return;
        };
        if let Some(mut dispose) = runtime.dispose.take() {
            dispose();
        }
        if let Some(mut overlay) = runtime.overlay.take() {
            overlay.hide();
        } else {
            let component = Rc::clone(&runtime.component);
            self.remove_extension_dialog(&component);
            if let Some(saved) = runtime.saved_editor_text.take() {
                self.inner.borrow().editor.set_text(&saved);
            }
        }
        // TS `close()`：resolve 之后（同一同步块内）调用 `component.dispose?.()`。
        runtime.component.borrow_mut().dispose();
    }

    /// 内部使用的 `select`（commands/session.rs 树导航完成后的分支摘要询问
    /// `navigate_tree_from_selector`）。
    ///
    /// 不走桥接通道：直接在 UI 线程上登记 responder、调用 `begin_extension_selector`
    /// 再 await。因此**必须在 UI 线程调用**（`UI_STATE` 是线程局部的）；trait 版本的
    /// `select` 则可以跨线程发消息。开头对已取消 signal 的检查与 trait 版本一致。
    pub(crate) async fn show_extension_selector(
        &mut self,
        title: &str,
        options: &[String],
        opts: Option<ExtensionUIDialogOptions>,
    ) -> Option<String> {
        if opts
            .as_ref()
            .and_then(|opts| opts.signal.as_ref())
            .is_some_and(|signal| signal.is_cancelled())
        {
            return None;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let token = next_token();
        store_responder(token, Responder::Select(tx));
        self.begin_extension_selector(
            token,
            title,
            options.to_vec(),
            opts,
            BlockedStatus {
                kind: kiana_tui::terminal_colors::ProgramStatusKind::Question,
                message: title.to_string(),
            },
        );
        rx.await.unwrap_or(None)
    }

    /// `showExtensionConfirm(title, message, opts?)`。
    ///
    /// 与桥接层 `confirm` 等价，只是省掉一次通道往返：选项固定 Yes/No，responder 是
    /// `Responder::Confirm`（`resolve_responder` 把 `Text("Yes")` 映射为 `true`）。
    /// 同样不做 signal 预检查，取消由 abort watcher 收尾。
    pub(crate) async fn show_extension_confirm(
        &mut self,
        title: &str,
        message: &str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let token = next_token();
        store_responder(token, Responder::Confirm(tx));
        self.begin_extension_selector(
            token,
            &format!("{title}\n{message}"),
            vec!["Yes".to_string(), "No".to_string()],
            opts,
            BlockedStatus {
                kind: kiana_tui::terminal_colors::ProgramStatusKind::Permission,
                message: title.to_string(),
            },
        );
        rx.await.unwrap_or(false)
    }

    /// `showExtensionInput(title, placeholder?, opts?)`。
    ///
    /// UI 线程直连版本（不经通道）：已取消的 signal 直接返回 `None`，否则登记 responder
    /// 并调用 `begin_extension_input`。
    pub(crate) async fn show_extension_input(
        &mut self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> Option<String> {
        if opts
            .as_ref()
            .and_then(|opts| opts.signal.as_ref())
            .is_some_and(|signal| signal.is_cancelled())
        {
            return None;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let token = next_token();
        store_responder(token, Responder::Text(tx));
        self.begin_extension_input(token, title, placeholder, opts);
        rx.await.unwrap_or(None)
    }

    /// `showExtensionNotify(message, type?)`。
    ///
    /// `Error` / `Warning` 分别走聊天区的错误/警告行，其它（含 `None`）走状态行。
    pub(crate) fn show_extension_notify(&mut self, message: &str, kind: Option<NotifyKind>) {
        match kind {
            Some(NotifyKind::Error) => self.show_error(message),
            Some(NotifyKind::Warning) => self.show_warning(message),
            _ => self.show_status(message),
        }
    }

    /// `showExtensionError(extensionPath, error, stack?)`。
    ///
    /// 往聊天容器追加两条组件：错误行（`Error` 色，`with_padding(.., 1, 0)`）与可选的
    /// 调用栈（`Dim` 色，逐行缩进两个空格）。`skip(1)` 丢掉栈的第一行——通常是重复的
    /// 错误消息。只请求渲染，不改变其它状态。
    pub(crate) fn show_extension_error(
        &mut self,
        extension_path: &str,
        error: &str,
        stack: Option<&str>,
    ) {
        let error_msg = format!("Extension \"{extension_path}\" error: {error}");
        let chat = Rc::clone(&self.inner.borrow().chat_container);
        {
            let mut chat = chat.borrow_mut();
            chat.add_child(Box::new(ThemedText::with_padding(
                Box::new(move || fg(ThemeColor::Error, &error_msg)),
                1,
                0,
            )));
            if let Some(stack) = stack {
                let stack_lines: Vec<String> = stack
                    .split('\n')
                    .skip(1)
                    .map(|line| line.trim().to_string())
                    .collect();
                if !stack_lines.is_empty() {
                    chat.add_child(Box::new(ThemedText::with_padding(
                        Box::new(move || {
                            stack_lines
                                .iter()
                                .map(|line| fg(ThemeColor::Dim, &format!("  {line}")))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }),
                        1,
                        0,
                    )));
                }
            }
        }
        self.request_render();
    }

    /// `setTheme(themeOrName)` 的控制器路径（`theme-controller.ts#setThemeName` /
    /// `#setThemeInstance`）。
    ///
    /// 与旧的直接 `set_theme` 相比，控制器会同步维护 `currentThemeSetting` /
    /// `activeThemeName` / `autoSyncEnabled`：设置 `"system"` 会打开终端明暗跟随
    ///（`setTerminalColorSchemeNotifications(true)`），固定主题名会关闭它。
    ///
    /// 实现要点：先把 `theme_controller` 从 `inner` 里 `take()` 出来，调用期间不持有
    /// `inner` 的 `RefCell` 借用——控制器会通过 `RendererThemeHost` 借渲染器，成功路径
    /// 还会调用 `settings_manager()` 读写设置；结束后再放回原字段。控制器被异步路径
    /// 暂时取走（为 `None`）时退回 `apply_theme_without_controller`。
    ///
    /// `set_theme_name(..., false)` 的 `show_error = false`：失败信息通过返回的
    /// `ThemeResult` 交给扩展，不在聊天区额外打印（`apply_theme_name` 里的错误行只在
    /// `show_error` 为真时出现）。
    pub(crate) fn set_theme_from_extension(&mut self, theme: ThemeOrName) -> ThemeResult {
        let controller = self.inner.borrow_mut().theme_controller.take();
        let Some(mut controller) = controller else {
            // 控制器正被 `waitForTerminalColors` 等异步路径 `take()` 成 `None`：退回直接应用。
            return self.apply_theme_without_controller(theme);
        };
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        let result = {
            let mut host = super::RendererThemeHost { renderer };
            match theme {
                ThemeOrName::Name(name) => {
                    let result = controller.set_theme_name(&mut host, &name, false);
                    if result.success {
                        // TS：`if (this.settingsManager.getTheme() !== themeOrName)`。
                        let settings_manager = self.settings_manager();
                        if settings_manager.get_theme().as_deref() != Some(name.as_str()) {
                            settings_manager.set_theme(name);
                        }
                    }
                    result
                }
                ThemeOrName::Theme(theme) => {
                    controller.set_theme_instance(&mut host, Arc::new(theme))
                }
            }
        };
        self.inner.borrow_mut().theme_controller = Some(controller);
        ThemeResult {
            success: result.success,
            error: result.error,
        }
    }

    /// 没有主题控制器时的直接应用路径（跨线程 / 控制器被暂时取走）。
    ///
    /// 与 trait 的跨线程实现同义：名字成功时把新值写进 settings（避免重复写入），失败时
    /// 原样返回错误；实例只替换全局主题、不写 settings。控制器不在，因此
    /// `activeThemeName` / auto-sync 这些控制器状态不会更新。
    fn apply_theme_without_controller(&mut self, theme: ThemeOrName) -> ThemeResult {
        match theme {
            ThemeOrName::Name(name) => {
                let result = kiana_theme::theme::set_theme(&name, true);
                if result.success {
                    let settings_manager = self.settings_manager();
                    if settings_manager.get_theme().as_deref() != Some(name.as_str()) {
                        settings_manager.set_theme(name);
                    }
                }
                ThemeResult {
                    success: result.success,
                    error: result.error,
                }
            }
            ThemeOrName::Theme(theme) => {
                kiana_theme::theme::set_theme_instance(Arc::new(theme));
                ThemeResult {
                    success: true,
                    error: None,
                }
            }
        }
    }

    /// `setCustomEditorComponent(factory)`。
    ///
    /// 用一个扩展提供的编辑器替换（或恢复）编辑器槽，并尽量保持默认编辑器的行为：
    ///
    /// 1. 记录当前活动编辑器的文本（`get_text`，不是展开文本），关闭进行中的选择器，
    ///    清空编辑器槽。
    /// 2. `Some(factory)`：用渲染器稳定句柄、编辑主题与 keybindings 调用工厂。若产物能
    ///    下转成 `CustomEditor`，复制默认编辑器的 Escape/Ctrl+D/粘贴图片/扩展快捷键回调
    ///    与全部 action handler（TS 的 `"actionHandlers" in newEditor` 鸭子判定）；随后
    ///    搬移文本、padding、自动补全可见行数、边框颜色函数与 autocomplete provider。
    ///    提交/变更回调通过**默认编辑器**转发，保持“同一回调”语义（默认编辑器仍在内存
    ///    里，只是不在渲染树中）。
    /// 3. `None`：恢复默认编辑器，把刚才的文本写回去。
    /// 4. 若有活动状态指示器，重新挂到新编辑器/状态栏（否则指示器消失）；最后发布快照、
    ///    恢复焦点、请求渲染。
    ///
    /// 失败模式：工厂产物不是 `CustomEditor` 时不会复制任何回调（`as_any_mut` 下转失败），
    /// 扩展编辑器需要自己实现这些键位。`factory` 是同步调用，不能 await。
    pub(crate) fn set_custom_editor_component(&mut self, factory: Option<EditorFactory>) {
        self.inner.borrow_mut().editor_component_factory = factory.clone();
        // 换编辑器时保留文本：先取出当前活动编辑器（默认或扩展）的原文，等新编辑器建好
        // 再写回。注意用 `get_text` 而不是 `get_expanded_text`。
        let current_text = self.inner.borrow().editor.get_text();
        self.dispose_active_selector();
        let container = Rc::clone(&self.inner.borrow().editor_container);
        container.borrow_mut().clear();

        match factory {
            Some(factory) => {
                let (handle, address) = {
                    let tui = tui_arc(self);
                    let theme = kiana_theme::get_editor_theme();
                    let keybindings = Arc::clone(&self.inner.borrow().keybindings);
                    let mut editor = factory(tui, &theme, keybindings);
                    // TS 的鸭子判定（`"actionHandlers" in newEditor`）：新编辑器是
                    // CustomEditor 时，复制默认编辑器的 Escape/Ctrl+D/粘贴图片/扩展快捷键
                    // 回调与全部应用动作处理器，否则这些键位在扩展编辑器里会静默失效。
                    if let Some(custom) = editor.as_any_mut().and_then(|any| {
                        any.downcast_mut::<crate::components::custom_editor::CustomEditor>()
                    }) {
                        let default = Rc::clone(&self.inner.borrow().default_editor);
                        custom.copy_handlers_from(&default);
                    }
                    let handle = Rc::new(RefCell::new(editor));
                    // 文本与外观
                    handle.borrow_mut().set_text(&current_text);
                    let (padding, max_visible, border) = {
                        let default = Rc::clone(&self.inner.borrow().default_editor);
                        let default = default.borrow();
                        (
                            default.editor().get_padding_x(),
                            default.editor().get_autocomplete_max_visible(),
                            Arc::clone(default.border_color_fn()),
                        )
                    };
                    handle.borrow_mut().set_padding_x(padding);
                    handle
                        .borrow_mut()
                        .set_autocomplete_max_visible(max_visible);
                    let border_for_editor = border.clone();
                    handle
                        .borrow_mut()
                        .set_border_color(Some(Box::new(move |text: &str| {
                            border_for_editor(text)
                        })));
                    if let Some(provider) = self.autocomplete_provider_for_editor() {
                        handle
                            .borrow_mut()
                            .set_autocomplete_provider(Box::new(provider));
                    }
                    // 复制默认编辑器回调（通过默认编辑器转发，保持“同一回调”语义）。
                    let weak = self.downgrade();
                    let submit_weak = weak.clone();
                    handle
                        .borrow_mut()
                        .set_on_submit(Some(Box::new(move |text: &str| {
                            if let Some(inner) = submit_weak.upgrade() {
                                let default = Rc::clone(
                                    &InteractiveMode { inner }.inner.borrow().default_editor,
                                );
                                let mut default = default.borrow_mut();
                                if let Some(callback) = default.editor_mut().on_submit.as_mut() {
                                    callback(text);
                                }
                            }
                        })));
                    let change_weak = weak;
                    handle
                        .borrow_mut()
                        .set_on_change(Some(Box::new(move |text: &str| {
                            if let Some(inner) = change_weak.upgrade() {
                                let default = Rc::clone(
                                    &InteractiveMode { inner }.inner.borrow().default_editor,
                                );
                                let mut default = default.borrow_mut();
                                if let Some(callback) = default.editor_mut().on_change.as_mut() {
                                    callback(text);
                                }
                            }
                        })));
                    let (proxy, address) = super::EditorComponentProxy::new(Rc::clone(&handle));
                    container.borrow_mut().add_child(proxy);
                    (handle, address)
                };
                self.inner.borrow_mut().editor = ActiveEditor::Custom { handle, address };
            }
            None => {
                let default = Rc::clone(&self.inner.borrow().default_editor);
                default.borrow_mut().set_text(&current_text);
                self.inner.borrow_mut().editor = ActiveEditor::Default {
                    handle: Rc::clone(&default),
                    address: 0,
                };
                let (proxy, address) = super::DefaultEditorProxy::new(default);
                container.borrow_mut().add_child(proxy);
                match &mut self.inner.borrow_mut().editor {
                    ActiveEditor::Default { address: slot, .. }
                    | ActiveEditor::Custom { address: slot, .. } => {
                        *slot = address;
                    }
                }
            }
        }

        if self.inner.borrow().active_status_indicator.is_some() {
            // TS：换编辑器后把活动状态重新挂到新编辑器（或状态栏），否则指示器会消失。
            self.reattach_active_status_indicator();
        }
        // 编辑器身份/文本变了：发布快照，让跨线程 getter 的编辑器字段同步更新。
        self.publish_ui_snapshot();
        self.restore_focus_to_editor();
        self.request_render();
    }

    /// 把焦点设到当前活动编辑器记录的地址。换编辑器/重建代理后地址会变，
    /// `set_custom_editor_component` 因此在末尾调用它。
    fn restore_focus_to_editor(&mut self) {
        let address = self.inner.borrow().editor.address();
        self.inner.borrow_mut().ui.set_focus_to_address(address);
    }

    /// `resetExtensionUI()`。
    ///
    /// 清除所有扩展 UI 的挂载状态：对话框/overlay、终端输入监听器、footer/header、
    /// widget、扩展状态行、自动补全包装、自定义编辑器、working 指示器配置与隐藏
    /// thinking 标签。
    ///
    /// 不触碰等待中的 responder（见 `close_all_extension_dialogs`），也不清理
    /// `SNAPSHOT_REGISTRY`。调用点：会话切换前的 `beforeSessionInvalidate` 回调与
    /// `/reload`。
    pub(crate) fn reset_extension_ui(&mut self) {
        // 只 dispose 对话框组件，等待中的对话框 Promise 保持挂起。
        // 1) 对话框 / overlay / 终端输入监听器。
        self.close_all_extension_dialogs();
        self.inner.borrow_mut().ui.hide_overlay();
        self.clear_extension_terminal_input_listeners();
        // 2) 扩展提供的 footer / header / widget 与状态行。
        self.set_extension_footer(None);
        self.set_extension_header(None);
        self.clear_extension_widgets();
        self.inner
            .borrow()
            .footer_data_provider
            .clear_extension_statuses();
        let footer = Rc::clone(&self.inner.borrow().footer);
        footer.borrow_mut().invalidate();
        // 3) 自动补全包装与自定义编辑器（换回默认编辑器并重建 provider）。
        self.inner
            .borrow_mut()
            .autocomplete_provider_wrappers
            .clear();
        self.set_custom_editor_component(None);
        self.setup_autocomplete_provider();
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_on_extension_shortcut(None);
        self.update_terminal_title();
        // 4) working 指示器与隐藏 thinking 标签恢复默认。
        {
            let mut inner = self.inner.borrow_mut();
            inner.working_message = None;
            inner.working_visible = true;
        }
        self.set_working_indicator(None);
        let is_working = self
            .inner
            .borrow()
            .active_status_indicator
            .as_ref()
            .is_some_and(|indicator| indicator.kind() == StatusIndicatorKind::Working);
        if is_working {
            let message = format!(
                "{} ({} to interrupt)",
                self.inner.borrow().default_working_message,
                key_text("app.interrupt")
            );
            if let Some(indicator) = &mut self.inner.borrow_mut().active_status_indicator {
                indicator.set_message(&message);
            }
        }
        self.set_hidden_thinking_label(None);
    }

    /// 关闭所有扩展对话框与 overlay（`resetExtensionUI` 的第一步）。
    ///
    /// TS 只 `dispose()` 对话框组件并清空栈；等待中的 `select` / `confirm` / `input` /
    /// `custom` Promise **不会被 resolve**（扩展的处理器永远挂起）。这里同样保留
    /// `UI_STATE.responders` 里的 oneshot 发送端，避免 Rust future 以默认值提前恢复。
    ///
    /// 同时清空 `UI_STATE.custom` / `custom_ready`：仍在运行的 `custom` 工厂 future 之后
    /// 还是会把组件推进 `custom_ready`，但 `mount_custom_component` 找不到 `CustomRuntime`，
    /// 组件不会被挂载，对应 future 也保持挂起。
    fn close_all_extension_dialogs(&mut self) {
        let dialogs = UI_STATE.with(|state| std::mem::take(&mut state.borrow_mut().dialogs));
        for (_token, mut runtime) in dialogs {
            if let Some(mut dispose) = runtime.dispose.take() {
                dispose();
            }
            if let Some(mut overlay) = runtime.overlay.take() {
                // TS `ui.hideOverlay()`：overlay 组件不 dispose（Promise 仍然挂起）。
                overlay.hide();
            } else {
                // TS：`dialog.component.dispose?.()`。
                runtime.component.borrow_mut().dispose();
            }
        }
        UI_STATE.with(|state| {
            state.borrow_mut().custom.clear();
            state.borrow_mut().custom_ready.clear();
            // 队列组件随对话框栈一起消失；等待中的审批 responder 按 reset 语义保留，
            // 对应 future 保持挂起（与 select/confirm/input 一致）。
            state.borrow_mut().approval_queue = None;
        });
        let was_non_empty = !self.inner.borrow().extension_dialogs.is_empty();
        self.inner.borrow_mut().extension_dialogs.clear();
        if was_non_empty {
            self.show_top_extension_dialog();
        }
    }

    /// `showExtensionCustom` 的 UI 线程部分（工厂完成时调用）。
    ///
    /// 先取 `CustomRuntime`：被 `resetExtensionUI` 清掉（`None`）或 `done()` 已先于组件
    /// 到达（`closed`）时直接返回——组件 Box 在此 drop，不会挂载。否则按 `overlay` 分流：
    /// * overlay：解析 `overlay_options`（`Dynamic` 在这里求值一次；缺省时用组件的
    ///   `width` 作为宽度），`show_overlay` 后把 `SharedOverlayHandle` 同时交给
    ///   `on_handle` 与对话框表；
    /// * 内联：登记 `DialogRuntime` 后 `push_extension_dialog`（`blocked` 为 `None`）。
    ///
    /// 两条路径都保存 `saved_editor_text`，供关闭时恢复编辑器。
    fn mount_custom_component(&mut self, token: u64, component: Box<dyn Component>) {
        let runtime = UI_STATE.with(|state| state.borrow_mut().custom.shift_remove(&token));
        let Some(runtime) = runtime else {
            return;
        };
        if runtime.closed {
            return;
        }
        // TS `resolveOptions()` 的 fallback：没有 overlayOptions 时用组件的 `width` 属性。
        let component_width = component.width();
        let shared = shared_from_boxed(component);
        let options = runtime.options.unwrap_or(CustomOptions {
            overlay: None,
            overlay_options: None,
            on_handle: None,
        });
        let overlay = options.overlay.unwrap_or(false);
        if overlay {
            let overlay_options = match options.overlay_options {
                Some(OverlayOptionsOrFn::Static(options)) => options,
                Some(OverlayOptionsOrFn::Dynamic(function)) => function(),
                None => match component_width {
                    // `const w = c.width; return w ? { width: w } : undefined;`
                    Some(width) if width != 0.0 && !width.is_nan() => OverlayOptions {
                        width: Some(kiana_tui::tui::SizeValue::Absolute(width)),
                        ..OverlayOptions::default()
                    },
                    _ => OverlayOptions::default(),
                },
            };
            let inner = Rc::new(RefCell::new(self.inner.borrow_mut().ui.show_overlay(
                Box::new(SharedComponentProxy::new(Rc::clone(&shared))),
                overlay_options,
            )));
            if let Some(on_handle) = &options.on_handle {
                on_handle(Box::new(SharedOverlayHandle(Rc::clone(&inner))));
            }
            UI_STATE.with(|state| {
                state.borrow_mut().dialogs.insert(
                    token,
                    DialogRuntime {
                        component: shared,
                        dispose: None,
                        overlay: Some(Box::new(SharedOverlayHandle(inner))),
                        saved_editor_text: Some(runtime.saved_editor_text),
                    },
                );
            });
            self.request_render();
        } else {
            UI_STATE.with(|state| {
                state.borrow_mut().dialogs.insert(
                    token,
                    DialogRuntime {
                        component: Rc::clone(&shared),
                        dispose: None,
                        overlay: None,
                        saved_editor_text: Some(runtime.saved_editor_text),
                    },
                );
            });
            self.push_extension_dialog(shared, None);
        }
    }

    /// 处理 `custom()` 的 `done(result)`（UI 变更同样延后一拍，见 [`Self::finish_dialog_token`]）。
    ///
    /// 与 `finish_dialog_token` 相同的“先同步取 token、再延后关 UI”模式。若组件还没挂载
    /// （工厂 future 未完成），`dialogs` 里没有条目，此时把 `CustomRuntime.closed` 置真；
    /// 等组件就绪后 `mount_custom_component` 会丢弃它。responder 在这里就 resolve，因此
    /// 扩展拿到结果与组件是否上屏无关。
    fn handle_custom_done(&mut self, token: u64, result: Box<dyn Any + Send>) {
        let responder = UI_STATE.with(|state| state.borrow_mut().responders.shift_remove(&token));
        let runtime = UI_STATE.with(|state| state.borrow_mut().dialogs.shift_remove(&token));
        if runtime.is_none() {
            UI_STATE.with(|state| {
                if let Some(custom) = state.borrow_mut().custom.get_mut(&token) {
                    custom.closed = true;
                }
            });
        }
        let weak = self.downgrade();
        tokio::task::spawn_local(async move {
            let Some(inner) = weak.upgrade() else {
                if let Some(responder) = responder {
                    resolve_responder_default(responder);
                }
                return;
            };
            let mut mode = InteractiveMode { inner };
            mode.close_dialog_runtime(runtime);
            if let Some(responder) = responder {
                resolve_responder(responder, DialogValue::Custom(result));
            }
            mode.request_render();
        });
    }

    /// 创建一个扩展 UI 上下文（每次 `bindExtensions` 一份）。
    ///
    /// 步骤：建通道 → 用当前状态初始化快照 → 覆盖线程局部 `EVENT_SENDER` → 把快照登记
    /// 进 `SNAPSHOT_REGISTRY` → 在 LocalSet 上 spawn `run_ui_loop` → 返回桥接对象。
    ///
    /// 每次调用都会新增一条通道和一个 drain 任务；各调用点
    ///（`bindCurrentSessionExtensions`、`createProjectTrustContext`、`show_extension_editor`、
    /// 测试接缝）拿到的上下文相互独立，请求之间没有全局顺序。
    pub(crate) fn create_extension_ui_context(&mut self) -> Arc<dyn ExtensionUiContext> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<UiMessage>();
        let snapshot = Arc::new(RwLock::new(self.ui_snapshot()));
        EVENT_SENDER.with(|slot| {
            *slot.borrow_mut() = Some(sender.clone());
        });
        register_snapshot(&snapshot);
        let weak = self.downgrade();
        let task_sender = sender.clone();
        tokio::task::spawn_local(async move {
            run_ui_loop(weak, receiver, task_sender).await;
        });
        Arc::new(InteractiveUiContext { sender, snapshot })
    }

    /// 从 `InteractiveModeInner` 读取一份新快照；`*_pending` 一律为 false（这是实时值，
    /// 不是“等待 drain 的本地写入”）。
    fn ui_snapshot(&self) -> UiSnapshot {
        let inner = self.inner.borrow();
        UiSnapshot {
            editor_text: inner.editor.get_expanded_text(),
            tools_expanded: inner.tool_output_expanded,
            editor_component: inner.editor_component_factory.clone(),
            editor_text_pending: false,
            tools_expanded_pending: false,
            editor_component_pending: false,
        }
    }

    /// 把当前状态发布到所有仍存活的上下文快照（同步 getter 的跨线程回退值）。
    ///
    /// 调用点：drain 循环每处理完一条消息、`set_custom_editor_component`，以及
    /// `widgets.rs` 里会改变编辑器/工具展开状态的扩展操作。
    pub(crate) fn publish_ui_snapshot(&self) {
        publish_snapshot_to_contexts(self.ui_snapshot());
    }
}

// ---------------------------------------------------------------------------
// 对话框辅助
// ---------------------------------------------------------------------------

/// 把一次完成值按 responder 的类型写回 oneshot。
///
/// 类型不匹配时按“空/否”处理，而不是报错：`Select` / `Text` 取 `DialogValue::text()`
/// （非 `Text` 变体 → `None`），`Confirm` 只认 `Bool(true)` 或文本 `"Yes"`，`Custom`
/// 取擦除结果（缺失时为 `()`）。发送失败（接收端已经 drop）被忽略——调用方可能已不再
/// 等待这个结果。
fn resolve_responder(responder: Responder, value: DialogValue) {
    match responder {
        Responder::Select(sender) => {
            let _ = sender.send(value.text());
        }
        Responder::Confirm(sender) => {
            // `confirm()` 走 select 实现（选项 Yes/No）：把选中的文本映射成布尔值。
            let result = match value {
                DialogValue::Bool(value) => value,
                DialogValue::Text(Some(value)) => value == "Yes",
                _ => false,
            };
            let _ = sender.send(result);
        }
        Responder::Text(sender) => {
            let _ = sender.send(value.text());
        }
        Responder::PermissionCard(sender) => {
            // fail-closed：没有可用的选择（UI 关闭 / reset）时一律按 Reject 收尾，绝不
            // 把一次未完成的审批当成 Allow。
            let choice = match value {
                DialogValue::PermissionCard(choice) => Some(choice),
                _ => Some(PermissionCardChoice::Reject {
                    reason: Some("Approval was cancelled.".to_string()),
                }),
            };
            let _ = sender.send(choice);
        }
        Responder::Custom(sender) => {
            let _ = sender.send(value.custom());
        }
    }
}

/// 用 `DialogValue::None` 收尾：模式销毁时让挂起的对话框 future 以默认值结束。
fn resolve_responder_default(responder: Responder) {
    resolve_responder(responder, DialogValue::None);
}

/// overlay 句柄的共享包装（同一个底层句柄可以既交给 `onHandle` 又保存在对话框栈里）。
///
/// 所有方法都转发给底层句柄；`Rc<RefCell<_>>` 让两处持有者看到同样的隐藏/聚焦状态。
/// 只在 UI 线程使用（底层 `OverlayHandle` 不是 `Send`）。
struct SharedOverlayHandle(Rc<RefCell<Box<dyn OverlayHandle>>>);

impl OverlayHandle for SharedOverlayHandle {
    fn hide(&mut self) {
        self.0.borrow_mut().hide();
    }
    fn set_hidden(&mut self, hidden: bool) {
        self.0.borrow_mut().set_hidden(hidden);
    }
    fn is_hidden(&self) -> bool {
        self.0.borrow().is_hidden()
    }
    fn focus(&mut self) {
        self.0.borrow_mut().focus();
    }
    fn unfocus(&mut self, options: Option<kiana_tui::tui::OverlayUnfocusOptions>) {
        self.0.borrow_mut().unfocus(options);
    }
    fn is_focused(&self) -> bool {
        self.0.borrow().is_focused()
    }
    fn bounds(&self) -> Option<kiana_tui::tui::OverlayBounds> {
        self.0.borrow().bounds()
    }
}

// ---------------------------------------------------------------------------
// UI drain 循环
// ---------------------------------------------------------------------------

/// UI 线程上的 drain 循环：串行处理请求与组件事件。
///
/// 每条消息的固定顺序是“处理 → `drain_custom_ready` → `publish_snapshot`”：
/// * `process_request` 可能改变编辑器/工具展开状态；
/// * `drain_custom_ready` 挂载刚完成的 `custom` 组件（可能再次改变 UI）；
/// * 最后发布快照，跨线程 getter 才能看到新值。
///
/// `weak` 升级失败说明 `InteractiveMode` 已销毁：请求走 `respond_request_default` 用
/// 默认值收尾，事件直接丢弃，循环继续消费。循环自己也持有 `sender` 克隆，所以外部把
/// 上下文全部 drop 后通道不会关闭，任务会一直等到运行时结束。
async fn run_ui_loop(
    weak: std::rc::Weak<std::cell::RefCell<super::InteractiveModeInner>>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<UiMessage>,
    sender: tokio::sync::mpsc::UnboundedSender<UiMessage>,
) {
    while let Some(message) = receiver.recv().await {
        match message {
            UiMessage::Request(request) => {
                let Some(inner) = weak.upgrade() else {
                    respond_request_default(request);
                    continue;
                };
                let mut mode = InteractiveMode { inner };
                process_request(&mut mode, request, &sender);
                drain_custom_ready(&weak);
                publish_snapshot(&weak);
            }
            UiMessage::Event(UiEvent::CustomDone { token, result }) => {
                if let Some(inner) = weak.upgrade() {
                    let mut mode = InteractiveMode { inner };
                    mode.handle_custom_done(token, result);
                }
                drain_custom_ready(&weak);
                publish_snapshot(&weak);
            }
            UiMessage::Event(UiEvent::CustomReady) => {
                drain_custom_ready(&weak);
                publish_snapshot(&weak);
            }
        }
    }
}

/// 升级 `Weak` 并发布快照；模式已销毁时什么也不做。
fn publish_snapshot(weak: &std::rc::Weak<std::cell::RefCell<super::InteractiveModeInner>>) {
    let Some(inner) = weak.upgrade() else {
        return;
    };
    let mode = InteractiveMode { inner };
    mode.publish_ui_snapshot();
}

/// 把 `custom_ready` 里排队的组件挂到 UI 上。
///
/// 必须在 UI 线程调用（组件是 `!Send`）。`weak` 升级失败时停止处理剩余项（模式已销毁）。
/// `CustomOutcome::Failed` 以 `DialogValue::None` 结束对话框；`Component` 交给
/// `mount_custom_component` 决定 overlay 还是内联。
fn drain_custom_ready(weak: &std::rc::Weak<std::cell::RefCell<super::InteractiveModeInner>>) {
    let ready = UI_STATE.with(|state| std::mem::take(&mut state.borrow_mut().custom_ready));
    for item in ready {
        let Some(inner) = weak.upgrade() else {
            break;
        };
        let mut mode = InteractiveMode { inner };
        match item.outcome {
            CustomOutcome::Component(component) => {
                mode.mount_custom_component(item.token, component);
            }
            CustomOutcome::Failed => {
                mode.finish_dialog_token(item.token, DialogValue::None);
            }
        }
    }
}

thread_local! {
    /// UI 线程创建过的上下文快照（Weak：上下文被扩展持有期间才存活）。
    static SNAPSHOT_REGISTRY: RefCell<Vec<std::sync::Weak<RwLock<UiSnapshot>>>> =
        const { RefCell::new(Vec::new()) };
}

/// 登记一个上下文快照（`Weak`），顺手清掉已经失效的条目。
///
/// 用 `Weak` 是因为扩展可能长期持有上下文，而快照的生命周期应跟随上下文；模式不负责
/// 主动注销，下一次注册或发布时自然回收。
fn register_snapshot(snapshot: &Arc<RwLock<UiSnapshot>>) {
    SNAPSHOT_REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.retain(|weak| weak.strong_count() > 0);
        registry.push(Arc::downgrade(snapshot));
    });
}

/// 把一份状态写入所有存活上下文（同步 getter 的跨线程回退值）。
///
/// 每个上下文拿到同一份 `UiSnapshot` 的克隆；某个上下文的 `RwLock` 中毒时跳过它（保留
/// 旧值），不影响其它上下文。失效的 `Weak` 在遍历中移除。
fn publish_snapshot_to_contexts(snapshot: UiSnapshot) {
    SNAPSHOT_REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.retain(|weak| {
            let Some(context) = weak.upgrade() else {
                return false;
            };
            if let Ok(mut slot) = context.write() {
                *slot = snapshot.clone();
            }
            true
        });
    });
}

/// 模式已销毁时用默认值收尾请求。
///
/// 只处理带 token 的对话框请求（`Select` / `Input` / `Editor` / `CustomStart`）：从
/// `UI_STATE.responders` 取出 responder 并 resolve 默认值。其它请求（notify、状态更新等）
/// 没有等待方，直接丢弃。
fn respond_request_default(request: UiRequest) {
    match request {
        UiRequest::Select { token, .. }
        | UiRequest::Input { token, .. }
        | UiRequest::Editor { token, .. }
        | UiRequest::CustomStart { token, .. }
        | UiRequest::PermissionCard { token, .. } => {
            let responder =
                UI_STATE.with(|state| state.borrow_mut().responders.shift_remove(&token));
            if let Some(responder) = responder {
                resolve_responder_default(responder);
            }
        }
        _ => {}
    }
}

/// 在 UI 线程上执行一条桥接请求。
///
/// 大多数分支只是一行转发到 `widgets.rs` 或本文件的方法；需要额外状态/顺序的是：
/// * `SetWorkingMessage`：无论当前是否 working 都记录，只有指示器处于 `Working` 时才
///   立即改文案；
/// * `PasteToEditor`：必须走 `handle_input` 的括号粘贴序列，编辑器才会做粘贴折叠；
/// * `Select` / `Input`：再检查一次 signal（桥接层检查后、drain 之前可能已经取消）；
/// * `CustomStart`：保存编辑器原文、登记 `CustomRuntime`、创建 done 回调并 spawn 工厂
///   future。
///
/// 除 `CustomStart` 的工厂任务与 `Select` / `Input` 可能注册的 abort watcher 外，
/// 所有分支都在当前同步调用里完成。
fn process_request(
    mode: &mut InteractiveMode,
    request: UiRequest,
    sender: &tokio::sync::mpsc::UnboundedSender<UiMessage>,
) {
    match request {
        UiRequest::Notify { message, kind } => mode.show_extension_notify(&message, kind),
        UiRequest::SetStatus { key, text } => mode.set_extension_status(&key, text.as_deref()),
        UiRequest::SetWorkingMessage(message) => {
            // 先记录新文案；只有当前指示器确实是 Working 时才立刻改写它的 message，
            // 否则留到下次进入 working 状态时使用（`working_message` 是持久配置）。
            {
                let mut inner = mode.inner.borrow_mut();
                inner.working_message = message;
            }
            let is_working = mode
                .inner
                .borrow()
                .active_status_indicator
                .as_ref()
                .is_some_and(|indicator| indicator.kind() == StatusIndicatorKind::Working);
            if is_working {
                let message = mode
                    .inner
                    .borrow()
                    .working_message
                    .clone()
                    .unwrap_or_else(|| mode.inner.borrow().default_working_message.clone());
                if let Some(indicator) = &mut mode.inner.borrow_mut().active_status_indicator {
                    indicator.set_message(&message);
                }
            }
        }
        UiRequest::SetWorkingVisible(visible) => mode.set_working_visible(visible),
        UiRequest::SetWorkingIndicator(options) => mode.set_working_indicator(options),
        UiRequest::SetHiddenThinkingLabel(label) => {
            mode.set_hidden_thinking_label(label.as_deref())
        }
        UiRequest::SetWidget {
            key,
            content,
            options,
        } => mode.set_extension_widget(&key, content, options),
        UiRequest::SetFooter(factory) => mode.set_extension_footer(factory),
        UiRequest::SetHeader(factory) => mode.set_extension_header(factory),
        UiRequest::SetTitle(title) => {
            let terminal = mode.inner.borrow().ui.terminal();
            terminal.borrow_mut().set_title(&title);
        }
        UiRequest::PasteToEditor(text) => {
            // TS: `this.editor.handleInput(`\x1b[200~${text}\x1b[201~`)` —— 作为括号粘贴
            // 输入喂给编辑器（会触发大内容折叠），而不是插入字面量转义序列。
            let bracketed = format!("\x1b[200~{text}\x1b[201~");
            match &mode.inner.borrow().editor {
                ActiveEditor::Default { handle, .. } => {
                    handle.borrow_mut().handle_input(&bracketed)
                }
                ActiveEditor::Custom { handle, .. } => handle.borrow_mut().handle_input(&bracketed),
            }
            mode.request_render();
        }
        UiRequest::SetEditorText(text) => {
            // 快照由 drain 循环在处理完后统一发布（pending 标志随之清零）。
            mode.inner.borrow().editor.set_text(&text);
        }
        UiRequest::AddAutocompleteProvider(factory) => {
            mode.inner
                .borrow_mut()
                .autocomplete_provider_wrappers
                .push(factory);
            mode.setup_autocomplete_provider();
        }
        UiRequest::SetEditorComponent(factory) => mode.set_custom_editor_component(factory),
        UiRequest::SetToolsExpanded(expanded) => mode.set_tools_expanded(expanded),
        UiRequest::SetThemeSetting(name) => {
            let settings_manager = mode.settings_manager();
            if settings_manager.get_theme().as_deref() != Some(name.as_str()) {
                settings_manager.set_theme(name);
            }
        }
        UiRequest::AddTerminalInput(handler) => mode.add_extension_terminal_input_listener(handler),
        UiRequest::RemoveTerminalInput(handler) => {
            mode.remove_extension_terminal_input_listener(&handler)
        }
        UiRequest::Select {
            token,
            title,
            options,
            opts,
            blocked,
        } => {
            // 桥接层发请求前检查过一次 signal，但取消可能发生在这条消息排队之后；
            // 这里再查一次，避免闪出一个已经失效的对话框。finish 会以 None 收尾。
            if opts
                .as_ref()
                .and_then(|opts| opts.signal.as_ref())
                .is_some_and(|signal| signal.is_cancelled())
            {
                mode.finish_dialog_token(token, DialogValue::None);
                return;
            }
            mode.begin_extension_selector(token, &title, options, opts, blocked);
        }
        UiRequest::Input {
            token,
            title,
            placeholder,
            opts,
        } => {
            // 与 Select 相同：排队期间被取消的请求直接以 None 结束。
            if opts
                .as_ref()
                .and_then(|opts| opts.signal.as_ref())
                .is_some_and(|signal| signal.is_cancelled())
            {
                mode.finish_dialog_token(token, DialogValue::None);
                return;
            }
            mode.begin_extension_input(token, &title, placeholder.as_deref(), opts);
        }
        UiRequest::Editor {
            token,
            title,
            prefill,
        } => mode.begin_extension_editor(token, &title, prefill.as_deref()),
        UiRequest::CustomStart {
            token,
            factory,
            options,
        } => {
            let saved_editor_text = mode.inner.borrow().editor.get_text();
            UI_STATE.with(|state| {
                state.borrow_mut().custom.insert(
                    token,
                    CustomRuntime {
                        closed: false,
                        options: options.map(SendCustomOptions::into_options),
                        saved_editor_text,
                    },
                );
            });
            let tui = tui_arc(mode);
            let theme = current_theme_arc();
            let keybindings = Arc::clone(&mode.inner.borrow().keybindings);
            let sender_for_done = sender.clone();
            let done: CustomDoneFn = Arc::new(move |result| {
                let _ =
                    sender_for_done.send(UiMessage::Event(UiEvent::CustomDone { token, result }));
            });
            let future = factory(tui, &theme, keybindings, done);
            let sender_for_ready = sender.clone();
            tokio::task::spawn_local(async move {
                let component = future.await;
                UI_STATE.with(|state| {
                    state.borrow_mut().custom_ready.push(CustomReady {
                        token,
                        outcome: CustomOutcome::Component(component),
                    });
                });
                let _ = sender_for_ready.send(UiMessage::Event(UiEvent::CustomReady));
            });
        }
        UiRequest::PermissionCard { token, request } => {
            mode.begin_permission_card(token, request);
        }
    }
}

// ---------------------------------------------------------------------------
// 集成测试接缝（doc(hidden)）
// ---------------------------------------------------------------------------

#[doc(hidden)]
impl InteractiveMode {
    /// 创建一个扩展 UI 上下文（等价于 `bindExtensions` 里的那一步）。
    pub fn test_extension_ui_context(&mut self) -> Arc<dyn ExtensionUiContext> {
        self.create_extension_ui_context()
    }

    /// 当前扩展对话框栈深度。
    pub fn test_dialog_count(&self) -> usize {
        self.inner.borrow().extension_dialogs.len()
    }

    /// 渲染第 `index` 个对话框并去掉 ANSI。
    pub fn test_dialog_render(&self, index: usize) -> String {
        let component = {
            let inner = self.inner.borrow();
            inner
                .extension_dialogs
                .get(index)
                .map(|dialog| Rc::clone(&dialog.component))
        };
        let Some(component) = component else {
            return String::new();
        };
        component
            .borrow_mut()
            .render(120)
            .iter()
            .map(|line| kiana_tui::text::strip_terminal_sequences(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 向栈顶对话框发送原始输入。
    pub fn test_send_to_top_dialog(&mut self, data: &str) {
        let component = {
            let inner = self.inner.borrow();
            inner
                .extension_dialogs
                .last()
                .map(|dialog| Rc::clone(&dialog.component))
        };
        if let Some(component) = component {
            component.borrow_mut().handle_input(data);
        }
    }

    /// 编辑器槽里的子项数量（对话框/重载框会替换编辑器）。
    pub fn test_editor_slot_child_count(&self) -> usize {
        self.inner.borrow().editor_container.borrow().children.len()
    }

    /// `resetExtensionUI()` 的测试接缝。
    pub fn test_reset_extension_ui(&mut self) {
        self.reset_extension_ui();
    }

    /// `setCustomEditorComponent(factory)` 的测试接缝。
    pub fn test_set_custom_editor_component(&mut self, factory: Option<EditorFactory>) {
        self.set_custom_editor_component(factory);
    }

    /// 向活动编辑器发送原始输入（默认或扩展编辑器）。
    pub fn test_send_to_active_editor(&mut self, data: &str) {
        match &self.inner.borrow().editor {
            ActiveEditor::Default { handle, .. } => handle.borrow_mut().handle_input(data),
            ActiveEditor::Custom { handle, .. } => handle.borrow_mut().handle_input(data),
        }
    }

    /// 在默认编辑器上安装 `onEscape`（`setupKeyHandlers` 的测试替身）。
    pub fn test_set_default_editor_on_escape(&mut self, handler: Box<dyn FnMut()>) {
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_on_escape(Some(handler));
    }

    /// 在默认编辑器上安装 `onCtrlD`。
    pub fn test_set_default_editor_on_ctrl_d(&mut self, handler: Box<dyn FnMut()>) {
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_on_ctrl_d(Some(handler));
    }

    /// 在默认编辑器上安装 `onPasteImage`。
    pub fn test_set_default_editor_on_paste_image(&mut self, handler: Box<dyn FnMut()>) {
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_on_paste_image(Some(handler));
    }

    /// 在默认编辑器上安装 `onExtensionShortcut`。
    pub fn test_set_default_editor_on_extension_shortcut(
        &mut self,
        handler: crate::components::custom_editor::ExtensionShortcutHandler,
    ) {
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_on_extension_shortcut(Some(handler));
    }

    /// 在默认编辑器上登记 `onAction(action, handler)`。
    pub fn test_set_default_editor_action_handler(
        &mut self,
        action: kiana_config::keybindings::AppKeybinding,
        handler: Box<dyn FnMut()>,
    ) {
        self.inner
            .borrow()
            .default_editor
            .borrow_mut()
            .set_action_handler(action, handler);
    }

    /// 焦点是否落在编辑器槽的当前组件上（reload blocker / 右键粘贴测试）。
    pub fn test_editor_slot_is_focused(&self) -> bool {
        let inner = self.inner.borrow();
        let container = Rc::clone(&inner.editor_container);
        let container = container.borrow();
        let Some(child) = container.children.first() else {
            return false;
        };
        let address = child.as_ref() as *const dyn Component as *const () as usize;
        let Some(focus) = inner.ui.focus() else {
            return false;
        };
        inner.renderer.borrow().focus_target_of_address(address) == Some(focus)
    }

    /// 直接向 TUI 分发原始输入（不经过终端解析循环）。
    pub fn test_ui_handle_input(&mut self, data: &str) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        renderer.borrow_mut().handle_terminal_input(data);
    }

    /// 当前焦点目标（调试/断言用）。
    pub fn test_ui_focus(&self) -> Option<kiana_tui::tui::FocusTarget> {
        self.inner.borrow().ui.focus()
    }

    /// 直接设置焦点目标（复现跨渲染器切换时的焦点状态）。
    pub fn test_set_ui_focus(&mut self, target: Option<kiana_tui::tui::FocusTarget>) {
        self.inner.borrow_mut().ui.set_focus(target);
    }

    /// 当前聚焦组件的身份（组件地址；`getFocusedComponent()` 的"引用相等"等价物）。
    pub fn test_ui_focus_identity(&self) -> Option<usize> {
        self.inner
            .borrow()
            .renderer
            .borrow()
            .focused_component_identity()
            .map(|identity| identity.address)
    }

    /// TUI 当前是否有 overlay。
    pub fn test_ui_has_overlay(&self) -> bool {
        self.inner.borrow().ui.has_overlay()
    }

    /// 立即渲染一帧（overlay / 选择等渲染侧行为）。
    pub fn test_ui_render_now(&mut self) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        renderer.borrow_mut().render_now(true);
    }

    /// 直接向 TUI 挂载一个子组件。
    pub fn test_ui_add_child(&mut self, component: Box<dyn Component>) {
        let renderer = Rc::clone(&self.inner.borrow().renderer);
        renderer.borrow_mut().add_child(component);
    }

    /// 全屏渲染器当前的屏幕行（`getScreenLines()`）。
    pub fn test_ui_screen_lines(&self) -> Vec<String> {
        self.inner.borrow().renderer.borrow().get_screen_lines()
    }

    /// 全屏文本选择是否仍然存在。
    pub fn test_ui_has_active_selection(&self) -> bool {
        self.inner.borrow().ui.has_active_selection()
    }

    /// 终端明暗通知（主题 auto-sync）是否开启。
    pub fn test_scheme_notifications_enabled(&self) -> bool {
        self.inner
            .borrow()
            .renderer
            .borrow()
            .terminal_color_scheme_notifications_enabled()
    }

    /// `switchTuiMode(mode, restoreProgress?)` 的测试接缝（`startRenderer` 固定为 true，
    /// 与 TS 的交互式调用一致）。
    pub fn test_switch_tui_mode(&mut self, mode: TuiMode, restore_progress: bool) -> bool {
        self.switch_tui_mode(mode, restore_progress, true)
    }

    /// 当前渲染器的模式（TS `this.renderer.mode` / 测试里的 `stableUi.mode`）。
    pub fn test_tui_mode(&self) -> TuiMode {
        self.inner.borrow().renderer.borrow().mode()
    }

    /// `setupAutocompleteProvider()` 的 triggerCharacters 合并结果（测试接缝）。
    ///
    /// 用给定 wrappers 临时重建一次 provider（与 TS 的
    /// `triggerCharacters.push(...provider.triggerCharacters)` + `new Set(...)` 同义），
    /// 随后恢复原 wrappers（不改变已安装的 provider）。
    pub fn test_autocomplete_trigger_characters(
        &mut self,
        wrappers: Vec<AutocompleteProviderFactory>,
    ) -> Option<Vec<char>> {
        let saved = std::mem::replace(
            &mut self.inner.borrow_mut().autocomplete_provider_wrappers,
            wrappers,
        );
        let provider = self.build_autocomplete_provider();
        self.inner.borrow_mut().autocomplete_provider_wrappers = saved;
        provider.trigger_characters()
    }
}
