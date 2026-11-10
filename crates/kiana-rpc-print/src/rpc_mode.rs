//! modes/rpc/rpc-mode.ts：RPC 服务端（JSON stdin/stdout 协议）与扩展 UI 子协议。
//!
//! TS 源：`packages/coding-agent/src/modes/rpc/rpc-mode.ts`（819 行）；规范：
//! `spec/ca-rpc-print.md` §2.6、§4.4、§7.1、§8.3 步骤 5-7。
//!
//! # 形态
//!
//! TS 的 `runRpcMode(runtimeHost)` 是一个闭包工厂：`session`、`pendingExtensionRequests`、
//! `shutdownRequested` 等都是 `runRpcMode` 的局部变量。Rust 侧把同一份状态放进 [`RpcServer`]
//! （`Arc<Self>`），并把与 `AgentSession` / `AgentSessionRuntime` 的耦合抽成两个宿主 seam：
//!
//! - [`AgentSessionFacade`]：33 个命令用到的会话方法（默认实现全部委托给
//!   [`AgentSessionFacade::agent_session`]）；
//! - [`RpcSessionHost`]：会话替换（new/switch/fork）、rebind 回调注册与 dispose。
//!
//! 生产适配器是 `RuntimeHostAdapter`：`AgentSessionRuntime` 是 `!Send`（资源加载器 trait
//! 不带 `Send + Sync`），因此它只在线程本地注册表里持有 `Rc<AgentSessionRuntime>`，
//! 适配器本身只是 `u64` 句柄（`regression_2860` 的同一模式）。所有 future 都是 `!Send`
//! （扩展派发使用 `LocalBoxFuture`），调用方必须在 `tokio::task::LocalSet` 上运行。
//!
//! # 与 TS 的逐条对应
//!
//! - `handle_input_line` 不做任何 schema 校验：解析失败写 `command: "parse"` 错误；
//!   `extension_ui_response` 只按 `type`/`id` 查 pending map；其余按原始 [`Value`] 的
//!   `type` 分发（未知/缺失 -> `Unknown command: <js_string>`；顶层 `null` -> 不写任何记录）。
//! - `success` / `error` 记录用 [`crate::rpc_types::RpcResponse`]（字段序与省略规则逐字一致）。
//! - 所有 stdout 记录都经 [`crate::json_event::serialize_wire_line`] /
//!   [`crate::json_event::serialize_event_line`]，绝不用 `serde_json::to_string`。
//! - 关停由 stdin EOF、SIGTERM/SIGHUP、扩展 `shutdownHandler`（延迟到 `agent_settled` 或
//!   当前命令处理完后）触发；`shutdown` 幂等，重入调用返回调用方自己的退出码。
//! - stdin EOF 立即 `void shutdown()`（TS 的 `onInputEnd`）：最后一段完整行仍会同步进入
//!   `handle_input_line`，但**不等待**命令完成——`dispose()` 中止在途命令。
//! - 信号用 `crate::signal` 的 `sigaction` + self-pipe 实现；`shutdown` 开头执行
//!   `process.off` 等价物（还原默认处置），dispose 期间的第二个信号因此按默认动作终止进程，
//!   `run_rpc_mode` 返回后也不再遗留进程级处理器。
//! - 会话事件订阅之外还订阅 `session.agent`（[`AgentSessionFacade::agent`]）：每个 agent
//!   事件后 `await` stdout 写链清空（背压）。
//! - `new_session` / `switch_session` / `fork` / `clone` 成功后调用两次 rebind
//!   （runtime 内部一次 + 本文件一次），这是 TS 现状，**不要**优化。
//!
//! # 执行流程
//!
//! 1. [`run_rpc_mode`] 让 [`RawStdout`] 接管 stdout（普通输出改道 stderr，协议记录独占
//!    真实 stdout），把 `AgentSessionRuntime` 装进线程本地槽位，再构造 [`RpcServer`]；
//! 2. 输入循环先 `rebind_session()`（绑定扩展 UI/错误/关停回调、订阅会话事件与 agent
//!    背压），再注册 SIGTERM/SIGHUP 处理器（只注册一次）；
//! 3. 每读入一行就 `spawn_line`：先手动 poll 一次驱动同步前缀（解析、字段提取等），
//!    挂起则交给 `spawn_local` 并发处理——命令不排队，与 TS 的
//!    `void handleInputLine(line)` 一致；
//! 4. stdin EOF / 进程信号 / 扩展 `shutdownHandler` 任一触发 `shutdown`（幂等）：还原
//!    信号处置、退订、`dispose()` runtime、停止读 stdin、按需 flush；退出码默认取第一个
//!    触发者传入的码（EOF/扩展为 0，`SIGTERM` 143、`SIGHUP` 129），关停期间的重入
//!    `shutdown` 会以自己的码抢占（见 [`RpcServer`] 的三态说明）。
//!
//! # 调用方
//!
//! 上游是 `kiana-coding-agent` 的 RPC 分支（`app.rs` 的 `run_main` 分发步骤）与
//! `bin/kiana-rpc` 入口（经 `main(["--mode","rpc"])` 走同一分支）；SDK 面把
//! [`run_rpc_mode`] 再导出为 `runRpcMode`。测试直接构造 [`RpcServer`]（注入替身宿主与
//! 记录型 `RawStdout`）后用 `run_with_input` 注入 reader，不触碰进程级 stdout。
//!
//! # 注意事项
//!
//! - 本文件的 async trait 都是 `#[async_trait(?Send)]`，任务用 `spawn_local` 派发，
//!   因此 [`RpcServer`] 的入口必须在 tokio 的 `LocalSet`（或等价的本地任务集）上执行，
//!   否则 `spawn_local` 会 panic；`AgentSessionRuntime` 是 `!Send`，生产适配器因此走
//!   线程本地槽位而不是共享指针。
//! - 非 Unix 平台没有 `sigaction`/对应的 tokio 信号源，信号表为空：EOF 与扩展
//!   `shutdownHandler` 仍然工作（`register_signal_handlers` 的 cfg 分支）。

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use kiana_agent_core::QueueMode;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{ImageContent, ImageKind, Model, ModelThinkingLevel};
use kiana_core::event_bus::Unsubscribe;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::events::{
    ForkPosition, InputSource, StreamingBehavior, UserBashEvent, UserBashEventResult,
    UserBashEventType,
};
use kiana_ext_api::types::{
    ExtensionCommandContextActions, ExtensionError, ExtensionMode, ForkOptions, NewSessionOptions,
    ResolvedCommand, SwitchSessionOptions,
};
use kiana_ext_api::ui::{
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionWidgetOptions, NotifyKind, ThemeInfo,
    ThemeOrName, ThemeResult, WidgetContent, WidgetPlacement,
};
use kiana_session::agent_session::{
    AgentSession, AgentSessionError, AgentSessionEvent, AgentSessionEventListener,
    AgentSessionUnsubscribe, ExecuteBashOptions, ExtensionBindings, ModelCycleResult,
    ModelMutationOptions, NavigateTreeOptions, PromptDisposition, PromptOptions,
    QueuedInputDisposition, SessionStats, TreeNavigationResult,
};
use kiana_session::agent_session_runtime::{
    AgentSessionRuntime, ForkSessionOptions, ForkSessionResult, NewSessionOptionsInput,
    ReplacedSessionCallback, SessionReplacementResult,
    SwitchSessionOptions as RuntimeSwitchOptions,
};
use kiana_session::sdk::{PromptTemplate, Skill};
use kiana_session_core::session_manager::{SessionEntry, SessionHeader, SessionTreeNode};

use kiana_extensions::{ExtensionErrorListener, ShutdownHandler};

use crate::RawStdout;
use crate::json_event::{serialize_event_line, serialize_wire_line};
use crate::jsonl::JsonlLineReader;
// TS `rpc-mode.ts` 的再导出面：
// `export type { RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState } from "./rpc-types.ts";`
pub use crate::rpc_types::{
    RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState,
};
use crate::rpc_types::{RpcNotifyType, RpcWidgetPlacement};

// ============================================================================
// 会话门面（host seam 1）
// ============================================================================

/// RPC 服务端使用的会话门面。
///
/// TS 的 `mapCommandToSession` 直接读 `AgentSession` 的 getter/方法；Rust 侧的
/// [`AgentSession`] 已经提供了同样的一组方法，因此默认实现全部委托给
/// [`agent_session`](Self::agent_session)。测试用实现只需覆写实际用到的方法
/// （未覆写的默认体会调用 `agent_session()`）。
// `async-trait` 给每个方法生成带 `#[must_use]` 的 future，clippy 的 `double_must_use`
// 看不到宏内属性（与 `kiana-builtin-extensions`/`kiana-mcp` 的同类 allow 一致）。
#[allow(clippy::double_must_use)]
#[async_trait(?Send)]
pub trait AgentSessionFacade: Send + Sync {
    /// 底层会话（默认实现的委托目标）。
    fn agent_session(&self) -> AgentSession;

    /// `session.model`。
    fn model(&self) -> Option<Model> {
        self.agent_session().model()
    }

    /// `session.thinkingLevel`。
    fn thinking_level(&self) -> ModelThinkingLevel {
        self.agent_session().thinking_level()
    }

    /// `session.isStreaming`。
    fn is_streaming(&self) -> bool {
        self.agent_session().is_streaming()
    }

    /// `session.isCompacting`。
    fn is_compacting(&self) -> bool {
        self.agent_session().is_compacting()
    }

    /// `session.steeringMode`。
    fn steering_mode(&self) -> QueueMode {
        self.agent_session().steering_mode()
    }

    /// `session.followUpMode`。
    fn follow_up_mode(&self) -> QueueMode {
        self.agent_session().follow_up_mode()
    }

    /// `session.sessionFile`。
    fn session_file(&self) -> Option<String> {
        self.agent_session().session_file()
    }

    /// `session.sessionId`。
    fn session_id(&self) -> String {
        self.agent_session().session_id()
    }

    /// `session.sessionName`。
    fn session_name(&self) -> Option<String> {
        self.agent_session().session_name()
    }

    /// `session.autoCompactionEnabled`。
    fn auto_compaction_enabled(&self) -> bool {
        self.agent_session().auto_compaction_enabled()
    }

    /// `session.messages`。
    fn messages(&self) -> Vec<AgentMessage> {
        self.agent_session().messages()
    }

    /// `session.messages.length`（O(1)，不克隆 transcript）。
    fn message_count(&self) -> usize {
        self.agent_session().message_count()
    }

    /// `session.pendingMessageCount`。
    fn pending_message_count(&self) -> usize {
        self.agent_session().pending_message_count()
    }

    /// `session.modelRuntime.getAvailableSnapshot()`。
    fn available_models(&self) -> Vec<Model> {
        self.agent_session()
            .model_runtime()
            .get_available_snapshot()
    }

    /// `session.setModel(model)`。
    async fn set_model(
        &self,
        model: Model,
        options: ModelMutationOptions,
    ) -> Result<(), AgentSessionError> {
        self.agent_session().set_model(model, options).await
    }

    /// `session.cycleModel()`（默认方向 `"forward"`）。
    async fn cycle_model(&self) -> Result<Option<ModelCycleResult>, AgentSessionError> {
        self.agent_session()
            .cycle_model("forward", ModelMutationOptions::default())
            .await
    }

    /// `session.setThinkingLevel(level)`。
    fn set_thinking_level(&self, level: ModelThinkingLevel, options: ModelMutationOptions) {
        self.agent_session().set_thinking_level(level, options);
    }

    /// `session.cycleThinkingLevel()`。
    fn cycle_thinking_level(&self, options: ModelMutationOptions) -> Option<ModelThinkingLevel> {
        self.agent_session().cycle_thinking_level(options)
    }

    /// `session.getAvailableThinkingLevels()`。
    fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        self.agent_session().get_available_thinking_levels()
    }

    /// `session.setSteeringMode(mode)`。
    fn set_steering_mode(&self, mode: QueueMode) {
        self.agent_session().set_steering_mode(mode);
    }

    /// `session.setFollowUpMode(mode)`。
    fn set_follow_up_mode(&self, mode: QueueMode) {
        self.agent_session().set_follow_up_mode(mode);
    }

    /// `session.compact(customInstructions?)`。
    async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<kiana_compaction::CompactionResult, AgentSessionError> {
        self.agent_session().compact(custom_instructions).await
    }

    /// `session.setAutoCompactionEnabled(enabled)`。
    fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.agent_session().set_auto_compaction_enabled(enabled);
    }

    /// `session.setAutoRetryEnabled(enabled)`。
    fn set_auto_retry_enabled(&self, enabled: bool) {
        self.agent_session().set_auto_retry_enabled(enabled);
    }

    /// `session.abortRetry()`。
    fn abort_retry(&self) {
        self.agent_session().abort_retry();
    }

    /// `session.prompt(text, options?)`。
    async fn prompt(&self, text: String, options: PromptOptions) -> Result<(), AgentSessionError> {
        self.agent_session().prompt(text, options).await
    }

    /// `session.steer(text, images?, { source: "rpc" })`。
    async fn steer(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: Option<InputSource>,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        self.agent_session().steer(text, images, source).await
    }

    /// `session.followUp(text, images?, { source: "rpc" })`。
    async fn follow_up(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        source: Option<InputSource>,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        self.agent_session().follow_up(text, images, source).await
    }

    /// `session.abort()`。
    async fn abort(&self) {
        self.agent_session().abort().await;
    }

    /// `session.waitForIdle()`。
    async fn wait_for_idle(&self) {
        self.agent_session().wait_for_idle().await;
    }

    /// `session.clearQueue()`。
    fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        self.agent_session().clear_queue()
    }

    /// `session.executeBash(command, undefined, options)`。
    async fn execute_bash(
        &self,
        command: String,
        options: ExecuteBashOptions,
    ) -> Result<kiana_ext_api::tool_result::BashResult, AgentSessionError> {
        self.agent_session()
            .execute_bash(command, None, Some(options))
            .await
    }

    /// `session.recordBashResult(command, result, { excludeFromContext })`。
    fn record_bash_result(
        &self,
        command: String,
        result: &kiana_ext_api::tool_result::BashResult,
        exclude_from_context: Option<bool>,
    ) {
        self.agent_session()
            .record_bash_result(command, result, exclude_from_context);
    }

    /// `session.abortBash()`。
    fn abort_bash(&self) {
        self.agent_session().abort_bash();
    }

    /// `session.setSessionName(name)`。
    fn set_session_name(&self, name: &str) {
        self.agent_session().set_session_name(name);
    }

    /// `session.getSessionStats()`。
    fn get_session_stats(&self) -> SessionStats {
        self.agent_session().get_session_stats()
    }

    /// `session.exportToHtml(outputPath)`（RPC 不传第二个参数）。
    async fn export_html(&self, output_path: Option<String>) -> Result<String, AgentSessionError> {
        self.agent_session().export_to_html(output_path, None)
    }

    /// `session.getUserMessagesForForking()`。
    fn get_user_messages_for_forking(&self) -> Vec<(String, String)> {
        self.agent_session().get_user_messages_for_forking()
    }

    /// `session.getLastAssistantText()`。
    fn get_last_assistant_text(&self) -> Option<String> {
        self.agent_session().get_last_assistant_text()
    }

    /// `session.bindExtensions(bindings)`。
    async fn bind_extensions(&self, bindings: ExtensionBindings) {
        self.agent_session().bind_extensions(bindings).await;
    }

    /// `session.subscribe(listener)`。
    fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        self.agent_session().subscribe(listener)
    }

    /// `session.agent`（TS 的公开字段）。
    ///
    /// RPC 模式用它建立 stdout 背压订阅（每个 agent 事件后等写链清空）。只有生产适配器
    /// 能提供；测试替身默认 `None`（不建立该订阅）。
    fn agent(&self) -> Option<kiana_agent_core::Agent> {
        None
    }

    /// `session.reload()`。
    async fn reload(&self) -> Result<(), AgentSessionError> {
        self.agent_session().reload(None).await
    }

    /// `session.navigateTree(targetId, options)`。
    async fn navigate_tree(
        &self,
        target_id: String,
        options: NavigateTreeOptions,
    ) -> Result<TreeNavigationResult, AgentSessionError> {
        self.agent_session().navigate_tree(target_id, options).await
    }

    /// `session.extensionRunner.getRegisteredCommands()`。
    fn registered_commands(&self) -> Vec<ResolvedCommand> {
        self.agent_session()
            .extension_runner()
            .get_registered_commands()
    }

    /// `session.promptTemplates`。
    fn prompt_templates(&self) -> Vec<PromptTemplate> {
        self.agent_session().prompt_templates()
    }

    /// `session.resourceLoader.getSkills().skills`。
    fn skills(&self) -> Vec<Skill> {
        self.agent_session().resource_loader().get_skills().skills
    }

    /// `session.sessionManager.getHeader()`。
    fn header(&self) -> Option<SessionHeader> {
        let manager = self.agent_session().session_manager();
        manager.get_header().cloned()
    }

    /// `session.sessionManager.getEntries()`。
    fn entries(&self) -> Vec<SessionEntry> {
        let manager = self.agent_session().session_manager();
        manager.get_entries()
    }

    /// `session.sessionManager.getTree()`。
    fn tree(&self) -> Vec<SessionTreeNode> {
        let manager = self.agent_session().session_manager();
        manager.get_tree()
    }

    /// `session.sessionManager.getLeafId()`。
    fn leaf_id(&self) -> Option<String> {
        let manager = self.agent_session().session_manager();
        manager.get_leaf_id().map(str::to_owned)
    }

    /// `session.sessionManager.getCwd()`。
    fn cwd(&self) -> String {
        let manager = self.agent_session().session_manager();
        manager.get_cwd().to_owned()
    }

    /// `session.extensionRunner.emitUserBash(event)`。
    async fn emit_user_bash(
        &self,
        event: UserBashEvent,
    ) -> Result<Option<UserBashEventResult>, String> {
        self.agent_session()
            .extension_runner()
            .emit_user_bash(event)
            .await
            .map_err(|error| error.to_string())
    }
}

// ============================================================================
// 运行时宿主（host seam 2）
// ============================================================================

/// RPC 服务端使用的运行时宿主。
// 同上：`async-trait` 的生成代码触发 `double_must_use`。
#[allow(clippy::double_must_use)]
#[async_trait(?Send)]
pub trait RpcSessionHost: Send + Sync {
    /// 当前会话门面。
    fn session(&self) -> Arc<dyn AgentSessionFacade>;

    /// `runtimeHost.setRebindSession(fn)`。
    fn set_rebind_session(
        &self,
        callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    );

    /// `runtimeHost.newSession(options?)`。
    async fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String>;

    /// `runtimeHost.switchSession(sessionPath, options?)`。
    async fn switch_session(
        &self,
        session_path: String,
        options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String>;

    /// `runtimeHost.fork(entryId, options?)`。
    async fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String>;

    /// `runtimeHost.dispose()`。
    async fn dispose(&self);
}

// ============================================================================
// 退出信号（测试 seam）
// ============================================================================

/// `shutdown` 收到的进程信号。
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSignal {
    /// `SIGTERM`。
    Term,
    /// `SIGHUP`（非 win32）。
    Hup,
}

impl RunSignal {
    /// 退出码：`SIGTERM` -> 143，`SIGHUP` -> 129。
    pub fn exit_code(self) -> i32 {
        match self {
            RunSignal::Term => 143,
            RunSignal::Hup => 129,
        }
    }

    /// TS 只在 `SIGTERM` 上跳过 `flushRawStdout()`。
    pub fn skips_flush(self) -> bool {
        matches!(self, RunSignal::Term)
    }
}

// ============================================================================
// RPC 服务端
// ============================================================================

/// `force_exit_code` 的“尚未发生重入关停”哨兵值（合法的退出码不会用到这个值）。
const NO_FORCE_EXIT: i32 = i32::MIN;

/// RPC 服务端（TS 的 `runRpcMode` 闭包状态）。
///
/// 关停的并发模型（三态 + 重入抢占）：
/// - 未开始：`shutting_down == false`，所有触发源（EOF、信号、扩展请求）都会进入
///   `perform_shutdown` 的完整序列；
/// - 进行中：`shutting_down == true` 但 `shutdown_exit` 还是 `None`；重入的 `shutdown`
///   不等待，而是把自己的退出码记进 `force_exit_code` 并唤醒所有等待者（TS 重入分支的
///   `process.exit` 等价物）；
/// - 已完成：`shutdown_exit` 变为 `Some(退出码)`，`await_shutdown_code` 的所有
///   等待者返回并退出输入循环。
///
/// 锁的粒度：各 `Mutex` 只保护单个槽位（当前会话、pending map、任务句柄列表……）且
/// `lock()` 立即释放（`lock(&x)` 是临时守卫），不存在跨 await 持锁；跨字段的一致性由
/// 原子量（`shutting_down` 的 swap 保证只有一个完整序列在跑）承担。
pub struct RpcServer {
    host: Arc<dyn RpcSessionHost>,
    io: Arc<dyn RawStdout>,
    /// 当前会话（rebind 时更新）。
    session: Mutex<Option<Arc<dyn AgentSessionFacade>>>,
    /// 会话事件订阅。
    unsubscribe: Mutex<Option<AgentSessionUnsubscribe>>,
    /// agent 层背压订阅（TS 的 `session.agent.subscribe(...)`）。
    ///
    /// listener 在每个 agent 事件后 `await` stdout 写链清空（[`RawStdout`] 的背压）。
    unsubscribe_backpressure: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// 待响应的扩展 UI 请求。
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    /// 扩展 `shutdownHandler` 置位。
    shutdown_requested: AtomicBool,
    /// 关停是否已经开始（`shuttingDown`）。
    shutting_down: AtomicBool,
    /// 第一个 `shutdown` 的退出码。
    exit_code: AtomicI32,
    /// 第一个 `shutdown` 完成后的退出码（`None` = 仍在进行）。等待者用 `watch` 订阅，
    /// 避免 `Notify::notify_waiters` 在“检查状态 -> 注册等待”之间丢通知。
    shutdown_exit: tokio::sync::watch::Sender<Option<i32>>,
    /// 重入 `shutdown`（扩展在关停期间再次请求 / 测试 seam 的第二次调用）的退出码。
    /// TS 的重入分支直接 `process.exit(exitCode)`，因此它必须先于第一个 `shutdown`
    /// 的完成接管退出。
    force_exit_code: AtomicI32,
    /// 停止读取 stdin（TS 的 `detachInput` + `stdin.pause()`，**不**关闭 stdin）。
    input_stop: CancellationToken,
    /// 信号处理器只注册一次。
    signals_registered: AtomicBool,
    /// 让信号监听任务退出（清理时与 `sigaction` 还原一起触发）。
    signals_stop: CancellationToken,
    /// `process.off` 的等价物：还原注册前的 `sigaction` 并取消监听任务。
    ///
    /// TS 在 `shutdown()` **开头**执行（dispose 期间到达的第二个信号因此走默认处置）。
    signal_cleanups: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    /// 信号处理任务句柄（仅用于持有；取消经 [`Self::signal_cleanups`]）。
    signal_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// 正在处理中的输入行任务（关停完成后取消，等价于 `process.exit` 丢弃在途命令）。
    line_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// 发生过一次 `toJsonEvent` 投影失败：抑制当前 run 的后续事件，直到 `agent_settled`。
    ///
    /// TS 的监听器抛错会让 agent run 以失败收场并合成失败生命周期事件；Rust 的监听器
    /// 签名没有错误通道（见 [`Self::rebind_session`] 的 listener 注释）。
    projection_failed: AtomicBool,
    /// `set_steering_mode` / `set_follow_up_mode` / `set_auto_compaction` 收到的原始值。
    ///
    /// TS 的 setter 不校验，把原始 JSON 写进 agent/settings，`get_state` 原样回显；
    /// Rust 的 `QueueMode`/`bool` 只能表示规范化后的值，因此另行保存原始值供 `get_state`
    /// 使用（见 [`RawSessionSettings`]）。
    raw_settings: RawSessionSettings,
}

/// `get_state` 回显用的原始 settings 值。
///
/// 每个槽位都是 `Option<Option<Value>>`：
/// - `None`：本次运行还没调用过该 setter，`get_state` 读会话的规范化 getter；
/// - `Some(None)`：setter 收到 JS `undefined`（键缺失）——steering/followUp 在
///   `JSON.stringify` 里省略该键；
/// - `Some(Some(value))`：原样回显该 JSON 值。
#[derive(Default)]
struct RawSessionSettings {
    steering_mode: Mutex<Option<Option<Value>>>,
    follow_up_mode: Mutex<Option<Option<Value>>>,
    auto_compaction_enabled: Mutex<Option<Option<Value>>>,
}

impl RpcServer {
    /// 创建服务端。
    ///
    /// 注意：**不**调用 [`RawStdout::take_over`]（测试不得触碰进程级 stdout 状态）；
    /// 生产入口 [`run_rpc_mode`] / [`run_rpc_mode_with_host`] 在构造前接管 stdout。
    pub fn new(host: Arc<dyn RpcSessionHost>, io: Arc<dyn RawStdout>) -> Arc<Self> {
        let server = Arc::new(Self {
            host,
            io,
            session: Mutex::new(None),
            unsubscribe: Mutex::new(None),
            unsubscribe_backpressure: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
            shutdown_requested: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            shutdown_exit: tokio::sync::watch::channel(None).0,
            force_exit_code: AtomicI32::new(NO_FORCE_EXIT),
            input_stop: CancellationToken::new(),
            signals_registered: AtomicBool::new(false),
            signals_stop: CancellationToken::new(),
            signal_cleanups: Mutex::new(Vec::new()),
            signal_tasks: Mutex::new(Vec::new()),
            line_tasks: Mutex::new(Vec::new()),
            projection_failed: AtomicBool::new(false),
            raw_settings: RawSessionSettings::default(),
        });
        // TS 在 `runRpcMode` 开头注册（早于第一次 `rebindSession()`）；放在 `new` 里
        // 让直接驱动 `rebind_session()` 的测试也拿到与生产一致的装配。
        let callback_server = Arc::clone(&server);
        server.host.set_rebind_session(Arc::new(move || {
            let server = Arc::clone(&callback_server);
            Box::pin(async move { server.rebind_session().await })
        }));
        server
    }

    /// 当前会话门面。
    ///
    /// 缓存最近一次 rebind 得到的会话；缓存为空时（构造后、第一次 rebind 前）回落到
    /// `host.session()` 并写入缓存。注意 rebind 总是覆盖缓存（而不是走这个惰性路径），
    /// 会话替换后旧门面立即失效。
    fn session(&self) -> Arc<dyn AgentSessionFacade> {
        if let Some(session) = lock(&self.session).clone() {
            return session;
        }
        let session = self.host.session();
        *lock(&self.session) = Some(Arc::clone(&session));
        session
    }

    /// 写出一条 wire 记录（`JSON.stringify` + `\n`）。
    ///
    /// 序列化失败只写 stderr（`write_record` 的行为），不会 panic——单条记录坏掉不应
    /// 终止整个协议循环。调用方需要保证顺序时自行在之后 `wait_for_backpressure()`。
    fn output<T: Serialize>(&self, record: &T) {
        write_record(&self.io, record);
    }

    /// `rebindSession()`：绑定扩展、重新订阅会话事件。
    #[doc(hidden)]
    pub async fn rebind_session(self: &Arc<Self>) {
        self.projection_failed.store(false, Ordering::SeqCst);
        let session = self.host.session();
        *lock(&self.session) = Some(Arc::clone(&session));

        let ui_context: Arc<dyn ExtensionUiContext> = Arc::new(RpcUiContext::new(
            Arc::clone(&self.io),
            Arc::clone(&self.pending),
        ));
        let shutdown_server = Arc::clone(self);
        let error_server = Arc::clone(self);
        let shutdown_handler: ShutdownHandler = Arc::new(move || {
            shutdown_server
                .shutdown_requested
                .store(true, Ordering::SeqCst);
        });
        let on_error: ExtensionErrorListener = Arc::new(move |error: ExtensionError| {
            error_server.output(&extension_error_value(&error));
        });
        let actions = self.command_context_actions();
        session
            .bind_extensions(ExtensionBindings {
                ui_context: Some(ui_context),
                mode: Some(ExtensionMode::Rpc),
                command_context_actions: Some(actions),
                abort_handler: None,
                shutdown_handler: Some(shutdown_handler),
                on_error: Some(on_error),
            })
            .await;

        if let Some(unsubscribe) = lock(&self.unsubscribe).take() {
            unsubscribe();
        }
        if let Some(unsubscribe) = lock(&self.unsubscribe_backpressure).take() {
            unsubscribe();
        }
        let listener_server = Arc::clone(self);
        let listener: AgentSessionEventListener = Arc::new(move |event: &AgentSessionEvent| {
            // 投影错误（`toJsonEvent` 的两个 throw）：TS 的异常会冒出 `_emit` 并让 agent
            // run 失败，随后 `handleRunFailure` 合成一条 assistant 失败消息的
            // message_start/message_end/turn_end/agent_end。Rust 的监听器签名
            // （`Fn(&AgentSessionEvent)`）没有错误通道，无法让 agent loop 以“监听器失败”
            // 收场，也无法合成上述事件（spec §2.2.6 的近似）。
            //
            // 可观察行为尽量对齐 TS：畸形事件不写出、不写 stderr；该 run 的后续事件同样
            // 不写出（TS 的 run 已经结束），到真实的 `agent_settled` 恢复——TS 的失败
            // run 也会写出它，且 RPC 的 shutdown 检查依赖它。
            if listener_server.projection_failed.load(Ordering::SeqCst) {
                if matches!(event, AgentSessionEvent::AgentSettled { .. }) {
                    listener_server
                        .projection_failed
                        .store(false, Ordering::SeqCst);
                    if let Ok(line) = serialize_event_line(event) {
                        listener_server.io.write_raw(&line);
                    }
                    let server = Arc::clone(&listener_server);
                    tokio::task::spawn_local(
                        async move { server.check_shutdown_requested().await },
                    );
                }
                return;
            }
            match serialize_event_line(event) {
                Ok(line) => {
                    listener_server.io.write_raw(&line);
                    if matches!(event, AgentSessionEvent::AgentSettled { .. }) {
                        let server = Arc::clone(&listener_server);
                        tokio::task::spawn_local(
                            async move { server.check_shutdown_requested().await },
                        );
                    }
                }
                Err(_error) => {
                    listener_server
                        .projection_failed
                        .store(true, Ordering::SeqCst);
                }
            }
        });
        // TS 的 `session` 是闭包变量：`bindExtensions` 期间扩展可能触发嵌套 rebind
        // （`session_start` 里调用 newSession），嵌套 rebind 会把变量改写成新会话。
        // 外层恢复执行后必须订阅**最新**会话，而不是本函数开始时捕获的旧会话，
        // 否则事件会来自已被替换的会话（print 模式同样在订阅前重新读取当前会话）。
        let session = self.session();
        let unsubscribe = session.subscribe(listener);
        *lock(&self.unsubscribe) = Some(unsubscribe);

        // TS 无条件订阅 `session.agent`：每个 agent 事件后等 stdout 写链清空。
        if let Some(agent) = session.agent() {
            let io = Arc::clone(&self.io);
            let listener: kiana_agent_core::AgentListener = Arc::new(move |_event, _signal| {
                let io = Arc::clone(&io);
                Box::pin(async move { io.wait_for_backpressure().await })
            });
            let subscription = agent.subscribe(listener);
            *lock(&self.unsubscribe_backpressure) =
                Some(Box::new(move || subscription.unsubscribe()));
        }
    }

    /// 扩展命令上下文的 6 个动作（options 原样转发给 host / session）。
    ///
    /// TS 的这些动作是普通 async 闭包：`runtimeHost.newSession()` 等 reject 时异常会抛回
    /// 等待中的扩展（扩展可以 `catch`），且不写 stderr。Rust 的冻结签名
    /// [`ExtensionCommandContextActions`] 把结果固定成 [`kiana_ext_api::types::NewSessionResult`]
    /// （只有 `cancelled`）或 `()`，没有错误通道；这里按“取消”收场并且**不写 stderr**
    /// （与 TS 的可观察 stdout/stderr 一致）。扩展无法区分取消与错误属冻结类型的限制。
    fn command_context_actions(self: &Arc<Self>) -> ExtensionCommandContextActions {
        let server = Arc::clone(self);
        let wait_for_idle: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync> =
            Arc::new(move || {
                let session = server.session();
                Box::pin(async move { session.wait_for_idle().await })
            });

        let server = Arc::clone(self);
        let new_session: Arc<
            dyn Fn(
                    Option<NewSessionOptions>,
                )
                    -> LocalBoxFuture<'static, kiana_ext_api::types::NewSessionResult>
                + Send
                + Sync,
        > = Arc::new(move |options| {
            let host = Arc::clone(&server.host);
            Box::pin(async move {
                match host.new_session(options).await {
                    Ok(result) => kiana_ext_api::types::NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => kiana_ext_api::types::NewSessionResult { cancelled: true },
                }
            })
        });

        let server = Arc::clone(self);
        let fork: Arc<
            dyn Fn(
                    String,
                    Option<ForkOptions>,
                )
                    -> LocalBoxFuture<'static, kiana_ext_api::types::NewSessionResult>
                + Send
                + Sync,
        > = Arc::new(move |entry_id, options| {
            let host = Arc::clone(&server.host);
            Box::pin(async move {
                match host.fork(entry_id, options).await {
                    Ok(result) => kiana_ext_api::types::NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => kiana_ext_api::types::NewSessionResult { cancelled: true },
                }
            })
        });

        let server = Arc::clone(self);
        let navigate_tree: Arc<
            dyn Fn(
                    String,
                    Option<kiana_ext_api::types::NavigateTreeOptions>,
                )
                    -> LocalBoxFuture<'static, kiana_ext_api::types::NewSessionResult>
                + Send
                + Sync,
        > = Arc::new(move |target_id, options| {
            let session = server.session();
            Box::pin(async move {
                let options = options.unwrap_or_default();
                let result = session
                    .navigate_tree(
                        target_id,
                        NavigateTreeOptions {
                            summarize: options.summarize,
                            custom_instructions: options.custom_instructions,
                            replace_instructions: options.replace_instructions,
                            label: options.label,
                        },
                    )
                    .await;
                match result {
                    Ok(result) => kiana_ext_api::types::NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => kiana_ext_api::types::NewSessionResult { cancelled: true },
                }
            })
        });

        let server = Arc::clone(self);
        let switch_session: Arc<
            dyn Fn(
                    String,
                    Option<SwitchSessionOptions>,
                )
                    -> LocalBoxFuture<'static, kiana_ext_api::types::NewSessionResult>
                + Send
                + Sync,
        > = Arc::new(move |session_path, options| {
            let host = Arc::clone(&server.host);
            Box::pin(async move {
                match host.switch_session(session_path, options).await {
                    Ok(result) => kiana_ext_api::types::NewSessionResult {
                        cancelled: result.cancelled,
                    },
                    Err(_error) => kiana_ext_api::types::NewSessionResult { cancelled: true },
                }
            })
        });

        let server = Arc::clone(self);
        let reload: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync> =
            Arc::new(move || {
                let session = server.session();
                Box::pin(async move {
                    let _ = session.reload().await;
                })
            });

        ExtensionCommandContextActions {
            wait_for_idle,
            new_session,
            fork,
            navigate_tree,
            switch_session,
            reload,
        }
    }

    /// 注册 SIGTERM / SIGHUP 处理器（只注册一次）。
    ///
    /// 用 `sigaction` + self-pipe 而不是 `tokio::signal::unix`：tokio 的处理器永不注销，
    /// 无法复刻 TS `process.off` 的两个语义（见 [`crate::signal`]）。注册顺序固定
    /// SIGTERM 先、SIGHUP 后（与 TS 的 signals 数组一致）。
    fn register_signal_handlers(self: &Arc<Self>) {
        if self.signals_registered.swap(true, Ordering::SeqCst) {
            return;
        }
        #[cfg(unix)]
        {
            use crate::signal::{self, SignalKind};

            for (kind, run_signal) in [
                (SignalKind::Term, RunSignal::Term),
                (SignalKind::Hup, RunSignal::Hup),
            ] {
                let Some(registration) = signal::register(kind) else {
                    continue;
                };
                let signal::Registration {
                    signal_number,
                    previous,
                    read_end,
                    write_end,
                } = registration;
                // SAFETY: `read_end` 是打开的管道读端，所有权交给 `AsyncFd`；写端由任务
                // 持有，不会提前关闭（否则 EOF 会被误判为一次信号）。
                let reader = match unsafe {
                    tokio::io::unix::AsyncFd::register_with_interest(
                        read_end,
                        tokio::io::Interest::READABLE,
                    )
                } {
                    Ok(reader) => reader,
                    Err(_) => {
                        signal::restore(signal_number, previous);
                        continue;
                    }
                };
                let server = Arc::clone(self);
                let stop = self.signals_stop.clone();
                let stop_for_task = stop.clone();
                let handle = tokio::task::spawn_local(async move {
                    let _write_keepalive = write_end;
                    loop {
                        tokio::select! {
                            biased;
                            // `process.off` 已还原默认处置；任务只需退出。
                            () = stop_for_task.cancelled() => return,
                            () = signal::read_wake_byte(&reader) => {}
                        }
                        // TS 的 handler：`killTrackedDetachedChildren()` 后
                        // `void shutdown(...)`（处理器本身不等待关停）。关停放独立任务，
                        // 监听循环才能继续接收信号；但 shutdown 一开始就会执行
                        // `process.off`（还原处置），之后的信号由内核按默认动作终止进程。
                        if !server.shutting_down.load(Ordering::SeqCst) {
                            kiana_core::shell::kill_tracked_detached_children();
                        }
                        let server = Arc::clone(&server);
                        tokio::task::spawn_local(async move {
                            let _ = server.shutdown_for_signal(Some(run_signal)).await;
                        });
                    }
                });
                lock(&self.signal_tasks).push(handle);
                lock(&self.signal_cleanups).push(Box::new(move || {
                    // `process.off(signal, handler)`：恢复注册前的处置（通常是 SIG_DFL），
                    // 并让监听任务离开 select。
                    signal::restore(signal_number, previous);
                    stop.cancel();
                }));
            }
        }
        // Windows：没有 `sigaction`/tokio SIGTERM 源，因此信号表为空——这是记录在案的
        // 平台限制（spec §2.6.8 的实现说明）。
        #[cfg(not(unix))]
        {
            // 无信号处理器：EOF 与扩展 shutdownHandler 仍然工作。
        }
    }

    /// 运行 RPC 模式（stdin -> stdout），返回退出码。
    ///
    /// 从进程 stdin 读 JSONL 协议请求，写出 wire 记录，直到 stdin EOF 或关停完成。
    /// 必须在 `LocalSet` 上调用（内部 `spawn_local`）；返回码交给调用方决定进程退出
    /// （生产入口 `bin/kiana-rpc` 用 `std::process::exit`）。
    pub async fn run(self: Arc<Self>) -> i32 {
        self.run_with_input(tokio::io::stdin()).await
    }

    /// 从任意异步 reader 读取 JSONL 并处理（测试 seam；与 [`run`](Self::run) 同一实现）。
    ///
    /// 返回退出码后，仍可能在途的行任务会被取消（见 `run_input_loop` 与
    /// `abort_line_tasks` 的注释）；调用方若复用同一个 [`RawStdout`]，不必担心
    /// 迟到的协议记录。
    #[doc(hidden)]
    pub async fn run_with_input<R: AsyncRead + Unpin>(self: Arc<Self>, input: R) -> i32 {
        let code = Arc::clone(&self).run_input_loop(input).await;
        // TS 的 `process.exit` 直接丢弃在途的 `handleInputLine`：关停完成后取消剩余行任务，
        // 避免它们在 `run_with_input` 返回后继续写 stdout（嵌入式调用方仍持有 io）。
        self.abort_line_tasks();
        code
    }

    async fn run_input_loop<R: AsyncRead + Unpin>(self: Arc<Self>, mut input: R) -> i32 {
        // 阶段 1：装配。rebind 必须先于信号注册——TS 的 `runRpcMode` 同序；随后进入
        // 阶段 2 的读循环，每轮 select 三个分支（见循环内的注释）。
        self.rebind_session().await;
        self.register_signal_handlers();

        // 阶段 2：读循环。64 KiB 块读 + [`JsonlLineReader`] 切行（跨块行由 reader 缓存）；
        // `biased` select 让关停分支优先于新输入，保证停止读 stdin 后不再派发新命令。
        let mut line_reader = JsonlLineReader::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let read = tokio::select! {
                biased;
                // `shutdown` 在 `dispose()` 之后取消 `input_stop`（TS 的 `detachInput` +
                // `stdin.pause()`）：立即停止读取，等 flush 结束后返回退出码。
                () = self.input_stop.cancelled() => return self.await_shutdown_code().await,
                // 关停完成（含重入强杀）时立刻返回：TS 在 dispose/flush 之后
                // `process.exit()`，**不等待**在途的 `handleInputLine`。
                code = self.await_shutdown_code() => return code,
                read = input.read(&mut chunk) => read,
            };
            match read {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    for line in line_reader.push_bytes(&chunk[..read]) {
                        self.spawn_line(line);
                    }
                }
            }
        }
        // EOF：TS 的 `onInputEnd` 立即 `void shutdown()`。最后一段完整行（含无 LF 的行）
        // 仍要先进入 `handleInputLine`（spec §2.6.11），但**不等待**它们完成：`dispose()`
        // 会中止在途命令，`process.exit()` 丢弃剩余任务。若关停早已被信号/扩展触发，
        // `finish_run` 会等待它（并可被重入强杀抢占）。
        for line in line_reader.finish() {
            self.spawn_line(line);
        }
        self.finish_run().await
    }

    /// 取消所有仍在运行的行任务（`process.exit` 丢弃在途的 `handleInputLine`）。
    fn abort_line_tasks(&self) {
        let tasks: Vec<_> = std::mem::take(&mut *lock(&self.line_tasks));
        for task in tasks {
            task.abort();
        }
    }

    /// EOF 后的收尾（TS 的 `onInputEnd`：`void shutdown()`）。
    ///
    /// TS 的 `shutdown` 是无条件调用：若已有 shutdown（信号 / 扩展请求）在进行，
    /// `shuttingDown` 已为 true，重入分支直接 `process.exit(0)`——即使那个 shutdown 还卡在
    /// `dispose()`。因此这里用 [`Self::shutdown_once`] 的重入语义，而不是只等待已有调用者。
    async fn finish_run(self: Arc<Self>) -> i32 {
        let cleanup = self.shutdown_once(0, None);
        tokio::pin!(cleanup);
        tokio::select! {
            biased;
            code = self.await_shutdown_code() => code,
            (code, _) = &mut cleanup => code,
        }
    }

    /// 等待“第一个 `shutdown` 完成”或“重入关停的强制退出码”，返回应使用的退出码。
    ///
    /// 重入关停（扩展在关停期间再次请求 / 测试 seam 的第二次调用）优先：TS 的重入分支
    /// 直接 `process.exit(exitCode)`，即使第一个 `shutdown` 还卡在 `dispose()`。
    async fn await_shutdown_code(&self) -> i32 {
        let force = self.force_exit_code.load(Ordering::SeqCst);
        if force != NO_FORCE_EXIT {
            return force;
        }
        let mut receiver = self.shutdown_exit.subscribe();
        loop {
            let force = self.force_exit_code.load(Ordering::SeqCst);
            if force != NO_FORCE_EXIT {
                return force;
            }
            if let Some(code) = *receiver.borrow_and_update() {
                return code;
            }
            if receiver.changed().await.is_err() {
                return self.exit_code.load(Ordering::SeqCst);
            }
        }
    }

    /// `void handleInputLine(line)`：并发处理，不排队。
    ///
    /// JS 调用 async 函数会**同步执行到第一个 `await`**，因此这里先手动 poll 一次再交给
    /// `spawn_local`：`data` 事件到达时命令的同步前缀（解析、`session.abort()` 的调用等）
    /// 立即发生，与 TS 的 `void handleInputLine(line)` 一致。EOF 关停不再等待行任务后，
    /// 这个同步前缀是“最后一行仍被处理”的唯一保证（spec §2.6.11）。
    fn spawn_line(self: &Arc<Self>, line: String) {
        {
            let mut tasks = lock(&self.line_tasks);
            tasks.retain(|task| !task.is_finished());
        }
        let server = Arc::clone(self);
        let mut future = Box::pin(async move {
            server.handle_input_line(&line).await;
        });
        // `Waker::noop`：只是驱动同步前缀；若挂起，交给 `spawn_local` 重新 poll（所有
        // 挂起中的 future 都会在重新 poll 时用运行时 waker 覆盖注册）。
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        if std::future::Future::poll(future.as_mut(), &mut context).is_pending() {
            lock(&self.line_tasks).push(tokio::task::spawn_local(future));
        }
    }

    /// 处理一行 stdin（TS 的 `handleInputLine`）。
    ///
    /// 错误传播（TS 的 try/catch 等价物）：
    /// - JSON 解析失败 -> `command: "parse"` 的错误响应（无 id，TS 的 `catch` 拿不到）；
    /// - 顶层 `null` -> 不写任何记录（TS 是 unhandled rejection）；
    /// - `handle_command` 的 `Err(message)` -> `success:false` 错误响应；
    /// - 命令内部提前返回的响应（`prompt` 的 spawn 分支）自己负责写出。
    ///
    /// 本函数内每次写出后 `wait_for_backpressure()`：响应记录在处理下一行前先落进写链
    /// （`prompt` 的响应在其 spawn 的任务里写出，不在此列）。
    #[doc(hidden)]
    pub async fn handle_input_line(self: &Arc<Self>, line: &str) {
        let parsed = match kiana_json::js_parse::parse(line) {
            Ok(value) => value,
            Err(error) => {
                self.output(&error_response(
                    None,
                    Some(Value::String("parse".to_string())),
                    Some(format!("Failed to parse command: {}", error.message)),
                ));
                self.io.wait_for_backpressure().await;
                return;
            }
        };

        // 顶层 null：TS 里 `command.id` 抛 TypeError，catch 块再次访问又抛，
        // 最终是 unhandled rejection —— 不写任何记录。
        if parsed.is_null() {
            return;
        }

        // extension_ui_response：只按 type/id 查 pending map，原样 resolve，不写记录。
        if let Value::Object(map) = &parsed
            && map.get("type").and_then(Value::as_str) == Some("extension_ui_response")
        {
            if let Some(id) = map.get("id").and_then(Value::as_str) {
                let sender = lock(&self.pending).remove(id);
                if let Some(sender) = sender {
                    let _ = sender.send(parsed.clone());
                }
            }
            return;
        }

        let id = command_field(&parsed, "id").cloned();
        let command = command_field(&parsed, "type").cloned();
        // Ok(Some)：立即响应；Ok(None)：`prompt` 已在 spawn 的任务里自行响应；
        // Err：catch 等价物，转成错误响应。非对象命令没有 `id`/`type` 字段，
        // 由 `handle_command` 的 `Unknown command` 分支统一兜住。
        match self.handle_command(&parsed).await {
            Ok(Some(response)) => {
                match response.raw_line.as_deref() {
                    Some(line) => self.io.write_raw(line),
                    None => self.output(&response),
                }
                self.io.wait_for_backpressure().await;
                self.check_shutdown_requested().await;
            }
            Ok(None) => {
                self.check_shutdown_requested().await;
            }
            Err(message) => {
                self.output(&error_response(id, command, Some(message)));
                self.io.wait_for_backpressure().await;
            }
        }
    }

    /// 33 个命令的分发（TS 的 `handleCommand`）。
    ///
    /// `Ok(None)` 表示不立即响应（只有 `prompt`）；`Err` 由 [`handle_input_line`] 的
    /// catch 转成 `{id?, type:"response", command:<原始 type>, success:false, error}`。
    async fn handle_command(
        self: &Arc<Self>,
        command: &Value,
    ) -> Result<Option<RpcResponse>, String> {
        let id = command_field(command, "id").cloned();
        let type_text = command
            .as_object()
            .and_then(|map| map.get("type"))
            .and_then(Value::as_str);
        let Some(type_text) = type_text else {
            return Err(format!(
                "Unknown command: {}",
                js_string(command.as_object().and_then(|map| map.get("type")))
            ));
        };
        let session = self.session();

        match type_text {
            // =============================================================
            // Prompting
            // =============================================================
            "prompt" => {
                // TS 把**原始** `command.message` / `command.images` 交给 `session.prompt()`；
                // 非字符串 `text` 在 `text.startsWith(...)` 上抛出，畸形 `images` 在
                // `_normalizePromptImages` 的迭代/`Buffer.from`/`mimeType.split` 上抛出，
                // 都被 `.catch` 捕获（响应仍是 `prompt` 错误、不会启动 run）。
                // 因此在 spawn 的任务里校验，让 `handleCommand()` 正常返回 `undefined`
                // （随后 `checkShutdownRequested()` 仍会执行，关停请求不会被丢弃）。
                let message = command_field(command, "message").cloned();
                let images = prompt_images_from(command);
                let streaming_behavior = streaming_behavior_from(command);
                let preflight_succeeded = Arc::new(AtomicBool::new(false));
                let flag = Arc::clone(&preflight_succeeded);
                let server = Arc::clone(self);
                let preflight_server = Arc::clone(self);
                let response_id = id.clone();
                let prompt_task = tokio::task::spawn_local(async move {
                    let message = match message {
                        Some(Value::String(text)) => text,
                        other => {
                            server.output(&error_response(
                                id,
                                Some(Value::String("prompt".to_string())),
                                Some(text_field_type_error(other.as_ref(), "text", "startsWith")),
                            ));
                            return;
                        }
                    };
                    let images = match images {
                        Ok(images) => images,
                        Err(error) => {
                            server.output(&error_response(
                                id,
                                Some(Value::String("prompt".to_string())),
                                Some(error),
                            ));
                            return;
                        }
                    };
                    let options = PromptOptions {
                        expand_prompt_templates: None,
                        images,
                        streaming_behavior,
                        source: Some(InputSource::Rpc),
                        preflight_result: Some(Arc::new(move |disposition: PromptDisposition| {
                            flag.store(true, Ordering::SeqCst);
                            preflight_server.output(&success_response(
                                response_id.clone(),
                                "prompt",
                                Some(disposition_data(disposition)),
                            ));
                        })),
                    };
                    if let Err(error) = session.prompt(message, options).await
                        && !preflight_succeeded.load(Ordering::SeqCst)
                    {
                        server.output(&error_response(
                            id,
                            Some(Value::String("prompt".to_string())),
                            Some(error.to_string()),
                        ));
                    }
                });
                // 与 `spawn_line` 一样登记任务：EOF/关停后 `abort_line_tasks()` 要能取消
                // 在途的 prompt，否则它会在 `run_with_input` 返回后继续向 stdout 写记录。
                {
                    let mut tasks = lock(&self.line_tasks);
                    tasks.retain(|task| !task.is_finished());
                    tasks.push(prompt_task);
                }
                Ok(None)
            }

            "steer" => {
                let message = required_text_field(command, "message", "text", "startsWith")?;
                let images = queued_images_from(command)?;
                let disposition = session
                    .steer(message, images, Some(InputSource::Rpc))
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(Some(success_response(
                    id,
                    "steer",
                    Some(queued_disposition_data(disposition)),
                )))
            }

            "follow_up" => {
                let message = required_text_field(command, "message", "text", "startsWith")?;
                let images = queued_images_from(command)?;
                let disposition = session
                    .follow_up(message, images, Some(InputSource::Rpc))
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(Some(success_response(
                    id,
                    "follow_up",
                    Some(queued_disposition_data(disposition)),
                )))
            }

            "abort" => {
                session.abort().await;
                Ok(Some(success_response(id, "abort", None)))
            }

            "clear_queue" => {
                let (steering, follow_up) = session.clear_queue();
                let mut data = Map::new();
                data.insert("steering".to_string(), string_array(&steering));
                data.insert("followUp".to_string(), string_array(&follow_up));
                Ok(Some(success_response(
                    id,
                    "clear_queue",
                    Some(Value::Object(data)),
                )))
            }

            "new_session" => {
                let parent_session = command_field(command, "parentSession")
                    .filter(|value| is_truthy(Some(value)))
                    .map(|value| js_string(Some(value)));
                let options = parent_session.map(|parent_session| NewSessionOptions {
                    parent_session: Some(parent_session),
                    setup: None,
                    with_session: None,
                });
                let result = self.host.new_session(options).await?;
                if !result.cancelled {
                    self.rebind_session().await;
                }
                Ok(Some(success_response(
                    id,
                    "new_session",
                    Some(cancelled_data(result.cancelled)),
                )))
            }

            // =============================================================
            // State
            // =============================================================
            "get_state" => Ok(Some(success_response(
                id,
                "get_state",
                Some(self.session_state_value(&session)?),
            ))),

            // =============================================================
            // Model
            // =============================================================
            "set_model" => {
                let provider_raw = command_field(command, "provider");
                let model_id_raw = command_field(command, "modelId");
                let provider = provider_raw.and_then(Value::as_str);
                let model_id = model_id_raw.and_then(Value::as_str);
                let models = session.available_models();
                let model = models.iter().find(|model| {
                    provider == Some(model.provider.as_str()) && model_id == Some(model.id.as_str())
                });
                let Some(model) = model.cloned() else {
                    return Err(format!(
                        "Model not found: {}/{}",
                        js_string(provider_raw),
                        js_string(model_id_raw)
                    ));
                };
                session
                    .set_model(model.clone(), ModelMutationOptions::default())
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(Some(success_response(
                    id,
                    "set_model",
                    Some(to_value(&model)?),
                )))
            }

            "cycle_model" => {
                let result = session
                    .cycle_model()
                    .await
                    .map_err(|error| error.to_string())?;
                let data = match result {
                    None => Value::Null,
                    Some(result) => model_cycle_data(&result)?,
                };
                Ok(Some(success_response(id, "cycle_model", Some(data))))
            }

            "get_available_models" => {
                let models = session.available_models();
                let mut data = Map::new();
                data.insert("models".to_string(), to_value(&models)?);
                Ok(Some(success_response(
                    id,
                    "get_available_models",
                    Some(Value::Object(data)),
                )))
            }

            // =============================================================
            // Thinking
            // =============================================================
            "set_thinking_level" => {
                let level = parse_thinking_level(command_field(command, "level"));
                session.set_thinking_level(level, ModelMutationOptions::default());
                Ok(Some(success_response(id, "set_thinking_level", None)))
            }

            "cycle_thinking_level" => {
                let level = session.cycle_thinking_level(ModelMutationOptions::default());
                let data = match level {
                    None => Value::Null,
                    Some(level) => {
                        let mut data = Map::new();
                        data.insert(
                            "level".to_string(),
                            Value::String(thinking_level_text(level)),
                        );
                        Value::Object(data)
                    }
                };
                Ok(Some(success_response(
                    id,
                    "cycle_thinking_level",
                    Some(data),
                )))
            }

            "get_available_thinking_levels" => {
                let levels: Vec<Value> = session
                    .get_available_thinking_levels()
                    .into_iter()
                    .map(|level| Value::String(thinking_level_text(level)))
                    .collect();
                let mut data = Map::new();
                data.insert("levels".to_string(), Value::Array(levels));
                Ok(Some(success_response(
                    id,
                    "get_available_thinking_levels",
                    Some(Value::Object(data)),
                )))
            }

            // =============================================================
            // Queue Modes
            // =============================================================
            "set_steering_mode" => {
                // TS 把原始值原样写进 agent/settings；`get_state` 会原样回显（含 `"bogus"`、
                // `undefined`）。Rust 的 `QueueMode` 只能表示两档，所以行为映射到
                // `mode === "all" ? All : OneAtATime`（TS 的 `peek()` 只认 `"all"`），
                // 回显则由 [`RawSessionSettings`] 记录原始 JSON。
                let raw = command_field(command, "mode").cloned();
                *lock(&self.raw_settings.steering_mode) = Some(raw.clone());
                session.set_steering_mode(js_queue_mode(raw.as_ref()));
                Ok(Some(success_response(id, "set_steering_mode", None)))
            }

            "set_follow_up_mode" => {
                let raw = command_field(command, "mode").cloned();
                *lock(&self.raw_settings.follow_up_mode) = Some(raw.clone());
                session.set_follow_up_mode(js_queue_mode(raw.as_ref()));
                Ok(Some(success_response(id, "set_follow_up_mode", None)))
            }

            // =============================================================
            // Compaction
            // =============================================================
            "compact" => {
                // TS 把原始值交给 `session.compact()`：`if (customInstructions)` 只做真值
                // 判断，随后用模板字符串插进摘要提示，因此真值非字符串会被 `String(v)`
                // 强转（如 `42` -> `"42"`、`{}` -> `"[object Object]"`），falsy 才丢弃。
                let custom_instructions =
                    truthy_js_string(command_field(command, "customInstructions"));
                let result = session
                    .compact(custom_instructions)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(Some(success_response(
                    id,
                    "compact",
                    Some(to_value(&result)?),
                )))
            }

            "set_auto_compaction" => {
                let raw = command_field(command, "enabled").cloned();
                // `settings.compaction.enabled = raw`；getter 是 `raw ?? true`。
                *lock(&self.raw_settings.auto_compaction_enabled) = Some(raw.clone());
                session.set_auto_compaction_enabled(enabled_value(raw.as_ref()));
                Ok(Some(success_response(id, "set_auto_compaction", None)))
            }

            // =============================================================
            // Retry
            // =============================================================
            "set_auto_retry" => {
                session.set_auto_retry_enabled(enabled_value(command_field(command, "enabled")));
                Ok(Some(success_response(id, "set_auto_retry", None)))
            }

            "abort_retry" => {
                session.abort_retry();
                Ok(Some(success_response(id, "abort_retry", None)))
            }

            // =============================================================
            // Bash
            // =============================================================
            "bash" => self.handle_bash_command(&session, command, id).await,

            "abort_bash" => {
                session.abort_bash();
                Ok(Some(success_response(id, "abort_bash", None)))
            }

            // =============================================================
            // Session
            // =============================================================
            "get_session_stats" => Ok(Some(success_response(
                id,
                "get_session_stats",
                Some(to_value(&session.get_session_stats())?),
            ))),

            "export_html" => {
                // TS：`session.exportToHtml(outputPath)` -> `exportSessionToHtml`。后者**先**
                // 检查会话文件（内存会话 / 文件不存在），**之后**才
                // `opts.outputPath ? normalizePath(opts.outputPath) : undefined`。因此真值
                // 非字符串的 `normalizePath` TypeError 必须让位给这两个 session 级错误。
                let output_path_raw = command_field(command, "outputPath");
                let output_path = if !is_truthy(output_path_raw) {
                    // `""`/`0`/`false`/`null`/缺失都走默认路径。
                    None
                } else {
                    match output_path_raw {
                        Some(Value::String(path)) => Some(path.clone()),
                        _ => {
                            let Some(session_file) = session.session_file() else {
                                return Err("Cannot export in-memory session to HTML".to_string());
                            };
                            if !std::path::Path::new(&session_file).exists() {
                                return Err("Nothing to export yet - start a conversation first"
                                    .to_string());
                            }
                            return Err("normalized.startsWith is not a function".to_string());
                        }
                    }
                };
                let path = session
                    .export_html(output_path)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut data = Map::new();
                data.insert("path".to_string(), Value::String(path));
                Ok(Some(success_response(
                    id,
                    "export_html",
                    Some(Value::Object(data)),
                )))
            }

            "switch_session" => {
                // TS 把原始值交给 `SessionManager.open()` -> `resolvePath()` ->
                // `normalizePath()`，非字符串在 `normalized.startsWith` 上抛 TypeError；
                // 缺失/`null` 分别是 "Cannot read properties of undefined/null ..."。
                let session_path = match command_field(command, "sessionPath") {
                    Some(Value::String(path)) => path.clone(),
                    other => {
                        return Err(text_field_type_error(other, "normalized", "startsWith"));
                    }
                };
                let result = self.host.switch_session(session_path, None).await?;
                if !result.cancelled {
                    self.rebind_session().await;
                }
                Ok(Some(success_response(
                    id,
                    "switch_session",
                    Some(cancelled_data(result.cancelled)),
                )))
            }

            "fork" => {
                let entry_id = js_string(command_field(command, "entryId"));
                let result = self.host.fork(entry_id, None).await?;
                if !result.cancelled {
                    self.rebind_session().await;
                }
                let mut data = Map::new();
                if let Some(text) = result.selected_text {
                    data.insert("text".to_string(), Value::String(text));
                }
                data.insert("cancelled".to_string(), Value::Bool(result.cancelled));
                Ok(Some(success_response(
                    id,
                    "fork",
                    Some(Value::Object(data)),
                )))
            }

            "clone" => {
                // TS: `if (!leafId)` —— 空串同样是 falsy。
                let Some(leaf_id) = session.leaf_id().filter(|leaf_id| !leaf_id.is_empty()) else {
                    return Err("Cannot clone session: no current entry selected".to_string());
                };
                let options = ForkOptions {
                    position: Some(ForkPosition::At),
                    with_session: None,
                };
                let result = self.host.fork(leaf_id, Some(options)).await?;
                if !result.cancelled {
                    self.rebind_session().await;
                }
                Ok(Some(success_response(
                    id,
                    "clone",
                    Some(cancelled_data(result.cancelled)),
                )))
            }

            "get_fork_messages" => {
                let messages: Vec<Value> = session
                    .get_user_messages_for_forking()
                    .into_iter()
                    .map(|(entry_id, text)| {
                        let mut message = Map::new();
                        message.insert("entryId".to_string(), Value::String(entry_id));
                        message.insert("text".to_string(), Value::String(text));
                        Value::Object(message)
                    })
                    .collect();
                let mut data = Map::new();
                data.insert("messages".to_string(), Value::Array(messages));
                Ok(Some(success_response(
                    id,
                    "get_fork_messages",
                    Some(Value::Object(data)),
                )))
            }

            "get_entries" => {
                let mut entries = session.entries();
                if let Some(since) = command.as_object().and_then(|map| map.get("since")) {
                    // TS 的 `e.id === command.since` 是严格相等：非字符串 since 永远不匹配。
                    let position = since
                        .as_str()
                        .and_then(|since| entries.iter().position(|entry| entry.id() == since));
                    let Some(position) = position else {
                        return Err(format!("Entry not found: {}", js_string(Some(since))));
                    };
                    entries = entries.split_off(position + 1);
                }
                let leaf_id = session.leaf_id().map(Value::String).unwrap_or(Value::Null);
                let response = value_line_on_large_stack(id, "get_entries", move || {
                    let entries =
                        serde_json::to_value(&entries).map_err(|error| error.to_string())?;
                    let mut data = Map::new();
                    data.insert("entries".to_string(), entries);
                    data.insert("leafId".to_string(), leaf_id);
                    Ok(Value::Object(data))
                })
                .await?;
                Ok(Some(response))
            }

            "get_tree" => {
                let tree = session.tree();
                let leaf_id = session.leaf_id().map(Value::String).unwrap_or(Value::Null);
                let response = value_line_on_large_stack(id, "get_tree", move || {
                    let tree = tree_nodes_value(&tree);
                    let mut data = Map::new();
                    data.insert("tree".to_string(), tree);
                    data.insert("leafId".to_string(), leaf_id);
                    Ok(Value::Object(data))
                })
                .await?;
                Ok(Some(response))
            }

            "get_last_assistant_text" => {
                let mut data = Map::new();
                if let Some(text) = session.get_last_assistant_text() {
                    data.insert("text".to_string(), Value::String(text));
                }
                Ok(Some(success_response(
                    id,
                    "get_last_assistant_text",
                    Some(Value::Object(data)),
                )))
            }

            "set_session_name" => {
                let name = required_text_field(command, "name", "command.name", "trim")?;
                let name = kiana_json::js_string::js_trim(&name);
                if name.is_empty() {
                    return Err("Session name cannot be empty".to_string());
                }
                session.set_session_name(name);
                Ok(Some(success_response(id, "set_session_name", None)))
            }

            // =============================================================
            // Messages
            // =============================================================
            "get_messages" => {
                let messages = session.messages();
                let response = value_line_on_large_stack(id, "get_messages", move || {
                    let messages =
                        serde_json::to_value(&messages).map_err(|error| error.to_string())?;
                    let mut data = Map::new();
                    data.insert("messages".to_string(), messages);
                    Ok(Value::Object(data))
                })
                .await?;
                Ok(Some(response))
            }

            // =============================================================
            // Commands
            // =============================================================
            "get_commands" => {
                let mut commands: Vec<Value> = Vec::new();
                for command in session.registered_commands() {
                    commands.push(slash_command_value(
                        &command.invocation_name,
                        command.description.as_deref(),
                        "extension",
                        &command.source_info,
                    )?);
                }
                for template in session.prompt_templates() {
                    commands.push(slash_command_value(
                        &template.name,
                        Some(template.description.as_str()),
                        "prompt",
                        &template.source_info,
                    )?);
                }
                for skill in session.skills() {
                    commands.push(slash_command_value(
                        &format!("skill:{}", skill.name),
                        Some(skill.description.as_str()),
                        "skill",
                        &skill.source_info,
                    )?);
                }
                let mut data = Map::new();
                data.insert("commands".to_string(), Value::Array(commands));
                Ok(Some(success_response(
                    id,
                    "get_commands",
                    Some(Value::Object(data)),
                )))
            }

            // =============================================================
            // Unknown
            // =============================================================
            other => Err(format!("Unknown command: {other}")),
        }
    }

    /// `bash` 分支：扩展 `user_bash` 优先，其次本地执行。
    ///
    /// 已知跨 crate 缺口：TS 把**原始** `command.command` / `command.excludeFromContext`
    /// 放进 `user_bash` 事件、`recordBashResult` 与 `executeBash`（只有 `spawn` 里做
    /// `String()`），因此 `{"type":"bash"}` 记录的消息没有 `command` 键、`{"command":42}`
    /// 记录数字。冻结的 [`UserBashEvent::command`] 是 `String`、`record_bash_result` /
    /// `execute_bash` 也只接受 `String`/`Option<bool>`，Rust 侧只能用
    /// `js_string`/真值转换（与 TS 进程 spawn 后的可见结果一致，但扩展与 get_messages
    /// 看到的是转换后的值）。
    async fn handle_bash_command(
        self: &Arc<Self>,
        session: &Arc<dyn AgentSessionFacade>,
        command: &Value,
        id: Option<Value>,
    ) -> Result<Option<RpcResponse>, String> {
        let command_text = js_string(command_field(command, "command"));
        let exclude_raw = command_field(command, "excludeFromContext");
        let exclude_from_context = match exclude_raw {
            None | Some(Value::Null) => false,
            Some(value) => is_truthy(Some(value)),
        };
        let exclude_option = match exclude_raw {
            None | Some(Value::Null) => None,
            Some(value) => Some(is_truthy(Some(value))),
        };
        let event = UserBashEvent {
            event_type: UserBashEventType::UserBash,
            command: command_text.clone(),
            exclude_from_context,
            cwd: session.cwd(),
        };
        let event_result = session.emit_user_bash(event).await?;
        let bash_id = command_field(command, "id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match event_result {
            Some(UserBashEventResult::Result(result)) => {
                session.record_bash_result(command_text, &result, exclude_option);
                Ok(Some(success_response(id, "bash", Some(to_value(&result)?))))
            }
            Some(UserBashEventResult::Operations(operations)) => {
                let result = session
                    .execute_bash(
                        command_text,
                        ExecuteBashOptions {
                            exclude_from_context: exclude_option,
                            id: bash_id,
                            operations: Some(operations),
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(Some(success_response(id, "bash", Some(to_value(&result)?))))
            }
            None => {
                let result = session
                    .execute_bash(
                        command_text,
                        ExecuteBashOptions {
                            exclude_from_context: exclude_option,
                            id: bash_id,
                            operations: None,
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(Some(success_response(id, "bash", Some(to_value(&result)?))))
            }
        }
    }

    /// `get_state` 的 data（键序 = §2.5.3 声明序）。
    fn session_state_value(&self, session: &Arc<dyn AgentSessionFacade>) -> Result<Value, String> {
        let mut state = Map::new();
        if let Some(model) = session.model() {
            state.insert("model".to_string(), to_value(&model)?);
        }
        state.insert(
            "thinkingLevel".to_string(),
            Value::String(thinking_level_text(session.thinking_level())),
        );
        state.insert(
            "isStreaming".to_string(),
            Value::Bool(session.is_streaming()),
        );
        state.insert(
            "isCompacting".to_string(),
            Value::Bool(session.is_compacting()),
        );
        match lock(&self.raw_settings.steering_mode).clone() {
            // 未调用过 setter：读会话的规范化 getter。
            None => {
                state.insert(
                    "steeringMode".to_string(),
                    queue_mode_text(session.steering_mode()),
                );
            }
            // TS 的 `agent.steeringMode = undefined` -> `JSON.stringify` 省略该键。
            Some(None) => {}
            Some(Some(value)) => {
                state.insert("steeringMode".to_string(), value);
            }
        }
        match lock(&self.raw_settings.follow_up_mode).clone() {
            None => {
                state.insert(
                    "followUpMode".to_string(),
                    queue_mode_text(session.follow_up_mode()),
                );
            }
            Some(None) => {}
            Some(Some(value)) => {
                state.insert("followUpMode".to_string(), value);
            }
        }
        if let Some(session_file) = session.session_file() {
            state.insert("sessionFile".to_string(), Value::String(session_file));
        }
        state.insert("sessionId".to_string(), Value::String(session.session_id()));
        if let Some(session_name) = session.session_name() {
            state.insert("sessionName".to_string(), Value::String(session_name));
        }
        match lock(&self.raw_settings.auto_compaction_enabled).clone() {
            None => {
                state.insert(
                    "autoCompactionEnabled".to_string(),
                    Value::Bool(session.auto_compaction_enabled()),
                );
            }
            // TS: `settings.compaction?.enabled ?? true`——`undefined`/`null` 落回默认 `true`。
            Some(None) | Some(Some(Value::Null)) => {
                state.insert("autoCompactionEnabled".to_string(), Value::Bool(true));
            }
            Some(Some(value)) => {
                state.insert("autoCompactionEnabled".to_string(), value);
            }
        }
        state.insert(
            "messageCount".to_string(),
            Value::Number(session.message_count().into()),
        );
        state.insert(
            "pendingMessageCount".to_string(),
            Value::Number(session.pending_message_count().into()),
        );
        Ok(Value::Object(state))
    }

    /// `checkShutdownRequested()`：扩展请求关停时执行 `shutdown()`。
    #[doc(hidden)]
    pub async fn check_shutdown_requested(self: &Arc<Self>) -> Option<i32> {
        if !self.shutdown_requested.load(Ordering::SeqCst) {
            return None;
        }
        Some(self.shutdown_once(0, None).await.0)
    }

    /// 进程信号触发关停（测试 seam；不调用 `process::exit`）。
    #[doc(hidden)]
    pub async fn shutdown_for_signal(self: &Arc<Self>, signal: Option<RunSignal>) -> i32 {
        let (code, skip_flush) = match signal {
            Some(signal) => (signal.exit_code(), signal.skips_flush()),
            None => (0, false),
        };
        // TS 的重入分支立即 `process.exit(exitCode)`：第一个调用者执行完整流程，
        // 重入调用返回调用方自己的退出码。
        let _ = self.shutdown_once(code, Some(skip_flush)).await;
        code
    }

    /// `shutdown(exitCode, signal)`：幂等；第一次调用者执行完整序列。
    ///
    /// 重入调用模拟 TS 的 `process.exit(exitCode)`：记录调用方的退出码并立刻唤醒所有
    /// 等待者。真实信号的“第二个信号”不再走这里——shutdown 开头已还原默认处置，内核直接
    /// 终止进程（见 [`Self::perform_shutdown`]）；这条分支覆盖扩展在关停期间再次请求关停
    /// 以及测试 seam 的直接调用。
    async fn shutdown_once(self: &Arc<Self>, code: i32, skip_flush: Option<bool>) -> (i32, bool) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            force_exit(self, code);
            return (code, false);
        }
        self.perform_shutdown(code, skip_flush).await;
        (code, true)
    }

    /// `shutdown` 的完整序列（TS §2.6.9 步骤 2-7）。
    async fn perform_shutdown(self: &Arc<Self>, code: i32, skip_flush: Option<bool>) {
        self.exit_code.store(code, Ordering::SeqCst);
        // TS 步骤 2：先移除信号处理器（`process.off`），Node 随即恢复默认处置。之后到达的
        // SIGTERM/SIGHUP 由内核按默认动作终止进程（dispose 期间的“第二个信号”因此是
        // `signal=SIGTERM/SIGHUP, code=null`，而不是走重入分支）。嵌入式调用方在
        // `run_rpc_mode` 返回后也不再被遗留的处理器吞掉信号。
        for cleanup in std::mem::take(&mut *lock(&self.signal_cleanups)) {
            cleanup();
        }
        // 退订会话事件与 agent 背压。
        if let Some(unsubscribe) = lock(&self.unsubscribe).take() {
            unsubscribe();
        }
        if let Some(unsubscribe) = lock(&self.unsubscribe_backpressure).take() {
            unsubscribe();
        }
        // `await runtimeHost.dispose()`（emit session_shutdown{reason:"quit"}）。
        self.host.dispose().await;
        // `detachInput()` + `stdin.pause()`：停止读取，不关闭 stdin。
        self.input_stop.cancel();
        // SIGTERM 不 flush；其它情况先 flush 再退出。
        if !skip_flush.unwrap_or(false) {
            self.io.flush().await;
        }
        self.shutdown_exit.send_replace(Some(code));
        // 信号监听任务已在开头的 cleanup 里取消；这里只回收句柄。
        let _tasks = std::mem::take(&mut *lock(&self.signal_tasks));
    }
}

// ============================================================================
// 扩展 UI 上下文（RPC 子协议）
// ============================================================================

/// 登记了 pending 请求的清理守卫。
///
/// `dialog` / `editor_dialog` 的 future 可能在等待期间被丢弃（扩展任务随 session
/// 替换/`LocalSet` 拆除而中止）。仅靠正常路径末尾的 `remove` 会把这个 UUID 和 oneshot
/// sender 永久留在 map 里；Drop 保证任何退出路径都会移除。
struct PendingRequest {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    id: String,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        lock(&self.pending).remove(&self.id);
    }
}

/// RPC 模式的 [`ExtensionUiContext`] 实现（TS 的 `createExtensionUIContext()`）。
///
/// 每次 `rebindSession()` 新建一个；所有请求经共享的 [`RawStdout`] 写出，pending map
/// 与服务器共用（`extension_ui_response` 由 `handle_input_line` 按 id resolve）。
struct RpcUiContext {
    io: Arc<dyn RawStdout>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

impl RpcUiContext {
    fn new(
        io: Arc<dyn RawStdout>,
        pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    ) -> Self {
        Self { io, pending }
    }

    /// 写出一条 `extension_ui_request`。
    fn send(&self, request: &RpcExtensionUIRequest) {
        write_record(&self.io, request);
    }

    /// `createDialogPromise`：signal 已中止 -> 默认值且不发请求；否则登记 pending 后写出，
    /// 在 response / timeout / signal 三者中最先到达者上解析（timeout 与 abort 都返回默认值）。
    async fn dialog<T>(
        &self,
        opts: Option<ExtensionUIDialogOptions>,
        default: T,
        build: impl FnOnce(&str) -> RpcExtensionUIRequest,
        parse: impl FnOnce(&Value) -> T,
    ) -> T {
        if let Some(signal) = opts.as_ref().and_then(|opts| opts.signal.as_ref())
            && signal.is_cancelled()
        {
            return default;
        }
        let id = Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(id.clone(), sender);
        // guard 持有一份 Arc；future 被取消时也会把 id 从 pending 里移除。
        let pending_guard = PendingRequest {
            pending: Arc::clone(&self.pending),
            id: id.clone(),
        };
        self.send(&build(&id));

        let signal = opts.as_ref().and_then(|opts| opts.signal.clone());
        let timeout = opts
            .as_ref()
            .and_then(|opts| opts.timeout)
            .filter(|timeout| *timeout != 0.0 && !timeout.is_nan());
        if let Some(timeout) = timeout {
            warn_timeout_delay(timeout);
        }
        let response = match (signal, timeout) {
            (Some(signal), Some(timeout)) => {
                tokio::select! {
                    biased;
                    () = signal.cancelled() => None,
                    value = receiver => value.ok(),
                    () = tokio::time::sleep(timeout_duration(timeout)) => None,
                }
            }
            (Some(signal), None) => {
                tokio::select! {
                    biased;
                    () = signal.cancelled() => None,
                    value = receiver => value.ok(),
                }
            }
            (None, Some(timeout)) => {
                tokio::select! {
                    biased;
                    value = receiver => value.ok(),
                    () = tokio::time::sleep(timeout_duration(timeout)) => None,
                }
            }
            (None, None) => receiver.await.ok(),
        };
        // 正常路径也走 guard：显式释放，避免再把 id 留在 map 里。
        drop(pending_guard);
        match response {
            Some(response) => parse(&response),
            None => default,
        }
    }

    /// `editor(title, prefill?)`：无 timeout / signal 支持。
    async fn editor_dialog(&self, title: &str, prefill: Option<&str>) -> Option<String> {
        let id = Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(id.clone(), sender);
        let pending_guard = PendingRequest {
            pending: Arc::clone(&self.pending),
            id: id.clone(),
        };
        self.send(&RpcExtensionUIRequest::editor(
            id.clone(),
            title,
            prefill.map(str::to_owned),
        ));
        let response = receiver.await.ok();
        drop(pending_guard);
        response.as_ref().and_then(parse_string_response)
    }
}

/// `parseResponse`（select / input / editor）：`cancelled` 真值 -> `None`；
/// 否则 `"value" in r ? r.value : undefined`。
///
/// 已知跨 crate 缺口：TS 原样返回 `r.value`（可能是 `null`/数字/对象）；冻结的
/// [`ExtensionUiContext::select`] 返回 `Option<String>`，无法承载非字符串值，这里只能
/// 映射为 `None`（JS 侧表现为 `undefined`）。需要 kiana-ext-api 改成 `Value` 才能完全对齐。
fn parse_string_response(response: &Value) -> Option<String> {
    let map = response.as_object()?;
    if is_truthy(map.get("cancelled")) {
        return None;
    }
    map.get("value").and_then(Value::as_str).map(str::to_owned)
}

/// `parseResponse`（confirm）：`cancelled` 真值 -> `false`；否则 `confirmed` 的真值。
///
/// TS 原样返回 `r.confirmed`（类型声明是 boolean，运行时未校验）。Rust 的 trait 返回 `bool`，
/// 因此按 JS 真值映射（`1`/`"yes"`/对象 -> true，`0`/`""`/`null`/缺失 -> false），与调用方
/// `if (confirmed)` 的语义一致；`confirmed === true` 之类的严格比较无法复刻（冻结签名）。
fn parse_confirm_response(response: &Value) -> bool {
    let Some(map) = response.as_object() else {
        return false;
    };
    if is_truthy(map.get("cancelled")) {
        return false;
    }
    map.get("confirmed")
        .is_some_and(|value| is_truthy(Some(value)))
}

/// `setTimeout` 的等价时长。Node 内部把 `!(delay >= 1 && delay <= TIMEOUT_MAX)` 的
/// 延迟（负数、0、`NaN`、`Infinity`、超过 2^31-1）统一改成 **1ms**；调用点已按
/// `if (opts?.timeout)` 过滤掉 0/NaN，这里仍按同一规则处理其余值。
fn timeout_duration(milliseconds: f64) -> std::time::Duration {
    const TIMEOUT_MAX: f64 = 2_147_483_647.0;
    if !(1.0..=TIMEOUT_MAX).contains(&milliseconds) {
        return std::time::Duration::from_millis(1);
    }
    std::time::Duration::from_secs_f64(milliseconds / 1000.0)
}

/// Node 对负延迟只发一次 `TimeoutNegativeWarning`（进程级去重），见 v26
/// `lib/internal/timers.js`；溢出警告则每次调用都发。
static NEGATIVE_TIMEOUT_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);
/// Node 只在进程的第一条警告后追加 `(Use node --trace-warnings ...)` 提示行。
static TIMEOUT_WARNING_HINT_PRINTED: AtomicBool = AtomicBool::new(false);

/// 复刻 Node `setTimeout` 对越界延迟的 stderr 警告（`process.emitWarning` 的默认文本）。
///
/// 负数 -> `TimeoutNegativeWarning`（每个进程仅第一条）；`> 2^31-1`（含 `Infinity`）
/// -> `TimeoutOverflowWarning`（每条都发）；`0 < delay < 1` 静默钳到 1ms。
/// 调用点已经保证 `delay` 是 JS 真值（非 0、非 `NaN`）。Node 的 `--trace-warnings`
/// 会额外打印 V8 调用栈，Rust 进程没有等价的 JS 栈，这里只输出默认三行。
fn warn_timeout_delay(milliseconds: f64) {
    if std::env::var_os("NODE_NO_WARNINGS").is_some_and(|value| value == "1") {
        return;
    }
    const TIMEOUT_MAX: f64 = 2_147_483_647.0;
    let (name, message) = if milliseconds > TIMEOUT_MAX {
        (
            "TimeoutOverflowWarning",
            format!(
                "{} does not fit into a 32-bit signed integer.",
                js_number_string(milliseconds)
            ),
        )
    } else if milliseconds < 0.0 {
        if NEGATIVE_TIMEOUT_WARNING_EMITTED.swap(true, Ordering::SeqCst) {
            return;
        }
        (
            "TimeoutNegativeWarning",
            format!("{} is a negative number.", js_number_string(milliseconds)),
        )
    } else {
        return;
    };
    let pid = std::process::id();
    if !TIMEOUT_WARNING_HINT_PRINTED.swap(true, Ordering::SeqCst) {
        eprintln!(
            "(node:{pid}) {name}: {message}\nTimeout duration was set to 1.\n(Use `node --trace-warnings ...` to show where the warning was created)"
        );
    } else {
        eprintln!("(node:{pid}) {name}: {message}\nTimeout duration was set to 1.");
    }
}

/// `String(number)` 的 JS 文本（`format_js_number` 把非有限值写成 `null`）。
fn js_number_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        kiana_json::js_num::format_js_number(value)
    }
}

impl ExtensionUiContext for RpcUiContext {
    fn select(
        &self,
        title: &str,
        options: &[String],
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        let title = title.to_string();
        let options = options.to_vec();
        let timeout = opts.as_ref().and_then(|opts| opts.timeout);
        Box::pin(async move {
            self.dialog(
                opts,
                None,
                |id| RpcExtensionUIRequest::select(id, title, options, timeout),
                parse_string_response,
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
        let title = title.to_string();
        let message = message.to_string();
        let timeout = opts.as_ref().and_then(|opts| opts.timeout);
        Box::pin(async move {
            self.dialog(
                opts,
                false,
                |id| RpcExtensionUIRequest::confirm(id, title, message, timeout),
                parse_confirm_response,
            )
            .await
        })
    }

    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>> {
        let title = title.to_string();
        let placeholder = placeholder.map(str::to_owned);
        let timeout = opts.as_ref().and_then(|opts| opts.timeout);
        Box::pin(async move {
            self.dialog(
                opts,
                None,
                |id| RpcExtensionUIRequest::input(id, title, placeholder, timeout),
                parse_string_response,
            )
            .await
        })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        self.send(&RpcExtensionUIRequest::notify(
            Uuid::new_v4().to_string(),
            message,
            kind.map(notify_type_value),
        ));
    }

    fn on_terminal_input(&self, _handler: kiana_ext_api::ui::TerminalInputHandler) -> Unsubscribe {
        Box::new(|| {})
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.send(&RpcExtensionUIRequest::set_status(
            Uuid::new_v4().to_string(),
            key,
            text.map(str::to_owned),
        ));
    }

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<kiana_ext_api::ui::WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    ) {
        // 组件工厂不发出任何记录（RPC 没有 TUI）。
        let lines = match content {
            None => None,
            Some(WidgetContent::Lines(lines)) => Some(lines),
            Some(WidgetContent::Component(_)) => return,
        };
        let placement = options
            .and_then(|options| options.placement)
            .map(widget_placement_value);
        self.send(&RpcExtensionUIRequest::set_widget(
            Uuid::new_v4().to_string(),
            key,
            lines,
            placement,
        ));
    }

    fn set_footer(&self, _factory: Option<kiana_ext_api::ui::FooterFactory>) {}

    fn set_header(&self, _factory: Option<kiana_ext_api::ui::HeaderFactory>) {}

    fn set_title(&self, title: &str) {
        self.send(&RpcExtensionUIRequest::set_title(
            Uuid::new_v4().to_string(),
            title,
        ));
    }

    fn custom(
        &self,
        _factory: kiana_ext_api::ui::CustomFactory,
        _options: Option<kiana_ext_api::ui::CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn Any + Send>> {
        // TS: `async () => undefined as never`。
        Box::pin(async { Box::new(()) as Box<dyn Any + Send> })
    }

    fn paste_to_editor(&self, text: &str) {
        self.set_editor_text(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.send(&RpcExtensionUIRequest::set_editor_text(
            Uuid::new_v4().to_string(),
            text,
        ));
    }

    fn get_editor_text(&self) -> String {
        String::new()
    }

    fn editor(&self, title: &str, prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>> {
        let title = title.to_string();
        let prefill = prefill.map(str::to_owned);
        Box::pin(async move { self.editor_dialog(&title, prefill.as_deref()).await })
    }

    fn add_autocomplete_provider(&self, _factory: kiana_ext_api::ui::AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<kiana_ext_api::ui::EditorFactory>) {}

    fn get_editor_component(&self) -> Option<kiana_ext_api::ui::EditorFactory> {
        None
    }

    fn theme(&self) -> &kiana_theme::theme::Theme {
        global_theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<kiana_theme::theme::Theme> {
        None
    }

    fn set_theme(&self, _theme: ThemeOrName) -> ThemeResult {
        ThemeResult {
            success: false,
            error: Some("Theme switching not supported in RPC mode".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

/// 当前全局主题（与 `kiana_extensions::runner::noop_ui::global_theme` 同法：每次访问重新读取
/// [`kiana_theme::theme::current_theme`]，未初始化时回退内置 `dark`，并用进程级历史表保证
/// `&'static Theme` 不悬空）。
///
/// 已知跨 crate 缺口（spec §8.4 第 12 项）：TS 的 `theme` 是 Proxy，未 `initTheme()` 时读取
/// 任意属性抛 `Theme not initialized. Call initTheme() first.`。冻结的
/// [`ExtensionUiContext::theme`] 返回 `&Theme`，没有错误通道（无法表达该异常）；而且扩展
/// 实际拿到的是 `kiana-extensions` 的 JS 桥（`ui::theme_value_from_ref` → `live_theme_value`），
/// 它忽略本方法的返回引用、同样回退内置 `dark`。要复刻该错误需要同时修改冻结的
/// kiana-ext-api 签名与 kiana-extensions 的桥接层，不在本单元的可改范围内。
fn global_theme() -> &'static kiana_theme::theme::Theme {
    use std::sync::LazyLock;

    static FALLBACK_THEME: LazyLock<Arc<kiana_theme::theme::Theme>> = LazyLock::new(|| {
        kiana_theme::theme::get_theme_by_name("dark")
            .expect("the builtin dark theme must always be available")
    });
    static HISTORY: LazyLock<Mutex<Vec<Arc<kiana_theme::theme::Theme>>>> =
        LazyLock::new(|| Mutex::new(Vec::new()));
    static CURRENT: LazyLock<Mutex<Option<Arc<kiana_theme::theme::Theme>>>> =
        LazyLock::new(|| Mutex::new(None));

    let theme = match kiana_theme::theme::current_theme() {
        Some(theme) => theme,
        None => Arc::clone(&FALLBACK_THEME),
    };
    let mut current = lock(&CURRENT);
    let same = current
        .as_ref()
        .is_some_and(|existing| Arc::ptr_eq(existing, &theme));
    if !same {
        *current = Some(Arc::clone(&theme));
        lock(&HISTORY).push(theme);
    }
    let pointer = Arc::as_ptr(current.as_ref().expect("theme just stored"));
    // SAFETY: 该 Arc 同时保存在 HISTORY（进程级、从不清理）里，因此 Theme 活到进程结束。
    unsafe { &*pointer }
}

// ============================================================================
// 生产适配器（AgentSessionRuntime -> seams）
// ============================================================================

thread_local! {
    /// 线程本地的 runtime 注册表（`AgentSessionRuntime` 是 `!Send`）。
    static RUNTIME_SLOTS: RefCell<HashMap<u64, Rc<AgentSessionRuntime>>> =
        RefCell::new(HashMap::new());
}

static NEXT_RUNTIME_SLOT: AtomicU64 = AtomicU64::new(1);

/// 生产 [`RpcSessionHost`]：把 `AgentSessionRuntime` 装进线程本地槽位，只保留句柄。
struct RuntimeHostAdapter {
    slot: u64,
    /// 槽位不可用时的兜底会话（正常路径不会用到）。
    fallback: AgentSession,
}

impl RuntimeHostAdapter {
    /// 安装 runtime 并返回适配器。
    fn install(runtime: AgentSessionRuntime) -> Arc<dyn RpcSessionHost> {
        let fallback = runtime.session();
        let slot = NEXT_RUNTIME_SLOT.fetch_add(1, Ordering::SeqCst);
        RUNTIME_SLOTS.with(|slots| {
            slots.borrow_mut().insert(slot, Rc::new(runtime));
        });
        Arc::new(Self { slot, fallback })
    }

    fn runtime(&self) -> Option<Rc<AgentSessionRuntime>> {
        RUNTIME_SLOTS.with(|slots| slots.borrow().get(&self.slot).cloned())
    }
}

impl Drop for RuntimeHostAdapter {
    fn drop(&mut self) {
        let _ = RUNTIME_SLOTS.try_with(|slots| {
            slots.borrow_mut().remove(&self.slot);
        });
    }
}

/// 生产 [`AgentSessionFacade`]：委托给真实的 [`AgentSession`]。
struct RuntimeSessionFacade {
    session: AgentSession,
}

impl AgentSessionFacade for RuntimeSessionFacade {
    fn agent_session(&self) -> AgentSession {
        self.session.clone()
    }

    fn agent(&self) -> Option<kiana_agent_core::Agent> {
        Some(self.session.agent())
    }
}

#[async_trait(?Send)]
impl RpcSessionHost for RuntimeHostAdapter {
    fn session(&self) -> Arc<dyn AgentSessionFacade> {
        let session = match self.runtime() {
            Some(runtime) => runtime.session(),
            None => self.fallback.clone(),
        };
        Arc::new(RuntimeSessionFacade { session })
    }

    fn set_rebind_session(
        &self,
        callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    ) {
        if let Some(runtime) = self.runtime() {
            // runtime 的回调带 `AgentSession` 参数；本文件总是重新读 `host.session()`。
            runtime.set_rebind_session(Some(Arc::new(move |_session| callback())));
        }
    }

    async fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let runtime = self
            .runtime()
            .ok_or_else(|| "RPC host runtime is not available".to_string())?;
        let options = options.map(|options| NewSessionOptionsInput {
            parent_session: options.parent_session,
            setup: options.setup,
            with_session: options.with_session.map(|callback| {
                let callback: ReplacedSessionCallback = callback;
                callback
            }),
        });
        runtime
            .new_session(options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn switch_session(
        &self,
        session_path: String,
        options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let runtime = self
            .runtime()
            .ok_or_else(|| "RPC host runtime is not available".to_string())?;
        let options = options.map(|options| RuntimeSwitchOptions {
            cwd_override: None,
            with_session: options.with_session.map(|callback| {
                let callback: ReplacedSessionCallback = callback;
                callback
            }),
            project_trust_context_factory: None,
        });
        runtime
            .switch_session(&session_path, options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        let runtime = self
            .runtime()
            .ok_or_else(|| "RPC host runtime is not available".to_string())?;
        let options = options.map(|options| ForkSessionOptions {
            position: options.position,
            with_session: options.with_session.map(|callback| {
                let callback: ReplacedSessionCallback = callback;
                callback
            }),
        });
        runtime
            .fork(&entry_id, options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn dispose(&self) {
        if let Some(runtime) = self.runtime() {
            runtime.dispose().await;
        }
    }
}

// ============================================================================
// 入口
// ============================================================================

/// `runRpcMode(runtimeHost)`：用真实的 `AgentSessionRuntime` 运行 RPC 模式。
///
/// 返回进程退出码（EOF/扩展关停 0，`SIGTERM` 143，`SIGHUP` 129）。调用前 `io.take_over()`
/// 接管 stdout——普通输出改道 stderr，协议记录独占真实 stdout；返回后接管状态**不**回退
/// （`app.rs` 的 RPC 分支依赖这一点，进程很快退出）。必须在 current-thread 运行时的
/// `LocalSet` 上执行（`AgentSessionRuntime` 与扩展派发都是 `!Send`）。
pub async fn run_rpc_mode(runtime: AgentSessionRuntime, io: Arc<dyn RawStdout>) -> i32 {
    io.take_over();
    let host = RuntimeHostAdapter::install(runtime);
    RpcServer::new(host, io).run().await
}

/// 用宿主 seam 运行 RPC 模式（测试与嵌入式调用方）。
///
/// 与 [`run_rpc_mode`] 的唯一区别是宿主由调用方提供（例如测试中的 `MockHost` 与记录型
/// `RawStdout`，绕过线程本地 runtime 槽位与进程级 stdout），`io.take_over()` 仍会执行。
/// 退出码语义相同；同样必须在 `LocalSet` 上执行。
pub async fn run_rpc_mode_with_host(host: Arc<dyn RpcSessionHost>, io: Arc<dyn RawStdout>) -> i32 {
    io.take_over();
    RpcServer::new(host, io).run().await
}

// ============================================================================
// 值工具
// ============================================================================

/// 解锁时统一忽略 poison（锁中毒）：本文件的锁只保护简单槽位，持锁期间 panic 后
/// 数据本身没有未定义行为（Rust 的 Mutex 只是标记 poison，值仍在），这里选择恢复继续
/// 服务，而不是让之后的每次访问都 panic。调用写法 `lock(&x)` 不绑定守卫，借用结束
/// 即释放。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 重入关停：记录第一个重入调用者的退出码并唤醒所有等待者（TS 的 `process.exit`）。
///
/// `compare_exchange` 只让第一个重入者留下退出码；每次都 `send_replace` 唤醒等待者，
/// 即使退出码已被占用——等待者总是能立刻看到 `Some(_)` 并返回。
fn force_exit(server: &RpcServer, code: i32) {
    server
        .force_exit_code
        .compare_exchange(NO_FORCE_EXIT, code, Ordering::SeqCst, Ordering::SeqCst)
        .ok();
    server.shutdown_exit.send_replace(Some(code));
}

/// 单条记录的统一出口：经 [`serialize_wire_line`]（遵守 `ARCHITECTURE.md` §4.3 的写出
/// 纪律），序列化失败只写 stderr，循环继续。
fn write_record<T: Serialize>(io: &Arc<dyn RawStdout>, record: &T) {
    match serialize_wire_line(record) {
        Ok(line) => io.write_raw(&line),
        Err(error) => eprintln!("{error}"),
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// 在 64 MiB 栈的线程上构造会话数据、重排键序并序列化成一整行响应。
///
/// `serde_json::to_value`、`reorder_wire_keys` 与 `Value::drop` 都是递归的；模型可控
/// 的深嵌套（长会话的 tree 每个条目约两层）会让默认 8 MiB 栈溢出。这里把**构造、
/// 重排、序列化和回收**全部放在大栈线程上，主线程只拿到一行浅字符串，因此不再需要
/// 人为的深度上限。
///
/// 返回的 [`RpcResponse`] 带 `raw_line`，由 [`RpcServer::handle_input_line`] 直接写出。
///
/// 异步等待大栈线程的结果（`oneshot`），而不是 `JoinHandle::join()`：调用方是
/// current-thread runtime 时，同步 join 会把整个 runtime（包括会话事件与其它 RPC 命令）
/// 阻塞到序列化结束。
async fn value_line_on_large_stack<F>(
    id: Option<Value>,
    command: &'static str,
    build_data: F,
) -> Result<RpcResponse, String>
where
    F: FnOnce() -> Result<Value, String> + Send + 'static,
{
    const STACK_SIZE: usize = 64 * 1024 * 1024;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let result = (|| {
                let mut data = build_data()?;
                crate::json_event::reorder_wire_keys(&mut data);
                let mut response = Map::new();
                if let Some(id) = id {
                    response.insert("id".to_string(), id);
                }
                response.insert("type".to_string(), Value::String("response".to_string()));
                response.insert("command".to_string(), Value::String(command.to_string()));
                response.insert("success".to_string(), Value::Bool(true));
                response.insert("data".to_string(), data);
                Ok::<String, String>(crate::jsonl::serialize_json_line(&Value::Object(response)))
            })();
            let _ = sender.send(result);
        })
        .map_err(|error| error.to_string())?;
    receiver
        .await
        .map_err(|_| "session value serialization thread panicked".to_string())?
        .map(RpcResponse::success_raw_line)
}

/// `command.<key>`（非对象命令没有字段）。
fn command_field<'a>(command: &'a Value, key: &str) -> Option<&'a Value> {
    command.as_object().and_then(|map| map.get(key))
}

/// JS 模板字符串里的 `String(value)`（`undefined` 用 `None`）。
fn js_string(value: Option<&Value>) -> String {
    kiana_json::js_string::js_string(value)
}

/// JS 真值判断（`undefined`/`null` 为假）。
fn is_truthy(value: Option<&Value>) -> bool {
    kiana_json::js_string::is_truthy(value)
}

/// `set_auto_compaction` / `set_auto_retry` 的 `enabled`：缺失或 `null` 时 TS 把
/// `undefined`/`null` 写进 settings，getter 的 `?? 默认值` 落回 `true`（settings-defaults
/// 的 `compaction.enabled` / `retry.enabled` 都是 true）。
///
/// 已知跨 crate 缺口：TS 的 `SettingsManager.setCompactionEnabled` 原样保存非布尔值，
/// `get_state.autoCompactionEnabled` 会回显 `0`/`"false"`（由 [`RawSessionSettings`] 复刻）；
/// 但 Rust 的 settings 只能保存 `bool`，因此这些畸形值在**写盘后**（新进程）会退化成
/// 真值对应的布尔（需要 kiana-config/kiana-session 改成可保存原始 JSON 值）。
fn enabled_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(value) => is_truthy(Some(value)),
    }
}

/// V8 在非字符串上调用方法时抛出的 TypeError 文本（`text.startsWith` /
/// `command.name.trim`）。JSON 字符串是原始值，方法调用正常；其余值没有该方法。
fn text_field_type_error(value: Option<&Value>, receiver: &str, method: &str) -> String {
    match value {
        None => format!("Cannot read properties of undefined (reading '{method}')"),
        Some(Value::Null) => format!("Cannot read properties of null (reading '{method}')"),
        Some(_) => format!("{receiver}.{method} is not a function"),
    }
}

/// 取字符串字段；缺失/`null` 按 TS 的字段访问抛 TypeError，其它非字符串值按 TS 的
/// 方法调用抛 `{receiver}.{method} is not a function`（**不**做 `String(value)` 强转）。
fn required_text_field(
    command: &Value,
    key: &str,
    receiver: &str,
    method: &str,
) -> Result<String, String> {
    match command_field(command, key) {
        Some(Value::String(text)) => Ok(text.clone()),
        other => Err(text_field_type_error(other, receiver, method)),
    }
}

/// 真值字段的 `String(value)`：falsy（缺失、`null`、`false`、`0`、`""`）返回 `None`，
/// 其余按 JS `String(v)` 强转（TS 里这些值会经模板字符串进入提示词）。
fn truthy_js_string(value: Option<&Value>) -> Option<String> {
    value
        .filter(|value| is_truthy(Some(value)))
        .map(|value| js_string(Some(value)))
}

/// Node `Buffer.from(value, "base64")` 对不可接受首参抛出的 TypeError 文本。
const BUFFER_FROM_PREFIX: &str = "The first argument must be of type string or an instance of \
Buffer, ArrayBuffer, or Array or an Array-like Object. ";

/// `Buffer.from(<data>, "base64")` 的 TypeError；`None` = 属性缺失（JS `undefined`）。
///
/// 返回 `None` 表示该值可被 `Buffer.from` 接受（字符串按 base64 解码、数组按字节截断），
/// 调用方继续执行 `processImage`（`mimeType.split(...)`）那一步。
fn buffer_argument_error(data: Option<&Value>) -> Option<String> {
    let suffix = match data {
        None => "Received undefined".to_string(),
        Some(Value::Null) => "Received null".to_string(),
        Some(Value::Bool(value)) => format!("Received type boolean ({value})"),
        Some(Value::Number(number)) => {
            // Node 的错误文本对 -0 保留负号（`String(-0)` 会丢掉）。
            let text = match number.as_f64() {
                Some(value) if value == 0.0 && value.is_sign_negative() => "-0".to_string(),
                Some(value) => js_number_string(value),
                None => number.to_string(),
            };
            format!("Received type number ({text})")
        }
        Some(Value::Object(_)) => "Received an instance of Object".to_string(),
        // 字符串与数组都是 `Buffer.from` 的合法输入。
        Some(Value::String(_)) | Some(Value::Array(_)) => return None,
    };
    Some(format!("{BUFFER_FROM_PREFIX}{suffix}"))
}

/// `_normalizePromptImages(images)`：把 RPC 的原始 `images` 转成 [`ImageContent`]，
/// 并在 TS 会抛错的地方返回**逐字**的 `Error.message`（由调用方通过错误响应写出）。
///
/// TS 的循环是 `for (const image of images) { processImage(Buffer.from(image.data, "base64"),
/// image.mimeType, ...) }`：
///
/// - falsy（`undefined`/`null`/`false`/`0`/`""`）-> 空数组；
/// - 真值但不可迭代（对象、非零数字、`true`）-> `images is not iterable`（V8 用变量名）；
/// - 非空字符串可迭代，但每个元素是单字符串、`.data` 为 `undefined` -> Buffer TypeError；
/// - 元素为 `null` 时 `.data` 访问先抛 `Cannot read properties of null (reading 'data')`；
/// - 元素 `data` 不是字符串/数组 -> Buffer TypeError；`data` 是字符串后 `mimeType` 必须
///   是字符串，否则 `baseMimeType()` 的 `.split` 抛 TypeError。
///
/// `data` 是数组（`Buffer.from(array)` 合法）时 TS 会把它当字节序列处理；Rust 的
/// [`ImageContent::data`] 只能是 base64 字符串，无法承载，故跳过该元素（TS 侧通常落到
/// “Image omitted” 提示）。`type` 字段 TS 从不校验，这里同样忽略并统一写成 `"image"`。
fn prompt_images_from(command: &Value) -> Result<Option<Vec<ImageContent>>, String> {
    let Some(images) = command_field(command, "images") else {
        return Ok(None);
    };
    if !is_truthy(Some(images)) {
        return Ok(None);
    }
    match images {
        Value::Array(items) => {
            let mut normalized = Vec::new();
            for item in items {
                if let Some(image) = normalize_prompt_image(item)? {
                    normalized.push(image);
                }
            }
            Ok(Some(normalized))
        }
        // 非空字符串会被逐码点迭代；首个字符的 `.data` 是 undefined。
        Value::String(_) => Err(buffer_argument_error(None).expect("undefined is rejected")),
        _ => Err("images is not iterable".to_string()),
    }
}

/// `_normalizePromptImages` 循环体里的单个元素。
fn normalize_prompt_image(item: &Value) -> Result<Option<ImageContent>, String> {
    let Some(object) = item.as_object() else {
        if item.is_null() {
            return Err("Cannot read properties of null (reading 'data')".to_string());
        }
        return Err(buffer_argument_error(None).expect("undefined is rejected"));
    };
    match object.get("data") {
        Some(Value::String(_)) | Some(Value::Array(_)) => {}
        other => return Err(buffer_argument_error(other).expect("value is rejected")),
    }
    match object.get("mimeType") {
        Some(Value::String(_)) => {}
        None => return Err("Cannot read properties of undefined (reading 'split')".to_string()),
        Some(Value::Null) => {
            return Err("Cannot read properties of null (reading 'split')".to_string());
        }
        Some(_) => return Err("mimeType.split is not a function".to_string()),
    }
    match (object.get("data"), object.get("mimeType")) {
        (Some(Value::String(data)), Some(Value::String(mime_type))) => Ok(Some(ImageContent {
            kind_image: ImageKind::Image,
            data: data.clone(),
            mime_type: mime_type.clone(),
        })),
        // `data` 是数组：Buffer 接受，但 Rust 的 base64 字符串无法承载。
        _ => Ok(None),
    }
}

/// `steer` / `follow_up` 的 `images`：TS 只做 `content.push(...images)`，不校验元素。
///
/// - falsy -> `None`；
/// - 真值但不可迭代 -> `Spread syntax requires ...iterable[Symbol.iterator] to be a function`；
/// - 数组 -> 保留能表示为 [`ImageContent`] 的元素（TS 把原始元素塞进消息 content，
///   非图片元素无法用 Rust 的消息类型承载）；
/// - 字符串可迭代（逐字符），Rust 无法承载这些字符，按无图片处理。
fn queued_images_from(command: &Value) -> Result<Option<Vec<ImageContent>>, String> {
    let Some(images) = command_field(command, "images") else {
        return Ok(None);
    };
    if !is_truthy(Some(images)) {
        return Ok(None);
    }
    match images {
        Value::Array(items) => Ok(Some(
            items
                .iter()
                .filter_map(|item| {
                    let object = item.as_object()?;
                    let data = object.get("data")?.as_str()?;
                    let mime_type = object.get("mimeType")?.as_str()?;
                    Some(ImageContent {
                        kind_image: ImageKind::Image,
                        data: data.to_string(),
                        mime_type: mime_type.to_string(),
                    })
                })
                .collect(),
        )),
        Value::String(_) => Ok(None),
        _ => {
            Err("Spread syntax requires ...iterable[Symbol.iterator] to be a function".to_string())
        }
    }
}

/// `streamingBehavior`：falsy -> `None`；`"followUp"` -> follow-up；其它真值 -> steer。
fn streaming_behavior_from(command: &Value) -> Option<StreamingBehavior> {
    match command_field(command, "streamingBehavior") {
        Some(Value::String(value)) if value == "followUp" => Some(StreamingBehavior::FollowUp),
        Some(value) if is_truthy(Some(value)) => Some(StreamingBehavior::Steer),
        _ => None,
    }
}

/// `ThinkingLevel` 字段的解析：未知/缺失/非字符串 -> `Off`（TS 的 clamp 落到第一档）。
fn parse_thinking_level(value: Option<&Value>) -> ModelThinkingLevel {
    match value.and_then(Value::as_str) {
        Some("minimal") => ModelThinkingLevel::Minimal,
        Some("low") => ModelThinkingLevel::Low,
        Some("medium") => ModelThinkingLevel::Medium,
        Some("high") => ModelThinkingLevel::High,
        Some("xhigh") => ModelThinkingLevel::Xhigh,
        Some("max") => ModelThinkingLevel::Max,
        _ => ModelThinkingLevel::Off,
    }
}

/// `ThinkingLevel` 的 wire 文本；序列化意外失败时回退 `"off"`（防御性兜底，
/// `ModelThinkingLevel` 的 serde 实现实际总是产出字符串）。
fn thinking_level_text(level: ModelThinkingLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "off".to_string())
}

/// TS 的队列语义只看 `mode === "all"`（`PendingMessageQueue.peek()`），其它值——包括
/// `undefined`、`null`、数字、对象——都按 `"one-at-a-time"` 处理。
///
/// 已知跨 crate 缺口：TS 把原始值存进 agent 与 settings，`get_state` 会回显原值；
/// Rust 的 [`QueueMode`] 枚举无法表示未知值，`get_state` 的回显由 [`RawSessionSettings`]
/// 单独保存（持久化到 settings 文件仍是两档，跨进程后丢失畸形值）。
fn js_queue_mode(value: Option<&Value>) -> QueueMode {
    match value {
        Some(Value::String(mode)) if mode == "all" => QueueMode::All,
        _ => QueueMode::OneAtATime,
    }
}

fn queue_mode_text(mode: QueueMode) -> Value {
    match mode {
        QueueMode::All => Value::String("all".to_string()),
        QueueMode::OneAtATime => Value::String("one-at-a-time".to_string()),
    }
}

fn string_array(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn disposition_data(disposition: PromptDisposition) -> Value {
    let text = match disposition {
        PromptDisposition::Handled => "handled",
        PromptDisposition::Queued => "queued",
        PromptDisposition::Started => "started",
    };
    let mut data = Map::new();
    data.insert("disposition".to_string(), Value::String(text.to_string()));
    Value::Object(data)
}

fn queued_disposition_data(disposition: QueuedInputDisposition) -> Value {
    let text = match disposition {
        QueuedInputDisposition::Handled => "handled",
        QueuedInputDisposition::Queued => "queued",
    };
    let mut data = Map::new();
    data.insert("disposition".to_string(), Value::String(text.to_string()));
    Value::Object(data)
}

fn cancelled_data(cancelled: bool) -> Value {
    let mut data = Map::new();
    data.insert("cancelled".to_string(), Value::Bool(cancelled));
    Value::Object(data)
}

/// `ModelCycleResult` 不是 `Serialize`：按 `{ model, thinkingLevel, isScoped }` 手工构造。
fn model_cycle_data(result: &ModelCycleResult) -> Result<Value, String> {
    let mut data = Map::new();
    data.insert("model".to_string(), to_value(&result.model)?);
    data.insert(
        "thinkingLevel".to_string(),
        Value::String(thinking_level_text(result.thinking_level)),
    );
    data.insert("isScoped".to_string(), Value::Bool(result.is_scoped));
    Ok(Value::Object(data))
}

/// 成功响应（`{id?, type, command, success:true, data?}`）。
fn success_response(id: Option<Value>, command: &str, data: Option<Value>) -> RpcResponse {
    RpcResponse::success(id, Some(Value::String(command.to_string())), data)
}

/// 失败响应（`{id?, type, command?, success:false, error}`）。
fn error_response(id: Option<Value>, command: Option<Value>, error: Option<String>) -> RpcResponse {
    RpcResponse {
        id,
        kind: crate::rpc_types::RpcResponseType::Response,
        command,
        success: false,
        data: None,
        error,
        raw_line: None,
    }
}

/// `extension_error` 记录（键序 `type`、`extensionPath`、`event`、`error`；无 `stack`）。
fn extension_error_value(error: &ExtensionError) -> Value {
    let mut value = Map::new();
    value.insert(
        "type".to_string(),
        Value::String("extension_error".to_string()),
    );
    value.insert(
        "extensionPath".to_string(),
        Value::String(error.extension_path.clone()),
    );
    value.insert("event".to_string(), Value::String(error.event.clone()));
    value.insert("error".to_string(), Value::String(error.error.clone()));
    Value::Object(value)
}

/// `RpcSlashCommand` 的一项：`name`、`description?`、`source`、`sourceInfo`。
fn slash_command_value(
    name: &str,
    description: Option<&str>,
    source: &str,
    source_info: &kiana_core::source_info::SourceInfo,
) -> Result<Value, String> {
    let mut item = Map::new();
    item.insert("name".to_string(), Value::String(name.to_string()));
    if let Some(description) = description {
        item.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    item.insert("source".to_string(), Value::String(source.to_string()));
    item.insert("sourceInfo".to_string(), to_value(source_info)?);
    Ok(Value::Object(item))
}

/// `SessionTreeNode` 没有 `Serialize`：`{ entry, children, label?, labelTimestamp? }`（递归
/// 用显式栈，深树不会栈溢出）。
fn tree_nodes_value(nodes: &[SessionTreeNode]) -> Value {
    // 每棵树单独用一个栈：若把所有根一次性压栈，先完成的根会被挂到下一个根的
    // frame 下，多根/孤儿会话的 `get_tree` 就会返回一条错误的嵌套链。
    Value::Array(nodes.iter().map(tree_node_value).collect())
}

/// 单棵树的后序迭代转换（深度不受调用栈限制）。
fn tree_node_value(root: &SessionTreeNode) -> Value {
    struct Frame<'a> {
        node: &'a SessionTreeNode,
        next_child: usize,
        children: Vec<Value>,
    }

    fn node_value(node: &SessionTreeNode, children: Vec<Value>) -> Value {
        let mut value = Map::new();
        value.insert(
            "entry".to_string(),
            serde_json::to_value(&node.entry).unwrap_or(Value::Null),
        );
        value.insert("children".to_string(), Value::Array(children));
        if let Some(label) = &node.label {
            value.insert("label".to_string(), Value::String(label.clone()));
        }
        if let Some(label_timestamp) = &node.label_timestamp {
            value.insert(
                "labelTimestamp".to_string(),
                Value::String(label_timestamp.clone()),
            );
        }
        Value::Object(value)
    }

    let mut stack: Vec<Frame<'_>> = vec![Frame {
        node: root,
        next_child: 0,
        children: Vec::new(),
    }];
    while let Some(frame) = stack.last_mut() {
        if frame.next_child < frame.node.children.len() {
            let child = &frame.node.children[frame.next_child];
            frame.next_child += 1;
            stack.push(Frame {
                node: child,
                next_child: 0,
                children: Vec::new(),
            });
        } else {
            let frame = stack.pop().expect("frame just borrowed");
            let value = node_value(frame.node, frame.children);
            match stack.last_mut() {
                Some(parent) => parent.children.push(value),
                None => return value,
            }
        }
    }
    unreachable!("the stack starts with one root frame")
}

fn notify_type_value(kind: NotifyKind) -> RpcNotifyType {
    match kind {
        NotifyKind::Info => RpcNotifyType::Info,
        NotifyKind::Warning => RpcNotifyType::Warning,
        NotifyKind::Error => RpcNotifyType::Error,
    }
}

fn widget_placement_value(placement: WidgetPlacement) -> RpcWidgetPlacement {
    match placement {
        WidgetPlacement::AboveEditor => RpcWidgetPlacement::AboveEditor,
        WidgetPlacement::BelowEditor => RpcWidgetPlacement::BelowEditor,
    }
}

/// 供测试断言错误文本时使用（与 TS `String(value)` 一致）。
#[doc(hidden)]
pub fn js_value_string(value: Option<&Value>) -> String {
    js_string(value)
}

#[cfg(test)]
mod session_value_depth_tests {
    use super::value_line_on_large_stack;
    use serde_json::Value;

    #[tokio::test]
    async fn very_deep_values_are_built_reordered_and_dropped_on_the_large_stack() {
        let mut value = Value::Null;
        for _ in 0..20_000 {
            value = Value::Array(vec![value]);
        }
        // 构造、重排、序列化与递归 Drop 都在 64 MiB 栈线程上；主线程只拿到一行字符串。
        let response = value_line_on_large_stack(None, "get_messages", move || {
            let mut data = serde_json::Map::new();
            data.insert("messages".to_string(), value);
            Ok(Value::Object(data))
        })
        .await
        .expect("deep value must serialize");
        let line = response.raw_line.expect("raw line");
        assert!(line.starts_with("{\"type\":\"response\",\"command\":\"get_messages\""));
        assert!(line.contains("\"messages\":"));
        assert!(line.ends_with("}\n"));
    }

    #[tokio::test]
    async fn long_linear_trees_are_not_rejected_by_a_depth_cap() {
        // 每条 entry 产生 entry + children 两层；1024 条大约 2048 层，旧实现的
        // 1024 上限会误拒这类正常长会话。
        let mut node = Value::Object(serde_json::Map::new());
        for _ in 0..1024 {
            let mut object = serde_json::Map::new();
            object.insert("entry".to_string(), Value::Null);
            object.insert("children".to_string(), Value::Array(vec![node]));
            node = Value::Object(object);
        }
        let response = value_line_on_large_stack(None, "get_tree", move || {
            let mut data = serde_json::Map::new();
            data.insert("tree".to_string(), Value::Array(vec![node]));
            data.insert("leafId".to_string(), Value::Null);
            Ok(Value::Object(data))
        })
        .await
        .expect("long tree must serialize");
        assert!(response.raw_line.is_some());
    }
}
