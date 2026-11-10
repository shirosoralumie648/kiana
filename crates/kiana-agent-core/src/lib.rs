//! 对应 `packages/agent`（`@earendil-works/kiana-agent-core`）：Agent 状态机、agent loop、
//! stream fn、事件。
//!
//! TS 源：`packages/agent/src/**`。跨 crate 契约集中在 [`types`]；依赖本 crate 的上层 crate
//! （`kiana-session-core`、`kiana-session`、`kiana-tools` 等）通过本 crate 的公开面接入（例如
//! [`agent::Agent`]、[`types`] 的契约类型与 [`agent_loop`] 的循环入口），完整依赖边见
//! `ARCHITECTURE.md` §2.2。
//!
//! 公开面与 `packages/agent/src/index.ts` 的再导出面一一对应，见 [`index`]；
//! `stream_fn::get_default_stream_fn` 与 TS 的 `getDefaultStreamFn` 一样只供包内使用，
//! 包公开面只导出 [`stream_fn::set_default_stream_fn`]。crate 根另外平铺再导出
//! [`agent::Agent`]、[`agent::AgentError`] 等 [`agent`] 条目与 [`types`] 中的常用契约类型；
//! 其中 [`types`] 的这份 `pub use` 列表的大部分条目由 `tests/public_api.rs` 的编译期导入
//! 锁定（`ToolArguments` 不在其导入清单中）。
//!
//! 行为文档见 crate 根的 `README.md`（对应 `packages/agent/README.md`）。
//!
//! # 这个 crate 解决什么问题
//!
//! 单次 LLM 调用只是“发请求、收一条回复”；agent 需要多轮：维护 transcript、发起流式请求、
//! 解析 tool call、执行工具、把工具结果写回上下文，再决定是否继续。本 crate 实现这个循环，
//! 并把每一步变成 [`types::AgentEvent`] 供 UI / 会话层消费。它不实现任何 provider 协议：
//! 流式请求由注入的 [`types::StreamFn`] 完成；本 crate 另外在 [`proxy`] 里提供 TS 的
//! `streamProxy`（转发到 `${proxyUrl}/api/stream`）。
//!
//! # 在系统中的位置
//!
//! - 上游：`kiana-ai` 提供模型、消息与流事件类型（如 `AssistantMessageEventStream`）；
//!   `kiana-json` 提供与 JS `JSON.stringify` 一致的写出，[`types`] 的 serde 实现用它保证
//!   落盘与上屏的字节一致。
//! - 下游：`kiana-session-core`、`kiana-session`、`kiana-tools`、`kiana-ext-api` 等通过本
//!   crate 的公开面接入（依赖边见各 crate 的 `Cargo.toml` 与 `ARCHITECTURE.md` §2.2）；
//!   `kiana-core` 只在 `Cargo.toml` 声明了对本 crate 的依赖，其源码目前未引用本 crate 的
//!   公开项。
//! - TS 对应：`packages/agent/src/**`（`agent.ts`、`agent-loop.ts`、`proxy.ts`、
//!   `stream-fn.ts`、`types.ts`、`index.ts`）。
//!
//! # 核心概念与类型
//!
//! - [`types::AgentMessage`]：transcript 的元素类型。标准四角色之外，`AgentMessage::Custom`
//!   可用任意 JSON 对象（`JsonObject`）承载 TS 的 declaration merging 自定义消息；其 `role`
//!   也可以是标准四类之一，这类 Custom 在 `Agent::new` 的默认 `convert_to_llm` 下会被投影
//!   回标准消息。LLM 只认识标准消息，所以每次请求前要经 `convert_to_llm` 投影（默认实现只
//!   保留 role 为标准四类之一的消息，见 [`agent::AgentOptions::convert_to_llm`]）。
//! - [`types::AgentEvent`]：一次 run 的事件集合（`agent_start`、`turn_start`、
//!   `message_start`/`message_update`/`message_end`、`tool_execution_start`/
//!   `tool_execution_update`/`tool_execution_end`、`turn_end`、`agent_end`）。listener 按
//!   订阅顺序串行 `await`；每个事件先应用状态变更（如 `message_end` 追加消息、
//!   `tool_execution_*` 维护 pending 集合）再派发，因此 listener 看到的状态已包含当前事件。
//! - [`types::StreamFn`]：`(model, context, options) -> 流` 的注入点；`Agent::new` 未显式
//!   收到它时会急切读取 [`stream_fn`] 里的进程级默认值。
//! - [`types::AgentTool`] / [`types::AgentToolResult`]：工具定义与结果。参数在 `execute`
//!   前按 JSON Schema 校验，校验失败或执行报错都会变成 `is_error` 的工具结果，而不是中止
//!   整个 run。
//! - [`agent::Agent`]：有状态包装，持有 transcript、工具、steering / follow-up 队列与
//!   listener，提供 `prompt*`、`continue_`、`steer`、`follow_up`、`abort`、`subscribe`。
//! - [`types::AgentState`]：状态的**快照**（`Agent::state()` 返回副本，不是活引用）。
//!
//! # 一次典型调用
//!
//! 1. 组装 [`agent::AgentOptions`]（`initial_state`、可选的 `stream_fn` 与各钩子），调用
//!    [`agent::Agent::new`]；`stream_fn` 为 `None` 且没有进程级默认值时立即返回
//!    [`agent::AgentError::DefaultStreamFn`]。
//! 2. 用 [`agent::Agent::subscribe`] 注册 listener；订阅本身不产生事件。
//! 3. 调用 `prompt`/`prompt_text`/`prompt_messages`（或 `continue_`）。在 tokio 运行时内，
//!    方法先登记当前 run、同步执行到 `agent_start` 派发的第一个挂起点（listener 立即完成时
//!    派发完成，否则停在尚未完成的 listener 上），再把 run 交给任务继续；返回的 future 只
//!    用于等待 run 结束，丢弃它不会中断 run（不在运行时内则登记与执行都推迟到 future 首次
//!    被 poll，见 [`agent`] 模块文档）。
//! 4. 循环内部（[`agent_loop`]）：可选的 `transform_context` → `convert_to_llm` → 调用
//!    `stream_fn` 并转发 assistant 流事件 → 工具预检与执行 → 按需进入下一轮。
//! 5. 对 [`agent::Agent`] 而言，run 以 `agent_end` 收尾；每条消息在 `message_end` 时写入
//!    [`types::AgentState`]。
//!
//! # 边界与失败模式
//!
//! - **构造期**：没有可用的流函数时 [`agent::Agent::new`] 失败，错误文本是当前稳定契约
//!   （见 [`stream_fn::NO_DEFAULT_STREAM_FN_MESSAGE`]）。
//! - **调用期**：已有 run 在跑、`continue_` 无消息（含全为 system）可继续，或尾部是 assistant
//!   且 steering / follow-up 队列都为空等前置条件失败，会直接以 [`agent::AgentError`] 返回，
//!   不产生任何事件。
//! - **run 内部失败**：流函数报错等不会从 `prompt*` 的返回值暴露，而是合成为一条
//!   `stopReason` 为 `error`/`aborted` 的 assistant 消息，并按 `message_start` →
//!   `message_end` → `turn_end` → `agent_end` 发出；调用方要靠事件或
//!   [`types::AgentState`] 判断失败。
//! - **低层 API**：[`agent_loop::agent_loop`] 的循环任务以 `Err` 或 panic 退出时不会
//!   `end()` 事件流，`result()` 永远 pending（TS 的 `.then` 没有 `.catch`）；需要消息数组
//!   的调用方应改用 [`agent_loop::run_agent_loop`]。[`agent_loop::agent_loop`] 与
//!   [`agent_loop::agent_loop_continue`] 必须在 tokio 运行时内调用（内部会 spawn），后者的
//!   前置校验会同步返回 [`agent_loop::AgentLoopError`]（空上下文 / 尾部 assistant）。
//! - **订阅**：[`agent::Subscription`] 在 drop 时**不**退订（对应 TS 丢弃 `subscribe`
//!   返回值的语义），必须显式 `unsubscribe`。
//!
//! # 阅读顺序
//!
//! 1. [`types`]：消息、事件、工具、钩子等跨 crate 契约与 JSON 形状。
//! 2. [`agent_loop`]：核心循环、流式上下文维护与工具批次执行。
//! 3. [`agent`]：有状态包装、队列、listener 与 run 生命周期。
//! 4. [`stream_fn`] / [`identity`]：默认流函数单例与请求身份。
//! 5. [`proxy`]（TS 的 `streamProxy`：转发到 `${proxyUrl}/api/stream`）、[`wire_keys`]
//!    （TS 运行时键序还原，供 print 与会话落盘复用）、[`index`]（`index.ts` 的 barrel）。

pub mod types;

pub mod agent;
pub mod agent_loop;
pub mod budget;
pub mod context_report;
pub mod identity;
pub mod index;
pub mod loop_guard;
pub mod proxy;
pub mod run_collector;
pub mod session_telemetry;
pub mod stream_fn;
pub mod wire_keys;

pub use agent::{
    Agent, AgentError, AgentInitialState, AgentListener, AgentOptions, AgentPrepareNextTurnFn,
    AgentPrepareNextTurnWithContextFn, ConvertToLlmFn, Subscription, WeakAgent,
};
pub use budget::{
    BudgetConfig, BudgetController, BudgetDecision, BudgetHandle, BudgetKind, BudgetSnapshot,
    BudgetStatus, BudgetStopReason, BudgetWarning, budget_handle, model_cost_known,
};
pub use context_report::{
    CondensationConfig, CondensationEvent, CondensationPhase, ContextReport, ContextReportOptions,
    ContextSegment, ContextSegmentKind, build_context_report, build_context_report_with,
    observe_condensation,
};
pub use identity::default_agent_identity;
pub use loop_guard::{
    DetectorThreshold, LoopDetection, LoopDetectorKind, LoopGuard, LoopGuardConfig,
    LoopGuardHandle, LoopGuardOutcome, LoopGuardSnapshot, repeat_key,
};
pub use run_collector::{
    AgentRunCollector, AgentRunCoverage, AgentRunSnapshot, AgentRunSummary, ChatRecord,
    ChatSummary, ChatToken, CostSummary, ErrorSummary, RunClock, SystemRunClock, ToolCounters,
    ToolRecord, ToolStatus, ToolSummary, ToolToken, UsageTotals, aggregate_agent_run_coverage,
    aggregate_agent_run_summaries,
};
pub use session_telemetry::{
    ApiCallRecord, CompactionRecord, HookRunRecord, SessionSpan, SessionTelemetry, ToolCallRecord,
    TurnTracker,
};
pub use types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentMessage, AgentState, AgentTool, AgentToolCallOutcome, AgentToolError, AgentToolExecute,
    AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext, BeforeToolCallResult,
    QueueMode, StreamFn, ThinkingLevel, ToolArguments, ToolExecutionMode,
};
