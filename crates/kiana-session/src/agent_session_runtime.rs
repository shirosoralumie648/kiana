//! core/agent-session-runtime.ts：new/switch/fork/import/dispose。
//!
//! TS 源：`packages/coding-agent/src/core/agent-session-runtime.ts`（447 行）。规范：
//! `spec/ca-session.md` §2.4.1-§2.4.8。
//!
//! # 并发形态
//!
//! TS 的字段是可变的普通属性；Rust 用 [`std::sync::Mutex`] 持有可变状态，因此
//! [`AgentSessionRuntime`] 的方法都借用 `&self`（调用方可以放进 `Arc` 后共享给扩展命令）。
//! 锁只在极短的同步段持有，**绝不跨 `.await`**；异步方法先取快照（`AgentSession` 与
//! `AgentSessionServices` 都是廉价克隆），等待结束后再写回。
//!
//! # 职责与边界
//!
//! runtime 只回答一个问题：“当前会话是哪个、怎么换成另一个”。它不实现对话本身
//! （prompt/压缩/重试在 `AgentSession`，见 [`crate::agent_session`]），也不负责落盘
//! 格式（在 kiana-session-core）。四个替换操作 —— [`AgentSessionRuntime::switch_session`]、
//! [`AgentSessionRuntime::new_session`]、[`AgentSessionRuntime::fork`]、
//! [`AgentSessionRuntime::import_from_jsonl`] —— 都走同一条替换骨架：
//!
//! ```text
//! 1. emit session_before_switch / session_before_fork（扩展可取消；取消则直接返回，
//!    不做任何替换）
//! 2. 记录 previous_session_file，准备新的 SessionManager（open / create / in_memory /
//!    create_branched_session，取决于操作与当前会话是否持久化）
//! 3. teardown_current：结算旧会话并触发 session_shutdown（顺序见该函数注释）
//! 4. 调用 create_runtime 工厂重建会话与服务（附 session_start 事件）
//! 5. apply：把新结果写回 Mutex 后的状态
//! 6. finish_session_replacement：依次 await rebind_session 与 with_session 回调
//! ```
//!
//! [`AgentSessionRuntime::dispose`] 是唯一的例外：只做 shutdown + invalidate +
//! dispose，不替换会话。
//!
//! # 宿主回调
//!
//! kiana-interactive 通过两者挂接 UI（前者在旧会话 dispose 前**同步**调用，宿主在此
//! 拆除旧 UI 状态）；kiana-rpc-print 只注册 `set_rebind_session`。后者在每次替换完成
//! 后带上新会话被 await（重新绑定订阅、设置等）。一次完整替换的回调顺序为
//! `session_shutdown → before_session_invalidate → …工厂重建… → rebind_session → with_session`。
//!
//! # 边界与失败模式
//!
//! - 所有可失败操作统一返回 [`AgentSessionRuntimeError`]；`Display` 文本逐字对齐 TS 的
//!   `error.message`，`tests/agent_session_runtime.rs` 用整串比较锁定。
//! - `session_before_switch` / `session_before_fork` 被取消时返回 `cancelled: true`，
//!   旧会话保持原样。但替换一旦进入 teardown 就不可回退：工厂失败时旧会话已 dispose、
//!   `apply` 未执行，getter 仍返回已失效的旧会话（与 TS 的执行顺序一致）。
//! - `import_from_jsonl` 把 `std::fs` 错误映射成 Node 风格错误文本（`node_fs_error`），
//!   并保留源文件权限；这些行为由同文件的 import_* 测试锁定。
//! - TS 用真值判断区分“没给”与“空串”（`parentSession`、`targetLeafId`、
//!   `currentSessionFile`），Rust 侧以 `filter(|s| !s.is_empty())` 复刻，调用方传
//!   空字符串等价于没传。

#![allow(clippy::result_large_err)]

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{Message, UserContent, UserContentBlock};
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventResult, ForkPosition, ProjectTrustContext,
    SessionBeforeForkEvent, SessionBeforeForkEventType, SessionBeforeSwitchEvent,
    SessionBeforeSwitchEventType, SessionEvent, SessionShutdownEvent, SessionShutdownEventType,
    SessionStartEvent, SessionStartEventType, SessionStartReason,
};
use kiana_ext_api::types::ReplacedSessionContextApi;
use kiana_extensions::runner::emit_session_shutdown_event;
use kiana_session_core::session_cwd::{
    MissingSessionCwdError, assert_session_cwd_exists as assert_session_cwd,
};
use kiana_session_core::session_manager::{
    NewSessionOptions, SessionEntry, SessionManager, SessionManagerError,
};

use crate::agent_session::AgentSession;

// ============================================================================
// 类型
// ============================================================================

/// 创建运行时并收集诊断后的结果。
///
/// TS: `agent-session-runtime.ts#CreateAgentSessionRuntimeResult`（extends
/// `CreateAgentSessionResult`；Rust 没有继承，字段平铺）。
///
/// 由 [`CreateAgentSessionRuntimeFactory`] 产出，作为一次会话替换的最终产物交给
/// [`AgentSessionRuntime`] 的 `apply` 写回状态。字段全部按值转移：`session` 与
/// `services` 是新会话的句柄（`Arc` 克隆，廉价），`diagnostics` 汇总本次扩展/资源
/// 加载的问题，`model_fallback_message` 是初始模型解析回退时的用户提示。
pub struct CreateAgentSessionRuntimeResult {
    /// 创建的会话。
    pub session: AgentSession,
    /// 加载的扩展。
    pub extensions_result: kiana_extensions::LoadExtensionsResult,
    /// 模型回退警告。
    pub model_fallback_message: Option<String>,
    /// cwd 绑定的服务。
    pub services: AgentSessionServices,
    /// 诊断。
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

/// 运行时工厂的输入。
///
/// TS: `agent-session-runtime.ts#CreateAgentSessionRuntimeFactory` 的参数对象。
///
/// 各次替换操作在调用工厂前组装：`session_manager` 是新会话的落盘管理器（所有权
/// 转移），`session_start_event` 携带替换原因（new/resume/fork）与被替换会话的文件，
/// `project_trust_context` 目前只有 `switch_session` 会从
/// `SwitchSessionOptions::project_trust_context_factory` 构造（fork/new/import 恒为
/// `None`）。
pub struct CreateAgentSessionRuntimeFactoryOptions {
    /// 目标 cwd。
    pub cwd: String,
    /// agent 配置目录。
    pub agent_dir: String,
    /// 会话管理器。
    pub session_manager: SessionManager,
    /// `session_start` 元数据。
    pub session_start_event: Option<SessionStartEvent>,
    /// 项目信任上下文。
    pub project_trust_context: Option<ProjectTrustContext>,
}

/// 创建完整运行时的工厂。
///
/// TS: `agent-session-runtime.ts#CreateAgentSessionRuntimeFactory`。future 不要求 `Send`
/// （扩展/资源加载在单线程 LocalSet 上运行）；闭包本身也不要求 `Send`，与 TS 的普通函数一致。
///
/// runtime 在 `new` 时持有这个工厂的 `Arc` 副本，之后每次替换操作（switch/new/fork/
/// import）都通过它重建整套会话与服务——宿主（如 kiana-coding-agent）传入的通常就是
/// `create_agent_session_from_services` 的包装。用 `Arc<dyn Fn>` 而不是泛型参数，是为了
/// 让 [`AgentSessionRuntime`] 可以作为具体类型放进前端结构体。
pub type CreateAgentSessionRuntimeFactory = Arc<
    dyn Fn(
        CreateAgentSessionRuntimeFactoryOptions,
    ) -> kiana_ext_api::LocalBoxFuture<
        'static,
        Result<CreateAgentSessionRuntimeResult, AgentSessionRuntimeError>,
    >,
>;

/// `createAgentSessionRuntime(createRuntime, options)` 的选项。
///
/// 只用于初始创建（不发生"替换"），因此没有 `session_start_event` 之外的替换钩子；
/// 对比 [`CreateAgentSessionRuntimeFactoryOptions`]：这里的 `session_start_event`
/// 由调用方直接给出（首个会话没有 previous_session_file），`project_trust_context`
/// 不暴露——初始创建时传 `None`（见 [`create_agent_session_runtime`]）。
pub struct CreateAgentSessionRuntimeOptions {
    /// 目标 cwd。
    pub cwd: String,
    /// agent 配置目录。
    pub agent_dir: String,
    /// 会话管理器。
    pub session_manager: SessionManager,
    /// `session_start` 元数据。
    pub session_start_event: Option<SessionStartEvent>,
}

/// `/import` 引用的 JSONL 文件不存在。
///
/// TS: `agent-session-runtime.ts#SessionImportFileNotFoundError`
///
/// 在调用 `before_switch` 钩子**之前**就检查，因此扩展取消不了这个错误——TS 同样先
/// `existsSync` 再派发事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionImportFileNotFoundError {
    /// 解析后的文件路径（已按进程 cwd 解析为绝对路径）。
    pub file_path: String,
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "File not found: {}", self.file_path)
    }
}

impl std::error::Error for SessionImportFileNotFoundError {}

impl SessionImportFileNotFoundError {
    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "SessionImportFileNotFoundError"
    }
}

/// 运行时替换/导入的错误。`Display` 逐字等于 TS 的 `error.message`。
///
/// 变体与 TS 抛出点一一对应（见 `spec/ca-session.md` §2.4.3-§2.4.6 的伪代码）：
/// 文本是冻结契约：`tests/agent_session_runtime.rs` 用整串 `to_string()` 比较锁定
/// （如 `#reports_why_an_unflushed_session_cannot_be_forked`），前端只按 `Display`
/// 原样展示，不做整串判别。`Other` 变体专门用于透传 Node 风格的 `fs` 错误文本
/// （见 `node_fs_error`），保持与 TS `error.message` 的字节级一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSessionRuntimeError {
    /// `/import` 路径不存在。
    SessionImportFileNotFound(SessionImportFileNotFoundError),
    /// 会话文件记录的 cwd 不存在。
    MissingSessionCwd(MissingSessionCwdError),
    /// fork 的条目 id 无效。
    InvalidEntryIdForForking,
    /// 持久化会话缺少会话文件。
    PersistedSessionMissingSessionFile,
    /// 会话尚未保存。
    SessionNotSavedYet,
    /// 无法创建分支会话。
    FailedToCreateForkedSession,
    /// 会话管理器错误（`SessionManagerError` 的 `message`）。
    SessionManager(String),
    /// 其它透传错误。
    Other(String),
}

impl std::fmt::Display for AgentSessionRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentSessionRuntimeError::SessionImportFileNotFound(error) => {
                std::fmt::Display::fmt(error, formatter)
            }
            AgentSessionRuntimeError::MissingSessionCwd(error) => {
                std::fmt::Display::fmt(error, formatter)
            }
            AgentSessionRuntimeError::InvalidEntryIdForForking => {
                formatter.write_str("Invalid entry ID for forking")
            }
            AgentSessionRuntimeError::PersistedSessionMissingSessionFile => {
                formatter.write_str("Persisted session is missing a session file")
            }
            AgentSessionRuntimeError::SessionNotSavedYet => formatter.write_str(
                "This session has not been saved yet. Send a message before cloning or forking it.",
            ),
            AgentSessionRuntimeError::FailedToCreateForkedSession => {
                formatter.write_str("Failed to create forked session")
            }
            AgentSessionRuntimeError::SessionManager(message)
            | AgentSessionRuntimeError::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AgentSessionRuntimeError {}

impl From<SessionManagerError> for AgentSessionRuntimeError {
    fn from(error: SessionManagerError) -> Self {
        AgentSessionRuntimeError::SessionManager(error.to_string())
    }
}

impl From<SessionImportFileNotFoundError> for AgentSessionRuntimeError {
    fn from(error: SessionImportFileNotFoundError) -> Self {
        AgentSessionRuntimeError::SessionImportFileNotFound(error)
    }
}

/// `setRebindSession` 的回调。
///
/// 每次会话替换完成后带上**新**会话被 await（见 `finish_session_replacement`）。
/// 前端在这里重新订阅 agent 事件、更新设置句柄等；宿主必须通过
/// [`AgentSessionRuntime::set_rebind_session`] 注册，未注册则该步为空操作。
pub type RebindSessionFn = Arc<dyn Fn(AgentSession) -> kiana_ext_api::LocalBoxFuture<'static, ()>>;

/// `withSession` 的回调（切换后绑定到新会话的命令上下文）。
///
/// 对应扩展 API `withSession` 选项：每次替换完成后在 `rebind_session` 之后被 await，
/// 参数是 [`AgentSession::create_replaced_session_context`] 构造的受限上下文——扩展
/// 通过它拿到新会话的 ctx，而旧 ctx 已在 `session.dispose()` 中失效。
pub type ReplacedSessionCallback =
    Arc<dyn Fn(Arc<dyn ReplacedSessionContextApi>) -> kiana_ext_api::LocalBoxFuture<'static, ()>>;

/// `newSession({ setup })` 的回调。
///
/// TS 的 `setup` 是 `async (sessionManager) => void`，在 `refreshContext()` 之前被
/// **await**。这里直接采用扩展 API 的冻结形态 [`kiana_ext_api::types::SetupFn`]
/// （`Arc<Mutex<SessionManager>>` + `LocalBoxFuture`），宿主层可以把
/// `kiana_ext_api::types::NewSessionOptions` 的回调原样转发进来。
pub type SessionSetupFn = kiana_ext_api::types::SetupFn;

/// `switchSession` 的结果。
///
/// TS: `{ cancelled: boolean }`
///
/// `cancelled: true` 表示 `session_before_switch` 钩子把 `cancel` 置为了 `Some(true)`，
/// 此时什么都没替换；`cancelled: false` 表示替换已完整走完（包括回调）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionReplacementResult {
    /// 是否被 `session_before_switch` 取消。
    pub cancelled: bool,
}

/// `fork` 的结果。
///
/// TS: `{ cancelled: boolean; selectedText?: string }`
///
/// `selected_text` 只在 `position: "before"` 且未取消时有值：被 fork 的那条用户消息的
/// 文本（`extract_user_message_text` 提取），供前端预填输入框（如选中代码再 fork）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkSessionResult {
    /// 是否被 `session_before_fork` 取消。
    pub cancelled: bool,
    /// `position: "before"` 时被 fork 的用户消息文本。
    pub selected_text: Option<String>,
}

/// 每次会话替换时构造项目信任上下文。
pub type ProjectTrustContextFactory = Arc<dyn Fn(&str) -> ProjectTrustContext>;

/// `switchSession(sessionPath, options?)` 的选项。
#[derive(Default)]
pub struct SwitchSessionOptions {
    /// 覆盖会话记录的 cwd。转发给 `SessionManager::open` 的第三个参数；不提供时优先
    /// 沿用会话文件 header 里记录的 cwd（header 未记录字符串 cwd 时回退进程 cwd），
    /// 并经 `assert_session_cwd` 校验存在性。
    pub cwd_override: Option<String>,
    /// 替换完成后的回调。
    pub with_session: Option<ReplacedSessionCallback>,
    /// 每次替换时构造项目信任上下文。`fork`/`new`/`import` 不消费该工厂
    /// （`project_trust_context` 恒为 `None`），只有 `switch_session` 使用。
    pub project_trust_context_factory: Option<ProjectTrustContextFactory>,
}

/// `newSession(options?)` 的选项。
#[derive(Default)]
pub struct NewSessionOptionsInput {
    /// 父会话文件。空字符串按 TS 的 falsy 语义视为未提供（不调用
    /// `SessionManager::new_session`），见本方法内的过滤逻辑。
    pub parent_session: Option<String>,
    /// 新会话管理器的 setup。
    pub setup: Option<SessionSetupFn>,
    /// 替换完成后的回调。
    pub with_session: Option<ReplacedSessionCallback>,
}

/// `fork(entryId, options?)` 的选项。
#[derive(Default)]
pub struct ForkSessionOptions {
    /// fork 位置；缺省 `"before"`。`"before"` 表示从**该用户消息的父节点**分叉（该消息
    /// 进入 `selected_text`），`"at"` 表示直接以该条目为新叶——两种位置对条目类型的要求
    /// 不同，见 `AgentSessionRuntime::fork` 内的分派。
    pub position: Option<ForkPosition>,
    /// 替换完成后的回调。
    pub with_session: Option<ReplacedSessionCallback>,
}

// ============================================================================
// 状态
// ============================================================================

/// `AgentSessionRuntime` 的可变字段（TS 上的普通属性），整体在其 `state` 字段的
/// [`Mutex`](std::sync::Mutex) 后面。
///
/// 两个回调字段由宿主（前端）通过公开 setter 注册，其余四个字段只在 `apply` 时整体
/// 被新结果覆盖；不存在单独写某个字段的方法，替换是原子的（锁内一次赋值）。
struct RuntimeState {
    session: AgentSession,
    services: AgentSessionServices,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
    rebind_session: Option<RebindSessionFn>,
    before_session_invalidate: Option<Arc<dyn Fn()>>,
}

/// 拥有当前 `AgentSession` 及其 cwd 绑定服务。
///
/// TS: `agent-session-runtime.ts#AgentSessionRuntime`
///
/// 前端持有它的长期句柄（放进 `Rc`，在单线程 `LocalSet` 上共享），在 `/new`、`/resume`、
/// `/fork`、`/import` 等命令里调用对应的 `*_session` 方法完成替换。所有方法都借用 `&self`，并发安全性由
/// 内部的 [`Mutex`] 提供；`dispose` 之后 runtime 不再可用（旧会话已失效，getter 仍会
/// 返回其句柄），应随宿主一起丢弃。
///
/// 替换骨架（各操作共用）与失败模式见文件级 `//!` 的“职责与边界”“边界与失败模式”。
pub struct AgentSessionRuntime {
    state: Mutex<RuntimeState>,
    create_runtime: CreateAgentSessionRuntimeFactory,
}

impl AgentSessionRuntime {
    /// `new AgentSessionRuntime(session, services, createRuntime, diagnostics?, modelFallback?)`。
    ///
    /// 初始状态没有任何宿主回调（`rebind_session` / `before_session_invalidate` 为
    /// `None`）；公开入口 [`create_agent_session_runtime`] 会先校验 cwd 再调工厂、
    /// 最后进入这里。`create_runtime` 被保存以供后续每次替换重建会话。
    pub fn new(
        session: AgentSession,
        services: AgentSessionServices,
        create_runtime: CreateAgentSessionRuntimeFactory,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
    ) -> Self {
        AgentSessionRuntime {
            state: Mutex::new(RuntimeState {
                session,
                services,
                diagnostics,
                model_fallback_message,
                rebind_session: None,
                before_session_invalidate: None,
            }),
            create_runtime,
        }
    }

    // -- getter -------------------------------------------------------------
    //
    // 以下 getter 都取锁后克隆返回，保证与并发替换互不撕裂：调用方拿到的快照要么完全
    // 是旧会话的、要么完全是新会话的。`cwd` 等价于 `services().cwd`（TS 的
    // `get cwd()` 转发），服务随替换一起更新，因此替换后 cwd 跟随新会话。

    /// `get services()`。
    pub fn services(&self) -> AgentSessionServices {
        lock(&self.state).services.clone()
    }

    /// `get session()`。
    pub fn session(&self) -> AgentSession {
        lock(&self.state).session.clone()
    }

    /// `get cwd()`（= `services.cwd`）。
    pub fn cwd(&self) -> String {
        lock(&self.state).services.cwd.clone()
    }

    /// `get diagnostics()`。
    pub fn diagnostics(&self) -> Vec<AgentSessionRuntimeDiagnostic> {
        lock(&self.state).diagnostics.clone()
    }

    /// `get modelFallbackMessage()`。
    pub fn model_fallback_message(&self) -> Option<String> {
        lock(&self.state).model_fallback_message.clone()
    }

    /// `setRebindSession(fn?)`。
    ///
    /// 传 `None` 清除回调（宿主退出前的清理，见
    /// `tests/agent_session_runtime_events.rs`）。回调在替换完成后、`with_session`
    /// 之前被 await，应保持幂等——绑定期间会话可能再次被替换。
    pub fn set_rebind_session(&self, rebind_session: Option<RebindSessionFn>) {
        lock(&self.state).rebind_session = rebind_session;
    }

    /// `setBeforeSessionInvalidate(fn?)`。
    ///
    /// 传 `None` 清除回调。回调是同步 `Fn()`：在 `teardown_current` /
    /// `dispose` 里于旧会话失效前被调用（宿主在此拆除引用旧会话的 UI 状态），
    /// 之后旧 ctx 使用即 panic（stale 检测，见 `tests/agent_session_runtime_events.rs`）。
    pub fn set_before_session_invalidate(&self, callback: Option<Arc<dyn Fn()>>) {
        lock(&self.state).before_session_invalidate = callback;
    }

    // -- 内部 ---------------------------------------------------------------

    fn apply(&self, result: CreateAgentSessionRuntimeResult) {
        // 锁内整体覆盖，是替换流程中“切换到新会话”的唯一落点；此后 getter 返回新会话。
        let mut state = lock(&self.state);
        state.session = result.session;
        state.services = result.services;
        state.diagnostics = result.diagnostics;
        state.model_fallback_message = result.model_fallback_message;
    }

    /// `emitBeforeSwitch(reason, targetSessionFile?)`。
    ///
    /// 返回 `Ok(true)` 表示扩展请求取消替换（`cancel == Some(true)`）。没有注册
    /// `session_before_switch` 处理器时直接放行（`Ok(false)`），不构造事件对象；
    /// 钩子本身失败（emit 返回错误）则透传为 [`AgentSessionRuntimeError::Other`]。
    async fn emit_before_switch(
        &self,
        session: &AgentSession,
        reason: kiana_ext_api::events::SessionBeforeSwitchReason,
        target_session_file: Option<String>,
    ) -> Result<bool, AgentSessionRuntimeError> {
        let runner = session.extension_runner();
        if !runner.has_handlers("session_before_switch") {
            return Ok(false);
        }
        let result = runner
            .emit(ExtensionEvent::Session(SessionEvent::BeforeSwitch(
                SessionBeforeSwitchEvent {
                    event_type: SessionBeforeSwitchEventType::SessionBeforeSwitch,
                    reason,
                    target_session_file,
                },
            )))
            .await
            .map_err(|error| AgentSessionRuntimeError::Other(error.message().to_string()))?;
        Ok(matches!(
            result,
            Some(ExtensionEventResult::SessionBeforeSwitch(result)) if result.cancel == Some(true)
        ))
    }

    /// `emitBeforeFork(entryId, { position })`。
    ///
    /// 与 [`Self::emit_before_switch`] 同构：无处理器放行，`cancel == Some(true)` 视为
    /// 取消，emit 失败透传为 `Other`。取消时 `fork` 不会读取条目，因此无效 entry id 在
    /// 被扩展取消的调用里不会报 `InvalidEntryIdForForking`。
    async fn emit_before_fork(
        &self,
        session: &AgentSession,
        entry_id: &str,
        position: ForkPosition,
    ) -> Result<bool, AgentSessionRuntimeError> {
        let runner = session.extension_runner();
        if !runner.has_handlers("session_before_fork") {
            return Ok(false);
        }
        let result = runner
            .emit(ExtensionEvent::Session(SessionEvent::BeforeFork(
                SessionBeforeForkEvent {
                    event_type: SessionBeforeForkEventType::SessionBeforeFork,
                    entry_id: entry_id.to_string(),
                    position,
                },
            )))
            .await
            .map_err(|error| AgentSessionRuntimeError::Other(error.message().to_string()))?;
        Ok(matches!(
            result,
            Some(ExtensionEventResult::SessionBeforeFork(result)) if result.cancel == Some(true)
        ))
    }

    /// `teardownCurrent(reason, targetSessionFile?)`：顺序严格。
    ///
    /// 按 `spec/ca-session.md` §2.4.2 的固定顺序结算旧会话，每一步都依赖前一步：
    ///
    /// 1. `session.abort()` —— 等待进行中的响应落地：被中断的工具调用留下持久化的
    ///    `toolResult` 条目而不是悬空（`tests/agent_session_runtime.rs`
    ///    `settles_the_active_response_before_session_replacement` 锁定该行为）；
    /// 2. `session_shutdown` 事件 —— 扩展在旧会话仍可用时做最后清理；
    /// 3. `before_session_invalidate` —— 同步回调（宿主拆除旧 UI 状态），此刻旧 ctx
    ///    尚未失效；
    /// 4. `session.dispose()` —— 取消 agent/压缩/订阅，并使旧扩展 ctx 失效（之后使用
    ///    即 panic）。
    ///
    /// 最后释放 CacheWarmer 槽位：`dispose` 无法回调 sdk 模块（所有权方向相反），
    /// 只能由 runtime 打断 `Core → CacheWarmer → slot → AgentSession → Core` 引用环
    /// （见 sdk.rs 的 `CACHE_WARMER_SESSIONS` 说明）。无返回值、不可取消：进入替换后
    /// 必然走到 `apply` 或以工厂错误告终。
    async fn teardown_current(
        &self,
        session: &AgentSession,
        reason: kiana_ext_api::events::SessionShutdownReason,
        target_session_file: Option<String>,
    ) {
        // 先让进行中的响应落地（含工具结果），写入旧会话后再替换。
        session.abort().await;
        let runner = session.extension_runner();
        emit_session_shutdown_event(
            &runner,
            SessionShutdownEvent {
                event_type: SessionShutdownEventType::SessionShutdown,
                reason,
                target_session_file,
            },
        )
        .await;
        let callback = lock(&self.state).before_session_invalidate.clone();
        if let Some(callback) = callback {
            callback();
        }
        // 第 9 周：在 tokio runtime 退出前冲刷遥测（不可达 exporter 不影响会话退出）。
        session.flush_telemetry().await;
        session.dispose();
        // 打断 CacheWarmer 与会话的引用环（见 sdk.rs#CACHE_WARMER_SESSIONS）。
        crate::sdk::release_cache_warmer_session(&session.session_id());
    }

    /// `finishSessionReplacement(withSession?)`。
    ///
    /// 替换骨架的最后一步：先 await 宿主注册的 `rebind_session`（重新绑定订阅/设置），
    /// 再 await 本操作的 `with_session`（扩展拿新会话的受限上下文）。两者都可能为
    /// `None`；失败不产生 `Result`（回调类型返回 `()`），回调内部错误由宿主自行处理。
    async fn finish_session_replacement(&self, with_session: Option<ReplacedSessionCallback>) {
        let session = self.session();
        let rebind = lock(&self.state).rebind_session.clone();
        if let Some(rebind) = rebind {
            rebind(session.clone()).await;
        }
        if let Some(with_session) = with_session {
            with_session(session.create_replaced_session_context()).await;
        }
    }

    // -- 会话替换 -----------------------------------------------------------

    /// `switchSession(sessionPath, options?)`。
    ///
    /// TS: `agent-session-runtime.ts#switchSession`
    ///
    /// 恢复一个已存在的会话文件。流程：`before_switch("resume", sessionPath)`（可取消）
    /// → `SessionManager::open` 打开文件 → `assert_session_cwd` 校验会话记录的 cwd 仍
    /// 存在（失败即 [`AgentSessionRuntimeError::MissingSessionCwd`]，旧会话不受影响）→
    /// teardown 旧会话 → 用工厂以 `reason: "resume"` 重建 → `apply` + 替换回调。
    ///
    /// 新会话的 cwd 来自会话文件（或 `cwd_override`），而非 runtime 当前的 `cwd`：
    /// 跨目录切换后 [`Self::cwd`] 跟随新会话（测试
    /// `updates_the_runtime_session_cwd_on_cross_cwd_session_replacement` 锁定）。
    ///
    /// 失败点集中在 teardown 之前（open/cwd 校验）或工厂内；一旦 teardown 开始就没有
    /// 回滚。
    pub async fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, AgentSessionRuntimeError> {
        let options = options.unwrap_or_default();
        let session = self.session();
        if self
            .emit_before_switch(
                &session,
                kiana_ext_api::events::SessionBeforeSwitchReason::Resume,
                Some(session_path.to_string()),
            )
            .await?
        {
            return Ok(SessionReplacementResult { cancelled: true });
        }

        let previous_session_file = session.session_file();
        let session_manager =
            SessionManager::open(session_path, None, options.cwd_override.as_deref())?;
        let cwd = self.cwd();
        assert_session_cwd(&session_manager, &cwd)
            .map_err(AgentSessionRuntimeError::MissingSessionCwd)?;
        self.teardown_current(
            &session,
            kiana_ext_api::events::SessionShutdownReason::Resume,
            session_manager.get_session_file().map(str::to_string),
        )
        .await;
        let services = self.services();
        let project_trust_context = options
            .project_trust_context_factory
            .as_ref()
            .map(|factory| factory(session_manager.get_cwd()));
        let result = (self.create_runtime)(CreateAgentSessionRuntimeFactoryOptions {
            cwd: session_manager.get_cwd().to_string(),
            agent_dir: services.agent_dir.clone(),
            session_manager,
            session_start_event: Some(session_start_event(
                SessionStartReason::Resume,
                previous_session_file,
            )),
            project_trust_context,
        })
        .await?;
        self.apply(result);
        self.finish_session_replacement(options.with_session).await;
        Ok(SessionReplacementResult { cancelled: false })
    }

    /// `newSession(options?)`。
    ///
    /// TS: `agent-session-runtime.ts#newSession`
    ///
    /// 开启一个空会话。新 `SessionManager` 的持久化与否跟随**旧**会话（`is_persisted`）：
    /// 持久化时在旧会话的 session 目录里 `create`，内存态则 `in_memory`；`parent_session`
    /// 提供时在新管理器上记录父子关系（空串按 TS falsy 语义忽略）。其余步骤同替换骨架：
    /// `before_switch("new")`（可取消）→ teardown（`reason: "new"`）→ 工厂重建 → apply
    /// → setup → 回调。
    ///
    /// `setup` 是扩展 API `newSession({ setup })` 的回调，在工厂重建**之后**、
    /// `refresh_context()` 之前执行，且拿到的是 `Arc<Mutex<SessionManager>>` 句柄——
    /// 因此这里先把管理器从会话里换出（放入占位符避免双重可变借用），await setup 完成
    /// 后再换回并 `refresh_context()`。TS 没有所有权限制，直接调用即可；这是 Rust 移植
    /// 的必要变形（占位符换出/换回的做法与 in-memory fork 一致）。
    pub async fn new_session(
        &self,
        options: Option<NewSessionOptionsInput>,
    ) -> Result<SessionReplacementResult, AgentSessionRuntimeError> {
        let options = options.unwrap_or_default();
        let session = self.session();
        if self
            .emit_before_switch(
                &session,
                kiana_ext_api::events::SessionBeforeSwitchReason::New,
                None,
            )
            .await?
        {
            return Ok(SessionReplacementResult { cancelled: true });
        }

        let previous_session_file = session.session_file();
        let cwd = self.cwd();
        let session_dir = session.session_manager().get_session_dir().to_string();
        let is_persisted = session.with_session_manager_mut(|manager| manager.is_persisted());
        let mut session_manager = if is_persisted {
            SessionManager::create(&cwd, Some(&session_dir), None)?
        } else {
            SessionManager::in_memory(&cwd, None, None)?
        };
        // TS 的 `if (options?.parentSession)`：空字符串是 falsy。
        if let Some(parent_session) = options
            .parent_session
            .clone()
            .filter(|parent_session| !parent_session.is_empty())
        {
            session_manager.new_session(Some(NewSessionOptions {
                id: None,
                parent_session: Some(parent_session),
            }))?;
        }
        let services = self.services();
        self.teardown_current(
            &session,
            kiana_ext_api::events::SessionShutdownReason::New,
            session_manager.get_session_file().map(str::to_string),
        )
        .await;
        let result = (self.create_runtime)(CreateAgentSessionRuntimeFactoryOptions {
            cwd: cwd.clone(),
            agent_dir: services.agent_dir.clone(),
            session_manager,
            session_start_event: Some(session_start_event(
                SessionStartReason::New,
                previous_session_file,
            )),
            project_trust_context: None,
        })
        .await?;
        self.apply(result);
        // TS: `if (options?.setup) { await options.setup(this.session.sessionManager); this.session.refreshContext(); }`
        //
        // setup 在 `refreshContext()` **之前**被 await。`AgentSession` 以 `Mutex<SessionManager>`
        // 持有管理器，而 `SetupFn` 拿到的是 `Arc<Mutex<SessionManager>>` 句柄；这里把管理器
        // 临时换出、交给 setup、等待完成后再放回（与 in-memory fork 换出管理器的做法一致）。
        if let Some(setup) = options.setup {
            let session = self.session();
            // TS 把真实 SessionManager 直接交给 setup；Rust 的 Core 持有内嵌
            // `Mutex<SessionManager>`，不能把锁守卫跨 await 借出去。这里用一份克隆作为
            // 共享管理器，并把 Core 的 `with_session*` 暂时重定向到它，使
            // `kiana.setSessionName()` 等 Core 路由写入与 setup 的直写落到同一份状态。
            let shared = Arc::new(Mutex::new(
                session.with_session_manager_mut(|manager| manager.clone()),
            ));
            session.set_session_override(Some(Arc::clone(&shared)));

            // 回写必须放在 Drop 里：setup future 被取消（shutdown/interrupt）或 panic 时，
            // 显式回写不会执行，setup 的所有写入会随着 `shared` 一起被丢弃。Drop 里先解除
            // 重定向，再尽力移动（`Arc::try_unwrap`，setup 没有遗留句柄时成立）或克隆回
            // Core，保证取消路径也不丢数据。
            struct RestoreSession<'a> {
                session: &'a AgentSession,
                shared: Option<Arc<Mutex<SessionManager>>>,
            }
            impl RestoreSession<'_> {
                fn handle(&self) -> Arc<Mutex<SessionManager>> {
                    Arc::clone(self.shared.as_ref().expect("restore runs after setup"))
                }
            }
            impl Drop for RestoreSession<'_> {
                fn drop(&mut self) {
                    let Some(shared) = self.shared.take() else {
                        return;
                    };
                    self.session.set_session_override(None);
                    let manager = match Arc::try_unwrap(shared) {
                        Ok(mutex) => mutex
                            .into_inner()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        // setup 留下了句柄（存进扩展/暂停中的任务）：只能克隆当前状态。
                        Err(shared) => lock(&shared).clone(),
                    };
                    self.session
                        .with_session_manager_mut(|current| *current = manager);
                }
            }

            let restore = RestoreSession {
                session: &session,
                shared: Some(shared),
            };
            setup(restore.handle()).await;
            // 显式结束重定向并把 setup 的修改写回 Core，然后才能 refreshContext。
            drop(restore);
            session.refresh_context();
        }
        self.finish_session_replacement(options.with_session).await;
        Ok(SessionReplacementResult { cancelled: false })
    }

    /// `fork(entryId, options?)`。
    ///
    /// TS: `agent-session-runtime.ts#fork`
    ///
    /// 从已有会话树的某个条目分叉出新会话，是四个替换操作里分支最多、失败点最多的一个。
    /// 阶段划分：
    ///
    /// 1. **取消钩子**：`before_fork` 可取消（取消时不做任何校验）。
    /// 2. **定位条目**：`entry_id` 必须存在，且类型/位置组合合法，否则返回
    ///    [`AgentSessionRuntimeError::InvalidEntryIdForForking`]——`position: "at"` 接受
    ///    任意条目（直接以它为新叶）；`position: "before"` 只接受 user 消息条目，新叶是
    ///    它的**父节点**，同时把该消息文本提取进 `selected_text`（供前端预填输入框）。
    /// 3. **按持久化模式分派**（persist / in-memory，见下方各段注释），产生新
    ///    `SessionManager`。
    /// 4. **替换骨架尾部**：teardown（`reason: "fork"`）→ 工厂以 `reason: "fork"` 重建
    ///    → apply → 回调。
    ///
    /// persist 模式下两条失败路径发生在 teardown 之前、旧会话保持不变：目标会话文件
    /// 尚未落盘（还没发过 user/assistant 消息）返回 `SessionNotSavedYet`；分支创建
    /// 返回空路径返回 `FailedToCreateForkedSession`。`target_leaf_id` 为空（如 fork 在
    /// 根节点之前）时走"新开一个以当前会话为父的会话"分支，而不是分支复制。
    pub async fn fork(
        &self,
        entry_id: &str,
        options: Option<ForkSessionOptions>,
    ) -> Result<ForkSessionResult, AgentSessionRuntimeError> {
        let options = options.unwrap_or_default();
        let position = options.position.unwrap_or(ForkPosition::Before);
        let session = self.session();
        if self.emit_before_fork(&session, entry_id, position).await? {
            return Ok(ForkSessionResult {
                cancelled: true,
                selected_text: None,
            });
        }

        let selected_entry = session.session_manager().get_entry(entry_id).cloned();
        let Some(selected_entry) = selected_entry else {
            return Err(AgentSessionRuntimeError::InvalidEntryIdForForking);
        };
        let mut target_leaf_id: Option<String>;
        let mut selected_text: Option<String> = None;
        if position == ForkPosition::At {
            target_leaf_id = Some(selected_entry.id().to_string());
        } else {
            let SessionEntry::Message(message_entry) = &selected_entry else {
                return Err(AgentSessionRuntimeError::InvalidEntryIdForForking);
            };
            if message_entry.message.role() != "user" {
                return Err(AgentSessionRuntimeError::InvalidEntryIdForForking);
            }
            target_leaf_id = selected_entry
                .parent_id()
                .map(str::to_string)
                .filter(|leaf_id| !leaf_id.is_empty());
            selected_text = Some(extract_user_message_text(&message_entry.message));
        }

        let previous_session_file = session.session_file();
        let cwd = self.cwd();
        let services = self.services();
        let is_persisted = session.with_session_manager_mut(|manager| manager.is_persisted());
        // ---- persist 模式：不改动旧管理器，新建或重开会话文件 ----
        if is_persisted {
            // TS 的 `if (!currentSessionFile)`：空字符串是 falsy。
            let Some(current_session_file) = session
                .session_file()
                .filter(|session_file| !session_file.is_empty())
            else {
                return Err(AgentSessionRuntimeError::PersistedSessionMissingSessionFile);
            };
            let session_dir = session.session_manager().get_session_dir().to_string();
            // target_leaf_id 为空：在当前会话“之前”没有可分叉的父节点，改为新开一个
            // 以当前会话为父的空会话。
            if is_falsy_id(&target_leaf_id) {
                let mut session_manager = SessionManager::create(&cwd, Some(&session_dir), None)?;
                session_manager.new_session(Some(NewSessionOptions {
                    id: None,
                    parent_session: Some(current_session_file),
                }))?;
                self.teardown_current(
                    &session,
                    kiana_ext_api::events::SessionShutdownReason::Fork,
                    session_manager.get_session_file().map(str::to_string),
                )
                .await;
                let result = (self.create_runtime)(CreateAgentSessionRuntimeFactoryOptions {
                    cwd: cwd.clone(),
                    agent_dir: services.agent_dir.clone(),
                    session_manager,
                    session_start_event: Some(session_start_event(
                        SessionStartReason::Fork,
                        previous_session_file.clone(),
                    )),
                    project_trust_context: None,
                })
                .await?;
                self.apply(result);
                self.finish_session_replacement(options.with_session).await;
                return Ok(ForkSessionResult {
                    cancelled: false,
                    selected_text,
                });
            }
            // 常规分支复制：重开当前会话文件，以 target_leaf_id 为新叶复制活动分支。
            // create_branched_session 在持久化模式下返回新文件路径（Some），空/None 即
            // 失败（对应 TS 对 `forkedSessionPath` 的真值检查）。
            if !Path::new(&current_session_file).exists() {
                return Err(AgentSessionRuntimeError::SessionNotSavedYet);
            }
            let leaf_id = target_leaf_id.clone().unwrap_or_default();
            let mut session_manager =
                SessionManager::open(&current_session_file, Some(&session_dir), None)?;
            let forked_session_path = session_manager.create_branched_session(&leaf_id)?;
            if forked_session_path.as_deref().is_none_or(str::is_empty) {
                return Err(AgentSessionRuntimeError::FailedToCreateForkedSession);
            }
            let cwd_of_session = session_manager.get_cwd().to_string();
            self.teardown_current(
                &session,
                kiana_ext_api::events::SessionShutdownReason::Fork,
                session_manager.get_session_file().map(str::to_string),
            )
            .await;
            let result = (self.create_runtime)(CreateAgentSessionRuntimeFactoryOptions {
                cwd: cwd_of_session,
                agent_dir: services.agent_dir.clone(),
                session_manager,
                session_start_event: Some(session_start_event(
                    SessionStartReason::Fork,
                    previous_session_file,
                )),
                project_trust_context: None,
            })
            .await?;
            self.apply(result);
            self.finish_session_replacement(options.with_session).await;
            return Ok(ForkSessionResult {
                cancelled: false,
                selected_text,
            });
        }

        // in-memory：复用同一个会话管理器（拆完再改）。Rust 的对象被会话持有，因此在
        // teardown（让旧 turn 落盘）之后把它从会话里换出来，再交给工厂。
        // TS 直接拿 `session.sessionManager` 改写；Rust 里 SessionManager 的可变所有权
        // 在 `AgentSession` 的 `Mutex` 后面，必须 `std::mem::replace` 换出（留下占位符），
        // 工厂把换出的管理器移交给新会话。分支方式与 persist 模式相同：
        // 无 target_leaf_id 时新开父会话，否则 `create_branched_session`（内存态返回
        // `None` 路径，直接重写内存中的条目链）。
        self.teardown_current(
            &session,
            kiana_ext_api::events::SessionShutdownReason::Fork,
            session
                .session_manager()
                .get_session_file()
                .map(str::to_string),
        )
        .await;
        let placeholder = SessionManager::in_memory(&cwd, None, None)?;
        let mut session_manager =
            session.with_session_manager_mut(|manager| std::mem::replace(manager, placeholder));
        if is_falsy_id(&target_leaf_id) {
            session_manager.new_session(Some(NewSessionOptions {
                id: None,
                parent_session: previous_session_file.clone(),
            }))?;
        } else {
            let leaf_id = target_leaf_id.take().unwrap_or_default();
            session_manager.create_branched_session(&leaf_id)?;
        }
        let result = (self.create_runtime)(CreateAgentSessionRuntimeFactoryOptions {
            cwd,
            agent_dir: services.agent_dir.clone(),
            session_manager,
            session_start_event: Some(session_start_event(
                SessionStartReason::Fork,
                previous_session_file.clone(),
            )),
            project_trust_context: None,
        })
        .await?;
        self.apply(result);
        self.finish_session_replacement(options.with_session).await;
        Ok(ForkSessionResult {
            cancelled: false,
            selected_text,
        })
    }

    /// 导入会话 JSONL 并切换。
    ///
    /// TS: `agent-session-runtime.ts#importFromJsonl`
    ///
    /// 把外部的会话 JSONL 复制进本会话的 session 目录再按 resume 流程切换。阶段：
    ///
    /// 1. **解析与存在性检查**：按进程 cwd 解析 `input_path`；文件不存在直接返回
    ///    [`AgentSessionRuntimeError::SessionImportFileNotFound`]（发生在 `before_switch`
    ///    之前，扩展取消不了）。
    /// 2. **准备目标目录**：session 目录为空串时合成 TS 的 `mkdirSync("")` 错误文本；
    ///    不存在则递归创建（失败映射为 Node 风格 mkdir 错误文本）。
    /// 3. **决定目标文件名**：与源文件同名则原地导入（不复制）；否则撞名时从 1 起追加
    ///    `name-1.ext`、`name-2.ext` … 后缀。
    /// 4. **替换骨架**：`before_switch("resume", destinationPath)`（可取消）→
    ///    `copy_file_exclusive`（`COPYFILE_EXCL` 语义）→ `SessionManager::open` +
    ///    cwd 校验 → teardown → 工厂重建 → apply。与 `switch_session` 不同，本方法
    ///    不向 `finish_session_replacement` 传 `with_session`（TS 伪代码同样调用
    ///    无参的 `finishSessionReplacement()`）。
    ///
    /// 复制失败的错误文本、权限保留等行为由 `tests/agent_session_runtime.rs` 的
    /// `import_maps_copyfile_failures_to_node_error_text`、
    /// `import_preserves_the_source_file_permissions`、
    /// `import_maps_mkdir_failures_to_node_error_text` 锁定。
    pub async fn import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> Result<SessionReplacementResult, AgentSessionRuntimeError> {
        let process_cwd = process_cwd();
        let resolved_path = resolve_path(input_path, &process_cwd);
        if !Path::new(&resolved_path).exists() {
            return Err(AgentSessionRuntimeError::SessionImportFileNotFound(
                SessionImportFileNotFoundError {
                    file_path: resolved_path,
                },
            ));
        }

        let session = self.session();
        let session_dir = session.session_manager().get_session_dir().to_string();
        if session_dir.is_empty() {
            // TS: `mkdirSync("", { recursive: true })` → ENOENT。
            return Err(AgentSessionRuntimeError::Other(
                "ENOENT: no such file or directory, mkdir ''".to_string(),
            ));
        }
        if !Path::new(&session_dir).exists() {
            std::fs::create_dir_all(&session_dir).map_err(|error| {
                AgentSessionRuntimeError::Other(node_fs_error(&error, "mkdir", &session_dir, None))
            })?;
        }

        let basename = Path::new(&resolved_path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| resolved_path.clone());
        let mut destination_path = join_path(&session_dir, &basename);
        let source_already_stored = resolve_path(&destination_path, &process_cwd) == resolved_path;
        if !source_already_stored {
            let name = Path::new(&destination_path)
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let extension = Path::new(&destination_path)
                .extension()
                .map(|extension| format!(".{}", extension.to_string_lossy()))
                .unwrap_or_default();
            let mut suffix = 1_u32;
            while Path::new(&destination_path).exists() {
                destination_path = join_path(&session_dir, &format!("{name}-{suffix}{extension}"));
                suffix += 1;
            }
        }
        if self
            .emit_before_switch(
                &session,
                kiana_ext_api::events::SessionBeforeSwitchReason::Resume,
                Some(destination_path.clone()),
            )
            .await?
        {
            return Ok(SessionReplacementResult { cancelled: true });
        }

        let previous_session_file = session.session_file();
        if !source_already_stored {
            copy_file_exclusive(&resolved_path, &destination_path)?;
        }

        let session_manager =
            SessionManager::open(&destination_path, Some(&session_dir), cwd_override)?;
        let cwd = self.cwd();
        assert_session_cwd(&session_manager, &cwd)
            .map_err(AgentSessionRuntimeError::MissingSessionCwd)?;
        let services = self.services();
        self.teardown_current(
            &session,
            kiana_ext_api::events::SessionShutdownReason::Resume,
            session_manager.get_session_file().map(str::to_string),
        )
        .await;
        let result = (self.create_runtime)(CreateAgentSessionRuntimeFactoryOptions {
            cwd: session_manager.get_cwd().to_string(),
            agent_dir: services.agent_dir.clone(),
            session_manager,
            session_start_event: Some(session_start_event(
                SessionStartReason::Resume,
                previous_session_file,
            )),
            project_trust_context: None,
        })
        .await?;
        self.apply(result);
        self.finish_session_replacement(None).await;
        Ok(SessionReplacementResult { cancelled: false })
    }

    /// `dispose()`。
    ///
    /// TS: `agent-session-runtime.ts#dispose`
    ///
    /// 退出前的最终清理，是 teardown 骨架的"不再替换"变体：`session_shutdown`
    /// （`reason: "quit"`，无目标文件）→ `before_session_invalidate`（同步）→
    /// `session.dispose()` → 释放 CacheWarmer 槽位。与 `teardown_current` 的关键差别是
    /// 没有 `abort()` await：不等待进行中的响应结算，而是由 `dispose` 同步取消 agent、
    /// 压缩/重试/订阅等后台任务；不触发 `before_switch`、不产生新会话、也没有 rebind。
    pub async fn dispose(&self) {
        let session = self.session();
        // SessionEnd hooks：best-effort，忽略决策；在扩展 shutdown 之前运行，便于
        // 审计看到完整的会话状态。
        session.run_session_end_hook("shutdown").await;
        let runner = session.extension_runner();
        emit_session_shutdown_event(
            &runner,
            SessionShutdownEvent {
                event_type: SessionShutdownEventType::SessionShutdown,
                reason: kiana_ext_api::events::SessionShutdownReason::Quit,
                target_session_file: None,
            },
        )
        .await;
        let callback = lock(&self.state).before_session_invalidate.clone();
        if let Some(callback) = callback {
            callback();
        }
        // 第 9 周：在 tokio runtime 退出前冲刷遥测（不可达 exporter 不影响会话退出）。
        session.flush_telemetry().await;
        session.dispose();
        // 打断 CacheWarmer 与会话的引用环（见 sdk.rs#CACHE_WARMER_SESSIONS）。
        crate::sdk::release_cache_warmer_session(&session.session_id());
    }
}

/// `createAgentSessionRuntime(createRuntime, options)`。
///
/// TS: `agent-session-runtime.ts#createAgentSessionRuntime`
///
/// 首次创建运行时的入口（不发生"替换"）：先校验初始会话的 cwd 存在（失败返回
/// `MissingSessionCwd`，工厂不会被调用），再执行工厂、把结果连同工厂本身装进
/// [`AgentSessionRuntime`]——工厂被保存下来，供之后的 switch/new/fork/import 重建会话。
/// 下游（kiana-coding-agent、前端）从此持有这个 runtime 而不是直接持有
/// [`AgentSession`]。
pub async fn create_agent_session_runtime(
    create_runtime: CreateAgentSessionRuntimeFactory,
    options: CreateAgentSessionRuntimeOptions,
) -> Result<AgentSessionRuntime, AgentSessionRuntimeError> {
    assert_session_cwd(&options.session_manager, &options.cwd)
        .map_err(AgentSessionRuntimeError::MissingSessionCwd)?;
    let result = create_runtime(CreateAgentSessionRuntimeFactoryOptions {
        cwd: options.cwd,
        agent_dir: options.agent_dir,
        session_manager: options.session_manager,
        session_start_event: options.session_start_event,
        project_trust_context: None,
    })
    .await?;
    Ok(AgentSessionRuntime::new(
        result.session,
        result.services,
        create_runtime,
        result.diagnostics,
        result.model_fallback_message,
    ))
}

// ============================================================================
// 辅助
// ============================================================================

/// 取锁；锁中毒（持锁线程 panic）时仍返回数据而不是传播 panic。
///
/// TS 没有锁也就没有中毒问题；这里选择“读取最后一致状态”而不是让后续所有调用崩溃，
/// 与 runtime 其余“尽力结算”的取向一致。只在极短同步段持锁，绝不跨 `.await`
/// （文件级 `//!` 的并发形态一节）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 进程 cwd；`std::env::current_dir` 失败（如目录被删除）时回退到 `"."`。
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

fn resolve_path(input: &str, base_dir: &str) -> String {
    kiana_core::paths::resolve_path(
        input,
        base_dir,
        &kiana_core::paths::PathInputOptions::default(),
    )
}

fn join_path(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).to_string_lossy().into_owned()
}

/// JS 的真值判断：`null` / `undefined` / `""` 都是假（fork 的 `targetLeafId`）。
fn is_falsy_id(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(str::is_empty)
}

/// 组装 `session_start` 事件体：替换操作的 `reason` 加被替换会话的文件。
fn session_start_event(
    reason: SessionStartReason,
    previous_session_file: Option<String>,
) -> SessionStartEvent {
    SessionStartEvent {
        event_type: SessionStartEventType::SessionStart,
        reason,
        previous_session_file,
    }
}

/// `extractUserMessageText(content)`：数组取 text 部分并用 **`""`** 拼接（与 session-manager
/// 的 `extractTextContent` 用空格不同）。
///
/// fork `position: "before"` 时提取被分叉的用户消息文本，返回值进入
/// [`ForkSessionResult::selected_text`]。覆盖三种输入形态：标准 user 消息、
/// toolResult（防御性分支：fork 路径只会以 user 消息进入本函数，该形态是为与 TS
/// 提取函数对齐而保留）与自定义 JSON 消息（`content` 为字符串或
/// `{type:"text"}` 分块数组）；图片块跳过，其余形态一律返回空串（与该函数的
/// 默认分支一致），不是错误路径。
fn extract_user_message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text(text) => Some(text.text.clone()),
                    UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        },
        AgentMessage::Standard(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text(text) => Some(text.text.clone()),
                UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        AgentMessage::Custom(object) => match object.get("content") {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(serde_json::Value::as_str) == Some("text") {
                        part.get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// `copyFileSync(src, dest, COPYFILE_EXCL)`。
///
/// Node 的 `copyFileSync` 语义：目标已存在 → `EEXIST`；源是目录 → `EISDIR`；复制成功后
/// **保留源文件权限**（libuv 的 `uv_fs_copyfile` 在 `fchmod` 里写入源 `st_mode`）。
/// 用 `create_new(true)` 保证 `COPYFILE_EXCL` 的原子性（不做先检查后打开的竞态窗口）；
/// 复制中途失败时删除半成品目标文件，再返回 Node 风格错误文本（import_* 测试锁定）。
fn copy_file_exclusive(source: &str, destination: &str) -> Result<(), AgentSessionRuntimeError> {
    use std::io::{Read, Write};
    let source_metadata = std::fs::metadata(source).map_err(|error| {
        AgentSessionRuntimeError::Other(node_fs_error(
            &error,
            "copyfile",
            source,
            Some(destination),
        ))
    })?;
    if source_metadata.is_dir() {
        return Err(AgentSessionRuntimeError::Other(format!(
            "EISDIR: illegal operation on a directory, copyfile '{source}' -> '{destination}'"
        )));
    }
    let copyfile_error = |error: &std::io::Error| {
        AgentSessionRuntimeError::Other(node_fs_error(error, "copyfile", source, Some(destination)))
    };
    let mut input = std::fs::File::open(source).map_err(|error| copyfile_error(&error))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| copyfile_error(&error))?;
    let mut buffer = Vec::new();
    let result = (|| -> std::io::Result<()> {
        input.read_to_end(&mut buffer)?;
        output.write_all(&buffer)?;
        output.set_permissions(source_metadata.permissions())?;
        Ok(())
    })();
    if let Err(error) = result {
        // libuv 在复制失败后删除未完成的目标文件。
        drop(output);
        let _ = std::fs::remove_file(destination);
        return Err(copyfile_error(&error));
    }
    Ok(())
}

/// Node 的 `fs` 错误文本：`{CODE}: {strerror(errno)}, {syscall} '{path}'`
/// （`copyfile` 的两个路径写作 `'{src}' -> '{dst}'`）。
///
/// 前端把错误文本原样展示/断言（如 import_* 测试对整串比较），因此 errno → 文本的
/// 映射按 Linux errno 值固定；无法映射的 `raw_os_error` 或非 OS 错误退回 `EIO`。
fn node_fs_error(
    error: &std::io::Error,
    syscall: &str,
    path: &str,
    destination: Option<&str>,
) -> String {
    let (code, description) = match error.raw_os_error() {
        Some(1) => ("EPERM", "operation not permitted"),
        Some(2) => ("ENOENT", "no such file or directory"),
        Some(13) => ("EACCES", "permission denied"),
        Some(17) => ("EEXIST", "file already exists"),
        Some(20) => ("ENOTDIR", "not a directory"),
        Some(21) => ("EISDIR", "illegal operation on a directory"),
        Some(22) => ("EINVAL", "invalid argument"),
        Some(24) => ("EMFILE", "too many open files"),
        Some(28) => ("ENOSPC", "no space left on device"),
        Some(30) => ("EROFS", "read-only file system"),
        Some(36) => ("ENAMETOOLONG", "file name too long"),
        Some(40) => ("ELOOP", "too many symbolic links encountered"),
        Some(122) => ("EDQUOT", "disk quota exceeded"),
        _ => match error.kind() {
            std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
            std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
            std::io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
            _ => ("EIO", "i/o error"),
        },
    };
    match destination {
        Some(destination) => {
            format!("{code}: {description}, {syscall} '{path}' -> '{destination}'")
        }
        None => format!("{code}: {description}, {syscall} '{path}'"),
    }
}

// 在 runtime 模块末尾再导出 services 的类型与函数：实现位于
// agent_session_services 模块，从 runtime 路径再导出可让调用方按运行时相关
// 模块引用同一组符号，避免在实现之外重复定义。
pub use crate::agent_session_services::{
    AgentSessionRuntimeDiagnostic, AgentSessionServices, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
