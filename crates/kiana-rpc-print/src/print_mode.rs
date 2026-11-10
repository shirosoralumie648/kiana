//! modes/print-mode.ts：单次运行（text / json）模式。
//!
//! TS 源：`packages/coding-agent/src/modes/print-mode.ts`（173 行）。规格：spec
//! §2.3（含 §2.3.1-§2.3.3）、§1.4 不变量 1/2/3/12、§2.8、§3.2、§3.4、§4.4.2、§5.1、
//! §5.2.1。
//!
//! # 行为要点
//!
//! - 信号处理器在 `rebindSession()` **之前**注册（SIGTERM 先、SIGHUP 后；win32 没有 SIGHUP）。
//!   处理器顺序固定：`killTrackedDetachedChildren()` -> dispose（幂等）-> `process.exit(143|129)`。
//! - JSON 模式的会话头在 `rebindSession()` **之前**写出（`getHeader()` 为 `null` 时跳过）。
//!   写出的是 `sessionManager.getHeader()` 的**原始 JSON 对象**（保留未知字段与文件键序，
//!   含 v1 迁移在末尾追加的 `version`），而不是 typed `SessionHeader`，见 [`PrintSession::header_json`]。
//! - 订阅在 text 模式也建立（listener 在 text 下是 no-op，与 TS 相同）。
//! - 文本分支只写最后一条 assistant 消息的 text 块，每块后追加 `\n`；`error`/`aborted`
//!   只写 stderr 并返回 1；`length` 先写文本再写 stderr 警告并返回 1。
//! - 所有 JSON 记录都经 [`serialize_wire_line`] / [`serialize_event_line`]（键序修复）。
//!
//! # 信号（Unix）
//!
//! TS 的 `process.on`/`process.off` 语义有两点必须复刻（review findings 1/2）：
//!
//! - `process.off` 只移除监听器，**不会取消已经触发**的处理器：已经进入
//!   `disposeRuntime()` 的 handler 仍然会走到 `process.exit(143|129)`。因此 Rust 侧
//!   不用 `JoinHandle::abort()` 做清理，而是用“取消只作用于等待信号的 select 分支”：
//!   一旦信号分支胜出，清理不再影响它。
//! - 移除最后一个监听器后 Node 恢复信号的默认处置：清理时把注册前保存的 `sigaction`
//!   还原，因此 `run_print_mode` 返回后（含 finally 里 dispose/flush 期间）SIGTERM/SIGHUP
//!   重新按默认动作终止进程。`tokio::signal::unix` 没有反注册（tokio 文档明说 libc handler
//!   永不注销），所以这里直接用 `sigaction` + self-pipe + [`tokio::io::unix::AsyncFd`]
//!   接收信号：信号处理器只做 async-signal-safe 的单字节 `write`，异步任务读管道后执行
//!   dispose -> exit。
//!
//! # 已知差异（见单元报告）
//!
//! 1. **信号**：Unix 用 `crate::signal` 的 self-pipe 实现（同一进程同一时刻只支持一个
//!    print 模式运行；TS 允许多个 `process.on` 监听器）。Windows 没有 `sigaction`，因此
//!    不注册任何处理器（TS 在 win32 仍注册 SIGTERM）。处理器 body 与 TS 相同：
//!    kill -> dispose -> exit，**两条信号路径都不 flush**（`print-mode.ts` 的 handler 从不
//!    调用 `flushRawStdout`；spec §2.3.2、§4.7。注意 RPC 的 SIGHUP 会 flush，见 spec
//!    §2.6.9——那是另一条 `shutdown(exitCode, signal)` 路径）。
//! 2. **错误通道收窄**：`ExtensionCommandContextActions` 在 Rust 没有错误通道，宿主/会话错误
//!    按 `cancelled: true` 收场（reload 忽略）且**不写 stderr**——TS 里这些错误 reject 回
//!    等待中的扩展（扩展可 `catch`），同样不写 stderr；扩展无法区分取消与错误属冻结类型的限制。
//! 3. **JSON 投影错误的收场**：TS 的 listener throw 被 `Agent.runWithLifecycle` 吞掉并转换成
//!    一条 assistant 失败消息的 message_start/message_end/turn_end/agent_end（prompt 正常
//!    resolve、`--mode json` 退出码 0、不写 stderr）。Rust 的监听器签名
//!    `Fn(&AgentSessionEvent)` 没有错误通道，也无法让 agent loop 以“监听器失败”收场，
//!    因此无法合成那 4 条事件、也无法让会话记录失败消息。可观察行为对齐到：畸形事件不写出、
//!    不写 stderr、退出码 0，并抑制该 run 的后续事件直到 `agent_settled`（随后恢复正常）。
//!
//! JSON 模式的 agent 级背压订阅（TS 的 `session.agent.subscribe`）通过
//! [`PrintSession::agent`] 实现：每个 agent 事件后 `await wait_for_backpressure()`。
//! 生产适配器从 `AgentSession::agent()` 取到 `Agent`；测试替身默认不建立该订阅。
//!
//! 自定义 `role: "assistant"` 的 JSON 消息：TS 用 `lastMessage?.role === "assistant"` 判定后
//! 把消息当 `AssistantMessage` 读字段；Rust 侧同样按 `role()` 判定，标准消息走类型化字段，
//! 自定义 JSON 消息按 JS 运行时语义读取（`content` 必须是可迭代对象，见
//! `custom_assistant_exit_code`）。
//!
//! 两个入口 future 都是 `!Send`（`bind_extensions` 内部 await 扩展派发，其
//! `LocalBoxFuture` 不是 `Send`），调用方必须在 `tokio::task::LocalSet` 内驱动；
//! 本模块只用 `tokio::task::spawn_local`，绝不用 `tokio::spawn`。
//!
//! # 在 crate 中的位置与阅读顺序
//!
//! 生产调用方是 `kiana-coding-agent` 的 `app.rs`：`AppMode::Print | AppMode::Json` 分支
//! 用全局 [`crate::GlobalRawStdout`] 调用 [`run_print_mode`]，退出码直接成为进程退出码。
//! 本文件对应 TS 的 `print-mode.ts`（约 173 行），是 `rpc_mode.rs` 的"单次运行"孪生：
//! 两者共享 [`crate::RawStdout`] 写出纪律、`toJsonEvent` 投影（[`crate::json_event`]）与
//! 信号接缝（`crate::signal`，私有模块），但 print 没有协议帧、没有客户端连接，输出直接
//! 面向进程 stdout。阅读顺序：先读本 `//!`，再看 [`PrintSession`] / [`PrintRuntimeHost`]
//! 两个 trait（它们是测试替身的注入面），最后读 `run_print_mode_impl` 的
//! "(2) 信号 -> (3) rebind -> (5)-(7) try -> (8) finally" 阶段注释——编号对应 TS 源
//! `runPrintMode` 内的语句顺序，对拍 TS 时可逐编号对照。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use kiana_agent_core::AgentMessage;
use kiana_agent_core::types::JsonObject;
use kiana_ai::types::{AssistantContentBlock, ImageContent, Message, StopReason};
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::types::{
    ExtensionCommandContextActions, ExtensionError, ExtensionMode, ForkOptions,
    NavigateTreeOptions as ExtNavigateTreeOptions, NewSessionOptions, NewSessionResult,
    SwitchSessionOptions as ExtSwitchSessionOptions,
};
use kiana_session::agent_session::{
    AgentSessionError, AgentSessionEventListener, AgentSessionUnsubscribe, ExtensionBindings,
    NavigateTreeOptions as SessionNavigateTreeOptions, PromptOptions, TreeNavigationResult,
};
use kiana_session::agent_session_runtime::{
    AgentSessionRuntime, ForkSessionOptions, ForkSessionResult, NewSessionOptionsInput,
    ReplacedSessionCallback, SessionReplacementResult, SwitchSessionOptions,
};
use kiana_session::{AgentSession, AgentSessionEvent, panel_commands};
use kiana_session_core::session_manager::SessionHeader;

use crate::RawStdout;
use crate::json_event::{serialize_event_line, serialize_wire_line};

/// print 模式的输出模式（TS：`mode: "text" | "json"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintOutputMode {
    /// 只输出最后一条 assistant 消息的 text 块。
    #[default]
    Text,
    /// 输出会话头 + 全量 JSON 事件流。
    Json,
}

/// print 模式的选项。
///
/// TS: `print-mode.ts#PrintModeOptions`。`messages` 默认空；`initial_message` 为空字符串时跳过
/// （TS 的 `if (initialMessage)`）；`initial_images` 只附加到 initial message。
#[derive(Debug, Clone, Default)]
pub struct PrintModeOptions {
    /// 输出模式。
    pub mode: PrintOutputMode,
    /// 在 initial message 之后依次 prompt 的额外消息。
    pub messages: Vec<String>,
    /// 第一条消息（可能含 `@file` 或管道 stdin 内容）。
    pub initial_message: Option<String>,
    /// 只附加到 initial message 的图片。
    pub initial_images: Option<Vec<ImageContent>>,
}

/// print/json 模式绑定的会话面。
///
/// TS `runPrintMode` 直接持有 `AgentSession`；Rust 侧用这个 trait 把会话与宿主解耦，便于
/// 对拍测试注入 mock。除 [`agent_session`](Self::agent_session) 外的默认方法都**逐字**委托给
/// `agent_session()`（TS 的 `session.<method>` 调用）。
///
/// 注意：`agent_session()` 可能 panic（mock 未覆盖某个被调用的方法时），mock 必须覆盖流程
/// 实际调用的每个方法。
// `async-trait` 给每个方法生成 `#[must_use]`，clippy 的 `double_must_use` 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait(?Send)]
pub trait PrintSession: Send + Sync {
    /// 底层 `AgentSession`（生产适配器返回克隆）。
    fn agent_session(&self) -> AgentSession;

    /// `session.messages`。
    fn messages(&self) -> Vec<AgentMessage> {
        self.agent_session().messages()
    }

    /// `session.sessionManager.getHeader()`。
    ///
    /// 这是 typed 视图（供需要结构体的调用方使用）；JSON 模式实际写出的是
    /// [`header_json`](Self::header_json)，按会话头文件的键序原样输出（当前稳定契约，改动需同步测试）。
    fn header(&self) -> Option<SessionHeader> {
        self.agent_session().session_manager().get_header().cloned()
    }

    /// `session.sessionManager.getHeader()` 的**原始 JSON 对象**（无 header 时为 `null`）。
    ///
    /// TS 的 `getHeader()` 返回 `JSON.parse` 得到的普通对象，`JSON.stringify` 因而保留
    /// 文件里的键序与未建模字段（v1 会话迁移时 `migrateV1ToV2` 把 `version` 追加到末尾）。
    /// typed [`SessionHeader`] 只能按结构体字段序输出并丢弃未知字段，故 JSON 模式走这里。
    fn header_json(&self) -> serde_json::Value {
        self.agent_session()
            .with_session_manager_mut(|manager| manager.get_header_json())
    }

    /// `session.subscribe(listener)`。
    fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        self.agent_session().subscribe(listener)
    }

    /// `session.agent`（TS 的公开字段）。
    ///
    /// JSON 模式用它建立 stdout 背压订阅（每个 agent 事件后等写链清空）。只有生产适配器
    /// 能提供；测试替身默认 `None`（不建立该订阅）。
    fn agent(&self) -> Option<kiana_agent_core::Agent> {
        None
    }

    /// `session.bindExtensions(bindings)`。
    async fn bind_extensions(&self, bindings: ExtensionBindings) {
        self.agent_session().bind_extensions(bindings).await;
    }

    /// `session.prompt(text, options?)`。
    async fn prompt(&self, text: String, options: PromptOptions) -> Result<(), AgentSessionError> {
        self.agent_session().prompt(text, options).await
    }

    /// `session.waitForIdle()`。
    async fn wait_for_idle(&self) {
        self.agent_session().wait_for_idle().await;
    }

    /// `session.navigateTree(targetId, options?)`。
    async fn navigate_tree(
        &self,
        target_id: String,
        options: SessionNavigateTreeOptions,
    ) -> Result<TreeNavigationResult, AgentSessionError> {
        self.agent_session().navigate_tree(target_id, options).await
    }

    /// `session.reload()`（TS 无参数）。
    async fn reload(&self) -> Result<(), AgentSessionError> {
        self.agent_session().reload(None).await
    }
}

/// print/json 模式的宿主面（TS: `AgentSessionRuntime`）。
///
/// 方法签名冻结在 spec/planner 里：`new_session`/`fork`/`switch_session` 直接接收扩展侧
/// 选项结构，保证 `setup`/`withSession` 不丢失。
// 同上：`async-trait` 生成的 `#[must_use]` 与返回类型的 `#[must_use]` 重复。
#[allow(clippy::double_must_use)]
#[async_trait(?Send)]
pub trait PrintRuntimeHost: Send + Sync {
    /// `runtimeHost.session`。
    fn session(&self) -> Arc<dyn PrintSession>;

    /// `runtimeHost.setRebindSession(cb)`；回调忽略传入的会话，重新读取 [`session`](Self::session)。
    fn set_rebind_session(
        &self,
        callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    );

    /// `runtimeHost.newSession(options?)`。
    async fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String>;

    /// `runtimeHost.fork(entryId, options?)`。
    async fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String>;

    /// `runtimeHost.switchSession(sessionPath, options?)`。
    async fn switch_session(
        &self,
        session_path: String,
        options: Option<ExtSwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String>;

    /// `runtimeHost.dispose()`（emit `session_shutdown{reason:"quit"}` 并 dispose 会话）。
    async fn dispose(&self);
}

/// `console.error` 的替身：生产路径固定写进程 stderr，测试可注入记录器。
///
/// 本 seam 是 `#[doc(hidden)]` 的测试接缝（冻结签名之外的补充）；实现必须附加一个尾随 `\n`。
#[doc(hidden)]
pub trait PrintErrorSink: Send + Sync {
    /// 写一行诊断（不含尾随换行）。
    fn write_line(&self, text: &str);
}

/// 生产 e.rror sink：`console.error` == `process.stderr.write(text + "\n")`。
struct StderrErrorSink;

impl PrintErrorSink for StderrErrorSink {
    fn write_line(&self, text: &str) {
        eprintln!("{text}");
    }
}

/// 信号种类（冻结的测试 seam）。
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintSignal {
    /// SIGTERM。
    Term,
    /// SIGHUP（非 win32）。
    Hup,
}

/// [`PrintSignal`] -> 进程信号接缝的信号种类（Unix）。
#[cfg(unix)]
impl From<PrintSignal> for crate::signal::SignalKind {
    fn from(signal: PrintSignal) -> Self {
        match signal {
            PrintSignal::Term => crate::signal::SignalKind::Term,
            PrintSignal::Hup => crate::signal::SignalKind::Hup,
        }
    }
}

/// 信号对应的退出码：SIGTERM 143、SIGHUP 129。
#[doc(hidden)]
pub fn print_signal_exit_code(signal: PrintSignal) -> i32 {
    match signal {
        PrintSignal::Term => 143,
        PrintSignal::Hup => 129,
    }
}

/// 信号路径是否跳过 flush：两条信号路径都跳过。
///
/// TS `print-mode.ts` 的信号 handler 只做 `killTrackedDetachedChildren()` ->
/// `disposeRuntime()` -> `process.exit(129|143)`，从不调用 `flushRawStdout()`（spec
/// §2.3.2、§4.7）；因此 print 的 SIGHUP 与 SIGTERM 一样立即退出，等待中的 raw stdout
/// 写入被丢弃。RPC 侧的 SIGHUP 才会 flush（spec §2.6.9），两者不要混淆。
///
/// 参数保留是为了维持冻结的 seam 签名（调用方按信号种类传入，未来若 TS 行为变化
/// 只需改这里）。
#[doc(hidden)]
pub fn print_signal_skips_flush(_signal: PrintSignal) -> bool {
    true
}

/// 信号处理器 body 的测试入口：执行与生产处理器完全相同的动作
/// （kill tracked children -> dispose 一次 -> 返回 129/143），不 flush、不调用
/// `process::exit`。
#[doc(hidden)]
pub async fn run_print_mode_signal_for_test(
    host: Arc<dyn PrintRuntimeHost>,
    options: PrintModeOptions,
    io: Arc<dyn RawStdout>,
    signal: PrintSignal,
) -> i32 {
    let _ = options;
    let shared = Arc::new(SharedState::default());
    run_signal_shutdown(host, io, shared, signal).await
}

// ============================================================================
// 生产运行时适配器（thread-local token）
// ============================================================================

thread_local! {
    /// 当前线程上的 print 模式 runtime。
    ///
    /// `AgentSessionRuntime` 是 `!Send`（资源加载器 trait 不带 Send/Sync），而
    /// [`PrintRuntimeHost`] 要求 `Send + Sync`。这里沿用
    /// `crates/kiana-session/tests/regression_2860_replaced_session_context.rs` 的线程本地句柄
    /// 方案：零大小的 `Send + Sync` token 在使用时回查线程本地 runtime。
    static LOCAL_RUNTIME: RefCell<Option<Rc<AgentSessionRuntime>>> = const { RefCell::new(None) };
}

/// 安装 runtime 的 RAII 守卫：离开 `run_print_mode` 时清除线程本地槽位。
struct LocalRuntimeGuard;

impl LocalRuntimeGuard {
    fn install(runtime: Rc<AgentSessionRuntime>) -> Self {
        LOCAL_RUNTIME.with(|slot| *lock_cell(slot) = Some(runtime));
        LocalRuntimeGuard
    }
}

impl Drop for LocalRuntimeGuard {
    fn drop(&mut self) {
        LOCAL_RUNTIME.with(|slot| {
            lock_cell(slot).take();
        });
    }
}

fn lock_cell<T>(cell: &RefCell<T>) -> std::cell::RefMut<'_, T> {
    // panic 而不是毒化传播：线程本地槽位只在 `run_print_mode` 顶部安装、guard drop 时
    // 清除，正常流程不存在重入；借用冲突只能来自误用（同一线程并发运行两个 print 模式）。
    cell.try_borrow_mut()
        .expect("print mode thread-local runtime is not re-entered")
}

fn local_runtime() -> Rc<AgentSessionRuntime> {
    LOCAL_RUNTIME
        .with(|slot| slot.borrow().clone())
        .expect("run_print_mode must be driven on the thread that installed the runtime")
}

/// 生产 [`PrintRuntimeHost`]：零大小的 `Send + Sync` 句柄，方法回查线程本地 runtime。
struct LocalPrintRuntimeHost;

/// 生产 [`PrintSession`] 适配器。
struct AgentSessionPrintSession(AgentSession);

#[async_trait(?Send)]
impl PrintSession for AgentSessionPrintSession {
    fn agent_session(&self) -> AgentSession {
        self.0.clone()
    }

    fn agent(&self) -> Option<kiana_agent_core::Agent> {
        Some(self.0.agent())
    }
}

#[async_trait(?Send)]
impl PrintRuntimeHost for LocalPrintRuntimeHost {
    fn session(&self) -> Arc<dyn PrintSession> {
        Arc::new(AgentSessionPrintSession(local_runtime().session()))
    }

    fn set_rebind_session(
        &self,
        callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    ) {
        // TS 的 `setRebindSession(async () => { await rebindSession(); })` 忽略传入的 session。
        local_runtime()
            .set_rebind_session(Some(Arc::new(move |_session: AgentSession| callback())));
    }

    async fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let runtime = local_runtime();
        let options = options.map(convert_new_session_options);
        runtime
            .new_session(options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        let runtime = local_runtime();
        let options = options.map(convert_fork_options);
        runtime
            .fork(&entry_id, options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn switch_session(
        &self,
        session_path: String,
        options: Option<ExtSwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let runtime = local_runtime();
        let options = options.map(convert_switch_session_options);
        runtime
            .switch_session(&session_path, options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn dispose(&self) {
        local_runtime().dispose().await;
    }
}

/// 扩展侧 `NewSessionOptions` -> 运行时输入（`withSession` 原样转换，见 regression #2860）。
fn convert_new_session_options(options: NewSessionOptions) -> NewSessionOptionsInput {
    NewSessionOptionsInput {
        parent_session: options.parent_session,
        setup: options.setup,
        with_session: options.with_session.map(|callback| {
            let callback: ReplacedSessionCallback = callback;
            callback
        }),
    }
}

/// 扩展侧 `ForkOptions` -> 运行时选项。
fn convert_fork_options(options: ForkOptions) -> ForkSessionOptions {
    ForkSessionOptions {
        position: options.position,
        with_session: options.with_session.map(|callback| {
            let callback: ReplacedSessionCallback = callback;
            callback
        }),
    }
}

/// 扩展侧 `SwitchSessionOptions` -> 运行时选项（TS 的 `cwdOverride`/信任工厂总是缺省）。
fn convert_switch_session_options(options: ExtSwitchSessionOptions) -> SwitchSessionOptions {
    SwitchSessionOptions {
        cwd_override: None,
        with_session: options.with_session.map(|callback| {
            let callback: ReplacedSessionCallback = callback;
            callback
        }),
        project_trust_context_factory: None,
    }
}

// ============================================================================
// 共享状态
// ============================================================================

/// 主流程与信号处理器共享的幂等状态（TS 的 `disposed` / `unsubscribe` 变量）。
#[derive(Default)]
struct SharedState {
    /// `disposed`：dispose 只执行一次（信号 + finally 竞争）。
    disposed: AtomicBool,
    /// 信号路径已开始（finally 不再 flush）。
    signal_triggered: AtomicBool,
    /// `session.subscribe` 的退订句柄。
    unsubscribe: Mutex<Option<AgentSessionUnsubscribe>>,
    /// agent 级背压订阅的退订句柄；rebind 时先退订旧订阅再建新的（字段与
    /// [`unsubscribe`](Self::unsubscribe) 的生命周期完全平行）。
    unsubscribe_backpressure: Mutex<Option<AgentSessionUnsubscribe>>,
    /// 发生过一次 `toJsonEvent` 投影失败：抑制当前 run 的后续事件，直到 `agent_settled`
    /// 或当前 `prompt` 返回（见模块文档第 3 条）。
    projection_failed: AtomicBool,
}

impl SharedState {
    fn projection_failed(&self) -> bool {
        self.projection_failed.load(Ordering::SeqCst)
    }

    fn set_projection_failed(&self) {
        self.projection_failed.store(true, Ordering::SeqCst);
    }

    fn clear_projection_failed(&self) {
        self.projection_failed.store(false, Ordering::SeqCst);
    }

    fn take_unsubscribe(&self) -> Option<AgentSessionUnsubscribe> {
        lock(&self.unsubscribe).take()
    }

    fn set_unsubscribe(&self, unsubscribe: AgentSessionUnsubscribe) {
        *lock(&self.unsubscribe) = Some(unsubscribe);
    }

    fn take_unsubscribe_backpressure(&self) -> Option<AgentSessionUnsubscribe> {
        lock(&self.unsubscribe_backpressure).take()
    }

    fn set_unsubscribe_backpressure(&self, unsubscribe: AgentSessionUnsubscribe) {
        *lock(&self.unsubscribe_backpressure) = Some(unsubscribe);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 毒化的锁也照常取值：槽位里的句柄不因某次持锁 panic 而失效，恢复比 panic 更接近
    // TS 无锁语义下的行为。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 从槽位克隆当前会话（不跨 await 持锁）。
// TS 的 `session` 是一个可被 rebind 覆盖的共享变量；Rust 用“锁内 Arc 克隆”复刻：
// 每个 `session.<method>` 调用点都重新取一次，保证拿到的是最新会话。
fn current_session(slot: &Arc<Mutex<Arc<dyn PrintSession>>>) -> Arc<dyn PrintSession> {
    Arc::clone(&lock(slot))
}

// ============================================================================
// 入口
// ============================================================================

/// 生产入口：`runPrintMode(runtimeHost, options)`。
///
/// 该 future 是 `!Send`，调用方必须在 `tokio::task::LocalSet` 内驱动。
///
/// 生产路径会注册 SIGTERM/SIGHUP 处理器（Unix）；[`run_print_mode_with_host`] 是测试 seam，
/// 不注册进程级信号处理器（信号 body 由 [`run_print_mode_signal_for_test`] 覆盖）。
pub async fn run_print_mode(
    runtime: AgentSessionRuntime,
    options: PrintModeOptions,
    io: Arc<dyn RawStdout>,
) -> i32 {
    let runtime = Rc::new(runtime);
    let _guard = LocalRuntimeGuard::install(runtime);
    let host: Arc<dyn PrintRuntimeHost> = Arc::new(LocalPrintRuntimeHost);
    run_print_mode_impl(host, options, io, Arc::new(StderrErrorSink), true).await
}

/// 宿主 seam：`runPrintMode(runtimeHost, options)`，供对拍测试注入 mock 宿主。
///
/// 与生产入口的唯一区别是**不注册**进程级信号处理器；其余行为逐条一致。
///
/// **仅供测试**：生产调用方（`kiana-coding-agent`）必须使用 [`run_print_mode`]，否则会丢失
/// `killTrackedDetachedChildren`、信号路径 dispose 与 143/129 退出码语义。在测试进程里注册
/// tokio 信号源会吞掉默认的 SIGTERM 行为，因此这里不注册。
pub async fn run_print_mode_with_host(
    host: Arc<dyn PrintRuntimeHost>,
    options: PrintModeOptions,
    io: Arc<dyn RawStdout>,
) -> i32 {
    run_print_mode_impl(host, options, io, Arc::new(StderrErrorSink), false).await
}

/// 带错误 sink 的隐藏入口（测试用，精确断言 stderr 行）。
#[doc(hidden)]
pub async fn run_print_mode_with_host_and_error_sink(
    host: Arc<dyn PrintRuntimeHost>,
    options: PrintModeOptions,
    io: Arc<dyn RawStdout>,
    errors: Arc<dyn PrintErrorSink>,
) -> i32 {
    run_print_mode_impl(host, options, io, errors, false).await
}

/// 带错误 sink **且注册真实信号处理器**的隐藏入口。
///
/// 生产路径 [`run_print_mode`] 的 mock 宿主版本：`/proc/self` 之外没有可注入的
/// `AgentSessionRuntime` 构造器，因此信号行为（process.on/off 语义、in-flight 不取消、
/// 默认处置恢复）只能在子进程里用 mock 宿主验证。测试进程**禁止**在自身调用它
/// （处理器触发时会 `process::exit`）。
#[doc(hidden)]
pub async fn run_print_mode_with_host_and_signal_handlers(
    host: Arc<dyn PrintRuntimeHost>,
    options: PrintModeOptions,
    io: Arc<dyn RawStdout>,
    errors: Arc<dyn PrintErrorSink>,
) -> i32 {
    run_print_mode_impl(host, options, io, errors, true).await
}

async fn run_print_mode_impl(
    // 阶段划分严格对应 TS `runPrintMode` 的语句顺序：
    // (1) 声明共享状态（`disposed`/`unsubscribe` 等变量的 Rust 版是 `SharedState`）；
    // (2) 注册信号处理器（先于 setRebindSession）；
    // (3) setRebindSession；
    // (5)-(7) try 主体（写 JSON 头 -> rebind -> 逐条 prompt -> 文本输出）；
    // (8) finally（移除信号处理器 -> dispose（幂等）-> flush，信号路径不 flush）。
    // catch 分支只在 `try_body` 返回 Err 时写 stderr 并置退出码 1。
    host: Arc<dyn PrintRuntimeHost>,
    options: PrintModeOptions,
    io: Arc<dyn RawStdout>,
    errors: Arc<dyn PrintErrorSink>,
    install_signal_handlers: bool,
) -> i32 {
    let PrintModeOptions {
        mode,
        messages,
        initial_message,
        initial_images,
    } = options;

    let shared = Arc::new(SharedState::default());
    // TS: `let session = runtimeHost.session;`（rebind 前先拿一次，供 JSON 头使用）。
    let session_slot: Arc<Mutex<Arc<dyn PrintSession>>> = Arc::new(Mutex::new(host.session()));
    let mut signal_cleanups: Vec<Box<dyn Fn()>> = Vec::new();

    // (2) 信号处理器必须在 setRebindSession / rebindSession 之前注册。
    if install_signal_handlers {
        register_signal_handlers(
            Arc::clone(&host),
            Arc::clone(&io),
            Arc::clone(&shared),
            &mut signal_cleanups,
        );
    }

    // (3) `runtimeHost.setRebindSession(async () => { await rebindSession(); })`。
    host.set_rebind_session(make_rebind_callback(
        Arc::clone(&host),
        Arc::clone(&session_slot),
        Arc::clone(&shared),
        Arc::clone(&io),
        Arc::clone(&errors),
        mode,
    ));

    // (5)-(7) try / catch。
    let exit_code = match try_body(
        &host,
        &session_slot,
        &shared,
        &io,
        &errors,
        mode,
        &messages,
        initial_message.as_deref().filter(|text| !text.is_empty()),
        initial_images,
    )
    .await
    {
        Ok(exit_code) => exit_code,
        Err(message) => {
            errors.write_line(&message);
            1
        }
    };

    // (8) finally：先移除信号处理器，再 dispose（幂等），最后 flush（信号路径不 flush）。
    for cleanup in signal_cleanups {
        cleanup();
    }
    dispose_runtime(&host, &shared).await;
    if !shared.signal_triggered.load(Ordering::SeqCst) {
        io.flush().await;
    }
    exit_code
}

/// 文本模式的面板命令拦截。
///
/// `None` 表示不是面板命令（或当前是 JSON 模式），调用方继续 `prompt`；`Some(Ok(()))`
/// 表示文本已经写出；`Some(Err(message))` 是用法的错误文本（由 `try_body` 转成
/// stderr + 退出码 1）。
///
/// 只在 `PrintOutputMode::Text` 生效：JSON 模式的事件流有固定协议，写裸文本会破坏它；
/// 非面板命令也不在这里取 `AgentSession`，保持既有 `prompt` 路径（测试替身不实现
/// `agent_session`）。
async fn try_panel_command(
    session_slot: &Arc<Mutex<Arc<dyn PrintSession>>>,
    io: &Arc<dyn RawStdout>,
    mode: PrintOutputMode,
    text: &str,
) -> Option<Result<(), String>> {
    if mode != PrintOutputMode::Text || !panel_commands::is_panel_command_text(text) {
        return None;
    }
    let session = current_session(session_slot);
    let agent_session = session.agent_session();
    match panel_commands::execute_headless(&agent_session, text).await {
        Some(Ok(output)) => {
            io.write_raw(&format!("{output}\n"));
            Some(Ok(()))
        }
        Some(Err(message)) => Some(Err(message)),
        None => None,
    }
}

/// `try` 主体（严格 TS 顺序）。
#[allow(clippy::too_many_arguments)]
async fn try_body(
    host: &Arc<dyn PrintRuntimeHost>,
    session_slot: &Arc<Mutex<Arc<dyn PrintSession>>>,
    shared: &Arc<SharedState>,
    io: &Arc<dyn RawStdout>,
    errors: &Arc<dyn PrintErrorSink>,
    mode: PrintOutputMode,
    messages: &[String],
    initial_message: Option<&str>,
    initial_images: Option<Vec<ImageContent>>,
) -> Result<i32, String> {
    // (5) JSON 模式：会话头在 rebindSession() 之前写。
    //
    // TS 写出的是 `session.sessionManager.getHeader()` 的原始对象：键序与未知字段来自
    // 会话文件（v1 迁移把 `version` 追加在末尾），因此必须用 [`PrintSession::header_json`]
    // 而不是 typed `header()`。`get_header_json()` 无 header 时返回 `null`，对应 TS 的
    // `if (header)`（null/undefined 都跳过）。
    if mode == PrintOutputMode::Json {
        let session = current_session(session_slot);
        let header = session.header_json();
        if !header.is_null() {
            let line = serialize_wire_line(&header).map_err(|error| error.to_string())?;
            io.write_raw(&line);
        }
    }

    rebind_session(host, session_slot, shared, io, errors, mode).await;

    // (5) initialMessage 为空字符串时跳过（TS 的 `if (initialMessage)`）。
    if let Some(initial_message) = initial_message {
        // 面板命令（`/permissions`、`/tasks`）在文本模式下直接输出稳定文本，不触发
        // 模型请求；JSON 模式仍走既有事件协议（设计文档 §6.3 的门槛）。
        if let Some(result) = try_panel_command(session_slot, io, mode, initial_message).await {
            result?;
            shared.clear_projection_failed();
        } else {
            let session = current_session(session_slot);
            let result = session
                .prompt(
                    initial_message.to_string(),
                    PromptOptions {
                        images: initial_images,
                        ..Default::default()
                    },
                )
                .await;
            // 该次 run 结束：解除投影失败后的抑制（TS 里失败的 run 已经结束，后续 prompt
            // 会开始新的 run）。
            shared.clear_projection_failed();
            result.map_err(|error| error.to_string())?;
        }
    }

    for message in messages {
        if let Some(result) = try_panel_command(session_slot, io, mode, message).await {
            result?;
            shared.clear_projection_failed();
            continue;
        }
        // TS 的循环每轮读取变量 `session`（扩展命令可能在 prompt 期间 rebind）。
        let session = current_session(session_slot);
        let result = session
            .prompt(message.clone(), PromptOptions::default())
            .await;
        shared.clear_projection_failed();
        result.map_err(|error| error.to_string())?;
    }

    shared.clear_projection_failed();

    // (6) 文本输出块：TS 只按 `lastMessage?.role === "assistant"` 判定。
    let exit_code = if mode == PrintOutputMode::Text {
        let session = current_session(session_slot);
        let state_messages = session.messages();
        match state_messages.last() {
            Some(message) if message.role() == "assistant" => match message {
                AgentMessage::Custom(object) => custom_assistant_exit_code(object, io, errors)?,
                AgentMessage::Standard(Message::Assistant(assistant)) => {
                    standard_assistant_exit_code(assistant, io, errors)
                }
                // `role() == "assistant"` 的标准消息只可能是 assistant。
                _ => 0,
            },
            _ => 0,
        }
    } else {
        0
    };

    Ok(exit_code)
}

/// 标准 assistant 消息的文本分支（TS `assistantMsg.stopReason` 等）。
///
/// 返回退出码：`error`/`aborted` 与 `length` 都是 1（错误/警告写 stderr），正常结束写
/// text 块后返回 0。text 块逐个写出（每块尾随一个 `\n`），非 text 块（如图片）跳过。
fn standard_assistant_exit_code(
    assistant: &kiana_ai::types::AssistantMessage,
    io: &Arc<dyn RawStdout>,
    errors: &Arc<dyn PrintErrorSink>,
) -> i32 {
    match assistant.stop_reason {
        StopReason::Error | StopReason::Aborted => {
            // TS: `errorMessage || `Request ${stopReason}``；空串是 falsy。
            let fallback = if assistant.stop_reason == StopReason::Error {
                "Request error"
            } else {
                "Request aborted"
            };
            let message = match assistant.error_message.as_deref() {
                Some(error_message) if !error_message.is_empty() => error_message,
                _ => fallback,
            };
            errors.write_line(message);
            1
        }
        _ => {
            for content in &assistant.content {
                if let AssistantContentBlock::Text(text) = content {
                    io.write_raw(&format!("{}\n", text.text));
                }
            }
            if assistant.stop_reason == StopReason::Length {
                errors.write_line(
                    "Response was truncated: the model reached its output token limit.",
                );
                return 1;
            }
            // 本地预算 cap 是异常终止（不是模型自然结束）；文本仍照常写出，但退出码
            // 与 `length` 一样为 1，并通过 raw_stop_reason 给出精确预算种类。
            let budget_kind = match assistant.stop_reason {
                StopReason::TurnCapped => Some("turn"),
                StopReason::TokenCapped => Some("token"),
                StopReason::CostCapped => Some("cost"),
                StopReason::TimeCapped => Some("wall-clock"),
                _ => None,
            };
            if let Some(kind) = budget_kind {
                errors.write_line(&format!(
                    "Request stopped: the {kind} budget was exhausted."
                ));
                return 1;
            }
            0
        }
    }
}

/// 自定义 `role: "assistant"` JSON 消息的文本分支。
///
/// TS 把最后一条消息断言成 `AssistantMessage` 后直接读字段，因此对象可能缺字段：
/// - `stopReason` 为 `"error"`/`"aborted"`：`errorMessage` truthy 时用它，否则
///   `Request <stopReason>`（二者都可能缺失）。
/// - 其它情况遍历 `content`：数组里 `type === "text"` 的元素写出其 `text`（模板字符串 =
///   JS `String(v)`，`undefined` 写出 `"undefined"`）；字符串按码点迭代但每个字符没有 `type`；
///   其它值（含缺失/`null`/对象）在 JS 里不可迭代 -> TypeError
///   `assistantMsg.content is not iterable`（数组里的 `null` 元素读 `.type` 时抛
///   `Cannot read properties of null (reading 'type')`）。异常走 catch：stderr + 1。
fn custom_assistant_exit_code(
    // 不变量：进入本函数前已用 `role() == "assistant"` 判定，但对象是用户/扩展注入的
    // 自由 JSON，字段可任意缺失或类型错误；本函数的每个分支都在复刻 TS 把该对象断言成
    // `AssistantMessage` 后读字段的 JS 运行时行为（包括哪些访问会 throw）。
    object: &JsonObject,
    io: &Arc<dyn RawStdout>,
    errors: &Arc<dyn PrintErrorSink>,
) -> Result<i32, String> {
    let stop_reason = object.get("stopReason").and_then(serde_json::Value::as_str);
    if matches!(stop_reason, Some("error") | Some("aborted")) {
        let error_message = object.get("errorMessage");
        let message = if kiana_json::js_string::is_truthy(error_message) {
            kiana_json::js_string::js_string(error_message)
        } else {
            format!("Request {}", stop_reason.unwrap_or_default())
        };
        errors.write_line(&message);
        return Ok(1);
    }

    match object.get("content") {
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                match item {
                    serde_json::Value::Null => {
                        return Err("Cannot read properties of null (reading 'type')".to_string());
                    }
                    serde_json::Value::Object(content)
                        if content.get("type").and_then(serde_json::Value::as_str)
                            == Some("text") =>
                    {
                        let text = kiana_json::js_string::js_string(content.get("text"));
                        io.write_raw(&format!("{text}\n"));
                    }
                    // 数字/布尔/字符串/数组的属性访问不抛错（只有 `null`/`undefined` 会）。
                    _ => {}
                }
            }
        }
        // 字符串可迭代（逐码点），但每个字符没有 `type` 属性，因此不写任何内容。
        Some(serde_json::Value::String(_)) => {}
        _ => return Err("assistantMsg.content is not iterable".to_string()),
    }

    if stop_reason == Some("length") {
        errors.write_line("Response was truncated: the model reached its output token limit.");
        return Ok(1);
    }
    Ok(0)
}

/// `setRebindSession` 回调：忽略参数，重新读取 `host.session()` 后重绑。
fn make_rebind_callback(
    host: Arc<dyn PrintRuntimeHost>,
    session_slot: Arc<Mutex<Arc<dyn PrintSession>>>,
    shared: Arc<SharedState>,
    io: Arc<dyn RawStdout>,
    errors: Arc<dyn PrintErrorSink>,
    mode: PrintOutputMode,
) -> Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync> {
    Arc::new(move || {
        let host = Arc::clone(&host);
        let session_slot = Arc::clone(&session_slot);
        let shared = Arc::clone(&shared);
        let io = Arc::clone(&io);
        let errors = Arc::clone(&errors);
        Box::pin(async move {
            rebind_session(&host, &session_slot, &shared, &io, &errors, mode).await;
        })
    })
}

/// `rebindSession()`：重新读取会话、绑定扩展、切换订阅。
///
/// 绑定顺序：先取会话并绑定扩展 -> 退订旧订阅 -> 建立新订阅 -> （JSON 模式）
/// 建立 agent 背压订阅。旧订阅先退订意味着重绑窗口内的事件会丢失（TS 同样如此）。
async fn rebind_session(
    host: &Arc<dyn PrintRuntimeHost>,
    session_slot: &Arc<Mutex<Arc<dyn PrintSession>>>,
    shared: &Arc<SharedState>,
    io: &Arc<dyn RawStdout>,
    errors: &Arc<dyn PrintErrorSink>,
    mode: PrintOutputMode,
) {
    // TS: `session = runtimeHost.session;`
    let session = host.session();
    *lock(session_slot) = Arc::clone(&session);
    // 新订阅从干净状态开始（TS 的新 listener 也没有旧的失败状态）。
    shared.clear_projection_failed();

    let bindings = ExtensionBindings {
        ui_context: None,
        // TS: `mode: mode === "json" ? "json" : "print"`。
        mode: Some(match mode {
            PrintOutputMode::Json => ExtensionMode::Json,
            PrintOutputMode::Text => ExtensionMode::Print,
        }),
        command_context_actions: Some(make_command_context_actions(
            Arc::clone(host),
            Arc::clone(session_slot),
        )),
        abort_handler: None,
        shutdown_handler: None,
        on_error: Some(Arc::new({
            let errors = Arc::clone(errors);
            move |error: ExtensionError| {
                errors.write_line(&format!(
                    "Extension error ({}): {}",
                    error.extension_path, error.error
                ));
            }
        })),
    };
    session.bind_extensions(bindings).await;

    // TS 先退订旧订阅（重绑期间事件丢失），再建立新订阅。
    if let Some(unsubscribe) = shared.take_unsubscribe() {
        unsubscribe();
    }
    if let Some(unsubscribe) = shared.take_unsubscribe_backpressure() {
        unsubscribe();
    }

    let listener: AgentSessionEventListener = match mode {
        PrintOutputMode::Json => {
            let io = Arc::clone(io);
            let shared = Arc::clone(shared);
            Arc::new(move |event: &AgentSessionEvent| {
                // listener 没有错误通道（签名固定为 `Fn(&AgentSessionEvent)`）。可观察行为
                // 尽量对齐 TS：畸形事件不写出、不写 stderr；该 run 的后续事件同样不写出
                // （TS 的 run 已经失败结束），到真实的 `agent_settled` 或 prompt 返回后恢复。
                // 见模块文档第 3 条。
                if shared.projection_failed() {
                    if matches!(event, AgentSessionEvent::AgentSettled { .. }) {
                        shared.clear_projection_failed();
                        if let Ok(line) = serialize_event_line(event) {
                            io.write_raw(&line);
                        }
                    }
                    return;
                }
                match serialize_event_line(event) {
                    Ok(line) => io.write_raw(&line),
                    Err(_error) => shared.set_projection_failed(),
                }
            })
        }
        // text 模式也订阅；listener 是 no-op（与 TS 相同的空转）。
        PrintOutputMode::Text => Arc::new(|_event: &AgentSessionEvent| {}),
    };
    // TS 的 `session` 是共享变量：`bindExtensions` 期间扩展命令可能再次 rebind，
    // 因此订阅必须读取最新会话（而不是本次 rebind 开始时的那一个）。
    let session = current_session(session_slot);
    let unsubscribe = session.subscribe(listener);
    shared.set_unsubscribe(unsubscribe);

    // TS 只在 JSON 模式订阅 `session.agent` 做背压：每个 agent 事件后
    // `await waitForRawStdoutBackpressure()`，让 agent loop 等 stdout 写链清空。
    // 测试替身默认 `agent()` 返回 None，此时不会建立订阅（见 [`PrintSession::agent`]）；
    // 背压回归测试会注入真实 Agent，使该订阅生效。
    if mode == PrintOutputMode::Json
        && let Some(agent) = session.agent()
    {
        let io = Arc::clone(io);
        let listener: kiana_agent_core::AgentListener = Arc::new(move |_event, _signal| {
            let io = Arc::clone(&io);
            Box::pin(async move { io.wait_for_backpressure().await })
        });
        let subscription = agent.subscribe(listener);
        shared.set_unsubscribe_backpressure(Box::new(move || subscription.unsubscribe()));
    }
}

/// 6 个命令上下文动作（与 RPC 相同；选项原样透传）。
///
/// 每个动作都是 `LocalBoxFuture` 工厂：闭包先克隆所需句柄再 `Box::pin`，返回的 future
/// 自包含、不借用外部状态。所有会话/宿主查找都经 [`current_session`] / `host`，因此
/// prompt 期间的 rebind 对扩展命令透明（动作总是操作当前会话）。
///
/// TS 的动作用普通 async 闭包，宿主/会话错误会 reject 回等待中的扩展（扩展可 `catch`），
/// 不写 stderr。Rust 的冻结签名没有错误通道（只有 `cancelled`/`()`），因此按“取消”收场
/// 且**不写 stderr**；扩展无法区分取消与错误属冻结类型的限制。
fn make_command_context_actions(
    host: Arc<dyn PrintRuntimeHost>,
    session_slot: Arc<Mutex<Arc<dyn PrintSession>>>,
) -> ExtensionCommandContextActions {
    let wait_for_idle = {
        let session_slot = Arc::clone(&session_slot);
        Arc::new(move || {
            let session_slot = Arc::clone(&session_slot);
            Box::pin(async move {
                current_session(&session_slot).wait_for_idle().await;
            }) as LocalBoxFuture<'static, ()>
        })
    };
    let new_session = {
        let host = Arc::clone(&host);
        Arc::new(move |options| {
            let host = Arc::clone(&host);
            Box::pin(async move {
                match host.new_session(options).await {
                    Ok(result) => NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => NewSessionResult { cancelled: true },
                }
            }) as LocalBoxFuture<'static, NewSessionResult>
        })
    };
    let fork = {
        let host = Arc::clone(&host);
        Arc::new(move |entry_id, options| {
            let host = Arc::clone(&host);
            Box::pin(async move {
                match host.fork(entry_id, options).await {
                    Ok(result) => NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => NewSessionResult { cancelled: true },
                }
            }) as LocalBoxFuture<'static, NewSessionResult>
        })
    };
    let navigate_tree = {
        let session_slot = Arc::clone(&session_slot);
        Arc::new(
            move |target_id: String, options: Option<ExtNavigateTreeOptions>| {
                let session_slot = Arc::clone(&session_slot);
                Box::pin(async move {
                    let session = current_session(&session_slot);
                    // TS 显式重打包 4 个字段（`undefined` 也显式传入）。
                    let navigate_options = SessionNavigateTreeOptions {
                        summarize: options.as_ref().and_then(|options| options.summarize),
                        custom_instructions: options
                            .as_ref()
                            .and_then(|options| options.custom_instructions.clone()),
                        replace_instructions: options
                            .as_ref()
                            .and_then(|options| options.replace_instructions),
                        label: options.as_ref().and_then(|options| options.label.clone()),
                    };
                    match session.navigate_tree(target_id, navigate_options).await {
                        Ok(result) => NewSessionResult {
                            cancelled: result.cancelled,
                        },
                        Err(_error) => NewSessionResult { cancelled: true },
                    }
                }) as LocalBoxFuture<'static, NewSessionResult>
            },
        )
    };
    let switch_session = {
        let host = Arc::clone(&host);
        Arc::new(move |session_path, options| {
            let host = Arc::clone(&host);
            Box::pin(async move {
                match host.switch_session(session_path, options).await {
                    Ok(result) => NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => NewSessionResult { cancelled: true },
                }
            }) as LocalBoxFuture<'static, NewSessionResult>
        })
    };
    let reload = {
        let session_slot = Arc::clone(&session_slot);
        Arc::new(move || {
            let session_slot = Arc::clone(&session_slot);
            Box::pin(async move {
                let _ = current_session(&session_slot).reload().await;
            }) as LocalBoxFuture<'static, ()>
        })
    };
    ExtensionCommandContextActions {
        wait_for_idle,
        new_session,
        fork,
        navigate_tree,
        switch_session,
        reload,
    }
}

// ============================================================================
// 信号
// ============================================================================

/// 注册 SIGTERM（始终）与 SIGHUP（非 win32）。
///
/// # 实现机制（Unix）
///
/// 对每个信号：
/// 1. `crate::signal::register` 用 `sigaction` 装一个只做单字节 `write` 的 libc 处理器
///    （async-signal-safe），并把注册前的 disposition 保存到 `Registration::previous`；
/// 2. 管道读端包进 [`tokio::io::unix::AsyncFd`]，`spawn_local` 的任务 select 等待
///    “读到信号字节”与“收到取消”两个分支；
/// 3. 清理闭包（finally 阶段执行）还原 `previous` 并向 watch 通道发取消。
///
/// # 失败路径
///
/// `register` 返回 `None` 或 `AsyncFd` 注册失败时跳过该信号：该信号恢复默认处置语义
/// （没有自定义处理器），另一个信号不受影响。
///
/// 用 `sigaction` + self-pipe 而不是 `tokio::signal::unix`：tokio 的 libc 信号处理器
/// **永不注销**（其文档明说），无法复刻 TS `process.off` 的两个语义（review findings 1/2）：
/// 已触发的处理器不可被清理取消；移除监听器后恢复信号默认处置。
///
/// 清理闭包只做两件事：还原注册前的 `sigaction`、唤醒等待任务让它退出；如果信号分支
/// 已经胜出（处理器正在 dispose），清理不会影响它——与 `process.off` 相同。
#[cfg(unix)]
fn register_signal_handlers(
    host: Arc<dyn PrintRuntimeHost>,
    io: Arc<dyn RawStdout>,
    shared: Arc<SharedState>,
    cleanups: &mut Vec<Box<dyn Fn()>>,
) {
    // 顺序固定：SIGTERM 先注册，SIGHUP 后注册（与 TS 的 signals 数组顺序一致）。
    // 任何一次注册失败（crate::signal::register 返回 None / AsyncFd 注册失败）都只跳过
    // 该信号，不影响另一个。TS 的 `process.on` 不会失败，无对拍基准；这里选择跳过该
    // 信号而不是中止运行。
    for signal_kind in [PrintSignal::Term, PrintSignal::Hup] {
        let Some(registration) = crate::signal::register(signal_kind.into()) else {
            continue;
        };
        let crate::signal::Registration {
            signal_number,
            previous,
            read_end,
            write_end,
        } = registration;
        // SAFETY: `read_end` 是打开的管道读端，所有权交给 `AsyncFd`，在其生命周期内
        // 不会被替换或关闭（写端由任务持有，同样不会提前关闭）。
        let reader = match unsafe {
            tokio::io::unix::AsyncFd::register_with_interest(
                read_end,
                tokio::io::Interest::READABLE,
            )
        } {
            Ok(reader) => reader,
            Err(_) => {
                // 注册失败（与 tokio::signal 的失败路径一致）：还原 disposition 并跳过。
                crate::signal::restore(signal_number, previous);
                continue;
            }
        };
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let host = Arc::clone(&host);
        let io = Arc::clone(&io);
        let shared = Arc::clone(&shared);
        tokio::task::spawn_local(async move {
            // 写端随任务存活：清理只还原 disposition，不关闭写端，避免“关闭 -> EOF”被
            // 误判为一次信号。
            let _write_keepalive = write_end;
            // biased：信号分支优先。若信号字节与取消同时就绪，说明处理器已经触发过，
            // 按 process.off 语义它必须继续执行到 process.exit。
            tokio::select! {
                biased;
                () = crate::signal::read_wake_byte(&reader) => {
                    let exit_code = run_signal_shutdown(host, io, shared, signal_kind).await;
                    std::process::exit(exit_code);
                }
                _ = cancel_rx.changed() => {}
            }
        });
        cleanups.push(Box::new(move || {
            // process.off 等价：恢复注册前的处置（通常是 SIG_DFL）。已经触发的处理器
            // 不受影响——它的任务已经离开 select，会继续 dispose -> exit。
            crate::signal::restore(signal_number, previous);
            let _ = cancel_tx.send(true);
        }));
    }
}

/// 非 Unix：没有 `sigaction`（Windows 也没有 tokio SIGTERM 源），因此不注册任何处理器
/// （已知差异，见模块文档）。
#[cfg(not(unix))]
fn register_signal_handlers(
    _host: Arc<dyn PrintRuntimeHost>,
    _io: Arc<dyn RawStdout>,
    _shared: Arc<SharedState>,
    _cleanups: &mut Vec<Box<dyn Fn()>>,
) {
}

/// 生产信号处理器的 body：kill 子进程 -> dispose（幂等）-> 返回退出码。
///
/// 退出码只取决于信号种类（SIGTERM 143、SIGHUP 129），与 dispose 结果无关——TS 的
/// handler 在 `.finally` 里无条件 `process.exit(129|143)`。
///
/// 两条信号路径都不 flush（TS 的 handler 直接 `process.exit`；spec §2.3.2、§4.7）。
async fn run_signal_shutdown(
    host: Arc<dyn PrintRuntimeHost>,
    io: Arc<dyn RawStdout>,
    shared: Arc<SharedState>,
    signal: PrintSignal,
) -> i32 {
    shared.signal_triggered.store(true, Ordering::SeqCst);
    kiana_core::shell::kill_tracked_detached_children();
    dispose_runtime(&host, &shared).await;
    if !print_signal_skips_flush(signal) {
        io.flush().await;
    }
    print_signal_exit_code(signal)
}

/// `disposeRuntime()`：幂等；先退订（避免 dispose 期间继续写协议），再 dispose。
///
/// 返回 `true` 表示本次调用实际执行了 dispose，`false` 表示已有别的路径（信号处理器或
/// finally）先做过了。信号处理器与主流程 finally 都会调它，`SharedState::disposed` 的
/// `swap` 保证宿主 `dispose()` 恰好执行一次。
async fn dispose_runtime(host: &Arc<dyn PrintRuntimeHost>, shared: &Arc<SharedState>) -> bool {
    if shared.disposed.swap(true, Ordering::SeqCst) {
        return false;
    }
    if let Some(unsubscribe) = shared.take_unsubscribe() {
        unsubscribe();
    }
    if let Some(unsubscribe) = shared.take_unsubscribe_backpressure() {
        unsubscribe();
    }
    host.dispose().await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`PrintSession`] 的默认方法编译期可用（生产适配器满足 trait 约束）。
    fn assert_trait_bounds<T: PrintSession + ?Sized>() {}
    fn assert_host_bounds<T: PrintRuntimeHost + ?Sized>() {}

    #[allow(dead_code)]
    fn bounds_are_send_sync() {
        assert_trait_bounds::<AgentSessionPrintSession>();
        assert_host_bounds::<LocalPrintRuntimeHost>();
    }

    #[test]
    fn signal_exit_codes_match_ts() {
        assert_eq!(print_signal_exit_code(PrintSignal::Term), 143);
        assert_eq!(print_signal_exit_code(PrintSignal::Hup), 129);
        assert!(print_signal_skips_flush(PrintSignal::Term));
        // TS print-mode.ts 的 SIGHUP 分支同样不 flush（handler 直接 process.exit(129)）。
        assert!(print_signal_skips_flush(PrintSignal::Hup));
    }
}
