//! `harness/generation.ts`（797 行）：内建 generation 任务。
//!
//! 任务阶段划分：`prepare` 渲染系统提示与工具装载、决定阈值压缩并进入 `request`；
//! `request` 清掉上一段 partial、请求模型并把终态消息分类；`retry` 睡到退避结束再回到
//! `prepare`；`poll` 睡到延迟响应的轮询点再取回结果；`tools` 逐个启动顺序轮的工具任务并在
//! 全部落定后 `finishToolRound`。`classify` 决定延迟轮询、工具轮、最终回答、溢出压缩或失败。
//!
//! 与 TS 的差异（交付说明）：
//! - `streamResponse` 的节流：TS 用 `setTimer` 起一个浮动 promise；Rust 在同一个任务里
//!   `select!` 事件与计时器通道，因此提交永不与事件处理并行（`inFlight` 恒为空），但
//!   "至多每 `partialIntervalMs` 提交一次、结束时不再提交待定 partial"的语义保持不变。
//!
//! `createGeneration` 的 `submissionIds` 通过 `Tx::create_task_with`（内部
//! `TaskCreationOptions`）传给事务：任务 trace 取自首个提交的 `trace`（缺省 `"root"`），其余
//! 提交各追加一条 link，与 TS `Transaction.createTask` 相同。
//!
//! # 这个模块解决什么问题
//!
//! 一次提交最终要变成一次或多次模型请求：渲染系统提示、安排工具声明、判断上下文是否需要
//! 压缩、发请求、处理 provider 的延迟响应、执行工具轮，最后产出回答或按策略重试。这些步骤
//! 之间要写条目、任务记录与 `kiana.live` 文档，进程随时可能在其中崩溃，所以本文件没有把
//! 流程写成一个长函数，而是拆成由 checkpoint 驱动的阶段：调度器每次进入阶段时都从任务记录
//! 里的 checkpoint 恢复，处理器先在 `runtime.commit(...)` 里写清「下一步是什么」，再在提交
//! 之外做不可回滚的动作（请求模型、睡到某个时刻）。
//!
//! # 在系统中的位置
//!
//! - 上游：`crate::harness::submissions` 在输入被放置时调用 [`start_run`]；`crate::harness::live`
//!   的调度器终态清理调用 [`convert_partial`]；`crate::harness::registry` 把 [`GENERATION_TASK`]
//!   放进内建任务表。
//! - 下游：模型请求经由 `runtime.models()`；工具轮创建 `kiana.tool` 子任务；阈值与溢出压缩
//!   创建 `kiana.compaction` 子任务。`kiana.live` 的 `run` / `generation` / `tools` 字段由本
//!   模块维护：`start_run` 建立 `run`，各阶段写 `generation` 与工具槽，结束路径调用
//!   `live::end_run` 清除展示（`run` 只在当前任务拥有它时移除；工具任务只就地更新自己
//!   槽位的进度）。
//! - hook：`beforeRequest` / `afterResponse` / `onYield` / `afterTools` 的处理器从运行端取出，
//!   类型见 [`GenerationHookTable`]。
//!
//! # 状态机（checkpoint 即恢复点）
//!
//! 调度器按 checkpoint 的 `phase` 字段选取处理器（见 [`GenerationCheckpoint`]），五个变体
//! 与五个阶段一一对应：
//!
//! 1. `prepare`（[`PrepareCheckpoint`]）：解析 agent 与模型，渲染系统提示、规划 `kiana.system`
//!    条目，判定阈值压缩。若需要阻塞压缩，创建子任务并进入 `waiting`；否则追加条目并把
//!    checkpoint 换成 `request`。
//! 2. `request`（[`RequestCheckpoint`]）：清掉上一次残留的 partial 并记录 `attempt`，按
//!    `cutoff` 重放上下文，跑 `beforeRequest`，流式请求模型，把终态消息交给 `classify`。
//! 3. `retry`（[`RetryCheckpoint`]）：睡到 `until`；到点后 `attempt + 1` 并回到 `prepare`。
//! 4. `poll`（[`PollCheckpoint`]）：睡到 `pollAt`，取回 provider 侧的延迟响应，再交给
//!    `classify`；每次轮询时间都严格晚于上一次。
//! 5. `tools`（[`ToolsCheckpoint`]）：顺序轮每进入一次只启动 `pending` 的第一个调用；
//!    `pending` 为空时进入 `finish_tool_round`，应用工具控制、排队边界与交接。
//!
//! 取消不在阶段表里：调度器调用任务定义里的 `abort` 处理器（[`AbortPhase`]），它把 partial
//! 转成 `aborted` 条目、为尚未启动的工具调用补 `aborted` 结果，并以 `TaskOutcome::Aborted`
//! 结束任务。
//!
//! # 提交与恢复的约束
//!
//! - **先读表、后写表**：一个提交闭包里，表读取必须在第一次表写入之前完成（约束见
//!   `prepare_boundary` 的文档）。`answer`、`finish_tool_round` 与 `afterFailedRun` 为
//!   `Continue` 的失败路径先准备边界，再取 `kiana.live` 草稿或追加条目。`Keep` 不读表；
//!   `Withdraw` 的 inbox 读取发生在 `end_failed_run` 里随后的 `withdraw_queued_inputs`，
//!   不在准备阶段。
//! - **commit 闭包不逃逸**：需要读时钟/设置的闭包捕获的是 `static_runtime` 造出的 `'static`
//!   借用；它只在 `runtime.commit(...).await` 期间有效（见 `static_runtime` 的 SAFETY 说明）。
//! - **checkpoint 是持久契约**：任务名 `kiana.generation`、版本 1，且 `migrate: None`；字段名
//!   （camelCase、`phase` 判别）与可选性决定升级前停在 `waiting`/`sleeping` 的任务还能不能
//!   解析。`PollCheckpoint::thinking_level` 的 `Option` 就是为旧记录保留的。
//! - **重试与压缩在终态消息之后重新读设置**：`classify` 每次都用当前 `runtime.settings()` 判定，
//!   而不是沿用 `prepare` 时的快照，因此设置变更对下一次尝试生效。
//!
//! # 失败模式
//!
//! - 没有配置模型或模型不在注册表：`fail_no_model` 以 `no_model` 结清本次运行的输入，并让
//!   任务以 `failed` 结束。
//! - 不可重试的模型错误、重试次数用尽，以及没有落到回答/工具轮/延迟响应的其它终态：
//!   `classify` 以 `model_error` 失败；错误文本取 `message.errorMessage`，缺失时用 stop
//!   reason 拼出。
//! - 溢出压缩或阻塞阈值压缩没有成功：下一次 `prepare` 读到子任务结局后失败——溢出压缩用
//!   当初保存的溢出文本，阈值压缩用 `Compaction ...` 文本。若压缩成功但上下文仍超过阈值，
//!   本次尝试照常发请求（`compacted` 已存在时不再重新判定阈值）。
//! - 取消：`abort` 提交 `TaskOutcome::Aborted`，`run.inputs` 以 `unanswered`/`aborted` 结清。
//!
//! # 阅读顺序
//!
//! [`GenerationCheckpoint`] 与五个 checkpoint 结构（数据形状）→ `prepare` → `request` →
//! `classify` → `start_tool_round` / `finish_tool_round` → `answer` → `abort` →
//! `stream_response` / `commit_partial`（partial 节流）→ [`generation_task`]（注册）。

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Context as AiContext, DeferredHandle, Message, Model,
    ModelThinkingLevel, StopReason, StreamOptions, Tool, ToolCall, UserContent, UserMessage,
    UserRole,
};
use kiana_ai::utils::overflow::is_context_overflow;
use kiana_ai::utils::retry::{is_retryable_assistant_error, retry_delay_ms_js};
use kiana_ai::utils::transcript::get_current_tools;
use kiana_chord::context::{Context, abort_error};
use serde::{Deserialize, Serialize};

use crate::harness::agent::{add_tools, section_extension};
use crate::harness::compaction::{
    CompactionInput, compaction_policy, create_compaction, estimate_context, select_cut,
    simple_stream_options,
};
use crate::harness::inbox::{
    Boundary, BoundaryAt, BoundaryResult, QueueModes, apply_boundary, prepare_boundary,
    withdraw_queued_inputs,
};
use crate::harness::live::{
    LIVE_DOC, PROGRESS_COMMIT, TaskCommit, end_run, hook_api, hook_failure, hook_handlers,
    state_checkpoint, static_runtime,
};
use crate::harness::partial_changes::PartialChanges;
use crate::harness::prompt::{SystemDraft, plan_system_entries, render_sections, replay_sections};
use crate::harness::provider::ensure_provider_session_id;
use crate::harness::tool::{TOOL_TASK, ToolTaskResult, append_tool_result, harness_error};
use crate::harness::types::{
    AfterFailedRun, BeforeRequestInput, CompactionPolicy, CompactionReason, CompactionResult,
    ContextView, ContextViewOptions, ConversationStreamOptions, GenerationHooks, HostCallback,
    ModelRef, PromptInput, PromptSection, ReportFrom, Settings, TaskRuntime, ToolControl,
    UserInput,
};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::types::{
    AssistantEntry, BoxError, CommitOptions, ConversationId, DocDraft, DocTarget, DocumentReader,
    EntryHead, EntryId, EntryQuery, JoinPolicy, JsonObject, JsonValue, NoData, ResetEntry,
    RunningTask, SubmissionId, SubmissionSettlement, SystemEntry, TaskDefinition, TaskId,
    TaskOptions, TaskOutcome, TaskOutcomeError, TaskOwnership, TaskPhase, TaskState, Tx,
    TypedEntryDraft, UserEntry, id_from_number,
};

/// generation 任务的输入（`Record<string, never>`）。
///
/// 参数是空对象：这个文件从不读取任务的 input。模型、工具、提示与策略大多由各阶段在需要时
/// 从运行端（agent 与 settings）重新读取；但 checkpoint 中已经固定下来的 `model` 引用、
/// `thinkingLevel`、`streamOptions` 与 `cutoff` 在重放时沿用旧快照（见 [`RequestCheckpoint`]）。
pub type GenerationInput = JsonObject;

/// generation 任务的结果。
///
/// TS: `GenerationResult`。只出现在 `TaskOutcome::Completed` 的 `result` 里；
/// `rename_all = "camelCase"` 把字段写成 `entryId`。
///
/// 两条完成路径写入的 `entryId` 含义不同：`answer` 指向最终回答的 assistant 条目；
/// `finish_tool_round` 指向发起这一轮工具调用的 assistant 条目——即使随后把运行交接给了
/// 新的 generation，结果仍指回这一条。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationResult {
    /// 回答条目。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
}

/// `prepare` 的 checkpoint：等待一次阻塞阈值压缩，或等待压缩完成后重来。
///
/// 两个可选来源字段把「在等哪个子任务、按什么标准算成功」编码进持久状态：
///
/// - `compacted` + `overflow`：请求因上下文溢出失败后启动的压缩（`classify` 写入）。恢复时
///   要求子任务 `completed` **且**结果里有 `entryId`，否则用保存的错误文本失败。
/// - `compacted` + `threshold: Some(true)`：`prepare` 自己启动的阻塞阈值压缩。恢复时只要求
///   子任务 `completed`：压缩拒绝执行或没有可压缩内容时它可能正常结束却不产出摘要条目，
///   这仍然算「本轮已尝试过压缩」。
///
/// 两者都为 `None` 表示普通准备（首次进入、重试后的下一次尝试）。`threshold` 只会写
/// `Some(true)`，`Some(false)` 没有语义。`attempt` 从 1 开始，`retry` 每次回到 `prepare`
/// 之前把它加一。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareCheckpoint {
    /// 尝试序号。
    pub attempt: i64,
    /// 本次 generation 等待的阻塞压缩；它不再启动其它压缩（spec §8.3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted: Option<TaskId<CompactionResult>>,
    /// 启动 `compacted` 的溢出错误文本；`prepare` 恢复时检查一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow: Option<String>,
    /// 阻塞阈值启动了 `compacted`；`prepare` 恢复时检查一次它的结局。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<bool>,
}

/// `request` 的 checkpoint：一次固定下来的请求。
///
/// 它保存重放这次请求所需的全部输入，而不是请求内容本身：恢复时用 `cutoff` 重新派生上下文，
/// 因此同一份 checkpoint 在崩溃前后看到同一段消息（`cutoff` 之后的新条目不会进入这次请求）。
/// `stream_options` 是 `prepare` 时从设置里取的快照：恢复后的重发沿用这份请求选项，不会重新
/// 推导。但这不表示恢复后完全不读设置——流式请求的 partial 节流（`stream_response` 每次请求都
/// 读 `settings.progress.partial_interval_ms`）与终态后的 `classify` 仍读取当前设置。
///
/// `compacted` 只是原样携带：`classify` 用它知道这次尝试已经用过一次压缩，溢出分支因此
/// 不会再次创建压缩任务。`model` 在每次恢复时重新在注册表里解析，消失则任务失败；
/// `thinking_level` 会在 `classify` 里记录到终态消息上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCheckpoint {
    /// 尝试序号。
    pub attempt: i64,
    /// 阻塞压缩。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted: Option<TaskId<CompactionResult>>,
    /// 请求的模型。
    pub model: ModelRef,
    /// 思考等级。
    pub thinking_level: ModelThinkingLevel,
    /// 设置里的请求选项；恢复后的重发原样使用。
    pub stream_options: ConversationStreamOptions,
    /// 请求包含的最新条目。
    pub cutoff: EntryId,
}

/// `retry` 的 checkpoint。
///
/// 表示正在为下一次尝试做持久退避：`until` 是 Harness 时钟（epoch 毫秒）上的绝对时间点。
/// 提前被唤醒（重启、宿主重新调度）时只要 `until > now` 就再睡一次，因此退避不会被恢复流程
/// 缩短。到点后 `attempt` 加一转入 `prepare`，真正的重试次数上限在 `classify` 里判定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryCheckpoint {
    /// 尝试序号。
    pub attempt: i64,
    /// 阻塞压缩。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted: Option<TaskId<CompactionResult>>,
    /// 退避结束的时钟点。
    pub until: i64,
}

/// `poll` 的 checkpoint。
///
/// 表示正在等待 provider 的延迟（deferred）响应。
///
/// 模型返回 `stopReason: "deferred"` 时，`classify` 不结束任务，而是把 provider 给的句柄与
/// 下一次轮询时间写进这里并让任务进入 `sleeping`；到点后 `poll` 调 `fetch_deferred` 取回结果，
/// 再走一遍同样的 `classify`。`poll_at` 由 `classify` 计算为
/// `max(now + pollAfterMs, 上一次 pollAt + 1)`，所以反复轮询时时间严格递增，不会因为两次
/// 请求落在同一毫秒而原地重试。
///
/// 取回前会重新解析 `model`（消失则失败）；`handle` 同时用于取回与取消；`cutoff` 让延迟
/// 结果回来后的分类与工具轮仍以同一段上下文为界。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollCheckpoint {
    /// 尝试序号。
    pub attempt: i64,
    /// 阻塞压缩。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted: Option<TaskId<CompactionResult>>,
    /// 请求的模型。
    pub model: ModelRef,
    /// 思考等级；旧版本的 checkpoint 没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 请求包含的最新条目。
    pub cutoff: EntryId,
    /// provider 侧延迟响应的句柄。
    pub handle: DeferredHandle,
    /// 下次轮询的时钟点。
    pub poll_at: i64,
}

/// `tools` 的 checkpoint：等待本轮的工具任务（generation 拥有它们，spec §8.5）。
///
/// 工具任务由 `create_tool_task` 建成 generation 的子任务（`TaskOwnership::Task`），
/// `finish_tool_round` 从 checkpoint 里的任务 id 取回它们的结局。`tools` 只记录**已创建**
/// 的任务：请求里没有提供的工具会当场得到一条 `tool_unavailable` 结果，不产生任务；顺序
/// 轮里排在后面的调用先留在 `pending`，每次进入 `tools` 阶段只启动一个，`pending` 为空
/// 表示本轮没有待启动的调用、进入收尾。`assistant` 也供 `abort` 读回未启动调用的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCheckpoint {
    /// 发起工具调用的回答。
    pub assistant: EntryId,
    /// 到目前为止创建的工具任务，按调用顺序；顺序轮每启动一个调用增长一项。
    pub tools: Vec<TaskId<ToolTaskResult>>,
    /// 顺序轮尚未启动的调用，按调用顺序。
    pub pending: Vec<String>,
}

/// generation 任务的 checkpoint（TS: `GenerationCheckpoint`）。
///
/// 序列化用内部标签 `phase`（`rename_all = "camelCase"` 把变体名写成 `prepare`/`request`/
/// `retry`/`poll`/`tools`）。调度器不认变体类型，只按这个字符串去阶段表里取处理器，因此
/// 变体名与字段名都是持久格式的一部分：改名会让恢复中的任务找不到阶段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum GenerationCheckpoint {
    /// 渲染系统提示并准备请求。
    Prepare(PrepareCheckpoint),
    /// 请求模型。
    Request(RequestCheckpoint),
    /// 退避后重试。
    Retry(RetryCheckpoint),
    /// 轮询延迟响应。
    Poll(PollCheckpoint),
    /// 等待工具轮。
    Tools(ToolsCheckpoint),
}

/// 内建 generation 任务名。
///
/// 这个字符串同时是任务定义名与持久记录里的 `kind`（`generation_task` 用它填
/// `TaskDefinition::name`），改名会让已有任务记录无法识别。
///
/// 同一个字面量还出现在别处，改名必须全部同步，否则恢复中的旧任务与事件识别都会失配：
///
/// - `crate::harness::live` 的 `RUN_TASK_KINDS`：决定哪些任务可以拥有 `kiana.live.run`；
/// - `crate::harness::events` 里直接写的 `"kiana.generation"`：用于 `TurnStart` 与 turn 结束
///   判定，以及打开事件流时扫描已处于 `completing` 的 generation 任务。这些字面量没有引用
///   本常量，改常量不会带着它们一起变。
pub const GENERATION_TASK_KIND: &str = "kiana.generation";

/// 延迟响应没有 `pollAfterMs` 时的默认轮询间隔（毫秒，`f64` 对应 JS 的 number）。
///
/// `classify` 在存 `poll_at` 之前对它求和并 `floor`，所以这是一个以毫秒为单位的时长，
/// 不是绝对时间点。
pub const DEFAULT_POLL_AFTER_MS: f64 = 5000.0;

/// generation 任务的 hook 表类型。
///
/// 处理器在注册表里以 `Arc<dyn GenerationHooks>` 存放；`hook_handlers` 把运行端
/// `HookRunner` 里的擦除值 downcast 回这个类型。
pub type GenerationHookTable = Arc<dyn GenerationHooks>;

/// 本模块使用的擦除运行端。
///
/// 任务定义最终以 `AnyTask` 形式存进注册表（见 [`GENERATION_TASK`]），输入/状态/结果的类型
/// 参数都被擦除成 `JsonValue`；每个阶段自己把 checkpoint JSON 解析成
/// [`GenerationCheckpoint`]。
type Runtime<'a> = dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable> + 'a;

/// 分类所需的一次请求信息。
///
/// `classify` 与随后的工具轮只依赖这些字段，不依赖阶段栈：普通请求在 `request` 里构造，
/// 延迟响应的轮询在 `poll` 里构造。`thinking_level` 与 `messages` 是 `Option`，因为旧
/// checkpoint（没有等级）与轮询路径（没有现成的消息快照）都可能缺它们。
#[derive(Debug, Clone)]
struct Request {
    attempt: i64,
    compacted: Option<TaskId<CompactionResult>>,
    model: ModelRef,
    thinking_level: Option<ModelThinkingLevel>,
    cutoff: EntryId,
    /// 传到 `cutoff` 的已提交模型上下文（阶段已经推导过时）。
    ///
    /// `request` 传的是**跑 `beforeRequest` 钩子之前**的 `view.messages`（与 TS 相同）：
    /// `start_tool_round` 用它推导工具可用性集合 `offered`。钩子可以替换真正发给模型的消息，
    /// 所以 `offered` 不一定是模型实际看到的工具声明。`poll` 传 `None`，届时再按 `cutoff`
    /// 重读上下文。
    messages: Option<Vec<Message>>,
    /// 来自轮询时设置，使仍然延迟的结果严格更晚轮询。
    poll_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Entry points shared with the rest of the harness
// ---------------------------------------------------------------------------

/// 追加一个被中断尝试留下的 partial 作为 `aborted` 的 assistant 条目（TS: `convertPartial`）。
///
/// 调用方负责替换或移除 `generation`。
///
/// 读的是 `live.generation.message` 的副本：把 `stopReason` 强制改成 `"aborted"` 后反序列化
/// 为 `AssistantMessage`，再经 `append_assistant` 追加（这条条目也会记用量）。`generation`
/// 或 `message` 缺失时直接返回 `Ok(())`，所以每个 abort/请求入口都可以无条件调用；live 草稿
/// 本身不会被修改。内容不是合法 assistant 消息时返回反序列化错误。
pub async fn convert_partial(
    tx: &dyn Tx,
    live: &mut DocDraft,
    conversation_id: ConversationId,
) -> Result<(), BoxError> {
    let partial = live
        .value()
        .get("generation")
        .and_then(|generation| generation.get("message"))
        .cloned();
    let Some(mut message) = partial else {
        return Ok(());
    };
    if let Some(object) = message.as_object_mut() {
        object.insert(
            "stopReason".to_owned(),
            JsonValue::String("aborted".to_owned()),
        );
    }
    let message: AssistantMessage = serde_json::from_value(message)?;
    append_assistant(tx, conversation_id, &message).await?;
    Ok(())
}

/// 追加一条 assistant 条目，并在同一次提交里把它的 usage 记进 `kiana.usage`。
///
/// 提醒：内建写入 assistant 条目的每条路径都走这里，用量账本才完整。
///
/// 用量按 `"provider/modelId"` 分桶（`UsageBucket::Models`）。`record_usage` 与
/// `append_entry` 共用调用方的 `tx`，所以账本与条目一起提交或一起回滚。
async fn append_assistant(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    message: &AssistantMessage,
) -> Result<crate::types::EntryRecord, BoxError> {
    record_usage(
        tx,
        conversation_id,
        UsageBucket::Models,
        &format!("{}/{}", message.provider, message.model),
        &message.usage,
    )
    .await?;
    let draft = AssistantEntry.typed_draft(TypedEntryDraft {
        model: Some(vec![Message::Assistant(message.clone())]),
        data: NoData,
        head: None,
        edits: None,
    })?;
    tx.append_entry(conversation_id, draft).await
}

/// 为 `inputs`（已放置的输入提交）启动一次运行：新的 generation 接管 `kiana.live.run`。
///
/// 任务创建与 `live.run = { taskId, inputs }` 在调用方的事务里一起提交；`inputs` 同时作为
/// `submissionIds` 传给任务创建，用来推导 trace 与 link。调用方只会在没有活动运行时调用
/// 它（前一次运行已经 `end_run`，或会话空闲）；本函数直接覆盖 `run`，不与旧值合并。
pub async fn start_run(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    live: &mut DocDraft,
    inputs: &[SubmissionId],
) -> Result<(), BoxError> {
    let task_id = create_generation(tx, conversation_id, Some(inputs)).await?;
    let mut run = serde_json::Map::new();
    run.insert("taskId".to_owned(), JsonValue::from(task_id.get()));
    run.insert(
        "inputs".to_owned(),
        JsonValue::Array(inputs.iter().map(|id| JsonValue::from(id.get())).collect()),
    );
    live.value_mut()
        .insert("run".to_owned(), JsonValue::Object(run));
    Ok(())
}

/// 把运行控制从 `from` 交给 `to`；运行的输入随之移动（TS: `handOver`）。
///
/// 只替换 `run.taskId`，`inputs` 原样保留（由接手的新 generation 继续负责结清）。当前
/// `run.taskId` 不等于 `from` 时什么也不做。
pub fn hand_over(live: &mut DocDraft, from: TaskId, to: TaskId) {
    let Some(run) = live
        .value_mut()
        .get_mut("run")
        .and_then(JsonValue::as_object_mut)
    else {
        return;
    };
    if run.get("taskId").and_then(JsonValue::as_i64) == Some(from.get()) {
        run.insert("taskId".to_owned(), JsonValue::from(to.get()));
    }
}

/// 创建一个会话拥有的 generation 任务（TS: `createGeneration`）。
///
/// 所有权是 `TaskOwnership::Conversation` 并显式绑定 `conversation_id`，任务输入是空对象。
/// `submission_ids` 只影响创建时的 trace/link（首条提交的 trace，其余追加 link），不写
/// `kiana.live.run`：运行控制由 [`start_run`] 或交接路径负责。
pub async fn create_generation(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    submission_ids: Option<&[SubmissionId]>,
) -> Result<TaskId, BoxError> {
    // `submissionIds` 让事务从首个提交的 trace 推出任务 trace，并为其余提交追加 link
    // （TS `Transaction.createTask`）。
    let mut options = crate::session::transaction::TaskCreationOptions::new(TaskOptions::new(
        TaskOwnership::Conversation,
    ));
    options.conversation_id = Some(conversation_id);
    options.submission_ids = submission_ids.map(<[SubmissionId]>::to_vec);
    tx.create_task_with(
        &GENERATION_TASK,
        JsonValue::Object(Default::default()),
        options,
    )
    .await
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

// 阶段结构体只是无状态的标记：`generation_task` 按名字把它们登记进阶段表，调度器按
// checkpoint 的 `phase` 字段选中一个并调用 `TaskPhase::run`；每个 `run` 都直接转发到本文件
// 里的同名自由函数。真正的状态转移发生在那些自由函数提交的 checkpoint 里，标记本身不持有
// 任何数据，`generation_task` 每次构造新的实例。

/// `prepare` 阶段。
pub struct PreparePhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable> for PreparePhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        prepare(task, runtime, context).await
    }
}

/// `request` 阶段。
pub struct RequestPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable> for RequestPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        request(task, runtime, context).await
    }
}

/// `retry` 阶段。
pub struct RetryPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable> for RetryPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        retry(task, runtime, context).await
    }
}

/// `poll` 阶段。
pub struct PollPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable> for PollPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        poll(task, runtime, context).await
    }
}

/// `tools` 阶段。
pub struct ToolsPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable> for ToolsPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        tools(task, runtime, context).await
    }
}

/// abort 处理器。
pub struct AbortPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable> for AbortPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, GenerationHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        abort(task, runtime, context).await
    }
}

// ---------------------------------------------------------------------------
// Checkpoint helpers
// ---------------------------------------------------------------------------

/// 当前任务状态里的 checkpoint 原样值。
///
/// `Terminal` / `Completing` 没有 checkpoint，返回错误；`abort` 有意用 `.ok()` 容忍这一点
/// （任务在收尾时被取消是正常路径）。`retry` / `poll` 用它把即将重新睡去的同一个 checkpoint
/// 交给 `sleep_until`，避免无意义地重新序列化。
fn raw_checkpoint(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
) -> Result<JsonValue, BoxError> {
    state_checkpoint(&task.state)
        .cloned()
        .ok_or_else(|| "Generation task has no checkpoint".into())
}

/// 把 checkpoint 解析成 [`GenerationCheckpoint`]。
///
/// 每个阶段开头都会校验自己拿到的变体；持久状态与处理器不符时返回
/// `"Generation task is not ..."`，而不是猜测如何继续。
fn parse_checkpoint(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
) -> Result<GenerationCheckpoint, BoxError> {
    let checkpoint = raw_checkpoint(task)?;
    Ok(serde_json::from_value(checkpoint)?)
}

/// checkpoint → JSON。序列化本身不会失败（纯数据结构），`Result` 只是为了匹配提交闭包的
/// 返回类型。
fn checkpoint_json(checkpoint: &GenerationCheckpoint) -> Result<JsonValue, BoxError> {
    Ok(serde_json::to_value(checkpoint)?)
}

/// 内建任务的运行端 ID（`R = JsonValue`）。
fn runtime_task_id(runtime: &Runtime<'_>) -> TaskId {
    runtime.task_id()
}

/// 子任务是否以 `completed` 结束（其余终态都视为失败）。
fn task_outcome_completed(outcome: Option<&TaskOutcome>) -> bool {
    matches!(outcome, Some(TaskOutcome::Completed { .. }))
}

/// 一次完成的压缩是否真的产出了摘要条目（`outcome.result.entryId !== undefined`）。
fn outcome_has_entry_id(outcome: Option<&TaskOutcome>) -> bool {
    matches!(
        outcome,
        Some(TaskOutcome::Completed { result })
            if result.get("entryId").and_then(JsonValue::as_i64).is_some()
    )
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

/// 阈值压缩的种类（TS: `"blocking" | "background"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThresholdCompaction {
    /// 阻塞：先压缩再重新准备。
    Blocking,
    /// 后台：本次请求照发，同时启动一次压缩。
    Background,
}

/// `prepare` 阶段：把「当前会话 + agent 配置」翻译成一次具体的请求。
///
/// 处理顺序固定如下：
///
/// 1. 读 agent 与 settings，解析 agent 的模型；没有模型时直接以 `no_model` 失败。
/// 2. 读 checkpoint，先验收上一次尝试留下的压缩子任务（两种来源的判定不同，见
///    [`PrepareCheckpoint`]）。
/// 3. 读当前上下文视图，按已有 system 消息重放已展示的分段，再渲染本阶段的分段。
/// 4. 规划 `kiana.system` 条目（分段 + 工具声明）并判定阈值压缩。
/// 5. 需要阻塞压缩时创建子任务、转入 `waiting`；结束后重新进入本阶段，此时 `compacted`
///    非空，第 4 步不再判定阈值。
/// 6. 否则在同一个提交里追加规划出的 system 条目，并把 checkpoint 换成 `request`。
async fn prepare(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &Runtime<'_>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let conversation_id = runtime.conversation_id();
    let agent = runtime.agent(context).await?;
    let settings = runtime.settings()?;
    // 先解析模型：模型缺失时不必依赖 checkpoint 的形状，直接走 no_model 失败路径。
    let model_ref = agent.model.clone();
    let resolved = model_ref.as_ref().and_then(|reference| {
        runtime
            .models()
            .get_model(&reference.provider, &reference.model_id)
    });
    let (Some(reference), Some(model)) = (model_ref, resolved) else {
        return fail_no_model(runtime, agent.model.as_ref(), context).await;
    };
    let GenerationCheckpoint::Prepare(checkpoint) = parse_checkpoint(task)? else {
        return Err("Generation task is not preparing".into());
    };
    let PrepareCheckpoint {
        attempt,
        compacted,
        overflow,
        threshold: blocked,
    } = checkpoint;
    // 溢出压缩的成功标准：子任务 completed 且结果里有 entryId。否则用当初保存的溢出文本
    // 原样失败——再请求一次只会得到同样的溢出。
    if let (Some(compacted), Some(overflow)) = (compacted, overflow.as_ref()) {
        let outcome = runtime
            .outcomes(&[id_from_number(compacted.get())], context)
            .await?
            .into_iter()
            .next();
        if !outcome_has_entry_id(outcome.as_ref()) {
            return fail_model_error(runtime, overflow.clone(), context).await;
        }
    }
    // 什么都没压或拒绝压缩的阈值压缩留下一个可能仍然放得下的请求；失败的那个留下一个它判定
    // 过大的上下文：再发出去最好的结果是没有给回答留空间。
    //
    // 与溢出分支不同，这里查不到子任务结局（`None`）不算失败，继续往下准备（TS 的
    // `outcome !== undefined` 语义）。
    if let (Some(compacted), Some(true)) = (compacted, blocked) {
        let outcome = runtime
            .outcomes(&[id_from_number(compacted.get())], context)
            .await?
            .into_iter()
            .next();
        if let Some(outcome) = outcome
            && !task_outcome_completed(Some(&outcome))
        {
            return fail_model_error(runtime, compaction_failure(&outcome), context).await;
        }
    }
    // 不截断的完整上下文：本次请求的内容与系统提示的规划都基于它。
    let view = runtime
        .context(conversation_id, context, ContextViewOptions::default())
        .await?;
    let shown = replay_sections(&view.messages);
    // 环境交给提示分段使用：`PromptInput.env` 的类型是 `Option<Arc<dyn ExecutionEnv>>`，而
    // `ExecutionEnv` 同时实现 `FileSystem` 与 `Shell`，所以分段不只能读文件、列目录，也能执行
    // 命令；本函数自己不直接使用它。
    // 构建失败不致命：非中止时上报为 host `env` 错误并继续，中止时把错误原样向上抛
    // （与 TS 的 try/catch 一致）。
    let env = match runtime.env(context).await {
        Ok(env) => env,
        Err(error) => {
            if context
                .abort_signal()
                .is_some_and(|signal| signal.is_aborted())
            {
                return Err(error);
            }
            runtime.report(
                error.as_ref(),
                Some(ReportFrom::Host {
                    callback: HostCallback::Env,
                }),
            );
            None
        }
    };
    // `shown` 是现有 system 消息已经展示的分段（渲染失败时用来保持旧文本）；`read` 让分段
    // 能读已提交的文档，其生命周期约束见 `prompt_reader`。
    let input = PromptInput {
        conversation_id,
        agent: Arc::clone(&agent),
        env,
        shown: shown.clone(),
        read: prompt_reader(runtime),
    };
    let mut report = |error: BoxError, section: &Arc<dyn PromptSection>| {
        let extension = section_extension(&agent, section.key());
        runtime.report(error.as_ref(), Some(ReportFrom::Extension { extension }));
    };
    let desired = render_sections(&agent.sections, &input, &shown, &mut report, context).await?;
    let tools: Vec<Tool> = agent
        .tools
        .iter()
        .map(|registration| registration.tool.clone())
        .collect();
    // 把渲染结果与视图里现有的 system 条目做差，得到本次需要追加的最少条目（含工具声明）。
    let entries = plan_system_entries(&view, &desired, &tools, runtime.now());
    let policy = compaction_policy(&settings, &reference);
    // 已经为这次尝试启动过压缩时不再判定阈值：否则压缩完成、重新 prepare 时会再次命中
    // 同一个阈值并反复创建压缩。
    let threshold = if compacted.is_none() {
        threshold_compaction(&view, &entries, model.context_window, &policy)
    } else {
        None
    };
    if threshold == Some(ThresholdCompaction::Blocking) {
        // 先压缩再重新准备；转录在压缩追加之前不变。
        //
        // 这个提交只做两件事：创建 generation 拥有的压缩子任务、把状态换成 `waiting`；
        // 不追加任何条目，因此恢复后的视图仍是压缩前的会话。
        let task_id = runtime_task_id(runtime);
        let change = TaskCommit::new(move |tx, _current| {
            Box::pin(async move {
                let child = create_compaction(
                    tx,
                    conversation_id,
                    CompactionInput {
                        reason: CompactionReason::Threshold,
                        instructions: None,
                    },
                    Some(task_id),
                )
                .await?;
                let checkpoint = GenerationCheckpoint::Prepare(PrepareCheckpoint {
                    attempt,
                    compacted: Some(child),
                    overflow: None,
                    threshold: Some(true),
                });
                Ok(Some(TaskState::Waiting {
                    checkpoint: checkpoint_json(&checkpoint)?,
                    on: vec![id_from_number(child.get())],
                    policy: JoinPolicy::AllSettled,
                }))
            })
        });
        return runtime
            .commit(&change, context, CommitOptions::default())
            .await;
    }
    // 后台压缩不阻塞本次请求：只在同一提交里登记一次会话拥有的压缩，请求照发。
    let background = threshold == Some(ThresholdCompaction::Background);
    let stream_options = settings.stream.clone();
    let thinking_level = agent.thinking_level;
    let change = TaskCommit::new(move |tx, _current| {
        let entries = entries.clone();
        let reference = reference.clone();
        let stream_options = stream_options.clone();
        Box::pin(async move {
            // cutoff 初值取会话当前最新条目（`scan_entries` 默认降序，limit 1 即第一条）；
            // 之后每追加一条 system 条目就把它推进到该条目的 id，请求因此包含刚写入的条目。
            let query = EntryQuery {
                conversation_id,
                min_entry_id: None,
                max_entry_id: None,
                order: None,
            };
            let mut cutoff = tx
                .scan_entries(&query, 1, None)
                .await?
                .items
                .first()
                .map(|entry| entry.id);
            for entry in &entries {
                let draft = SystemEntry.typed_draft(entry.clone())?;
                let record = tx.append_entry(conversation_id, draft).await?;
                cutoff = Some(record.id);
            }
            // 会话完全没有条目、本次也没有要追加的 system 条目时，请求没有内容可发。
            let Some(cutoff) = cutoff else {
                return Err(
                    format!("Conversation {conversation_id} has no entries to send").into(),
                );
            };
            // 在这次提交里检查，准备期间被允许的压缩因此算数：`compactions` 非空说明已经有
            // 压缩在活动，不再重复创建。
            if background {
                let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
                let live = tx.doc(&target).await?;
                if !live.value().contains_key("compactions") {
                    create_compaction(
                        tx,
                        conversation_id,
                        CompactionInput {
                            reason: CompactionReason::Threshold,
                            instructions: None,
                        },
                        None,
                    )
                    .await?;
                }
            }
            let checkpoint = GenerationCheckpoint::Request(RequestCheckpoint {
                attempt,
                compacted,
                model: reference,
                thinking_level,
                stream_options,
                cutoff,
            });
            Ok(Some(TaskState::Running {
                checkpoint: checkpoint_json(&checkpoint)?,
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// `prepare` 决定启动哪种阈值压缩（spec §8.3）：超过 `contextWindow - reserveTokens` 时阻塞，
/// 超过后台阈值时后台，且只有范围选择找得到切点时才有。
///
/// 计算分三步：
///
/// 1. 策略关闭、模型没有 context window（`None` 或 `0`）时不压缩；`0` 对应 JS 的
///    `contextWindow <= 0`。
/// 2. 估计这次请求的大小：当前 `view` 的 token 数，加上规划中 system 条目携带的模型消息
///    （`entry.model`）——新分段与工具声明也占上下文，不能只看已有转录。
/// 3. 阻塞阈值是 `window - reserveTokens`，后台阈值再低 `backgroundTokens`；比较用严格
///    大于。`backgroundTokens <= 0` 时后台压缩关闭。
///
/// 最后要求 `select_cut` 能找到切点。它返回 `None` 有两种情形：没有可压缩的前缀，或整段上下文
/// 都放得进 `keepRecentTokens`（`select_cut` 的「边界」）。两种情况下启动压缩都不会产出摘要，
/// 所以这里返回 `None` 表示本次不压缩。
fn threshold_compaction(
    view: &ContextView,
    planned: &[SystemDraft],
    context_window: Option<u64>,
    policy: &CompactionPolicy,
) -> Option<ThresholdCompaction> {
    if !policy.enabled {
        return None;
    }
    // TS: `contextWindow <= 0`；`undefined` 会让后续算术变成 NaN，同样落空。
    let window = context_window.filter(|window| *window != 0)?;
    let extra: Vec<Message> = planned
        .iter()
        .flat_map(|entry| entry.model.clone().unwrap_or_default())
        .collect();
    let tokens = estimate_context(view, &extra);
    let blocking = window as f64 - policy.reserve_tokens;
    let background = blocking - policy.background_tokens;
    let over = if tokens > blocking {
        Some(ThresholdCompaction::Blocking)
    } else if policy.background_tokens > 0.0 && tokens > background {
        Some(ThresholdCompaction::Background)
    } else {
        None
    }?;
    select_cut(view, policy.keep_recent_tokens)?;
    Some(over)
}

/// 一次没有完成的阻塞压缩的错误文本。
///
/// `Failed`/`Faulted` 取错误消息，`Orphaned` 取原因，`Aborted` 用固定文本
/// `Compaction was aborted`（当前稳定契约，改动需同步测试）。
/// `Completed` 分支只为让 match 穷尽：唯一调用点已经排除了 completed 的子任务。
fn compaction_failure(outcome: &TaskOutcome) -> String {
    match outcome {
        TaskOutcome::Failed { error, .. } | TaskOutcome::Faulted { error } => {
            format!("Compaction failed: {}", error.message)
        }
        TaskOutcome::Orphaned { reason } => format!("Compaction failed: {reason}"),
        TaskOutcome::Aborted { .. } => "Compaction was aborted".to_owned(),
        TaskOutcome::Completed { .. } => "Compaction completed".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// request / retry / poll
// ---------------------------------------------------------------------------

/// `request` 阶段：把 checkpoint 里固定的那次请求发出去。
///
/// 流程：
///
/// 1. 先提交一次清场：若有上一次中断留下的 partial，把它追加成 `aborted` 条目
///    （`convert_partial`），并把 `kiana.live.generation` 重置为 `{ attempt }`，确保展示里
///    只有当前尝试。
/// 2. 重新解析模型——从写下 checkpoint 到真正发请求之间模型可能已被移除。
/// 3. 以 `cutoff` 派生请求上下文，跑 `beforeRequest` 钩子链：有返回就替换消息；处理器出错时
///    中止则向上抛，否则上报并跳过（`hook_failure`）。
/// 4. 确保 provider 会话 id（`kiana.provider` 文档不存在时会另起一次提交创建并写入 UUID；
///    文档存在但 `sessionId` 缺失或不是字符串时按 TS 返回 `None`、不补写，见
///    `ensure_provider_session_id`），把设置快照翻译成 kiana-ai 请求选项。
/// 5. 流式请求，把终态消息交给 `classify`。
///
/// 交给 `classify` 的 `Request.messages` 是**钩子之前**的 `view.messages`：工具轮
/// 用这份快照推导 `offered`；钩子替换了真正发送的消息时，工具可用性仍按快照判定。
async fn request(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &Runtime<'_>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let GenerationCheckpoint::Request(checkpoint) = parse_checkpoint(task)? else {
        return Err("Generation task is not requesting".into());
    };
    let RequestCheckpoint {
        attempt,
        compacted,
        model: reference,
        thinking_level,
        stream_options,
        cutoff,
    } = checkpoint;
    let conversation_id = runtime.conversation_id();
    // 清场提交：旧的 partial 先变成本次尝试之前的 `aborted` 条目，`generation` 只保留 attempt。
    let change = TaskCommit::new(move |tx, _current| {
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            convert_partial(tx, &mut live, conversation_id).await?;
            live.value_mut().insert(
                "generation".to_owned(),
                serde_json::json!({ "attempt": attempt }),
            );
            Ok(None)
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await?;
    // 重新解析模型：checkpoint 只存 provider/modelId，等待期间注册表可能变化。
    let Some(model) = runtime
        .models()
        .get_model(&reference.provider, &reference.model_id)
    else {
        return fail_no_model(runtime, Some(&reference), context).await;
    };
    // 截到 `cutoff` 的视图是 `beforeRequest` 钩子链的输入；钩子返回替换时可以改写真正发送的
    // 消息（下面循环里的 `messages`），但恢复后重发仍以同一 `cutoff` 为界，不会看到更晚的条目。
    let view = runtime
        .context(
            conversation_id,
            context,
            ContextViewOptions { at: Some(cutoff) },
        )
        .await?;
    let mut messages = view.messages.clone();
    let api = hook_api(runtime);
    for hook in
        hook_handlers::<dyn GenerationHooks, _, _, _, _>(runtime, "beforeRequest", context).await?
    {
        match hook
            .handler
            .before_request(
                BeforeRequestInput {
                    messages: messages.clone(),
                },
                Arc::clone(&api),
                context,
            )
            .await
        {
            Ok(Some(replaced)) => messages = replaced.messages,
            Ok(None) => {}
            // TS `hooks.each`：普通错误上报并跳过，只有中止才向上传播。
            Err(error) => hook_failure(
                runtime.signal().is_aborted(),
                hook.extension.as_deref(),
                error,
                |error, from| runtime.report(error, from),
            )?,
        }
    }
    let signal = runtime.signal();
    // provider 会话 id 是跨请求复用的粘性标识。`kiana.provider` 文档不存在时，这个调用会另起
    // 一次提交把它创建出来并写入新的 UUID；文档已存在但 `sessionId` 缺失或不是字符串时按 TS
    // 返回 `None`，不补写，本次请求也就不带会话 id。
    let session_id = ensure_provider_session_id(runtime, context).await?;
    let mut options = simple_stream_options(
        &stream_options,
        Some(&signal),
        context.telemetry(),
        session_id,
        None,
        stream_options.cache_retention,
    );
    // off 时不设置 reasoning：该字段保持缺省，不向 provider 发送 reasoning 配置。
    if thinking_level != ModelThinkingLevel::Off {
        options.reasoning = thinking_level_option(thinking_level);
    }
    let message = stream_response(runtime, &model, &messages, options, attempt, context).await?;
    // `messages` 固定用钩子之前的视图快照，供工具轮推导 `offered`：钩子即使替换了真正发送的
    // 消息，工具可用性也按这份快照判定（与 TS 相同）。`poll_at: None` 表示这是普通请求而不是
    // 延迟轮询。
    let request = Request {
        attempt,
        compacted,
        model: reference,
        thinking_level: Some(thinking_level),
        cutoff,
        messages: Some(view.messages),
        poll_at: None,
    };
    classify(runtime, &request, message, context).await
}

/// 退避的睡眠之后；在 `until` 之前的运行（提前唤醒，或旧版本的记录）再睡一次。
///
/// 到点后把 `attempt` 加一、丢弃失败现场（`overflow`/`threshold` 归零，只保留 `compacted`），
/// 回到 `prepare`。是否值得重试不在这里判断：`classify` 已经按 `attempt <= maxRetries`
/// 决定了进入退避，所以本阶段只负责把记录送回准备流程（除非准备本身失败）。
async fn retry(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &Runtime<'_>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let GenerationCheckpoint::Retry(checkpoint) = parse_checkpoint(task)? else {
        return Err("Generation task is not retrying".into());
    };
    let until = checkpoint.until;
    if until > runtime.now() {
        return runtime
            .sleep_until(until, raw_checkpoint(task)?, context)
            .await;
    }
    // 退避消耗一次尝试：下一次请求的 attempt 是这次失败的 attempt + 1。
    let attempt = checkpoint.attempt + 1;
    let compacted = checkpoint.compacted;
    let conversation_id = runtime.conversation_id();
    let change = TaskCommit::new(move |tx, _current| {
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            live.value_mut().insert(
                "generation".to_owned(),
                serde_json::json!({ "attempt": attempt }),
            );
            let checkpoint = GenerationCheckpoint::Prepare(PrepareCheckpoint {
                attempt,
                compacted,
                overflow: None,
                threshold: None,
            });
            Ok(Some(TaskState::Running {
                checkpoint: checkpoint_json(&checkpoint)?,
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 睡到 `pollAt` 之后；在它之前的运行（提前唤醒，或旧版本的记录）再睡一次。
///
/// 到点后用 checkpoint 里的 handle 取回延迟结果。`fetch_deferred` 返回 `AssistantMessage`
/// 而不是 `Result`：流以错误收场时由 kiana-ai 生成一条 error 终态消息，`classify` 因此能
/// 用同一套压缩/重试/失败逻辑处理延迟响应。`base` 只带 signal 与 telemetry——结果已经在
/// provider 侧生成，不需要重发原请求的选项。
async fn poll(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &Runtime<'_>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let GenerationCheckpoint::Poll(checkpoint) = parse_checkpoint(task)? else {
        return Err("Generation task is not polling".into());
    };
    let PollCheckpoint {
        attempt,
        compacted,
        model: reference,
        thinking_level,
        cutoff,
        handle,
        poll_at,
    } = checkpoint;
    if poll_at > runtime.now() {
        return runtime
            .sleep_until(poll_at, raw_checkpoint(task)?, context)
            .await;
    }
    // 模型可能已在等待期间被移除。
    let Some(model) = runtime
        .models()
        .get_model(&reference.provider, &reference.model_id)
    else {
        return fail_no_model(runtime, Some(&reference), context).await;
    };
    // 取回延迟结果只需要中止信号与 telemetry，不重发请求选项。
    let base = StreamOptions {
        signal: Some(runtime.signal().token().clone()),
        telemetry: Some(context.telemetry()),
        ..Default::default()
    };
    let options = kiana_ai::models::ModelsDeferredFetchOptions {
        base: kiana_ai::types::DeferredFetchOptions { base, wait: None },
        ..Default::default()
    };
    let message = runtime
        .models()
        .fetch_deferred(&model, &handle, options)
        .await;
    // `messages: None` 表示工具轮需要时再按 `cutoff` 重读上下文；`poll_at` 让 `classify`
    // 在新的延迟结果上继续保证轮询时间递增。
    let request = Request {
        attempt,
        compacted,
        model: reference,
        thinking_level,
        cutoff,
        messages: None,
        poll_at: Some(poll_at),
    };
    classify(runtime, &request, message, context).await
}

// ---------------------------------------------------------------------------
// tools
// ---------------------------------------------------------------------------

/// `tools` 阶段：顺序轮里启动下一个工具调用，或收尾整轮。
///
/// `pending` 非空时只取第一个 call ID：为它创建工具任务、回填 live 槽位，并以 `waiting`
/// 只等这个子任务；它结束后调度器再次进入本阶段，下一个调用才会启动——顺序执行就是靠
/// 「一次只启动一个」实现的。`pending` 为空时交给 `finish_tool_round` 收尾；并行轮的工具
/// 任务在 `start_tool_round` 里已一次性创建，因此第一次进入本阶段时 `pending` 就是空的。
async fn tools(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &Runtime<'_>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let GenerationCheckpoint::Tools(checkpoint) = parse_checkpoint(task)? else {
        return Err("Generation task is not running tools".into());
    };
    let ToolsCheckpoint {
        assistant,
        tools,
        pending,
    } = checkpoint;
    // 没有待启动的调用：整轮结束，交收尾逻辑处理工具控制与后续运行。
    let Some((next, rest)) = pending.split_first() else {
        return finish_tool_round(runtime, assistant, &tools, context).await;
    };
    let next = next.clone();
    let rest = rest.to_vec();
    let task_id = runtime_task_id(runtime);
    let conversation_id = runtime.conversation_id();
    let change = TaskCommit::new(move |tx, _current| {
        let next = next.clone();
        let tools = tools.clone();
        let rest = rest.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            let child = create_tool_task(tx, task_id, assistant, &next).await?;
            // 只回填匹配 callId 且还没有 taskId 的槽位：并行轮里已启动的槽位不会被覆盖。
            if let Some(slots) = live
                .value_mut()
                .get_mut("tools")
                .and_then(JsonValue::as_array_mut)
                && let Some(object) = slots
                    .iter_mut()
                    .find(|slot| {
                        slot.get("callId").and_then(JsonValue::as_str) == Some(next.as_str())
                            && slot.get("taskId").is_none()
                    })
                    .and_then(JsonValue::as_object_mut)
            {
                object.insert("taskId".to_owned(), JsonValue::from(child.get()));
            }
            let mut all = tools;
            all.push(child);
            let checkpoint = GenerationCheckpoint::Tools(ToolsCheckpoint {
                assistant,
                tools: all,
                pending: rest,
            });
            Ok(Some(TaskState::Waiting {
                checkpoint: checkpoint_json(&checkpoint)?,
                on: vec![id_from_number(child.get())],
                policy: JoinPolicy::AllSettled,
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

// ---------------------------------------------------------------------------
// classify / answer / tool round
// ---------------------------------------------------------------------------

/// 在一个提交里分类终态的 provider 消息，并清掉 partial。
///
/// 这是「一次请求结束后做什么」的唯一出口，`request` 与 `poll` 都走到这里。判定顺序：
///
/// 1. 已中止就直接返回中止错误；已提交状态的清理留给 `abort` 处理器。
/// 2. 把请求时用的思考等级写回消息（TS 也这样做），转录因此记录实际档位。
/// 3. 延迟响应（`stopReason == "deferred"` 且有句柄）：写 `poll` checkpoint，任务转入
///    `sleeping`；这一轮不跑 `afterResponse`。
/// 4. 跑 `afterResponse` 钩子：普通错误上报并跳过，中止向上抛。
/// 5. 有工具调用且 `stopReason == toolUse` → 进入工具轮；`stop`/`length`/`toolUse`（但没有
///    调用）→ 产出最终回答。
/// 6. 其余情况读当前设置判定：上下文溢出、本次尝试还没压缩过、压缩开启且找得到切点 →
///    追加这条错误消息、创建溢出压缩、转回 `prepare` 等待；否则按重试策略决定是退避
///    `sleeping` 还是失败终态。
///
/// 需要 `prepare_failure` 读表时，它必须在提交的第一次表写入之前执行（见模块文档的
/// 「先读表、后写表」）。
async fn classify(
    runtime: &Runtime<'_>,
    request: &Request,
    mut message: AssistantMessage,
    context: &dyn Context,
) -> Result<(), BoxError> {
    // 提交闭包要读 `runtime.now()`（TS 在 commit 回调里求值）；闭包只活到 `commit` 返回。
    let runtime = static_runtime(runtime);
    // 中止标记或关闭：abort 调用或重新打开的运行处理已提交状态。
    if runtime.signal().is_aborted() {
        return Err(Box::new(abort_error(&runtime.signal())));
    }
    let conversation_id = runtime.conversation_id();
    let Request {
        attempt,
        compacted,
        model: reference,
        thinking_level,
        cutoff,
        ..
    } = request.clone();
    // 记录请求的等级，正如 kiana-agent-core 的 agent loop 所做的。
    if let Some(level) = thinking_level {
        message.thinking_level = Some(level);
    }
    // 延迟响应的下一次轮询点：至少等 provider 要求的时长，且严格晚于上一次轮询（`+1`），
    // 两者取大后 floor 到整毫秒。
    if message.stop_reason == StopReason::Deferred && message.deferred.is_some() {
        let handle = message.deferred.clone().expect("checked above");
        let poll_after = handle.poll_after_ms.unwrap_or(DEFAULT_POLL_AFTER_MS);
        let previous = request
            .poll_at
            .map(|at| at as f64 + 1.0)
            .unwrap_or(f64::NEG_INFINITY);
        let poll_at = ((runtime.now() as f64) + poll_after).max(previous).floor() as i64;
        let change = TaskCommit::new(move |tx, _current| {
            let handle = handle.clone();
            let reference = reference.clone();
            Box::pin(async move {
                let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
                let mut live = tx.doc(&target).await?;
                live.value_mut().insert(
                    "generation".to_owned(),
                    serde_json::json!({
                        "attempt": attempt,
                        "deferred": { "pollAt": poll_at },
                    }),
                );
                let checkpoint = GenerationCheckpoint::Poll(PollCheckpoint {
                    attempt,
                    compacted,
                    model: reference,
                    thinking_level,
                    cutoff,
                    handle,
                    poll_at,
                });
                Ok(Some(TaskState::Sleeping {
                    checkpoint: checkpoint_json(&checkpoint)?,
                    until: poll_at,
                }))
            })
        });
        return runtime
            .commit(&change, context, CommitOptions::default())
            .await;
    }
    // `afterResponse` 在延迟分支之后：延迟结果每次轮询都会重新分类，但只有最终消息才跑钩子。
    let api = hook_api(runtime);
    for hook in
        hook_handlers::<dyn GenerationHooks, _, _, _, _>(runtime, "afterResponse", context).await?
    {
        match hook
            .handler
            .after_response(message.clone(), Arc::clone(&api), context)
            .await
        {
            Ok(()) => {}
            Err(error) => hook_failure(
                runtime.signal().is_aborted(),
                hook.extension.as_deref(),
                error,
                |error, from| runtime.report(error, from),
            )?,
        }
    }
    // 工具调用只认 content 里的 toolCall 块；没有调用的 `toolUse` 会落到下面的最终回答分支。
    let calls: Vec<ToolCall> = message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContentBlock::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .collect();
    if message.stop_reason == StopReason::ToolUse && !calls.is_empty() {
        return start_tool_round(runtime, request, &message, &calls, context).await;
    }
    if matches!(
        message.stop_reason,
        StopReason::Stop | StopReason::Length | StopReason::ToolUse
    ) {
        return answer(runtime, &message, context).await;
    }
    // 重试与压缩策略管辖下一次尝试，所以现在读取，而不是在准备时固定。
    let settings = runtime.settings()?;
    // 这里只传 `None`：只按错误文本判定溢出；静默溢出与 length 截断的判定需要 context
    // window，本调用点没有提供。
    let overflow = message.stop_reason == StopReason::Error && is_context_overflow(&message, None);
    let compaction = compaction_policy(&settings, &reference);
    // 每个尝试最多做一次溢出压缩：`compacted` 非空说明已经压缩过，再溢出就只能失败。
    if overflow && compacted.is_none() && compaction.enabled {
        let view = runtime
            .context(
                conversation_id,
                context,
                ContextViewOptions { at: Some(cutoff) },
            )
            .await?;
        if select_cut(&view, compaction.keep_recent_tokens).is_some() {
            let text = message
                .error_message
                .clone()
                .unwrap_or_else(|| "Context overflow".to_owned());
            let task_id = runtime_task_id(runtime);
            let message_for_commit = message.clone();
            let text_for_commit = text.clone();
            let change = TaskCommit::new(move |tx, _current| {
                let message = message_for_commit.clone();
                let text = text_for_commit.clone();
                Box::pin(async move {
                    let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
                    let mut live = tx.doc(&target).await?;
                    // 先追加这条失败的回答（含用量），再清掉 partial 展示并创建压缩子任务；
                    // checkpoint 保存溢出文本，供 `prepare` 判定压缩是否真的产出了摘要。
                    append_assistant(tx, conversation_id, &message).await?;
                    live.value_mut().shift_remove("generation");
                    let child = create_compaction(
                        tx,
                        conversation_id,
                        CompactionInput {
                            reason: CompactionReason::Overflow,
                            instructions: None,
                        },
                        Some(task_id),
                    )
                    .await?;
                    let checkpoint = GenerationCheckpoint::Prepare(PrepareCheckpoint {
                        attempt,
                        compacted: Some(child),
                        overflow: Some(text),
                        threshold: None,
                    });
                    Ok(Some(TaskState::Waiting {
                        checkpoint: checkpoint_json(&checkpoint)?,
                        on: vec![id_from_number(child.get())],
                        policy: JoinPolicy::AllSettled,
                    }))
                })
            });
            return runtime
                .commit(&change, context, CommitOptions::default())
                .await;
        }
    }
    let policy = settings.retry.clone();
    // 溢出从不重试：只有压缩能让下一次请求放得下。
    // `attempt` 从 1 开始，`attempt <= maxRetries` 表示首次请求之外最多再重试 `maxRetries` 次；
    // 这不是模型请求总数的上界：溢出压缩后的重发（`prepare`/`request` 在同一 `attempt` 下再发
    // 一次）与崩溃后按同一 checkpoint 重放都不消耗 `attempt`。
    let retry = message.stop_reason == StopReason::Error
        && !overflow
        && is_retryable_assistant_error(&message)
        && policy.enabled
        && attempt <= policy.max_retries;
    let until = if retry {
        runtime.now()
            + retry_delay_ms_js(
                policy.base_delay_ms,
                policy.max_agent_delay_ms,
                attempt as f64,
            ) as i64
    } else {
        0
    };
    // 失败文本优先用 provider 的 errorMessage；没有时用 stop reason 拼出模板文本
    // （当前稳定契约，改动需同步测试）。
    let text = message.error_message.clone().unwrap_or_else(|| {
        format!(
            "Model response ended with stop reason {}",
            stop_reason_name(message.stop_reason)
        )
    });
    // live 展示的退避错误文本；缺省为空串（TS 的 `errorMessage ?? ""`）。
    let error_message = message.error_message.clone().unwrap_or_default();
    let task_id = runtime_task_id(runtime);
    let settings = Arc::clone(&settings);
    let change = TaskCommit::new(move |tx, _current| {
        let message = message.clone();
        let text = text.clone();
        let error_message = error_message.clone();
        let settings = Arc::clone(&settings);
        Box::pin(async move {
            // 在第一次表写入之前读取；只有失败会用到它。
            let failure = if retry {
                None
            } else {
                Some(prepare_failure(tx, conversation_id, &settings).await?)
            };
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            // 无论退避还是最终失败，这次尝试的消息都追加进转录（含错误与用量）。
            append_assistant(tx, conversation_id, &message).await?;
            let Some(failure) = failure else {
                live.value_mut().insert(
                    "generation".to_owned(),
                    serde_json::json!({
                        "attempt": attempt,
                        "retry": { "at": until, "error": error_message },
                    }),
                );
                let checkpoint = GenerationCheckpoint::Retry(RetryCheckpoint {
                    attempt,
                    compacted,
                    until,
                });
                return Ok(Some(TaskState::Sleeping {
                    checkpoint: checkpoint_json(&checkpoint)?,
                    until,
                }));
            };
            end_failed_run(
                tx,
                runtime,
                conversation_id,
                &mut live,
                task_id,
                failure,
                &SubmissionSettlement::Unanswered {
                    reason: "model_error".to_owned(),
                    detail: Some(JsonValue::String(text.clone())),
                },
            )
            .await?;
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: TaskOutcomeError {
                        message: text,
                        detail: Some(serde_json::json!({ "reason": "model_error" })),
                    },
                    result: None,
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 最终回答；`final` 边界放置排队的项（spec §6）。
///
/// `onYield` 钩子先跑：第一个返回 continuation 的处理器胜出，之后遍历到的处理器直接跳过。
/// 钩子抛错时中止向上抛，否则上报并跳过——回答仍然提交。
///
/// 提交里依次：准备边界（读表）→ 追加回答条目 → 构造 `completed` 终态 → 应用 final 边界。
/// 若钩子给了 continuation 且没有排队用户、也没有排队 reset，就用它作为 user 消息新建一个
/// generation 并交接运行；此时**不调用** `end_run`，`run.inputs` 留给接手者结清。否则结束
/// 本次运行（输入结清为 `done`），有被放置的排队用户时立即启动下一次运行。
async fn answer(
    runtime: &Runtime<'_>,
    message: &AssistantMessage,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let runtime = static_runtime(runtime);
    let mut continuation: Option<UserInput> = None;
    // 第一个 continuation 胜出；后续处理器仍被遍历但直接跳过。
    let api = hook_api(runtime);
    for hook in
        hook_handlers::<dyn GenerationHooks, _, _, _, _>(runtime, "onYield", context).await?
    {
        if continuation.is_some() {
            continue;
        }
        match hook
            .handler
            .on_yield(message.clone(), Arc::clone(&api), context)
            .await
        {
            Ok(Some(output)) => continuation = Some(output.continue_run),
            Ok(None) => {}
            // 抛错的处理器被上报并跳过：回答仍然提交（TS `hooks.each`）。
            Err(error) => hook_failure(
                runtime.signal().is_aborted(),
                hook.extension.as_deref(),
                error,
                |error, from| runtime.report(error, from),
            )?,
        }
    }
    let conversation_id = runtime.conversation_id();
    let settings = runtime.settings()?;
    let task_id = runtime_task_id(runtime);
    let message = message.clone();
    let change = TaskCommit::new(move |tx, _current| {
        let message = message.clone();
        let continuation = continuation.clone();
        let settings = Arc::clone(&settings);
        Box::pin(async move {
            // 队列模式在边界决定时读 Session 线。
            // 边界要读表，必须在这次提交的第一次表写入（追加回答条目）之前准备好。
            let mut boundary =
                prepare_boundary(tx, conversation_id, QueueModes::from_settings(&settings)).await?;
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            let entry = append_assistant(tx, conversation_id, &message).await?;
            // 结果指向刚追加的回答条目。
            let result = GenerationResult {
                entry_id: Some(entry.id),
            };
            let terminal = TaskState::Terminal {
                outcome: TaskOutcome::Completed {
                    result: serde_json::to_value(result)?,
                },
            };
            // 与 TS 一样在提交里读时钟：边界决定和 continuation 的 user 消息各读一次。
            let BoundaryResult { users, reset } =
                apply_boundary(tx, &mut boundary, BoundaryAt::Final, runtime.now()).await?;
            // continuation 只在没有排队用户、也没有排队 reset 时生效：否则排队输入优先，
            // 本次回答照常结束运行。
            if let Some(continuation) = continuation
                && users.is_empty()
                && !reset
            {
                let user = UserMessage {
                    role: UserRole::User,
                    content: continuation,
                    timestamp: runtime.now(),
                };
                let draft = UserEntry.typed_draft(TypedEntryDraft {
                    model: Some(vec![Message::User(user)]),
                    data: NoData,
                    head: None,
                    edits: None,
                })?;
                tx.append_entry(conversation_id, draft).await?;
                let child = create_generation(tx, conversation_id, None).await?;
                // 交接运行控制但保留 `inputs` 列表：新的 generation 负责结清它们。
                hand_over(&mut live, task_id, child);
                live.value_mut().shift_remove("generation");
                return Ok(Some(terminal));
            }
            end_run(
                tx,
                live.value_mut(),
                task_id,
                &SubmissionSettlement::Done { answer: entry.id },
            );
            if !users.is_empty() {
                start_run(tx, conversation_id, &mut live, &users).await?;
            }
            Ok(Some(terminal))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 在一个提交里追加发起工具调用的回答并启动它的工具轮（spec §8.3）。
///
/// 步骤：
///
/// 1. 在提交之外准备判断所需的信息：消息快照（正常请求用 `request.messages`，延迟轮询没有
///    快照时按 `cutoff` 重读）、`offered` 工具集合、agent 的工具表与设置。
/// 2. 决定是否顺序执行：设置整体要求顺序，或任一**已提供**的调用其工具声明为顺序模式。
/// 3. 提交里：追加 assistant 条目；逐个调用分流——未提供的工具立即写 `tool_unavailable`
///    结果；顺序轮里已有任务时把 call ID 放进 `pending`；否则创建工具任务。最后写 live
///    槽位、删掉 `generation`，以 `waiting` 等已创建的任务（并行轮是全部，顺序轮是第一个）。
///
/// `offered` 取自 `request.messages`（普通请求是**跑 `beforeRequest` 之前**的 `view.messages`；
/// 延迟轮询没有快照时按 `cutoff` 重读），由 `get_current_tools` 从消息里的工具声明算出，而不是
/// agent 的完整工具表。`beforeRequest` 钩子可以替换真正发给模型的消息，这里仍按该快照判定工具
/// 可用性（与 TS 相同），因此 `offered` 与模型实际看到的工具声明可能不一致。
async fn start_tool_round(
    runtime: &Runtime<'_>,
    request: &Request,
    message: &AssistantMessage,
    calls: &[ToolCall],
    context: &dyn Context,
) -> Result<(), BoxError> {
    let runtime = static_runtime(runtime);
    let conversation_id = runtime.conversation_id();
    // 普通请求手里有钩子之前的消息快照；延迟轮询没有，按 `cutoff` 重读同一段上下文。
    let messages = match &request.messages {
        Some(messages) => messages.clone(),
        None => {
            runtime
                .context(
                    conversation_id,
                    context,
                    ContextViewOptions {
                        at: Some(request.cutoff),
                    },
                )
                .await?
                .messages
        }
    };
    // `offered` 来自上面那份消息快照，而不是模型实际收到的消息：钩子改写消息时，不在快照里的
    // 工具名仍会被判为不可用，即使 agent 里注册过。
    let offered: HashSet<String> = get_current_tools(&messages)
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    // 轮次开始时读取；工具在它的工具任务解析它时解析。
    let agent = runtime.agent(context).await?;
    let tools = agent.tools.clone();
    let settings = runtime.settings()?;
    // 顺序模式：设置整体要求，或任一已提供的调用把工具声明为顺序。
    let sequential = settings.tool_execution
        == crate::harness::types::ToolExecutionMode::Sequential
        || calls.iter().any(|call| {
            offered.contains(&call.name)
                && tools
                    .iter()
                    .find(|tool| tool.name() == call.name)
                    .and_then(|tool| tool.execution_mode)
                    .is_some_and(|mode| {
                        mode == crate::harness::types::ToolExecutionMode::Sequential
                    })
        });
    let task_id = runtime_task_id(runtime);
    let message = message.clone();
    let calls = calls.to_vec();
    let change = TaskCommit::new(move |tx, _current| {
        let message = message.clone();
        let calls = calls.clone();
        let offered = offered.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            let entry = append_assistant(tx, conversation_id, &message).await?;
            let mut slots: Vec<JsonValue> = Vec::new();
            let mut task_ids: Vec<TaskId<ToolTaskResult>> = Vec::new();
            let mut pending: Vec<String> = Vec::new();
            // 按调用顺序分流：未提供 → 立即错误结果；顺序轮后续 → 排队；其余 → 创建任务。
            for call in &calls {
                if !offered.contains(&call.name) {
                    // 模型调用了不在 `offered` 快照里的工具：当场写错误结果，不创建任务。
                    let unavailable = harness_error(
                        "tool_unavailable",
                        &format!("Tool {} is not available", call.name),
                    );
                    let result = append_tool_result(
                        tx,
                        conversation_id,
                        call,
                        &unavailable,
                        runtime.now(),
                        None,
                    )
                    .await?;
                    slots.push(serde_json::json!({
                        "callId": call.id,
                        "name": call.name,
                        "status": "done",
                        "entry": result.id.get(),
                    }));
                    continue;
                }
                if sequential && !task_ids.is_empty() {
                    // 顺序轮：前面已经有任务，先只记 call ID，等前一个任务结束再启动。
                    pending.push(call.id.clone());
                    slots.push(serde_json::json!({
                        "callId": call.id,
                        "name": call.name,
                        "status": "pending",
                    }));
                    continue;
                }
                // 并行轮的全部调用、或顺序轮的第一个调用：立即创建子任务。
                let child = create_tool_task(tx, task_id, entry.id, &call.id).await?;
                task_ids.push(child);
                slots.push(serde_json::json!({
                    "callId": call.id,
                    "name": call.name,
                    "taskId": child.get(),
                    "status": "pending",
                }));
            }
            // live 槽位保存整轮展示（含未提供、尚未启动的调用）；checkpoint 只记任务 id
            // 与待启动的 call ID。
            live.value_mut().shift_remove("generation");
            live.value_mut()
                .insert("tools".to_owned(), JsonValue::Array(slots));
            let checkpoint = GenerationCheckpoint::Tools(ToolsCheckpoint {
                assistant: entry.id,
                tools: task_ids.clone(),
                pending,
            });
            // 并行轮等全部任务；顺序轮只等刚创建的那个，`pending` 留到下次进入 `tools`。
            Ok(Some(TaskState::Waiting {
                checkpoint: checkpoint_json(&checkpoint)?,
                on: task_ids.iter().map(|id| id_from_number(id.get())).collect(),
                policy: JoinPolicy::AllSettled,
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 本轮的工具有了终态：应用它们的控制、追加 `handoff` 的 reset，然后在最终边界结束运行
/// （`terminate`）、在排队的 reset 处结束、或在 `postTools` 边界交给下一次 generation
/// （有 handoff 时在它的新上下文里，spec §8.5）。
///
/// 控制取自每个工具任务结果里的 `control`：`addTools` 追加给下一轮，`terminate` 要求本轮
/// 全部调用都请求终止，`handoff` 取调用顺序里最后一个（`controls` 反向查找）。
///
/// `terminate` 要求 live `tools` 里**每个**槽都带 `taskId` 且对应 control 的 `terminate`
/// 为 true；没有任务的槽（未提供的工具、尚未启动的顺序调用）拿不到 control，因此整轮无法
/// terminate。三个分支都返回 `completed` 终态（提交本身出错时阶段直接返回错误，不写终态），
/// `entryId` 固定为 `assistant`（发起这轮调用的回答），即使随后交接给了新的 generation。
async fn finish_tool_round(
    runtime: &Runtime<'_>,
    assistant: EntryId,
    tools: &[TaskId<ToolTaskResult>],
    context: &dyn Context,
) -> Result<(), BoxError> {
    let runtime = static_runtime(runtime);
    let conversation_id = runtime.conversation_id();
    let ids: Vec<TaskId> = tools.iter().map(|id| id_from_number(id.get())).collect();
    // `outcomes` 与 `tools` 同序；`controls` 因此也能按下标与任务对齐（失败/中止的任务为 None）。
    let outcomes = runtime.outcomes(&ids, context).await?;
    let controls: Vec<Option<ToolControl>> = tools
        .iter()
        .enumerate()
        .map(|(index, _)| match outcomes.get(index) {
            // 版本 1 的结果没有 kind；generation 轮次读到的每个结果都是模型发起的调用。
            Some(TaskOutcome::Completed { result }) => {
                if result.get("kind").and_then(JsonValue::as_str) == Some("nested") {
                    None
                } else {
                    result.get("control").and_then(|control| {
                        serde_json::from_value::<ToolControl>(control.clone()).ok()
                    })
                }
            }
            _ => None,
        })
        .collect();
    // 从已提交快照读本轮槽位：`results` 是 afterTools 钩子要的工具结果条目；`terminate`
    // 之后也要求每个槽都带 taskId。
    let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
    let snapshot = runtime.snapshot(&target, context).await?;
    let slots: Vec<JsonValue> = snapshot
        .as_ref()
        .and_then(|live| live.get("tools"))
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();
    let results: Vec<EntryId> = slots
        .iter()
        .filter_map(|slot| slot.get("entry").and_then(JsonValue::as_i64))
        .map(id_from_number)
        .collect();
    let api = hook_api(runtime);
    for hook in
        hook_handlers::<dyn GenerationHooks, _, _, _, _>(runtime, "afterTools", context).await?
    {
        match hook
            .handler
            .after_tools(assistant, results.clone(), Arc::clone(&api), context)
            .await
        {
            Ok(()) => {}
            Err(error) => hook_failure(
                runtime.signal().is_aborted(),
                hook.extension.as_deref(),
                error,
                |error, from| runtime.report(error, from),
            )?,
        }
    }
    // 槽位存的是任务 id，controls 按 `tools` 下标对齐，这里按 id 找回它的 control。
    let control_for = |id: TaskId| -> Option<&Option<ToolControl>> {
        tools
            .iter()
            .position(|tool| tool.get() == id.get())
            .map(|index| &controls[index])
    };
    // 轮次的每个调用（包括没有任务就回答的）都必须请求终止。
    // 没有 taskId 的槽（未提供的工具、尚未启动的顺序调用）在这里恒不满足，整轮不会 terminate。
    let terminate = !slots.is_empty()
        && slots.iter().all(|slot| {
            slot.get("taskId")
                .and_then(JsonValue::as_i64)
                .map(id_from_number)
                .is_some_and(|id| {
                    control_for(id)
                        .and_then(|control| control.as_ref())
                        .and_then(|control| control.terminate)
                        == Some(true)
                })
        });
    // 按任务顺序拼接所有 `addTools`；去重与已有工具的合并交给 `add_tools`。
    let added: Vec<String> = controls
        .iter()
        .filter_map(|control| control.as_ref())
        .flat_map(|control| control.add_tools.clone().unwrap_or_default())
        .collect();
    // 调用顺序里最后一个 handoff 胜出。
    let handoff: Option<String> = controls
        .iter()
        .rev()
        .filter_map(|control| control.as_ref())
        .find_map(|control| control.handoff.clone());
    let settings = runtime.settings()?;
    let task_id = runtime_task_id(runtime);
    let change = TaskCommit::new(move |tx, _current| {
        let added = added.clone();
        let handoff = handoff.clone();
        let settings = Arc::clone(&settings);
        Box::pin(async move {
            // 边界读表必须在任何写入之前；`add_tools` 与 live/条目写入都排在它后面。
            let mut boundary =
                prepare_boundary(tx, conversation_id, QueueModes::from_settings(&settings)).await?;
            if !added.is_empty() {
                add_tools(tx, conversation_id, &added).await?;
            }
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            // TS 在提交里读一次时钟，handoff 与边界决定共用。
            let now = runtime.now();
            if let Some(handoff) = handoff {
                // handoff 的 reset 排在每个排队项之前，因此排队的写入与被选中的 steer 落在它的
                // 上下文里。
                let user = UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text(handoff),
                    timestamp: now,
                };
                let draft = ResetEntry.typed_draft(TypedEntryDraft {
                    model: Some(vec![Message::User(user)]),
                    data: NoData,
                    head: Some(EntryHead::SelfHead),
                    edits: None,
                })?;
                let entry = tx.append_entry(conversation_id, draft).await?;
                boundary.head = Some(entry.id);
            }
            // terminate：所有调用都要求结束 → final 边界，运行以 done（answer=assistant）收尾。
            if terminate {
                let BoundaryResult { users, .. } =
                    apply_boundary(tx, &mut boundary, BoundaryAt::Final, now).await?;
                end_run(
                    tx,
                    live.value_mut(),
                    task_id,
                    &SubmissionSettlement::Done { answer: assistant },
                );
                if !users.is_empty() {
                    start_run(tx, conversation_id, &mut live, &users).await?;
                }
            } else {
                // 否则走 postTools 边界：排队项可以插在工具结果之后、下一次模型请求之前。
                let BoundaryResult { users, reset } =
                    apply_boundary(tx, &mut boundary, BoundaryAt::PostTools, now).await?;
                if reset {
                    // 排队的 reset 在回答之前切断了运行的上下文。
                    end_run(
                        tx,
                        live.value_mut(),
                        task_id,
                        &SubmissionSettlement::Unanswered {
                            reason: "reset".to_owned(),
                            detail: None,
                        },
                    );
                    if !users.is_empty() {
                        start_run(tx, conversation_id, &mut live, &users).await?;
                    }
                } else {
                    // 没有 reset：清掉本轮展示；边界放置的用户项并入本次运行的输入列表，
                    // 交给下一个 generation 结清（运行控制随 `hand_over` 转交）。
                    live.value_mut().shift_remove("tools");
                    live.value_mut().shift_remove("nestedTools");
                    if let Some(run) = live
                        .value_mut()
                        .get_mut("run")
                        .and_then(JsonValue::as_object_mut)
                        && run.get("taskId").and_then(JsonValue::as_i64) == Some(task_id.get())
                        && let Some(inputs) =
                            run.get_mut("inputs").and_then(JsonValue::as_array_mut)
                    {
                        for user in &users {
                            inputs.push(JsonValue::from(user.get()));
                        }
                    }
                    let child = create_generation(tx, conversation_id, None).await?;
                    hand_over(&mut live, task_id, child);
                }
            }
            // 结果固定为发起本轮工具调用的 assistant 条目（交接后也指回它）。
            let result = GenerationResult {
                entry_id: Some(assistant),
            };
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Completed {
                    result: serde_json::to_value(result)?,
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

// ---------------------------------------------------------------------------
// abort
// ---------------------------------------------------------------------------

/// 取消处理器：把这次运行收尾成 `aborted`。
///
/// 步骤：
///
/// 1. 解析 checkpoint；`Terminal`/`Completing` 没有 checkpoint，用 `.ok()` 让这些记录也能
///    走完通用清理。
/// 2. `poll` 状态下尽力取消 provider 侧的延迟响应；取消失败只上报为 host `models` 错误，
///    不影响本地收尾。
/// 3. `tools` 状态下按 checkpoint 的 `pending` 读回调用参数。abort 在本轮工具任务终态之后
///    运行（spec §8.5），所以只有这些从未启动的调用需要补 `aborted` 结果。
/// 4. 提交：把 live 里的 partial 转成 `aborted` 回答条目、为未启动调用写工具结果、`end_run`
///    把 `run.inputs` 结清为 `unanswered`/`aborted`，任务终态为 `TaskOutcome::Aborted`。
async fn abort(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &Runtime<'_>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let runtime = static_runtime(runtime);
    let checkpoint = parse_checkpoint(task).ok();
    // 延迟响应还在 provider 侧：尽力取消；失败只上报，不影响本地状态。
    if let Some(GenerationCheckpoint::Poll(poll)) = &checkpoint {
        let model = runtime
            .models()
            .get_model(&poll.model.provider, &poll.model.model_id);
        if let Some(model) = model {
            let base = StreamOptions {
                signal: Some(runtime.signal().token().clone()),
                telemetry: Some(context.telemetry()),
                ..Default::default()
            };
            let options = kiana_ai::models::ModelsDeferredCancelOptions {
                base,
                ..Default::default()
            };
            if let Err(error) = runtime
                .models()
                .cancel_deferred(&model, &poll.handle, options)
                .await
            {
                runtime.report(
                    &error,
                    Some(ReportFrom::Host {
                        callback: HostCallback::Models,
                    }),
                );
            }
        }
    }
    let conversation_id = runtime.conversation_id();
    // 在本轮的工具任务终态之后运行；从未启动的调用得到 `aborted` 结果（spec §8.5）。
    let unstarted = match &checkpoint {
        Some(GenerationCheckpoint::Tools(tools)) => {
            read_calls(runtime, tools.assistant, &tools.pending, context).await?
        }
        _ => Vec::new(),
    };
    let task_id = runtime_task_id(runtime);
    let change = TaskCommit::new(move |tx, _current| {
        let unstarted = unstarted.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            // 已提交的 partial 变成 stopReason=aborted 的 assistant 条目（也记用量）。
            convert_partial(tx, &mut live, conversation_id).await?;
            // 为每个从未启动的调用补一条 aborted 工具结果。
            for call in &unstarted {
                let result = harness_error("aborted", &format!("Tool {} was aborted", call.name));
                // TS 在提交里为每个未启动的调用读时钟。
                append_tool_result(tx, conversation_id, call, &result, runtime.now(), None).await?;
            }
            // 结清本次运行的输入，并移除 generation/tools 展示（`end_run` 总是删除它们）。
            end_run(
                tx,
                live.value_mut(),
                task_id,
                &SubmissionSettlement::Unanswered {
                    reason: "aborted".to_owned(),
                    detail: None,
                },
            );
            // 取消不产出结果；`reason: None` 表示没有更具体的取消原因。
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Aborted {
                    reason: None,
                    result: None,
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// assistant 条目按给定顺序的 `call_ids` 调用（TS: `readCalls`）。
///
/// 即按 `call_ids` 的顺序从条目里读回工具调用。只读 `model[0]`，且仅当它是 assistant
/// 消息；调用按 id 精确匹配，找不到的 id 静默跳过。每个 id 最多返回一次（每次 `find`
/// 取第一条匹配的调用）。
async fn read_calls(
    runtime: &Runtime<'_>,
    assistant: EntryId,
    call_ids: &[String],
    context: &dyn Context,
) -> Result<Vec<ToolCall>, BoxError> {
    let message = runtime
        .entry(assistant, context)
        .await?
        .and_then(|record| record.model)
        .and_then(|mut messages| messages.drain(..).next());
    let calls: Vec<ToolCall> = match message {
        Some(Message::Assistant(assistant)) => assistant
            .content
            .into_iter()
            .filter_map(|content| match content {
                AssistantContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut result = Vec::new();
    for id in call_ids {
        if let Some(call) = calls.iter().find(|call| &call.id == id) {
            result.push(call.clone());
        }
    }
    Ok(result)
}

/// 一个属于 generation 的、用于调用 `call_id` 的工具任务。
///
/// 输入是 `{ kind: "model", assistant, callId }`；所有权为当前 generation
/// （`TaskOwnership::Task`），`finish_tool_round` 之后能从 checkpoint 里的任务 id 取回结局。
async fn create_tool_task(
    tx: &dyn Tx,
    owner: TaskId,
    assistant: EntryId,
    call_id: &str,
) -> Result<TaskId<ToolTaskResult>, BoxError> {
    let options = TaskOptions::new(TaskOwnership::Task {
        task_id: id_from_number(owner.get()),
    });
    let task_id = tx
        .create_task(
            &TOOL_TASK,
            serde_json::json!({ "kind": "model", "assistant": assistant, "callId": call_id }),
            options,
        )
        .await?;
    Ok(id_from_number(task_id.get()))
}

// ---------------------------------------------------------------------------
// Failure handling
// ---------------------------------------------------------------------------

/// 失败运行结束时从表里需要的东西，在提交的第一次表写入之前读好。
///
/// `boundary` 只在策略是 `Continue` 时存在：只有它需要在失败时立即放置排队输入。`Keep` 不读
/// 表；`Withdraw` 也会读 `kiana.inbox`，但那次读取发生在 `end_failed_run` 里随后的
/// `withdraw_queued_inputs`，不是在这里提前准备的。
/// 结构体本身不做事，只是把「已读好的边界」传过提交闭包。
struct Failure {
    /// `settings.afterFailedRun` 的快照。
    policy: AfterFailedRun,
    /// 已准备的 final 边界；仅 `Continue` 有。
    boundary: Option<Boundary>,
}

/// 按 `afterFailedRun` 决定失败时要读哪些表。
///
/// 只有 `Continue` 需要在失败时立即放置排队项，因此只有它准备边界。调用点必须保证这发生
/// 在提交的任何表写入之前（`prepare_boundary` 的约束）。
async fn prepare_failure(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    settings: &Settings,
) -> Result<Failure, BoxError> {
    let policy = settings.after_failed_run;
    if policy != AfterFailedRun::Continue {
        return Ok(Failure {
            policy,
            boundary: None,
        });
    }
    let boundary =
        prepare_boundary(tx, conversation_id, QueueModes::from_settings(settings)).await?;
    Ok(Failure {
        policy,
        boundary: Some(boundary),
    })
}

/// 结清一次失败的运行，并按 `settings.afterFailedRun` 处理排在它后面的输入（spec §6）。
///
/// 顺序是：`end_run`（按 `settlement` 结清本次运行的输入并清掉展示）→ `Withdraw` 时撤回
/// 队列里剩余的输入（`unanswered`/`run_failed`；排队的写入保留）→ `Continue` 时应用已准备
/// 的 final 边界，并把放置出的用户输入立刻启动成下一次运行。`Keep` 只做第一步。
async fn end_failed_run(
    tx: &dyn Tx,
    runtime: &Runtime<'_>,
    conversation_id: ConversationId,
    live: &mut DocDraft,
    task_id: TaskId,
    failure: Failure,
    settlement: &SubmissionSettlement,
) -> Result<(), BoxError> {
    end_run(tx, live.value_mut(), task_id, settlement);
    if failure.policy == AfterFailedRun::Withdraw {
        withdraw_queued_inputs(tx, conversation_id, Some("run_failed")).await?;
    }
    let Some(mut boundary) = failure.boundary else {
        return Ok(());
    };
    // TS 在提交里读时钟（`applyBoundary(..., runtime.now())`）。
    let BoundaryResult { users, .. } =
        apply_boundary(tx, &mut boundary, BoundaryAt::Final, runtime.now()).await?;
    if !users.is_empty() {
        start_run(tx, conversation_id, live, &users).await?;
    }
    Ok(())
}

/// 把运行的输入以 `unanswered`/`model_error` 结清，并以 `text` 失败。
///
/// `text` 同时是失败任务的错误消息、提交记录的 detail，以及 `TaskOutcome::Failed.error`
/// 的 `message`；`detail` 里的 `reason` 固定为 `"model_error"`。`afterFailedRun` 的三种处理
/// 由 `end_failed_run` 执行。
async fn fail_model_error(
    runtime: &Runtime<'_>,
    text: String,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let runtime = static_runtime(runtime);
    let conversation_id = runtime.conversation_id();
    let settings = runtime.settings()?;
    let task_id = runtime_task_id(runtime);
    let change = TaskCommit::new(move |tx, _current| {
        let text = text.clone();
        let settings = Arc::clone(&settings);
        Box::pin(async move {
            // 先读表（`prepare_failure` 可能准备边界），再打开 live 草稿。
            let failure = prepare_failure(tx, conversation_id, &settings).await?;
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            end_failed_run(
                tx,
                runtime,
                conversation_id,
                &mut live,
                task_id,
                failure,
                &SubmissionSettlement::Unanswered {
                    reason: "model_error".to_owned(),
                    detail: Some(JsonValue::String(text.clone())),
                },
            )
            .await?;
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: TaskOutcomeError {
                        message: text,
                        detail: Some(serde_json::json!({ "reason": "model_error" })),
                    },
                    result: None,
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 把运行的输入以 `unanswered`/`no_model` 结清并失败。
///
/// 消息分两种：agent 没有配置模型时是 `"No model is configured"`；配置了但注册表里找不到
/// 时是 `"Model {provider}/{modelId} is not available"`。结算记录**不带 detail**，而任务的
/// `detail` 是 `{ "reason": "no_model" }`（当前稳定契约，改动需同步测试）。
async fn fail_no_model(
    runtime: &Runtime<'_>,
    reference: Option<&ModelRef>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let runtime = static_runtime(runtime);
    let message = match reference {
        None => "No model is configured".to_owned(),
        Some(reference) => format!(
            "Model {}/{} is not available",
            reference.provider, reference.model_id
        ),
    };
    let conversation_id = runtime.conversation_id();
    let settings = runtime.settings()?;
    let task_id = runtime_task_id(runtime);
    let change = TaskCommit::new(move |tx, _current| {
        let message = message.clone();
        let settings = Arc::clone(&settings);
        Box::pin(async move {
            // 先读表（`prepare_failure` 可能准备边界），再打开 live 草稿。
            let failure = prepare_failure(tx, conversation_id, &settings).await?;
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            end_failed_run(
                tx,
                runtime,
                conversation_id,
                &mut live,
                task_id,
                failure,
                &SubmissionSettlement::Unanswered {
                    reason: "no_model".to_owned(),
                    detail: None,
                },
            )
            .await?;
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: TaskOutcomeError {
                        message,
                        detail: Some(serde_json::json!({ "reason": "no_model" })),
                    },
                    result: None,
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// 流式请求一次并返回终态消息。partial 作为拖尾写入最多每 `progress.partialIntervalMs`
/// （默认 100ms）提交一次，一次只有一个提交在飞，且延迟持久化（`PROGRESS_COMMIT`）；结束时
/// 停止节流，因此不会有过期的 partial 落在结局之后。提交追加文本与思考增长的部分，并应用
/// 自上次提交以来的参数变更（`PartialChanges`），代价随新文本而非字符串长度增长。
///
/// 执行结构（与 TS 的浮动 promise 版不同，见模块文档）：
///
/// 1. `take_receiver` 取事件通道；取不到时退化为只等 `stream.result()`，不产生 partial
///    提交。
/// 2. 事件循环用 `tokio::select!` 同时等事件与计时器通道。计时器到点就把 `pending` 里的
///    最新 partial 提交一次再继续；事件到达时更新 `PartialChanges` 并刷新 `pending`。
/// 3. `Done`/`Error` 与没有 `partial` 的事件被跳过；`content` 为空的开场 partial 不提交。
/// 4. 流结束（通道关闭）时先撤销计时器，再 `await` 最终结果；`pending` 里未提交的 partial
///    被丢弃，避免旧 partial 落在终态之后。
///
/// 提交是内联 `await` 的，因此事件处理与提交不会并行（模块文档里的差异说明）。
async fn stream_response(
    runtime: &Runtime<'_>,
    model: &Model,
    messages: &[Message],
    options: kiana_ai::types::SimpleStreamOptions,
    attempt: i64,
    context: &dyn Context,
) -> Result<AssistantMessage, BoxError> {
    let interval = runtime.settings()?.progress.partial_interval_ms;
    // 工具声明由 `kiana.system` 条目携带（见 `plan_system_entries`），消息里已经包含它们，
    // 所以这里的 `tools` 留空。
    let stream = runtime.models().stream_simple(
        model,
        &AiContext {
            system_prompt: None,
            messages: messages.to_vec(),
            tools: None,
        },
        options.into(),
    );
    let Some(mut receiver) = stream.take_receiver() else {
        // 没有 tokio 运行时（take_receiver 不可用）：只等最终结果。
        // 它也会在事件转发已被取走时返回 None；这里的流是新建的，正常只会在无运行时时发生。
        return Ok(stream.result().await?);
    };
    // 所有 partial 提交共用一个收集器；只有这个事件循环会 `observe`/`take` 它。
    let changes = Arc::new(Mutex::new(PartialChanges::new()));
    // 计时器与事件循环之间只传一个「到点」信号；真正的提交在循环里串行执行。
    let (timer_tx, mut timer_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut timer: Option<crate::timers::Disarm> = None;
    let mut pending: Option<AssistantMessage> = None;
    loop {
        // 有计时器在等时，事件与到点信号二选一；否则直接阻塞等下一个事件。
        let event = if timer.is_some() {
            tokio::select! {
                event = receiver.recv() => event,
                _ = timer_rx.recv() => {
                    timer = None;
                    if let Some(partial) = pending.take() {
                        commit_partial(runtime, context, &changes, &partial, attempt).await?;
                    }
                    continue;
                }
            }
        } else {
            receiver.recv().await
        };
        let Some(event) = event else {
            break;
        };
        // 没有内容、例如 kiana-ai 的开场 `start` 事件什么都显示不了；延迟响应永远过不了它，
        // 因此它从不留下 partial。
        if matches!(
            event,
            kiana_ai::types::AssistantMessageEvent::Done { .. }
                | kiana_ai::types::AssistantMessageEvent::Error { .. }
        ) {
            continue;
        }
        let Some(partial) = event.partial() else {
            continue;
        };
        if partial.content.is_empty() {
            continue;
        }
        changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(&event);
        // `pending` 始终是最新 partial，到点才提交；连续事件不会各自触发提交。
        pending = Some(partial.clone());
        if timer.is_none() {
            timer = Some(arm_timer(runtime, &timer_tx, interval));
        }
    }
    // 流先结束：撤销还没触发的计时器，未提交的 pending 直接丢弃。
    if let Some(disarm) = timer.take() {
        disarm();
    }
    let message = stream.result().await?;
    Ok(message)
}

/// 用运行端的计时器在 `interval` 之后通知事件循环。
///
/// 计时器回调的类型是同步的 `Fn()`，不能在里面 `await` 提交；它只向无界通道发一个 `()`，
/// 由事件循环的 `select!` 分支执行提交。返回的撤销函数在流结束时调用，避免请求结束后仍有
/// 计时器触发。
fn arm_timer(
    runtime: &Runtime<'_>,
    sender: &tokio::sync::mpsc::UnboundedSender<()>,
    interval: f64,
) -> crate::timers::Disarm {
    let sender = sender.clone();
    let callback: crate::timers::TimerCallback = Arc::new(move || {
        let _ = sender.send(());
    });
    let disarm = runtime.set_timer(callback, interval);
    Box::new(disarm)
}

/// 提交一个 partial：同步复制（provider 还在改它的 partial），取走自上次提交以来的变化，
/// 然后在一个延迟持久化的提交里写 `kiana.live.generation.message`。提交失败时把变化放回去，
/// 并像 TS 的浮动 promise 一样只报告（不打断流）。
///
/// 细节：
///
/// - `serde_json::to_value` 是同步深拷贝；之后的提交闭包只操作副本，provider 继续修改原
///   partial 不会与提交竞争。
/// - `generation` 可能已被别的提交删掉（中止、重试、收尾），提交时按需补一个 `{ attempt }`。
/// - 提交用 `PROGRESS_COMMIT`（延迟持久化），下一次写 `kiana.live` 的提交或定时器才落盘。
/// - `PartialChanges` 的 `take`/`commit` 都在互斥锁内；锁中毒被当作可用状态处理。
/// - 提交失败时 `restore` 把取走的增量放回去，下次提交重新应用；除非运行已中止，否则上报。
///   无论提交成功与否都返回 `Ok(())`，进度写失败不会打断流；只有序列化失败会向上抛。
async fn commit_partial(
    runtime: &Runtime<'_>,
    context: &dyn Context,
    changes: &Arc<Mutex<PartialChanges>>,
    partial: &AssistantMessage,
    attempt: i64,
) -> Result<(), BoxError> {
    let conversation_id = runtime.conversation_id();
    let mut message = serde_json::to_value(partial)?;
    let taken = changes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take(&mut message);
    let message_for_commit = message.clone();
    let taken_for_commit = taken.clone();
    let changes_for_commit = Arc::clone(changes);
    let change = TaskCommit::new(move |tx, _current| {
        let changes = Arc::clone(&changes_for_commit);
        let mut message = message_for_commit.clone();
        let taken = taken_for_commit.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            let value = live.value_mut();
            if !value.contains_key("generation") {
                value.insert(
                    "generation".to_owned(),
                    serde_json::json!({ "attempt": attempt }),
                );
            }
            let generation = value
                .get_mut("generation")
                .expect("generation exists after the insert above");
            changes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .commit(generation, &mut message, &taken);
            Ok(None)
        })
    });
    match runtime.commit(&change, context, PROGRESS_COMMIT).await {
        Ok(()) => Ok(()),
        Err(error) => {
            changes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .restore(&taken);
            // 中止标记或关闭之后的拒绝是预期内的；已提交状态仍然一致。
            if !runtime.signal().is_aborted() {
                runtime.report(error.as_ref(), None);
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt input / small helpers
// ---------------------------------------------------------------------------

/// `PromptInput.read` 的运行端视图。
///
/// `PromptInput.read` 的类型是 `Arc<dyn DocumentReader>`（没有生命周期参数），所以这里持有
/// 的是 `prompt_reader` 延长出来的 `'static` 借用；真正可用的范围见那里的 SAFETY 说明。
/// 两个方法只是把调用转发给运行端。
struct RuntimeDocuments {
    runtime: &'static Runtime<'static>,
}

#[async_trait]
impl DocumentReader for RuntimeDocuments {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        DocumentReader::snapshot(self.runtime, target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        DocumentReader::snapshot_as_of(self.runtime, target, at, context).await
    }
}

/// 为一次分段渲染构造 `Arc<dyn DocumentReader>`。
///
/// `PromptInput.read` 没有生命周期参数，而运行端只以借用形式传给阶段；这里把借用延长到
/// `'static`，**只允许在紧随其后的 `render_sections` 调用期间使用**：该 `Arc` 在 `prepare`
/// 返回前就被丢弃，因此引用永远不会超过运行端。
///
/// # Safety（修改前必须重新确认的前提）
///
/// - 阶段签名拿到的 `&Runtime` 在整个阶段调用期间有效，`prepare` 返回前它不会失效。
/// - `'static` 只是类型层面的谎报；真正的约束是 `Arc` 不能逃出本次阶段调用。它只被放进
///   `PromptInput { read }`，而 `input` 是 `prepare` 的局部变量，随函数返回一起丢弃。
/// - 若某个 `PromptSection` 实现把 `read` 存进全局状态或跨任务继续使用，阶段返回后再调用
///   它就是悬垂引用（未定义行为）。新增使用点时必须保持“本阶段内用完即弃”。
fn prompt_reader(runtime: &Runtime<'_>) -> Arc<dyn DocumentReader> {
    // SAFETY: 见上。
    let runtime: &'static Runtime<'static> = unsafe { std::mem::transmute(runtime) };
    Arc::new(RuntimeDocuments { runtime })
}

/// 思考档位 → kiana-ai 的 `ThinkingLevel`；`Off` 映射为 `None`（请求里不发送 reasoning）。
fn thinking_level_option(level: ModelThinkingLevel) -> Option<kiana_ai::types::ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(kiana_ai::types::ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(kiana_ai::types::ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(kiana_ai::types::ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(kiana_ai::types::ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(kiana_ai::types::ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(kiana_ai::types::ThinkingLevel::Max),
    }
}

/// TS `message.stopReason` 的字面量。
///
/// 用于失败文本 `Model response ended with stop reason ...`，字面量是当前稳定契约，改动需
/// 同步测试（`toolUse` 是驼峰，其余小写）。
fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        StopReason::Deferred => "deferred",
        StopReason::TurnCapped => "turnCapped",
        StopReason::TokenCapped => "tokenCapped",
        StopReason::CostCapped => "costCapped",
        StopReason::TimeCapped => "timeCapped",
    }
}

// ---------------------------------------------------------------------------
// Task definition
// ---------------------------------------------------------------------------

/// 内建 generation 任务定义（TS: `GenerationTask`）。
///
/// 五个阶段按 `prepare`、`request`、`retry`、`poll`、`tools` 的顺序登记（测试
/// `the_task_registers_every_phase_it_commits_to` 锁定了这个顺序）；`initial` 固定返回
/// `{ phase: "prepare", attempt: 1 }`。
///
/// 版本是 1 且 `migrate: None`：没有更早的格式需要迁移，但也意味着 checkpoint 形状一旦
/// 改动，升级前处于等待/睡眠的任务将无法恢复。
///
/// 这里不挂任务级 `hooks` 表；hook 处理器由运行端（注册表）提供，各阶段用
/// `hook_handlers` 按名字取。
pub fn generation_task() -> crate::types::Task<JsonValue, JsonValue, JsonValue, GenerationHookTable>
{
    let mut phases: IndexMap<
        String,
        Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, GenerationHookTable>>,
    > = IndexMap::new();
    phases.insert("prepare".to_owned(), Arc::new(PreparePhase));
    phases.insert("request".to_owned(), Arc::new(RequestPhase));
    phases.insert("retry".to_owned(), Arc::new(RetryPhase));
    phases.insert("poll".to_owned(), Arc::new(PollPhase));
    phases.insert("tools".to_owned(), Arc::new(ToolsPhase));
    crate::types::tasks::define_task(TaskDefinition {
        name: GENERATION_TASK_KIND.to_owned(),
        version: 1,
        initial: Arc::new(|_input| serde_json::json!({ "phase": "prepare", "attempt": 1 })),
        phases,
        abort: Arc::new(AbortPhase),
        migrate: None,
        hooks: None,
    })
}

/// 擦除后的内建 generation 任务（注册表用）。
///
/// 只构造一次；`registry::BUILTIN_TASKS` 克隆它放进每个 registry 的内建任务表。类型擦除
/// （`AnyTask`）是调度器按名字取阶段所必需的，代价是阶段内部要自己解析 JSON checkpoint。
pub static GENERATION_TASK: LazyLock<crate::harness::types::AnyTask> =
    LazyLock::new(|| crate::harness::live::erase_task_definition(&generation_task().definition));

#[cfg(test)]
mod tests {
    // 这些测试锁定对外可见、可持久化的形状与纯函数行为，而不是阶段编排：
    // - checkpoint 的 JSON 形状（`phase` 标签、camelCase 字段、可选字段的省略）；
    // - 阈值压缩的判定与「必须先找得到切点」这一前置条件；
    // - 失败文本、`hand_over` 的运行控制转移、stop reason 字面量；
    // - assistant 消息序列化时省略未定义成员（partial 提交的形状）。
    use kiana_ai::types::{
        AssistantMessage, StopReason, TextContent, TextKind, UserContent, UserContentBlock,
        UserMessage, UserRole,
    };
    use serde_json::json;

    use super::*;
    use crate::types::EntryRecord;

    fn user_message(text: &str) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_owned()),
            timestamp: 0,
        })
    }

    fn entry(id: i64) -> EntryRecord {
        EntryRecord {
            key_order: None,
            model: None,
            data: None,
            edits: None,
            kind: "kiana.user".to_owned(),
            id: id_from_number(id),
            conversation_id: id_from_number(1),
            head: None,
            by_task_id: None,
        }
    }

    fn view_with(messages: Vec<Message>, contributions: Vec<Vec<Message>>) -> ContextView {
        ContextView {
            head: None,
            entries: contributions
                .iter()
                .enumerate()
                .map(|(index, _)| entry(index as i64 + 1))
                .collect(),
            contributions,
            messages,
        }
    }

    fn policy(enabled: bool, reserve: f64, keep_recent: f64, background: f64) -> CompactionPolicy {
        CompactionPolicy {
            enabled,
            reserve_tokens: reserve,
            keep_recent_tokens: keep_recent,
            background_tokens: background,
        }
    }

    #[test]
    fn the_task_registers_every_phase_it_commits_to() {
        let task = generation_task();
        let keys: Vec<&str> = task.definition.phases.keys().map(String::as_str).collect();
        assert_eq!(keys, ["prepare", "request", "retry", "poll", "tools"]);
        assert_eq!(
            (task.definition.initial)(JsonValue::Object(Default::default())),
            json!({ "phase": "prepare", "attempt": 1 })
        );
        assert_eq!(task.definition.name, GENERATION_TASK_KIND);
        assert_eq!(task.definition.version, 1);
    }

    #[test]
    fn every_checkpoint_keeps_its_phase_tag_and_camel_case_fields() {
        let prepare = GenerationCheckpoint::Prepare(PrepareCheckpoint {
            attempt: 2,
            compacted: Some(id_from_number(9)),
            overflow: Some("too long".to_owned()),
            threshold: Some(true),
        });
        assert_eq!(
            serde_json::to_value(&prepare).unwrap(),
            json!({
                "phase": "prepare",
                "attempt": 2,
                "compacted": 9,
                "overflow": "too long",
                "threshold": true,
            })
        );
        assert_eq!(
            serde_json::from_value::<GenerationCheckpoint>(serde_json::to_value(&prepare).unwrap())
                .unwrap(),
            prepare
        );

        let request = GenerationCheckpoint::Request(RequestCheckpoint {
            attempt: 1,
            compacted: Some(id_from_number(4)),
            model: ModelRef {
                provider: "p".to_owned(),
                model_id: "m".to_owned(),
            },
            thinking_level: ModelThinkingLevel::High,
            stream_options: ConversationStreamOptions::default(),
            cutoff: id_from_number(12),
        });
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["phase"], json!("request"));
        assert_eq!(value["thinkingLevel"], json!("high"));
        assert_eq!(value["streamOptions"], json!({}));
        assert_eq!(value["cutoff"], json!(12));
        assert_eq!(value["model"], json!({ "provider": "p", "modelId": "m" }));
        assert_eq!(
            serde_json::from_value::<GenerationCheckpoint>(value).unwrap(),
            request
        );

        let retry = GenerationCheckpoint::Retry(RetryCheckpoint {
            attempt: 3,
            compacted: None,
            until: 1000,
        });
        assert_eq!(
            serde_json::to_value(&retry).unwrap(),
            json!({ "phase": "retry", "attempt": 3, "until": 1000 })
        );
        assert_eq!(
            serde_json::from_value::<GenerationCheckpoint>(serde_json::to_value(&retry).unwrap())
                .unwrap(),
            retry
        );

        let poll = GenerationCheckpoint::Poll(PollCheckpoint {
            attempt: 1,
            compacted: None,
            model: ModelRef {
                provider: "p".to_owned(),
                model_id: "m".to_owned(),
            },
            thinking_level: None,
            cutoff: id_from_number(7),
            handle: DeferredHandle {
                provider: "p".to_owned(),
                model_id: "m".to_owned(),
                api: "a".to_owned(),
                id: "h".to_owned(),
                ..Default::default()
            },
            poll_at: 5000,
        });
        let value = serde_json::to_value(&poll).unwrap();
        assert_eq!(value["phase"], json!("poll"));
        assert_eq!(value["pollAt"], json!(5000));
        assert!(value.get("thinkingLevel").is_none());
        assert_eq!(
            serde_json::from_value::<GenerationCheckpoint>(value).unwrap(),
            poll
        );

        let tools = GenerationCheckpoint::Tools(ToolsCheckpoint {
            assistant: id_from_number(3),
            tools: vec![id_from_number(8)],
            pending: vec!["call-2".to_owned()],
        });
        assert_eq!(
            serde_json::to_value(&tools).unwrap(),
            json!({
                "phase": "tools",
                "assistant": 3,
                "tools": [8],
                "pending": ["call-2"],
            })
        );
        assert_eq!(
            serde_json::from_value::<GenerationCheckpoint>(serde_json::to_value(&tools).unwrap())
                .unwrap(),
            tools
        );
    }

    #[test]
    fn threshold_compaction_is_off_without_a_policy_or_a_window() {
        let messages = vec![user_message(&"x".repeat(8_000))];
        let view = view_with(messages.clone(), vec![messages]);
        assert!(threshold_compaction(&view, &[], Some(1), &policy(false, 0.0, 0.0, 0.0)).is_none());
        assert!(threshold_compaction(&view, &[], None, &policy(true, 0.0, 0.0, 0.0)).is_none());
        assert!(threshold_compaction(&view, &[], Some(0), &policy(true, 0.0, 0.0, 0.0)).is_none());
    }

    #[test]
    fn threshold_compaction_needs_a_cut() {
        // 单条贡献没有可压缩的范围。
        let messages = vec![user_message(&"x".repeat(8_000))];
        let view = view_with(messages.clone(), vec![messages]);
        assert!(threshold_compaction(&view, &[], Some(1), &policy(true, 0.0, 0.0, 0.0)).is_none());
    }

    #[test]
    fn threshold_compaction_blocks_above_the_context_window() {
        let messages = vec![user_message(&"x".repeat(8_000)), user_message("short")];
        let view = view_with(
            messages.clone(),
            messages.into_iter().map(|message| vec![message]).collect(),
        );
        assert_eq!(
            threshold_compaction(&view, &[], Some(1), &policy(true, 0.0, 0.0, 0.0)),
            Some(ThresholdCompaction::Blocking)
        );
    }

    #[test]
    fn threshold_compaction_runs_in_the_background_below_the_blocking_threshold() {
        let messages = vec![user_message(&"x".repeat(8_000)), user_message("short")];
        let view = view_with(
            messages.clone(),
            messages.into_iter().map(|message| vec![message]).collect(),
        );
        let tokens = estimate_context(&view, &[]);
        let window = tokens.ceil() as u64 + 5;
        assert_eq!(
            threshold_compaction(&view, &[], Some(window), &policy(true, 0.0, 0.0, 10.0)),
            Some(ThresholdCompaction::Background)
        );
        // 后台阈值关闭时同样的窗口不压缩。
        assert!(
            threshold_compaction(&view, &[], Some(window), &policy(true, 0.0, 0.0, 0.0)).is_none()
        );
    }

    #[test]
    fn threshold_compaction_counts_planned_entries() {
        let messages = vec![user_message("short"), user_message("short")];
        let view = view_with(
            messages.clone(),
            messages.into_iter().map(|message| vec![message]).collect(),
        );
        let tokens = estimate_context(&view, &[]);
        let window = tokens.ceil() as u64 + 400;
        let policy = policy(true, 0.0, 0.0, 0.0);
        assert!(threshold_compaction(&view, &[], Some(window), &policy).is_none());
        let planned = vec![SystemDraft {
            model: Some(vec![user_message(&"x".repeat(8_000))]),
            data: NoData,
            head: None,
            edits: None,
        }];
        assert_eq!(
            threshold_compaction(&view, &planned, Some(window), &policy),
            Some(ThresholdCompaction::Blocking)
        );
    }

    #[test]
    fn compaction_failures_read_like_the_ts_ones() {
        assert_eq!(
            compaction_failure(&TaskOutcome::Failed {
                error: TaskOutcomeError {
                    message: "boom".to_owned(),
                    detail: None,
                },
                result: None,
            }),
            "Compaction failed: boom"
        );
        assert_eq!(
            compaction_failure(&TaskOutcome::Faulted {
                error: TaskOutcomeError {
                    message: "boom".to_owned(),
                    detail: None,
                },
            }),
            "Compaction failed: boom"
        );
        assert_eq!(
            compaction_failure(&TaskOutcome::Orphaned {
                reason: "missing".to_owned(),
            }),
            "Compaction failed: missing"
        );
        assert_eq!(
            compaction_failure(&TaskOutcome::Aborted {
                reason: None,
                result: None,
            }),
            "Compaction was aborted"
        );
    }

    #[test]
    fn a_completed_outcome_needs_an_entry_id() {
        assert!(outcome_has_entry_id(Some(&TaskOutcome::Completed {
            result: json!({ "entryId": 4 }),
        })));
        assert!(!outcome_has_entry_id(Some(&TaskOutcome::Completed {
            result: json!({}),
        })));
        assert!(!outcome_has_entry_id(Some(&TaskOutcome::Failed {
            error: TaskOutcomeError {
                message: "boom".to_owned(),
                detail: None,
            },
            result: Some(json!({ "entryId": 4 })),
        })));
        assert!(!outcome_has_entry_id(None));
        assert!(task_outcome_completed(Some(&TaskOutcome::Completed {
            result: json!({}),
        })));
    }

    #[test]
    fn hand_over_moves_run_control_only_when_the_task_matches() {
        let mut live = DocDraft::new(JsonObject::new());
        live.value_mut()
            .insert("run".to_owned(), json!({ "taskId": 1, "inputs": [5, 6] }));
        hand_over(&mut live, id_from_number(2), id_from_number(3));
        assert_eq!(
            live.value().get("run"),
            Some(&json!({ "taskId": 1, "inputs": [5, 6] }))
        );
        hand_over(&mut live, id_from_number(1), id_from_number(3));
        assert_eq!(
            live.value().get("run"),
            Some(&json!({ "taskId": 3, "inputs": [5, 6] }))
        );
    }

    #[test]
    fn stop_reason_names_match_the_ts_literals() {
        assert_eq!(stop_reason_name(StopReason::ToolUse), "toolUse");
        assert_eq!(stop_reason_name(StopReason::Aborted), "aborted");
        assert_eq!(stop_reason_name(StopReason::Deferred), "deferred");
    }

    #[test]
    fn assistant_messages_serialize_without_undefined_members() {
        // `copyJson(partial, { omitUndefinedProperties: true })` 的等价：`None` 字段不出现，
        // 因此 partial 的提交与 TS 形状一致。
        let message = AssistantMessage {
            stop_reason: StopReason::Stop,
            content: vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "hi".to_owned(),
                text_signature: None,
            })],
            ..Default::default()
        };
        let value = serde_json::to_value(&message).unwrap();
        assert!(value.get("errorMessage").is_none());
        assert!(value.get("deferred").is_none());
        assert_eq!(value["content"][0]["text"], json!("hi"));
        let _ = UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "x".to_owned(),
            text_signature: None,
        })]);
    }
}
