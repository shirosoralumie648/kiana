//! `harness/compaction.ts`：内建压缩任务（spec §8.7）。
//!
//! TS: `packages/durable/src/harness/compaction.ts`（483 行）。选择模型上下文的一段旧前缀，
//! 让它被摘要替换，并放置一条 `head` 指向第一个原样保留条目的摘要条目。generation 拥有的压缩
//! 阻塞它并直接追加；会话拥有的压缩通过 write 提交放置摘要。
//!
//! # 这个模块解决什么问题
//!
//! 会话越聊越长，模型上下文最终会超过 context window，或挤占本该留给回答的空间。压缩把上下文的
//! 一段**旧前缀**交给摘要模型总结，再追加一条特殊条目：它的 `head` 指向 `first_kept`（第一个
//! 原样保留的条目），`model` 是包装后的摘要文本。之后派生上下文时旧前缀被这条摘要替换，模型
//! 只看到「摘要 + 最近保留的消息」。
//!
//! # 在系统中的位置
//!
//! - 上游（调用方）：`generation` 的 prepare 阶段在超过阈值时创建阻塞或后台压缩，response 阶段
//!   在 context overflow 时创建阻塞压缩；宿主手工压缩走 `Conversation` trait 的 `compact()`
//!   （会话句柄由 `Harness::open()` 返回的 `HarnessImpl` 的 `conversation()` 取得）。三者都
//!   在自己的事务里调用 [`create_compaction`]，任务记录与 live 状态因此和调用方的写入一起提交。
//! - 注册：`registry` 把 [`COMPACTION_TASK`] 放进内建任务表，登记名是 `kiana.compaction`。
//! - 下游：`kiana.live` 文档的 `compactions` / `compactionFailure` 字段（进度与失败展示），
//!   以及 `admit_submission`（会话拥有的摘要作为 write 提交排队放置）。
//!
//! # 执行流程（checkpoint 即状态机）
//!
//! 调度器按 checkpoint JSON 的 `phase` 字段选择处理器，三个阶段对应
//! [`CompactionCheckpoint`] 的三个变体：
//!
//! 1. `select`（[`SelectPhase`]）：解析策略与模型，用 [`select_cut`] 找切点。没有可压缩内容
//!    就直接成功结束；`beforeCompact` hook 可以拒绝或直接给出摘要；否则把摘要请求参数写入
//!    checkpoint，进入 `summarize`。
//! 2. `summarize`（[`SummarizePhase`]）：在 `tail` 处重放上下文，请求 provider 生成摘要。
//!    成功则放置摘要条目并终态；可重试的错误写入退避时间，进入 `retry`。
//! 3. `retry`（[`RetryPhase`]）：未到时间继续睡；到点后 `attempt + 1`，回到 `summarize`。
//!
//! 外部取消时调度器调用任务定义里的 abort 处理器 [`AbortPhase`]：移除压缩状态，任务以
//! `aborted` 终态结束。
//!
//! # 重要约束与失败模式
//!
//! - **`tail` 之前的前缀不可变**：`summarize` 把上下文截断到 `request.tail` 重放，假设 select
//!   选中的前缀没有变化（tail 之后追加的条目不在重放范围内）。若 `first_kept` 在重放结果里
//!   找不到，代码按 JS `slice(0, -1)` 语义回退（[`slice_end`]），这时摘要范围可能不是 select
//!   当时选中的范围。
//! - **摘要请求永远不带 `deferred`**：`summary_text` 要求 `stop`，延迟响应不会同步返回文本，
//!   会被判为失败。`select` 原样保存 `settings.stream`，`summarize` 在发请求前显式剔除。
//! - **放置路径取决于拥有权**：generation 拥有的压缩（`owner` 存在）直接追加条目；会话拥有的
//!   压缩必须走 `admit_submission`，避免绕过正在进行的 generation 破坏转录顺序。成功放置会
//!   清除 `compactionFailure`，失败则写入它。
//! - **模型不可用**：没有配置模型或模型不在注册表时，任务以 `failed` 结束（`reason: "no_model"`），
//!   不会尝试摘要。
//! - 纯函数（[`serialize_conversation`]、[`truncate_chars`] 等）刻意复刻 TS 的格式与 UTF-16
//!   计数；注释里的「逐字」格式是摘要质量的一部分，不要为了好看改写。
//!
//! # 阅读顺序
//!
//! [`CompactionInput`] / [`SummaryRequest`] / [`CompactionCheckpoint`]（数据形状）→
//! [`select_cut`] 与 [`summarized_messages`]（选段）→ `select` → `summarize` →
//! `place_summary`（放置）→ [`create_compaction`]（入口）。[`serialize_conversation`] 与
//! [`truncate_chars`] 是独立的纯函数，可单独阅读。

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Context as AiContext, Message, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, SystemContent, SystemMessage, SystemRole, ThinkingLevel,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::estimate::{calculate_context_tokens, estimate_message_tokens};
use kiana_ai::utils::retry::{is_retryable_assistant_error, retry_delay_ms_js};
use kiana_chord::context::{Context, abort_error};
use serde::{Deserialize, Serialize};

use crate::harness::context::order_tool_results;
use crate::harness::inbox::QueueModes;
use crate::harness::live::{
    COMPACTION_TASK_KIND, LIVE_DOC, TaskCommit, add_compaction_status, compaction_status,
    fail_compaction_status, hook_failure, hook_handlers, remove_compaction_status,
    state_checkpoint, static_runtime,
};
use crate::harness::submissions::admit_submission;
use crate::harness::types::{
    AnyTask, BeforeCompactDecision, BeforeCompactInput, CompactionHooks, CompactionPolicy,
    CompactionReason, CompactionResult, ContextView, ContextViewOptions, ConversationStreamOptions,
    HookApi, ModelRef, Settings, SubmissionDraft, TaskRuntime,
};
use crate::harness::usage::{UsageBucket, record_usage};
use crate::types::{
    BoxError, CommitOptions, ConversationId, DocDraft, DocTarget, EntryDraft, EntryHead, EntryId,
    JsonValue, RunningTask, TaskDefinition, TaskId, TaskOptions, TaskOutcome, TaskOwnership,
    TaskPhase, TaskRecord, TaskState, Tx, id_from_number,
};

/// 摘要源里保留的最长工具结果文本。
///
/// 计数单位是 UTF-16 code unit（JS `string.length`），由 [`truncate_chars`] 执行截断；超出的
/// 部分替换成 `[... N more characters truncated]` 说明。只影响摘要输入，不改写会话里的条目。
pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// 摘要前缀。
///
/// 摘要条目里的 user 消息文本是 `SUMMARY_PREFIX + 摘要正文 + SUMMARY_SUFFIX`；这段文本会进入
/// 之后模型请求的上下文，逐字匹配 TS，改动会直接改变模型看到的内容。
pub const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
/// 摘要后缀。
///
/// 与 [`SUMMARY_PREFIX`] 成对包裹摘要正文。
pub const SUMMARY_SUFFIX: &str = "\n</summary>";

/// 摘要系统的提示词。
///
/// 这是发请求时的 system 消息正文。逐字复制自 TS（spec §2.12.1）；两段之间的空行也是内容，
/// 不要格式化或改写，否则摘要输出格式可能漂移。
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// 摘要请求的提示词。
///
/// 逐字复制自 TS（spec §2.12.1）的 Markdown 模板；空行、`- [x]` / `- [ ]` 都是内容。
/// [`summary_prompt`] 把它接在序列化后的对话之后，并可追加 `Additional focus`。
pub const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work. If the conversation starts with an earlier summary, preserve its information and fold the newer messages into it.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// 压缩任务的输入（TS: `CompactionInput`）。
///
/// 创建任务时序列化进 `TaskRecord.input`，各阶段再按需反序列化。`reason` 决定 live 状态、摘要
/// 条目的 `data.reason`，并影响 [`create_compaction`] 的 `background` 默认值（`manual` 不是
/// 后台任务）；`instructions` 会传给 `beforeCompact` hook，并追加到摘要提示词的
/// `Additional focus`，**不参与切点选择**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionInput {
    /// 压缩原因。
    pub reason: CompactionReason,
    /// 额外的关注点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// 固定下来的摘要请求（TS: `SummaryRequest`）。
///
/// 在 `select` 阶段构造并作为 checkpoint 持久化；`summarize` 只读它，`retry` 在 `attempt + 1`
/// 后复用它。`tail` 固定了重放上下文的位置，`first_kept` 既是摘要条目的 `head`，也是 summarize
/// 时在重放结果里定位切点的锚点。`max_tokens` 保留为 `f64`（TS 的 `number`），真正发请求时在
/// `summarize` 里截断成非负 `u64`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRequest {
    /// 尝试次数（从 1 开始）。
    pub attempt: i64,
    /// 请求用的模型。
    pub model: ModelRef,
    /// 思考等级。
    pub thinking_level: ModelThinkingLevel,
    /// 设置里的请求选项。
    pub stream_options: ConversationStreamOptions,
    /// 摘要的最大 token（TS 里总是数字：`Math.min(floor(0.8 * reserveTokens), model.maxTokens || Infinity)`）。
    pub max_tokens: f64,
    /// 选择范围时上下文的最新条目。
    pub tail: EntryId,
    /// 第一个原样保留的条目；摘要条目的 `head`。
    pub first_kept: EntryId,
}

/// 重试阶段固定下来的请求（TS: `{ phase: "retry"; until } & SummaryRequest`）。
///
/// TS 的交叉类型在 Rust 里展开成独立字段：请求部分与 [`SummaryRequest`] 相同，额外记录退避结束
/// 的时钟点。`until` 是 Harness 时钟的 epoch 毫秒，`retry` 阶段用 `runtime.now()` 与它比较。
/// [`RetryCheckpoint::new`] 与 [`RetryCheckpoint::request`] 负责两种形状之间的往返。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryCheckpoint {
    /// 尝试次数。
    pub attempt: i64,
    /// 请求用的模型。
    pub model: ModelRef,
    /// 思考等级。
    pub thinking_level: ModelThinkingLevel,
    /// 设置里的请求选项。
    pub stream_options: ConversationStreamOptions,
    /// 摘要的最大 token。
    pub max_tokens: f64,
    /// 选择范围时上下文的最新条目。
    pub tail: EntryId,
    /// 第一个原样保留的条目。
    pub first_kept: EntryId,
    /// 退避结束的时钟点。
    pub until: i64,
}

impl RetryCheckpoint {
    /// 由请求与退避时间点构造。
    pub fn new(request: SummaryRequest, until: i64) -> Self {
        RetryCheckpoint {
            attempt: request.attempt,
            model: request.model,
            thinking_level: request.thinking_level,
            stream_options: request.stream_options,
            max_tokens: request.max_tokens,
            tail: request.tail,
            first_kept: request.first_kept,
            until,
        }
    }

    /// 去掉 `until` 的请求部分。
    pub fn request(&self) -> SummaryRequest {
        SummaryRequest {
            attempt: self.attempt,
            model: self.model.clone(),
            thinking_level: self.thinking_level,
            stream_options: self.stream_options.clone(),
            max_tokens: self.max_tokens,
            tail: self.tail,
            first_kept: self.first_kept,
        }
    }
}

/// 压缩任务的 checkpoint（TS: `CompactionCheckpoint`）。
///
/// 这同时是任务的状态机。序列化后 `phase` 字段是变体名的小写形式（`select` / `summarize` /
/// `retry`），调度器读这个字段来选择阶段处理器；`Summarize` / `Retry` 把请求参数一并持久化，
/// 所以进程重启后不需要重跑 `select` 就能恢复这次摘要请求。
///
/// ```text
/// select ──选中切点──▶ summarize ──可重试错误──▶ retry
///   │                     │                      │
///   │                     │◀──退避到点, attempt+1┘
///   ├─无内容/被拒绝──▶ terminal(completed)
///   ├─hook 提供摘要──▶ terminal(completed, 已放置条目)
///   │                     ├─成功──▶ terminal(completed)
///   │                     └─失败──▶ terminal(failed)
/// ```
///
/// 终态（`Terminal`）没有 checkpoint，`state_checkpoint` 返回 `None`；阶段函数遇到缺失或形状
/// 不符的 checkpoint 会返回 `Err`，调度器据此把任务标成 faulted。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "lowercase")]
pub enum CompactionCheckpoint {
    /// 选择范围。
    ///
    /// 任务的初始 checkpoint（`initial` 返回 `{"phase":"select"}`）；阶段本身不需要额外数据。
    Select,
    /// 请求摘要。
    ///
    /// 保存 `select` 选出的完整请求参数，供 `summarize` 重放上下文并调用 provider。
    Summarize(SummaryRequest),
    /// 退避后重试。
    ///
    /// 请求参数加 `until`；`retry` 阶段到点后把 attempt 加一，转回 `summarize`。
    Retry(RetryCheckpoint),
}

/// 压缩任务的 hook 表类型。
///
/// 任务定义里的 `hooks` 字段为 `None`（见 [`compaction_task`]）；处理器在运行期从运行端
/// `hooks()` 中按名字 `beforeCompact` 解析出来，类型就是这里的 `Arc<dyn CompactionHooks>`。
pub type CompactionHookTable = Arc<dyn CompactionHooks>;

// `ModelThinkingLevel::Off` 对应 TS 的「不传 reasoning」：`summarize` 用它设置
// `options.reasoning`，所以 Off 必须映射成 `None` 而不是某个具体等级；其余等级一一对应。
fn thinking_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// JS 的毫秒数（number）→ Rust 的 `u64`（截断到非负）。
///
/// 非有限值（包括 `+∞`）与 `<= 0` 一律变成 0。注意这不同于 `value as u64` 的饱和转换
/// （`+∞ as u64` 会是 `u64::MAX`）；调用点转换的是请求的 timeout / maxRetryDelay。
fn ms_to_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value as u64
    }
}

/// 会话请求选项 → kiana-ai 的简单请求选项（TS 的 `{ ...streamOptions, signal, telemetry, sessionId }`）。
///
/// 参数与转换规则：
///
/// - `signal` 克隆成取消令牌；`telemetry` 挂到请求上；`session_id` 原样透传（旧会话可能为
///   `None`，见 `ensure_provider_session_id`）。
/// - `max_tokens` 覆盖模型默认输出上限；`cache_retention` 是本次请求的缓存偏好（摘要路径固定
///   传入 `CacheRetention::None`，即显式关闭缓存标记）。
/// - `stream.timeout_ms` / `max_retry_delay_ms` 是 JS 浮点毫秒，经 `ms_to_u64` 转换；负的
///   `max_retries` 钳成 0；`headers` 的每个值包成 `Some(...)`（该类型用 `None` 表示「不要发送
///   同名默认头」）。
///
/// `deferred` 会从 `stream` 复制进返回值（与 TS 的对象展开一致）；是否真的发出延迟请求由调用方
/// 决定，摘要路径在 `summarize` 里显式剔除。
///
/// 返回值里的 `reasoning` / `tool_choice` / `thinking_budgets` 仍是默认的 `None`，由调用方按需
/// 设置（`summarize` 随后设置 `reasoning`）。
pub fn simple_stream_options(
    stream: &ConversationStreamOptions,
    signal: Option<&kiana_chord::context::AbortSignal>,
    telemetry: Arc<dyn kiana_telemetry::TelemetrySpan>,
    session_id: Option<String>,
    max_tokens: Option<u64>,
    cache_retention: Option<kiana_ai::types::CacheRetention>,
) -> SimpleStreamOptions {
    use kiana_ai::types::StreamOptions;
    let mut base = StreamOptions {
        transport: stream.transport,
        timeout_ms: stream.timeout_ms.map(ms_to_u64),
        max_retries: stream.max_retries.map(|value| value.max(0) as u32),
        max_retry_delay_ms: stream.max_retry_delay_ms.map(ms_to_u64),
        headers: stream.headers.clone().map(|headers| {
            headers
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect()
        }),
        metadata: stream.metadata.clone(),
        cache_retention,
        session_id,
        max_tokens,
        telemetry: Some(telemetry),
        ..Default::default()
    };
    base.signal = signal.map(|signal| signal.token().clone());
    SimpleStreamOptions {
        base,
        // 与 TS 的 `{ ...streamOptions }` 一致，`deferred` 也带给 provider；摘要请求调用方
        // （[`summarize`]）按 TS 显式剔除它。
        deferred: stream.deferred.as_ref().map(deferred_request),
        ..Default::default()
    }
}

/// `ConversationStreamOptions.deferred` → kiana-ai 的 `DeferredRequest`。
///
/// `Enabled(bool)` 原样映射；窗口 `15m` / `1h` / `24h` 对应 `M15` / `H1` / `H24`；`window: None`
/// 保持 `None`，原样交给 provider 解释（kiana-ai 的 `DeferredRequest` 文档如此约定）。
fn deferred_request(
    deferred: &crate::harness::types::Deferred,
) -> kiana_ai::types::DeferredRequest {
    use crate::harness::types::{Deferred, DeferredWindow};
    match deferred {
        Deferred::Enabled(enabled) => kiana_ai::types::DeferredRequest::Enabled(*enabled),
        Deferred::Window { window } => kiana_ai::types::DeferredRequest::Window {
            window: window.map(|window| match window {
                DeferredWindow::FifteenMinutes => kiana_ai::types::DeferredWindow::M15,
                DeferredWindow::OneHour => kiana_ai::types::DeferredWindow::H1,
                DeferredWindow::OneDay => kiana_ai::types::DeferredWindow::H24,
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// Policies and selection
// ---------------------------------------------------------------------------

/// `model` 的压缩策略：`modelOverrides` 的条目逐字段覆盖 harness 级策略（TS: `compactionPolicy`）。
///
/// 覆盖表的键是 `"provider/modelId"`。覆盖项里缺省的字段（`None`）沿用 harness 级值，因此一次
/// 覆盖只改它声明的维度；没有覆盖项时返回 `settings.compaction` 的四个基础字段（这些字段在
/// `ResolvedCompactionSettings` 里已经解析成具体值，不再是 `Option`）。
pub fn compaction_policy(settings: &Settings, model: &ModelRef) -> CompactionPolicy {
    let resolved = &settings.compaction;
    let policy = CompactionPolicy {
        enabled: resolved.enabled,
        reserve_tokens: resolved.reserve_tokens,
        keep_recent_tokens: resolved.keep_recent_tokens,
        background_tokens: resolved.background_tokens,
    };
    let Some(overrides) = &resolved.model_overrides else {
        return policy;
    };
    let Some(override_) = overrides.get(&format!("{}/{}", model.provider, model.model_id)) else {
        return policy;
    };
    CompactionPolicy {
        enabled: override_.enabled.unwrap_or(policy.enabled),
        reserve_tokens: override_.reserve_tokens.unwrap_or(policy.reserve_tokens),
        keep_recent_tokens: override_
            .keep_recent_tokens
            .unwrap_or(policy.keep_recent_tokens),
        background_tokens: override_
            .background_tokens
            .unwrap_or(policy.background_tokens),
    }
}

/// 一次摘要请求的最大 token：
/// `Math.min(Math.floor(0.8 * reserveTokens), model.maxTokens > 0 ? model.maxTokens : Infinity)`。
///
/// 0.8 是 TS 公式里的固定系数。模型上限缺失或为 0 时视为无穷大（TS 的 `maxTokens > 0`
/// 判断）；结果保持 `f64`，调用方发请求时再转成非负 `u64`。
pub fn summary_max_tokens(reserve_tokens: f64, max_tokens: Option<u64>) -> f64 {
    let reserve = (0.8 * reserve_tokens).floor();
    let model_max = match max_tokens {
        Some(max_tokens) if max_tokens > 0 => max_tokens as f64,
        _ => f64::INFINITY,
    };
    reserve.min(model_max)
}

/// `view.entries` 里摘要保留的第一个条目的下标；没有可压缩内容时 `None`（TS: `selectCut`）。
///
/// 从尾部往回走，直到保留 `keep_recent_tokens`，然后在那个条目或其后的第一个候选处切开。
///
/// # 步骤
///
/// 1. 若有 head marker，索引 0 是 marker 贡献本身，不能作为切点，从 1 开始。
/// 2. 在 `[start, contributions.len())` 中收集候选（规则见 `is_candidate`）。
/// 3. 从尾部向前按条目累计 `estimate_message_tokens`；第一次达到 `keep_recent_tokens` 时，切到
///    「第一个下标 ≥ 当前位置的候选」，没有则取最后一个候选。
/// 4. 切点之前必须至少有一个非空贡献，否则返回 `None`。
///
/// # 边界
///
/// - 整段上下文都放得进 `keep_recent_tokens` 时第 3 步不会命中，返回 `None`，调用方直接以
///   「无需压缩」完成任务。
/// - 返回值是 `entries` / `contributions` 的下标，不是条目 ID；两者一一对应（由上下文的
///   `derive_range` 保证），调用方用 `view.entries[cut].id` 取锚点。
/// - `keep_recent_tokens <= 0` 且有候选时，在最后一个候选处切开，即从最后一个候选开始原样保留。
pub fn select_cut(view: &ContextView, keep_recent_tokens: f64) -> Option<usize> {
    let contributions = &view.contributions;
    let start = if view.head.is_none() { 0 } else { 1 };
    let mut candidates: Vec<usize> = Vec::new();
    for index in start..contributions.len() {
        if is_candidate(contributions, index) {
            candidates.push(index);
        }
    }
    let mut kept = 0.0f64;
    let mut cut: Option<usize> = None;
    if contributions.len() > start {
        for index in (start..contributions.len()).rev() {
            for message in &contributions[index] {
                kept += estimate_message_tokens(message) as f64;
            }
            if kept < keep_recent_tokens {
                continue;
            }
            cut = candidates
                .iter()
                .find(|candidate| **candidate >= index)
                .copied()
                .or_else(|| candidates.last().copied());
            break;
        }
    }
    let cut = cut?;
    for contribution in contributions.iter().take(cut).skip(start) {
        if !contribution.is_empty() {
            return Some(cut);
        }
    }
    None
}

// 判断下标 `index` 的条目能否作为切点（摘要前缀在此条目之前结束）。
//
// 规则（TS `isCandidate`）：
// 1. 首个贡献消息是 assistant → 可以切：切在 assistant 条目前时，该 assistant 及其后续工具
//    结果都落在保留段，不会被切点拆开；
// 2. 否则首个消息必须是 user；以 tool result 开头的条目永远不能作为切点；
// 3. user 条目若「前一个 assistant 的工具调用结果」会出现在它之后，就不能切：否则切点会把
//    调用留在摘要里、把结果留给保留段，后续请求会看到孤立的结果或缺失的调用。
//
// 第 3 步先向前找最近的 assistant（逐条目向前，找到即停）收集工具调用 ID，再向后扫：先遇到
// 匹配的 tool result → 不是候选；先遇到 assistant（同一贡献里 `position > 0` 也算）→ 结果
// 区间已经结束，是候选。两条都没遇到时默认可以切。
fn is_candidate(contributions: &[Vec<Message>], index: usize) -> bool {
    let first = contributions[index].first();
    if matches!(first, Some(Message::Assistant(_))) {
        return true;
    }
    if !matches!(first, Some(Message::User(_))) {
        return false;
    }
    // 本条目之后、下一个 assistant 之前出现的“前一个 assistant 调用的结果”属于它之前。
    let mut calls: Vec<String> = Vec::new();
    if index > 0 {
        for before in (0..index).rev() {
            let Some(assistant) =
                contributions[before]
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        Message::Assistant(assistant) => Some(assistant),
                        _ => None,
                    })
            else {
                continue;
            };
            calls = assistant
                .content
                .iter()
                .filter_map(|content| match content {
                    AssistantContentBlock::ToolCall(call) => Some(call.id.clone()),
                    _ => None,
                })
                .collect();
            break;
        }
    }
    if calls.is_empty() {
        return true;
    }
    for (offset, contribution) in contributions[index..].iter().enumerate() {
        let after = index + offset;
        for (position, message) in contribution.iter().enumerate() {
            match message {
                Message::Assistant(_) if after > index || position > 0 => return true,
                Message::ToolResult(result) if calls.contains(&result.tool_call_id) => {
                    return false;
                }
                _ => {}
            }
        }
    }
    true
}

/// 切点之前条目的模型消息：先按模型上下文排序（TS: `summarizedMessages`）。
///
/// `cut` 是条目下标：先拼接 `contributions[..cut]`（若存在 head marker，它的贡献在最前），
/// 再交给 `order_tool_results`。后者把每个 assistant 的工具结果排到调用之后，并为缺失的结果
/// 合成占位消息；因此返回值可能包含会话里并不存在的消息，只用于构造摘要输入。
pub fn summarized_messages(view: &ContextView, cut: usize) -> Vec<Message> {
    let flat: Vec<Message> = view
        .contributions
        .iter()
        .take(cut)
        .flat_map(|messages| messages.iter().cloned())
        .collect();
    order_tool_results(&flat)
}

/// JS `array.slice(0, end)` 的负数语义（`summarize` 把 `findIndex` 的 `-1` 直接切片）。
///
/// `end < 0` 按 `len + end` 计算并在 0 处饱和；`end >= 0` 截到 `len`。`summarize` 用 `-1`
/// 表示「重放结果里找不到 `first_kept`」，于是得到 `len - 1`，与 TS 的 `.slice(0, -1)` 一致。
///
/// 生产代码只传 `-1`（本文件的测试还覆盖其它正数/负数取值）；`end == i64::MIN` 时 `-end` 在
/// 开启溢出检查的构建（dev/test 默认）会 panic。
pub fn slice_end(len: usize, end: i64) -> usize {
    if end < 0 {
        len.saturating_sub((-end) as usize)
    } else {
        (end as usize).min(len)
    }
}

/// `view` 之上、其后追加 `extra` 的一次请求大小（TS: `estimateContext`）。
///
/// # 估算方式
///
/// 1. 选择基线时只看 `head` 标记之后的条目（`entry.id > head.id`）：标记之前的 assistant
///    usage 描述的是压缩前的旧上下文，不能当作当前上下文的基线。
/// 2. 从最新条目往前，找最近一条「`calculate_context_tokens(usage) > 0`」的 assistant 消息，
///    把它的 usage 当作已测基线；其余消息仍按字符估算。
/// 3. token 数 = 基线 usage + `view.messages` 中该消息之后的估算 + `extra` 的估算。
///
/// 第 3 步用**结构相等**在 `view.messages` 里定位基线消息（TS 的 `lastIndexOf` 是引用相等），
/// 存在两条完全相同的 assistant 消息时可能定位到较晚的一条。若基线消息不在 `view.messages`
/// 里，`lastIndexOf` 的 `-1` 语义让累加从下标 0 开始，于是 usage 与消息估算可能重复计算。
/// 调用方是 generation 的阈值判断，它把计划写入的 system 条目作为 `extra`。
pub fn estimate_context(view: &ContextView, extra: &[Message]) -> f64 {
    let after = view
        .head
        .as_ref()
        .map(|head| head.id.get())
        .unwrap_or(i64::MIN);
    let mut measured: Option<&AssistantMessage> = None;
    for index in (0..view.entries.len()).rev() {
        if view.entries[index].id.get() <= after {
            continue;
        }
        measured = view.contributions[index]
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) if calculate_context_tokens(&assistant.usage) > 0 => {
                    Some(assistant)
                }
                _ => None,
            });
        if measured.is_some() {
            break;
        }
    }
    let mut tokens = 0.0f64;
    let from = match measured {
        Some(measured) => {
            tokens += calculate_context_tokens(&measured.usage) as f64;
            view.messages
                .iter()
                .rposition(|message| matches!(message, Message::Assistant(assistant) if assistant == measured))
                .map(|index| index + 1)
                .unwrap_or(0)
        }
        None => 0,
    };
    for message in view.messages.iter().skip(from) {
        tokens += estimate_message_tokens(message) as f64;
    }
    for message in extra {
        tokens += estimate_message_tokens(message) as f64;
    }
    tokens
}

// ---------------------------------------------------------------------------
// Summary text
// ---------------------------------------------------------------------------

/// 干净 `stop`、有文本且没有工具调用时的摘要；其它都不是摘要（TS: `summaryText`）。
///
/// 文本块按出现顺序用 `\n` 连接，最后 `trim`。只有 `Stop` 才可能成功：`Length`、`Error`、
/// `Aborted`、`Deferred` 等都返回 `None`，由调用方转入重试或 [`summary_failure`]。
pub fn summary_text(message: &AssistantMessage) -> Option<String> {
    if message.stop_reason != StopReason::Stop
        || message
            .content
            .iter()
            .any(|content| matches!(content, AssistantContentBlock::ToolCall(_)))
    {
        return None;
    }
    let text = message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim().to_owned();
    if text.is_empty() { None } else { Some(text) }
}

/// 摘要失败的原因（TS: `summaryFailure`）。
///
/// 检查顺序即优先级：`error` / `aborted`（优先用 provider 的 `error_message`，缺失时用 stop
/// reason 名）→ `length` → 有工具调用 → 其余归为「没有文本」。在不可重试的失败路径上，返回的
/// 字符串会写进 `live.compactionFailure`；本文件的测试锁定了这些措辞，改动前先确认调用方展示。
pub fn summary_failure(message: &AssistantMessage) -> String {
    if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        return format!(
            "Summarization failed: {}",
            message
                .error_message
                .clone()
                .unwrap_or_else(|| stop_reason_text(message.stop_reason))
        );
    }
    if message.stop_reason == StopReason::Length {
        return "Summarization hit the token limit; the summary is incomplete".to_owned();
    }
    if message
        .content
        .iter()
        .any(|content| matches!(content, AssistantContentBlock::ToolCall(_)))
    {
        return "Summarization attempted to call a tool".to_owned();
    }
    "Summarization produced no text".to_owned()
}

// `error_message` 缺失时的兜底文本：使用 stopReason 的稳定名称（`toolUse` 为 camelCase）。
// 唯一调用点只传 `Error`/`Aborted`，其余变体不可达，且本文件的测试未覆盖这些兜底字符串。
fn stop_reason_text(reason: StopReason) -> String {
    match reason {
        StopReason::Pending => "pending".to_owned(),
        StopReason::Stop => "stop".to_owned(),
        StopReason::Length => "length".to_owned(),
        StopReason::ToolUse => "toolUse".to_owned(),
        StopReason::Error => "error".to_owned(),
        StopReason::Aborted => "aborted".to_owned(),
        StopReason::Deferred => "deferred".to_owned(),
        StopReason::TurnCapped => "turnCapped".to_owned(),
        StopReason::TokenCapped => "tokenCapped".to_owned(),
        StopReason::CostCapped => "costCapped".to_owned(),
        StopReason::TimeCapped => "timeCapped".to_owned(),
    }
}

/// 摘要器的 user 消息：序列化后的会话、提示词与额外说明（TS: `summaryPrompt`）。
///
/// 结构固定为 `<conversation>` + [`serialize_conversation`] 的输出 + `</conversation>` +
/// 空行 + [`SUMMARIZATION_PROMPT`]；`instructions` 为 `Some` 时在末尾追加
/// `\n\nAdditional focus: ...`（空串也会追加）。这是提示词文本而不是可逆编码，消息内容里
/// 出现的分隔符字样不会被转义。
pub fn summary_prompt(messages: &[Message], instructions: Option<&str>) -> String {
    let focus = instructions
        .map(|instructions| format!("\n\nAdditional focus: {instructions}"))
        .unwrap_or_default();
    format!(
        "<conversation>\n{}\n</conversation>\n\n{}{}",
        serialize_conversation(messages),
        SUMMARIZATION_PROMPT,
        focus
    )
}

/// 消息的纯文本（TS: `serializeConversation`）。system 消息与 thinking 省略。
///
/// 输出是若干段用空行（`\n\n`）连接的文本：
///
/// - `[User]: ...`，文本块之间用 `\n` 连接；空文本不产生段；
/// - `[Assistant]: ...`（多个文本块用 `\n` 连接）与
///   `[Assistant tool calls]: name(key=JSON, ...)`（多个调用用 `; ` 连接）；
/// - `[Tool result]: ...`，先 [`truncate_chars`] 截到 [`TOOL_RESULT_MAX_CHARS`]。
///
/// system 消息、thinking 块、图片块都被跳过，只保留文本与工具调用。工具调用参数用
/// `kiana_json::js_object::stringify` 序列化，保持与 JS `JSON.stringify` 相同的键序与数字格式。
/// 这份文本直接进入摘要模型，测试锁定了具体输出，不要按普通日志格式改写。
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let text = user_content_text(&user.content);
                if !text.is_empty() {
                    parts.push(format!("[User]: {text}"));
                }
            }
            Message::Assistant(assistant) => {
                let text: Vec<&str> = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect();
                let calls: Vec<String> = assistant
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        AssistantContentBlock::ToolCall(call) => Some(format!(
                            "{}({})",
                            call.name,
                            call.arguments
                                .iter()
                                .map(|(key, value)| format!(
                                    "{}={}",
                                    key,
                                    kiana_json::js_object::stringify(value)
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    parts.push(format!("[Assistant]: {}", text.join("\n")));
                }
                if !calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let text = blocks_text(&result.content);
                if !text.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_chars(&text, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }
    parts.join("\n\n")
}

fn user_content_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks_text(blocks),
    }
}

fn blocks_text(blocks: &[UserContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// JS `text.slice(0, maxChars)` + 截断说明（按 UTF-16 code unit 计数）。
///
/// `max_chars` 与提示里的「剩余字符数」都按 UTF-16 code unit 计（JS `string.length`），不是
/// 字节数或 Unicode 标量数。截断点可能落在一个代理对中间：`from_utf16_lossy` 会把落单的代理项
/// 替换成 U+FFFD，而 JS 的 `slice` 会保留孤立代理项，这是两种运行时之间的固有差异。
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    let length = crate::harness::live::utf16_len(text);
    if length <= max_chars {
        return text.to_owned();
    }
    let units: Vec<u16> = text.encode_utf16().take(max_chars).collect();
    let head = String::from_utf16_lossy(&units);
    format!(
        "{head}\n\n[... {} more characters truncated]",
        length - max_chars
    )
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

/// `select` 阶段。
///
/// 调度器按当前 checkpoint 的 `phase` 字段（这里是 `select`）选中它。类型本身无状态，只负责把
/// 擦除后的 `JsonValue` 运行端转交给模块内的 `select` 函数。
pub struct SelectPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, CompactionHookTable> for SelectPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        select(task, runtime, context).await
    }
}

/// `summarize` 阶段。
///
/// 对应 [`CompactionCheckpoint::Summarize`]。只转发到 `summarize`；重试后的再次摘要也走同一
/// 处理器，区别只是 checkpoint 里的 `attempt` 已经加一。
pub struct SummarizePhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, CompactionHookTable> for SummarizePhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        summarize(task, runtime, context).await
    }
}

/// `retry` 阶段。
///
/// 对应 [`CompactionCheckpoint::Retry`]：退避未结束时继续 `sleeping`，到点后把 `attempt` 加一
/// 并转回 `summarize`。只转发到 `retry_phase`。
pub struct RetryPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, CompactionHookTable> for RetryPhase {
    async fn run(
        &self,
        task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        retry_phase(task, runtime, context).await
    }
}

/// abort 处理器。
///
/// 不在 `phases` 表里，而是由任务定义的 `abort` 字段注册（见 [`compaction_task`]）。外部取消时
/// 运行：移除该任务的 `compactions` 状态并把任务置为 `aborted` 终态。它不触碰
/// `compactionFailure`——失败记录保留到下一次成功放置摘要（TS 同）。
pub struct AbortPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, CompactionHookTable> for AbortPhase {
    async fn run(
        &self,
        _task: &RunningTask<JsonValue, JsonValue, JsonValue>,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let conversation_id = runtime.conversation_id();
        let task_id = id_from_number(runtime.task_id().get());
        // 只提交 live 状态与终态，不需要读时钟/设置，因此不需要延长 runtime 借用。
        let change = TaskCommit::new(move |tx, _current| {
            Box::pin(async move {
                let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
                let mut live = tx.doc(&target).await?;
                // live 里可能已有上一次的 compactionFailure；abort 不像成功放置那样清除它。
                remove_compaction_status(live.value_mut(), task_id);
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
}

// `select` 阶段：确定「摘要哪一段」，并把后续请求参数固化进 checkpoint。
//
// 只有三种走向：
// - 无模型 / 无切点 / hook 拒绝 → 终态（failed 或 completed）；
// - hook 直接给出摘要 → 立即走放置路径（`place`）并终态；
// - 否则提交 `Running` + `Summarize` checkpoint，把模型、尝试次数、切点与 `tail` 记住。
//
// 切点不会在 summarize 阶段重算：`tail` 之前的条目被假定不变，summarize 用 `tail` 重放上下文，
// 再按 `first_kept` 重新定位切点。
async fn select(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let conversation_id = runtime.conversation_id();
    let agent = runtime.agent(context).await?;
    let settings = runtime.settings()?;
    // agent.model 是当前配置的模型引用；模型没配置或不在注册表里都按「模型不可用」失败。
    let reference = agent.model.clone();
    let model = reference.as_ref().and_then(|reference| {
        runtime
            .models()
            .get_model(&reference.provider, &reference.model_id)
    });
    let Some(model) = model else {
        return fail_no_model(runtime, reference.as_ref(), context).await;
    };
    // 能解析出 model 就说明 reference 也存在，这里的 expect 不会真的 panic。
    let reference = reference.expect("model exists only with a reference");
    let policy = compaction_policy(&settings, &reference);
    let view = runtime
        .context(conversation_id, context, ContextViewOptions::default())
        .await?;
    // 没有切点 = 没有可压缩的内容（整段放得下，或找不到合法切点）：无需压缩，直接完成。
    let Some(cut) = select_cut(&view, policy.keep_recent_tokens) else {
        return complete(runtime, context).await;
    };
    // ContextView 的 entries 与 contributions 一一对应，切点下标可直接用来取条目 ID。
    let first_kept = view.entries[cut].id;
    let input: CompactionInput = serde_json::from_value(task.input.clone())?;
    let compaction = BeforeCompactInput {
        reason: input.reason,
        entries: view.entries[..cut].to_vec(),
        messages: summarized_messages(&view, cut),
        first_kept,
        instructions: input.instructions.clone(),
    };
    // hook_api 把运行端借用延长成 'static；这个 Arc 只在本次 hook 调用期间有效，
    // 处理器不得把它保留到调用返回之后。
    let api: Arc<dyn HookApi> = crate::harness::live::hook_api(runtime);
    let mut decision: Option<BeforeCompactDecision> = None;
    for hook in
        hook_handlers::<dyn CompactionHooks, _, _, _, _>(runtime, "beforeCompact", context).await?
    {
        // 第一个决定胜出：一旦有决定，后面的处理器不再被调用。
        if decision.is_some() {
            continue;
        }
        match hook
            .handler
            .before_compact(compaction.clone(), Arc::clone(&api), context)
            .await
        {
            Ok(value) => decision = value,
            // TS `hooks.each`：抛错的处理器上报后跳过，后面的处理器仍可给出决定。
            Err(error) => hook_failure(
                runtime.signal().is_aborted(),
                hook.extension.as_deref(),
                error,
                |error, from| runtime.report(error, from),
            )?,
        }
    }
    // hook 的两种决定都绕过模型摘要：Decline 表示不需要压缩，Summary 直接使用给定文本。
    match decision {
        Some(BeforeCompactDecision::Decline(_)) => return complete(runtime, context).await,
        Some(BeforeCompactDecision::Summary(summary)) => {
            return place(runtime, first_kept, summary.summary, context).await;
        }
        None => {}
    }
    let max_tokens = summary_max_tokens(policy.reserve_tokens, model.max_tokens);
    // tail 取条目 ID 的最大值（从 first_kept 起），不是最后一个下标：summarize 以它为界重放上下文。
    let tail = view
        .entries
        .iter()
        .map(|entry| entry.id)
        .fold(first_kept, |tail, id| if id > tail { id } else { tail });
    let request = SummaryRequest {
        attempt: 1,
        model: reference,
        thinking_level: agent.thinking_level,
        stream_options: settings.stream.clone(),
        max_tokens,
        tail,
        first_kept,
    };
    let checkpoint = serde_json::to_value(CompactionCheckpoint::Summarize(request))?;
    // 只更新任务状态，不动文档；下一次调度按 phase="summarize" 进入 SummarizePhase。
    let change = TaskCommit::new(move |_tx, _current| {
        let checkpoint = checkpoint.clone();
        Box::pin(async move { Ok(Some(TaskState::Running { checkpoint })) })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

// `summarize` 阶段：请求 provider 生成摘要，并按结果转移。
//
// 关键前提：用 `request.tail` 重放上下文，保证与 select 看到的前缀一致；`first_kept` 在重放
// 结果里的位置就是切点。切点找不到时沿用 JS `slice(0, -1)` 的回退（见 `slice_end`）。
//
// 结束路径（都在一个提交里完成）：
// - 有摘要 → `place_summary` 放置条目，终态 completed；
// - 可重试错误 → 更新 live 的 retry 信息，checkpoint 转 `Retry`，状态 `sleeping`；
// - 其它失败 → 写 `compactionFailure`，终态 failed（detail.reason = "model_error"）。
// 中止优先于以上所有路径：直接返回 Err，本次提交（usage、终态、摘要条目）都不执行，交给
// abort 处理器清理状态。
async fn summarize(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    // 提交闭包要读时钟/设置并调用 place_summary；闭包只活到 commit 返回，借用不会逃逸。
    let runtime = static_runtime(runtime);
    let checkpoint = checkpoint_of(task)?;
    // phase 不匹配说明任务定义或迁移有问题；返回 Err 让调度器把它标成 faulted。
    let request = match checkpoint {
        CompactionCheckpoint::Summarize(request) => request,
        other => return Err(format!("Compaction task is not summarizing: {other:?}").into()),
    };
    let conversation_id = runtime.conversation_id();
    let model = runtime
        .models()
        .get_model(&request.model.provider, &request.model.model_id);
    let Some(model) = model else {
        return fail_no_model(runtime, Some(&request.model), context).await;
    };
    // `tail` 处的上下文不可变，所以这就是 `select` 选中的范围。
    let view = runtime
        .context(
            conversation_id,
            context,
            ContextViewOptions {
                at: Some(request.tail),
            },
        )
        .await?;
    // -1 表示 first_kept 不在重放结果里；slice_end 把 -1 还原成 JS 的「去掉最后一条」。
    let cut = view
        .entries
        .iter()
        .position(|entry| entry.id == request.first_kept)
        .map(|index| index as i64)
        .unwrap_or(-1);
    let now = runtime.now();
    let input: CompactionInput = serde_json::from_value(task.input.clone())?;
    let summarized = summarized_messages(&view, slice_end(view.contributions.len(), cut));
    // 摘要请求固定是「system 提示 + user 文本」两条消息；不使用 AiContext.system_prompt，
    // `tools` 为 None（请求里没有任何工具）。
    let messages: Vec<Message> = vec![
        Message::System(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
            sections: None,
            tools_removed: None,
            tools_added: None,
            timestamp: now,
        }),
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(
                kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: summary_prompt(&summarized, input.instructions.as_deref()),
                    text_signature: None,
                },
            )]),
            timestamp: now,
        }),
    ];
    // 旧会话可能还没有 provider session id；这一步可能补写一次迁移提交，失败则本阶段返回 Err。
    let session_id = crate::harness::provider::ensure_provider_session_id(runtime, context).await?;
    // TS: `const { deferred: _deferred, ...forwarded } = streamOptions;` —— 摘要请求永不延迟，
    // 否则 stopReason 会是 `deferred` 而拿不到摘要文本。
    let mut forwarded = request.stream_options.clone();
    forwarded.deferred = None;
    // cacheRetention 显式传 None（TS 的 `cacheRetention: "none"`）；maxTokens 在这里截成非负 u64。
    let mut options = simple_stream_options(
        &forwarded,
        Some(&runtime.signal()),
        context.telemetry(),
        session_id,
        Some(request.max_tokens.max(0.0) as u64),
        Some(kiana_ai::types::CacheRetention::None),
    );
    options.reasoning = thinking_level(request.thinking_level);
    let message = runtime
        .models()
        .complete_simple(
            &model,
            &AiContext {
                system_prompt: None,
                messages: messages.clone(),
                tools: None,
            },
            options.into(),
        )
        .await;
    // 中止标记或关闭：abort 调用或重新打开的任务处理已提交状态。
    if runtime.signal().is_aborted() {
        return Err(Box::new(abort_error(&runtime.signal())));
    }
    let summary = summary_text(&message);
    // provider 的失败也表达为 AssistantMessage（stop_reason=Error）。只有同时满足「可重试错误
    // 模式 + 策略开启 + attempt 未超上限」才退避重试；attempt 从 1 开始。
    let policy = runtime.settings()?.retry.clone();
    let retry = message.stop_reason == StopReason::Error
        && is_retryable_assistant_error(&message)
        && policy.enabled
        && request.attempt <= policy.max_retries;
    // 退避按 JS 数值语义计算（f64 的 attempt 与安全整数上限），与 TS 的 retryDelayMs 一致。
    let until = if retry {
        runtime.now()
            + retry_delay_ms_js(
                policy.base_delay_ms,
                policy.max_agent_delay_ms,
                request.attempt as f64,
            ) as i64
    } else {
        0
    };
    let task_id = id_from_number(runtime.task_id().get());
    let first_kept = request.first_kept;
    let provider = message.provider.clone();
    let model_id = message.model.clone();
    let usage = message.usage.clone();
    let error_message = message.error_message.clone();
    let retry_request = request.clone();
    let failure_text = summary_failure(&message);
    let change = TaskCommit::new(move |tx, current| {
        let summary = summary.clone();
        let retry_request = retry_request.clone();
        let usage = usage.clone();
        let provider = provider.clone();
        let model_id = model_id.clone();
        let error_message = error_message.clone();
        let failure_text = failure_text.clone();
        Box::pin(async move {
            // 先记账：usage 在成功/重试/失败分支之前统一写入，三种结果都会记账。
            record_usage(
                tx,
                conversation_id,
                UsageBucket::Models,
                &format!("{provider}/{model_id}"),
                &usage,
            )
            .await?;
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            // 成功路径：放置摘要条目（内部会清掉 compactions 状态与 compactionFailure）。
            if let Some(summary) = summary {
                return Ok(Some(
                    place_summary(
                        tx, runtime, task_id, current, &mut live, first_kept, &summary,
                    )
                    .await?,
                ));
            }
            if retry {
                // live 里的 retry 只用于展示（下一次尝试时间与错误）；真正驱动恢复的是
                // Sleeping 状态里保存的 Retry checkpoint。
                if let Some(status) = compaction_status(live.value_mut(), task_id)
                    && let Some(object) = status.as_object_mut()
                {
                    let mut retry_state = serde_json::Map::new();
                    retry_state.insert("at".to_owned(), JsonValue::from(until));
                    retry_state.insert(
                        "error".to_owned(),
                        JsonValue::String(error_message.clone().unwrap_or_default()),
                    );
                    object.insert("retry".to_owned(), JsonValue::Object(retry_state));
                }
                let checkpoint = serde_json::to_value(CompactionCheckpoint::Retry(
                    RetryCheckpoint::new(retry_request, until),
                ))?;
                return Ok(Some(TaskState::Sleeping { checkpoint, until }));
            }
            // 不可重试失败：写 compactionFailure 并终态；detail.reason 与 generation 的失败约定一致。
            fail_compaction_status(live.value_mut(), task_id, &failure_text);
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: crate::types::TaskOutcomeError {
                        message: failure_text,
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

// `retry` 阶段：退避睡眠与 attempt+1。
//
// 状态转移：Sleeping(retry, until) ──到点──▶ Running(summarize, attempt+1)。
// 未到点时把同一个 Retry checkpoint 带 phase 标签再存一次，因为 sleep_until 需要完整
// checkpoint；重启后调度器仍能按 phase="retry" 找到这个处理器。
async fn retry_phase(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let checkpoint = checkpoint_of(task)?;
    let CompactionCheckpoint::Retry(retry) = checkpoint else {
        return Err("Compaction task is not retrying".into());
    };
    if retry.until > runtime.now() {
        // TS 原样重存 `task.state.checkpoint`（带 `phase` 标签），重启后才能再解析回 retry。
        let until = retry.until;
        let checkpoint = serde_json::to_value(CompactionCheckpoint::Retry(retry))?;
        return runtime.sleep_until(until, checkpoint, context).await;
    }
    // 到点：attempt 加一，清掉 live 的 retry 展示，转回 summarize。
    let attempt = retry.attempt + 1;
    let request = retry.request();
    let conversation_id = runtime.conversation_id();
    let task_id = id_from_number(runtime.task_id().get());
    let change = TaskCommit::new(move |tx, _current| {
        let request = request.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            if let Some(status) = compaction_status(live.value_mut(), task_id)
                && let Some(object) = status.as_object_mut()
            {
                object.insert("attempt".to_owned(), JsonValue::from(attempt));
                object.remove("retry");
            }
            let checkpoint =
                serde_json::to_value(CompactionCheckpoint::Summarize(SummaryRequest {
                    attempt,
                    ..request
                }))?;
            Ok(Some(TaskState::Running { checkpoint }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

// 从当前任务状态取 checkpoint 并反序列化。
//
// 失败模式：终态没有 checkpoint（`state_checkpoint` 返回 `None`）→ 字符串错误；JSON 形状与
// `CompactionCheckpoint` 不符 → serde 错误。两者都会让阶段返回 Err，由调度器把任务标成 faulted。
fn checkpoint_of(
    task: &RunningTask<JsonValue, JsonValue, JsonValue>,
) -> Result<CompactionCheckpoint, BoxError> {
    let checkpoint = state_checkpoint(&task.state).ok_or("Compaction task has no checkpoint")?;
    Ok(serde_json::from_value(checkpoint.clone())?)
}

/// 在它自己的提交里放置 hook 提供的摘要（TS: `place`）。
///
/// hook 路径把摘要文本直接交给 `place_summary`，与模型摘要共用同一条放置逻辑。
async fn place(
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    first_kept: EntryId,
    summary: String,
    context: &dyn Context,
) -> Result<(), BoxError> {
    // 提交闭包里的 `placeSummary` 要读时钟与设置；闭包只活到 `commit` 返回。
    let runtime = static_runtime(runtime);
    let conversation_id = runtime.conversation_id();
    let task_id = id_from_number(runtime.task_id().get());
    let change = TaskCommit::new(move |tx, current| {
        let summary = summary.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            // TS 的 `placeSummary` 在提交里读时钟与队列模式。
            let state = place_summary(
                tx, runtime, task_id, current, &mut live, first_kept, &summary,
            )
            .await?;
            Ok(Some(state))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 放置摘要条目并完成（TS: `placeSummary`）。
///
/// 阻塞压缩（由 generation 拥有）直接追加；会话拥有的压缩作为 write 提交被接纳。
///
/// 副作用都在调用方的事务里：
///
/// - 移除本任务的 `compactions` 状态，并清掉 `compactionFailure`（成功产出了摘要）；
/// - 追加或排队一条 `kiana.compaction` 条目：`head = first_kept`、`model = [包装后的 user
///   消息]`、`data = { reason }`；
/// - `CompactionResult` 只填一个 ID：阻塞路径是 `entryId`，会话拥有路径是 `submissionId`
///   （此时写入可能还在队列里，尚未成为条目）。
///
/// 会话拥有路径的 `requestId = "compaction:{taskId}"` 让同一任务重复放置时复用已有提交。
#[allow(clippy::too_many_arguments)]
async fn place_summary(
    tx: &dyn Tx,
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    task_id: TaskId,
    current: &TaskRecord,
    live: &mut DocDraft,
    first_kept: EntryId,
    summary: &str,
) -> Result<TaskState, BoxError> {
    // 成功产出摘要：移除进行中状态，并清掉上一次的失败记录。
    remove_compaction_status(live.value_mut(), task_id);
    live.value_mut().shift_remove("compactionFailure");
    // user 消息文本 = 前缀 + 摘要正文 + 后缀；这是之后模型看到的替换内容。
    let text = format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}");
    let input: CompactionInput = serde_json::from_value(current.input.clone())?;
    let message = Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::Text(kiana_ai::types::TextContent {
            kind_text: kiana_ai::types::TextKind::Text,
            text,
            text_signature: None,
        })]),
        timestamp: runtime.now(),
    });
    let draft = EntryDraft {
        kind: crate::types::CompactionEntry.kind.to_string(),
        model: Some(vec![message]),
        // reason 来自任务输入（manual / threshold / overflow），记录这条摘要为何产生。
        data: Some(serde_json::to_value(crate::types::CompactionData {
            reason: input.reason,
        })?),
        head: Some(EntryHead::Id(first_kept)),
        edits: None,
        // TS 的裸草稿字面量：`{ kind, head, model, data }`。
        key_order: Some(
            ["kind", "head", "model", "data"]
                .iter()
                .map(|key| (*key).to_owned())
                .collect(),
        ),
    };
    // 拥有权决定放置路径：会话拥有（owner 为 None）必须经过准入队列；generation 拥有则直接追加。
    let result = if current.owner.is_none() {
        // requestId 固定为 compaction:{taskId}，同一任务重复放置会复用已有提交。
        let admission = admit_submission(
            tx,
            current.conversation_id,
            &SubmissionDraft {
                request_id: Some(format!("compaction:{}", task_id.get())),
                kind: crate::harness::types::SubmissionDraftKind::Write { entry: draft },
            },
            runtime.now(),
            QueueModes::from_settings(&*runtime.settings()?),
            None,
        )
        .await?;
        CompactionResult {
            entry_id: None,
            submission_id: Some(admission.id),
        }
    } else {
        CompactionResult {
            entry_id: Some(tx.append_entry(current.conversation_id, draft).await?.id),
            submission_id: None,
        }
    };
    Ok(TaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: serde_json::to_value(result)?,
        },
    })
}

/// 移除状态并完成，不带摘要（TS: `complete`）。
///
/// 没有可压缩内容、或 hook 决定放弃压缩时走这里：只移除 `compactions` 状态并返回空结果 `{}`。
/// 刻意不清 `compactionFailure`——本次没有产出摘要，旧的失败记录仍然有效。
async fn complete(
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let conversation_id = runtime.conversation_id();
    let task_id = id_from_number(runtime.task_id().get());
    let change = TaskCommit::new(move |tx, _current| {
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            remove_compaction_status(live.value_mut(), task_id);
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Completed {
                    result: serde_json::json!({}),
                },
            }))
        })
    });
    runtime
        .commit(&change, context, CommitOptions::default())
        .await
}

/// 「模型不可用」的终态：写 `compactionFailure` 并以失败结束，不尝试摘要。
///
/// `reference` 为 `None` 表示 agent 没有配置模型；`Some` 表示配置了但注册表查不到，错误文本会
/// 带上 `provider/modelId`。detail 里的 `reason: "no_model"` 与 generation 的失败约定一致
/// （模型请求失败用 `"model_error"`）。
async fn fail_no_model(
    runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, CompactionHookTable>,
    reference: Option<&ModelRef>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let message = match reference {
        None => "No model is configured".to_owned(),
        Some(reference) => format!(
            "Model {}/{} is not available",
            reference.provider, reference.model_id
        ),
    };
    let conversation_id = runtime.conversation_id();
    let task_id = id_from_number(runtime.task_id().get());
    let change = TaskCommit::new(move |tx, _current| {
        let message = message.clone();
        Box::pin(async move {
            let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
            let mut live = tx.doc(&target).await?;
            fail_compaction_status(live.value_mut(), task_id, &message);
            Ok(Some(TaskState::Terminal {
                outcome: TaskOutcome::Failed {
                    error: crate::types::TaskOutcomeError {
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
// Task definition and creation
// ---------------------------------------------------------------------------

/// 内建压缩任务定义（TS: `CompactionTask`）。
///
/// 版本 1；初始 checkpoint 是 `{"phase":"select"}`；`phases` 依次登记 `select` / `summarize`
/// / `retry`（顺序与 TS 声明一致；运行期按 checkpoint 的 `phase` 名字查表，不依赖顺序）；
/// `abort` 是 [`AbortPhase`]。没有 `migrate`（TS 同样没有，当前只有版本 1），
/// `hooks` 为 `None`——hook 处理器来自运行端的 hook 表，而不是任务定义。
pub fn compaction_task() -> crate::types::Task<JsonValue, JsonValue, JsonValue, CompactionHookTable>
{
    let mut phases: indexmap::IndexMap<
        String,
        Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, CompactionHookTable>>,
    > = indexmap::IndexMap::new();
    phases.insert("select".to_owned(), Arc::new(SelectPhase));
    phases.insert("summarize".to_owned(), Arc::new(SummarizePhase));
    phases.insert("retry".to_owned(), Arc::new(RetryPhase));
    crate::types::tasks::define_task(TaskDefinition {
        name: COMPACTION_TASK_KIND.to_owned(),
        version: 1,
        initial: Arc::new(|_input| serde_json::json!({ "phase": "select" })),
        phases,
        abort: Arc::new(AbortPhase),
        migrate: None,
        hooks: None,
    })
}

/// 擦除后的内建压缩任务（注册表用）。
///
/// `LazyLock` 保证每个进程只构造一次；`registry` 与 [`create_compaction`] 通过 clone 共享同一个
/// `Arc<TaskDefinition>`，所以运行期不会出现两份阶段表。
pub static COMPACTION_TASK: LazyLock<AnyTask> =
    LazyLock::new(|| crate::harness::live::erase_task_definition(&compaction_task().definition));

/// 创建一次压缩任务，并在同一次提交里把它的状态写进 `kiana.live`。
///
/// `owner` 是等待它的 generation（阻塞压缩）；没有 owner 时它由会话拥有，且除非是 manual
/// 否则是 background 的。
///
/// # 参数与副作用
///
/// - `input` 原样序列化成任务输入；`reason` 同时写进 live 状态与之后摘要条目的 `data`。
/// - `owner == Some(task_id)`：任务归属该 generation，`blocking = true`，generation 用
///   `JoinPolicy::AllSettled` 等待它；摘要条目由该任务直接追加。
/// - `owner == None`：会话拥有的顶层任务；`manual` 之外的 threshold/overflow 压缩同时标记为
///   `background`（排除在普通 idle 等待、会话 abort 与级联之外），摘要通过提交队列放置。
/// - 除任务记录外，还向 `kiana.live` 的 `compactions` 追加一条状态（`attempt = 1`，无 retry）。
///
/// 返回带结果品牌的 `TaskId`；真正的 `CompactionResult`（`entryId` 或 `submissionId`）由放置
/// 路径决定。
///
/// # 约束
///
/// 本函数只在传入的事务里写，不负责提交；现有调用方都把它放进一次提交，因此任务记录、live
/// 状态与调用方的其它写入（例如 generation 的 checkpoint）原子生效。它也不做去重：同一会话是否
/// 已有压缩由调用方判断（后台阈值压缩会先检查 `live.compactions` 是否存在）。
pub async fn create_compaction(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    input: CompactionInput,
    owner: Option<TaskId>,
) -> Result<TaskId<CompactionResult>, BoxError> {
    let ownership = match owner {
        None => TaskOwnership::Conversation,
        Some(owner) => TaskOwnership::Task {
            task_id: id_from_number(owner.get()),
        },
    };
    // background 只对会话拥有的任务有意义；手工压缩保持前台语义（计入 idle 等待与取消级联）。
    let background = owner.is_none() && input.reason != CompactionReason::Manual;
    let mut options = TaskOptions::new(ownership);
    options.conversation_id = Some(conversation_id);
    options.background = Some(background);
    let task_id = tx
        .create_task(&COMPACTION_TASK, serde_json::to_value(&input)?, options)
        .await?;
    let status = crate::harness::live::CompactionStatus {
        task_id: id_from_number(task_id.get()),
        reason: input.reason,
        blocking: owner.is_some(),
        attempt: 1,
        retry: None,
    };
    let target = DocTarget::conversation(&LIVE_DOC, conversation_id);
    let mut live = tx.doc(&target).await?;
    add_compaction_status(live.value_mut(), &status);
    Ok(id_from_number(task_id.get()))
}

// 本文件的单元测试只覆盖能脱离调度器验证的纯函数：
// - 序列化 / 截断 / 摘要判定 / 策略公式 / 切点选择 / 请求选项映射。
// 阶段行为（`beforeCompact` hook 抛错与「第一个决定胜出」、摘要放置的条目形状与 head、
// `SummarizePhase` 不转发 `deferred`）由 tests/harness_compaction.rs 覆盖；
// 依赖完整 Harness 的阈值/溢出编排与失败重试同样不在本文件范围（见该文件的模块说明）。
#[cfg(test)]
mod tests {
    use kiana_ai::types::{
        AssistantContentBlock, AssistantMessage, AssistantRole, StopReason, TextContent, TextKind,
        ToolCall, ToolCallKind, ToolResultMessage, ToolResultRole, UserContent, UserMessage,
        UserRole,
    };
    use serde_json::json;

    use super::*;

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.to_owned()),
            timestamp: 0,
        })
    }

    fn assistant_text(text: &str) -> Message {
        Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: text.to_owned(),
                text_signature: None,
            })],
            ..Default::default()
        })
    }

    fn tool_result(call_id: &str, text: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: call_id.to_owned(),
            tool_name: "read".to_owned(),
            content: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: text.to_owned(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        })
    }

    #[test]
    fn serialize_conversation_omits_system_and_thinking() {
        let mut thinking = AssistantMessage {
            role: AssistantRole::Assistant,
            ..Default::default()
        };
        thinking.content.push(AssistantContentBlock::Thinking(
            kiana_ai::types::ThinkingContent {
                kind_thinking: kiana_ai::types::ThinkingKind::Thinking,
                thinking: "hidden".to_owned(),
                thinking_signature: None,
                redacted: None,
            },
        ));
        thinking
            .content
            .push(AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "visible".to_owned(),
                text_signature: None,
            }));
        let messages = vec![
            Message::System(SystemMessage {
                role: SystemRole::System,
                content: SystemContent::Text("sys".to_owned()),
                sections: None,
                tools_removed: None,
                tools_added: None,
                timestamp: 0,
            }),
            user("hi"),
            Message::Assistant(thinking),
            tool_result("c1", "out"),
        ];
        assert_eq!(
            serialize_conversation(&messages),
            "[User]: hi\n\n[Assistant]: visible\n\n[Tool result]: out"
        );
    }

    #[test]
    fn serialize_conversation_prints_tool_calls_with_json_arguments() {
        let message = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "c1".to_owned(),
                name: "read".to_owned(),
                arguments: [("path".to_owned(), json!("/tmp"))].into_iter().collect(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            })],
            ..Default::default()
        });
        assert_eq!(
            serialize_conversation(&[message]),
            "[Assistant tool calls]: read(path=\"/tmp\")"
        );
    }

    #[test]
    fn truncate_keeps_the_js_character_count() {
        let text = "abcdef";
        assert_eq!(truncate_chars(text, 6), "abcdef");
        assert_eq!(
            truncate_chars(text, 3),
            "abc\n\n[... 3 more characters truncated]"
        );
    }

    #[test]
    fn summary_text_requires_a_clean_stop_with_text() {
        let mut message = AssistantMessage {
            role: AssistantRole::Assistant,
            stop_reason: StopReason::Stop,
            ..Default::default()
        };
        message
            .content
            .push(AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "  hi  ".to_owned(),
                text_signature: None,
            }));
        assert_eq!(summary_text(&message), Some("hi".to_owned()));
        message.stop_reason = StopReason::Length;
        assert_eq!(summary_text(&message), None);
        message.stop_reason = StopReason::Stop;
        message.content.clear();
        assert_eq!(summary_text(&message), None);
    }

    #[test]
    fn summary_failure_names_each_kind() {
        let mut message = AssistantMessage {
            stop_reason: StopReason::Error,
            error_message: Some("boom".to_owned()),
            ..Default::default()
        };
        assert_eq!(summary_failure(&message), "Summarization failed: boom");
        message.stop_reason = StopReason::Length;
        assert_eq!(
            summary_failure(&message),
            "Summarization hit the token limit; the summary is incomplete"
        );
        message.stop_reason = StopReason::Stop;
        message
            .content
            .push(AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "c1".to_owned(),
                name: "read".to_owned(),
                arguments: Default::default(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }));
        assert_eq!(
            summary_failure(&message),
            "Summarization attempted to call a tool"
        );
        message.content.clear();
        assert_eq!(summary_failure(&message), "Summarization produced no text");
    }

    #[test]
    fn summary_prompt_wraps_the_conversation_and_focus() {
        let prompt = summary_prompt(&[user("hi")], Some("be brief"));
        assert!(prompt.starts_with("<conversation>\n[User]: hi\n</conversation>\n\n"));
        assert!(prompt.ends_with("\n\nAdditional focus: be brief"));
    }

    #[test]
    fn slice_end_matches_js_negative_slicing() {
        assert_eq!(slice_end(5, 3), 3);
        assert_eq!(slice_end(5, -1), 4);
        assert_eq!(slice_end(0, -1), 0);
        assert_eq!(slice_end(5, 9), 5);
    }

    #[test]
    fn select_cut_picks_the_first_candidate_after_the_budget() {
        // 4 个 user 条目；每条估算相同，所以预算决定切点。
        let contributions: Vec<Vec<Message>> = (0..4)
            .map(|index| vec![user(&format!("m{index}"))])
            .collect();
        let view = ContextView {
            head: None,
            entries: Vec::new(),
            contributions,
            messages: Vec::new(),
        };
        let tokens = estimate_message_tokens(&user("m0")) as f64;
        // 预算为 0：从尾部开始，第一个下标 3 就满足，切在 3。
        assert_eq!(select_cut(&view, 0.0), Some(3));
        // 预算覆盖两条：切在 2。
        assert_eq!(select_cut(&view, tokens * 2.0), Some(2));
    }

    #[test]
    fn select_cut_rejects_a_user_entry_whose_result_follows() {
        let mut assistant = AssistantMessage {
            role: AssistantRole::Assistant,
            ..Default::default()
        };
        assistant
            .content
            .push(AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "c1".to_owned(),
                name: "read".to_owned(),
                arguments: Default::default(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }));
        let contributions = vec![
            vec![user("a")],
            vec![Message::Assistant(assistant)],
            vec![user("steer-1")],
            vec![tool_result("c1", "out")],
            vec![assistant_text("done")],
        ];
        // 下标 2 的第一条是 user，但紧随其后的 tool result 属于下标 1 的调用 → 不是候选。
        assert!(!is_candidate(&contributions, 2));
        assert!(is_candidate(&contributions, 1));
        assert!(is_candidate(&contributions, 4));
    }

    #[test]
    fn summary_max_tokens_follows_the_js_math_min() {
        // floor(0.8 * reserve) 与模型上限取小；模型上限 <= 0 或缺失视为 Infinity。
        assert_eq!(summary_max_tokens(1000.0, None), 800.0);
        assert_eq!(summary_max_tokens(1000.0, Some(500)), 500.0);
        assert_eq!(summary_max_tokens(1000.0, Some(5000)), 800.0);
        assert_eq!(summary_max_tokens(1000.0, Some(0)), 800.0);
        assert_eq!(summary_max_tokens(1.0, None), 0.0);
    }

    #[test]
    fn simple_stream_options_carries_deferred_and_cache_settings() {
        use crate::harness::types::{Deferred, DeferredWindow};
        use kiana_ai::types::{CacheRetention, DeferredRequest};
        let stream = ConversationStreamOptions {
            cache_retention: Some(CacheRetention::Long),
            deferred: Some(Deferred::Enabled(true)),
            timeout_ms: Some(1500.0),
            ..Default::default()
        };
        let options = simple_stream_options(
            &stream,
            None,
            kiana_telemetry::noop_telemetry_context(),
            Some("session".to_owned()),
            None,
            stream.cache_retention,
        );
        assert_eq!(options.deferred, Some(DeferredRequest::Enabled(true)));
        assert_eq!(options.base.cache_retention, Some(CacheRetention::Long));
        assert_eq!(options.base.session_id.as_deref(), Some("session"));
        assert_eq!(options.base.timeout_ms, Some(1500));

        let windowed = ConversationStreamOptions {
            deferred: Some(Deferred::Window {
                window: Some(DeferredWindow::OneDay),
            }),
            ..Default::default()
        };
        let options = simple_stream_options(
            &windowed,
            None,
            kiana_telemetry::noop_telemetry_context(),
            None,
            None,
            None,
        );
        assert_eq!(
            options.deferred,
            Some(DeferredRequest::Window {
                window: Some(kiana_ai::types::DeferredWindow::H24),
            })
        );
        assert_eq!(options.base.cache_retention, None);
    }
}
