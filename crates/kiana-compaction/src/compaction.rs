//! core/compaction/compaction.ts（压缩切点、prompt、completeSummarization）。
//!
//! 纯函数式的压缩逻辑：token 估算、切点选择、摘要 prompt 组装，以及所有摘要调用共用的
//! LLM 收敛点 [`complete_summarization`]。会话管理（I/O）由 kiana-session 负责。
//!
//! # 这个模块解决什么问题
//! 上下文窗口接近上限时，继续把完整历史发给模型会失败或超出预算。压缩把旧历史交给 LLM
//! 生成一份结构化摘要，后续请求只携带“摘要 + 最近消息”，任务仍能继续。本文件只负责
//! 决定总结哪些消息、保留哪些消息，以及调用模型生成摘要；把摘要写成压缩条目、落盘、
//! 发事件等 I/O 由 `kiana-session` 的 `AgentSession` 完成。
//!
//! # 在系统中的位置
//! - 上游：`kiana-session` 的自动/手动压缩先调用 [`prepare_compaction`] 得到
//!   [`CompactionPreparation`]，再调用 [`compact`]；`session_before_compact` 扩展事件把
//!   同一份 preparation 暴露给 `kiana-extensions`，扩展可以取消压缩或直接提供
//!   [`CompactionResult`]。
//! - 下游：`kiana-ai`（模型/消息类型、`complete_simple`、重试与取消）、
//!   `kiana-session-core`（会话条目、投影、`convert_to_llm`）、本 crate 的 `utils`
//!   （文件操作收集、消息序列化）。
//! - 分支摘要（`branch_summarization` 模块）复用本文件的 [`CompactionError`]、
//!   [`complete_summarization`] 与 `build_summarization_context`，两条摘要链路共享重试与
//!   错误语义。
//!
//! # 一次压缩的执行流程
//! 1. [`prepare_compaction`]：修复 session-core 无法类型化的 `message` 条目，构建投影，
//!    定位上一次压缩边界，按 `keep_recent_tokens` 选切点，产出待摘要消息、turn 前缀消息、
//!    文件操作与 `tokens_before`。
//! 2. [`compact`]：普通情况一次 LLM 摘要；切分 turn 时先摘要历史、再摘要 turn 前缀，
//!    最后拼成一份文本。
//! 3. 文件列表拼到摘要末尾并写入 `details`，由调用方保存。
//!
//! # 重要约束
//! - 所有面向 TS 的数值/字符串语义按 JS 行为复刻：UTF-16 长度、`Number()` 转换、
//!   `NaN` 传播。代码里的 `Option`/`NaN`/宽松 JSON 分支是在 Rust 类型系统中保留 JS 在
//!   畸形会话上的结果，不要按“更 Rust 化”重写。
//! - prompt 常量属于当前稳定契约，由文件底部测试锁定长度；它们直接进入模型上下文，改动需同步测试。
//! - [`CompactionError`] 的 `Display` 文本就是 TS 的 `error.message`，属于用户可见输出。
//! - 取消信号来自 `StreamOptions.signal`，经 `AbortScope` 传给重试循环；在重试层面它只
//!   负责打断退避，更底层的请求中断由 `kiana-ai` 的流实现处理。

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::types::{AgentMessage, StreamFn, ThinkingLevel};
use kiana_ai::compat::complete_simple;
use kiana_ai::types::TranscriptContext;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheRetention, Context, JsonValue, Message, Model,
    ProviderEnv, ProviderHeaders, SimpleStreamOptions, StopReason, StreamOptions, SystemContent,
    SystemMessage, TextContent, TextKind, ToolResultMessage, Usage, UsageCost, UserContent,
    UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::error_body::safe_json_stringify;
use kiana_ai::utils::retry::{OnRetryFinished, RetryCallbacks, RetryPolicy, retry_assistant_call};
use kiana_ai::utils::text::content_text_assistant;
use kiana_ai::utils::transcript::{get_current_system_message, normalize_context};
use kiana_ai::utils::uuid::uuidv7;
use kiana_session_core::messages::convert_to_llm;
use kiana_session_core::session_manager::{
    LeafId, ProjectedSessionEntry, SessionEntry, SessionMessageEntry, SessionMessageType,
    SessionProjection, build_session_projection, session_entry_to_context_messages,
};
use kiana_session_core::usage_totals::combine_usage;
use serde::{Deserialize, Serialize};

use crate::utils::{
    FileOperations, FileValue, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, json_object_to_value,
    serialize_conversation,
};

// ============================================================================
// 文件操作跟踪
// ============================================================================

/// `CompactionEntry.details` 中保存的文件跟踪信息。
///
/// 该结构同时是 [`CompactionResult`] 的 `details` 内容与会话条目 `details` 的读回形状：
/// 下一次 [`prepare_compaction`] 会把 `readFiles` / `modifiedFiles` 重新并入文件跟踪。
/// 路径元素保留原始 JSON 值（[`FileValue`]），排序与渲染由 `utils` 按 JS 规则处理。
///
/// TS: `compaction.ts#CompactionDetails`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    /// 只读文件。
    pub read_files: Vec<FileValue>,
    /// 修改过的文件。
    pub modified_files: Vec<FileValue>,
}

/// 条目 `fromHook` 的 JS 真值判断。
///
/// TS 直接读取条目对象的 `fromHook`：任意真值（`1`、`"yes"`、非空数组/对象……）都算
/// “扩展生成”，其 details 不再参与文件跟踪。typed 字段把非布尔值归一成 `None`
/// （kiana-session-core 的宽松反序列化），因此从文件解析的条目优先读 `raw_entry`；通过
/// append API 新建的条目没有原始对象，回退到 typed 布尔值。
///
/// TS: `compaction.ts#extractFileOperations` / `branch-summarization.ts#prepareBranchEntries`
pub(crate) fn entry_from_hook_truthy(entry: &SessionEntry) -> bool {
    let raw = match entry {
        SessionEntry::Compaction(compaction) => compaction.raw_entry.as_ref(),
        SessionEntry::BranchSummary(branch) => branch.raw_entry.as_ref(),
        SessionEntry::Unknown(object) => return object.get("fromHook").is_some_and(js_truthy),
        _ => None,
    };
    match raw.and_then(|raw| raw.get("fromHook")) {
        Some(value) => js_truthy(value),
        None => match entry {
            SessionEntry::Compaction(compaction) => compaction.from_hook == Some(true),
            SessionEntry::BranchSummary(branch) => branch.from_hook == Some(true),
            _ => false,
        },
    }
}

/// 从消息与前一次压缩条目中提取文件操作。
///
/// 两个来源：`prev_compaction_index` 指向的压缩条目 `details`（仅当它不由 `fromHook`
/// 生成时），以及 `messages` 里 toolCall 推导出的读写操作。`entries` 是投影后的来源条目
/// 列表，`prev_compaction_index` 与投影条目下标一一对应。
///
/// TS: `compaction.ts#extractFileOperations`
fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    // 上一次压缩的 details（仅 kiana 生成的条目；fromHook 的条目跳过）。
    if let Some(entry) = prev_compaction_index.and_then(|index| entries.get(index)) {
        let details = match entry {
            SessionEntry::Compaction(compaction) => compaction.details.as_ref(),
            SessionEntry::Unknown(object) => {
                object.get("details").filter(|details| js_truthy(details))
            }
            _ => None,
        };
        if !entry_from_hook_truthy(entry)
            && let Some(details) = details
        {
            if let Some(read_files) = details.get("readFiles").and_then(JsonValue::as_array) {
                for path in read_files {
                    // TS 的 Set 接受任意元素（不做 `typeof === "string"` 检查）：原始值入集合，
                    // 只在渲染时按 JS 规则转换（见 [`FileValue`]）。
                    file_ops.read.insert(FileValue::from(path.clone()));
                }
            }
            if let Some(modified_files) = details.get("modifiedFiles").and_then(JsonValue::as_array)
            {
                for path in modified_files {
                    file_ops.edited.insert(FileValue::from(path.clone()));
                }
            }
        }
    }

    // 消息中的 toolCall。
    for message in messages {
        extract_file_ops_from_message(message, &mut file_ops);
    }

    file_ops
}

// ============================================================================
// 消息提取
// ============================================================================

/// 从一个投影条目中取出压缩用的 AgentMessage。
///
/// `compaction` 来源条目不贡献对话；system 消息属于提示状态而不是对话，因此被过滤。
///
/// TS: `compaction.ts#getMessagesFromProjectedEntryForCompaction`
fn messages_from_projected_entry_for_compaction(
    entry: &ProjectedSessionEntry,
) -> Vec<AgentMessage> {
    if entry.source_entry.kind() == "compaction" {
        return Vec::new();
    }
    entry
        .messages
        .iter()
        .filter(|message| message.role() != "system")
        .cloned()
        .collect()
}

/// 把 kiana-session-core 降级为 [`SessionEntry::Unknown`] 的 `type: "message"` 条目重建为 typed
/// 消息条目。
///
/// TS 加载会话文件时不做字段校验：`timestamp` 等结构字段类型异常（例如数字时间戳）不会
/// 影响 `entry.type === "message"` 的判断，消息照常进入上下文。kiana-session-core 对这类条目
/// 退回 [`SessionEntry::Unknown`]，而 `session_entry_to_context_messages` 对未知 `message`
/// 条目返回空。这里用原始 `message` 对象重建 typed 条目（消息以
/// [`AgentMessage::Custom`] 承载，`session_entry_to_context_messages` 会归一化
/// `content: null`），使投影、切点与 token 估算基于同一份消息表示。
///
/// `message` 不是对象（字符串/数字/数组/null）时 `AgentMessage` 无法表示，保持原条目；
/// TS 的原样透传值同样会被 `convertToLlm` 丢弃，模型可见上下文一致。
fn recover_unknown_message_entry(entry: &SessionEntry) -> Option<SessionEntry> {
    let SessionEntry::Unknown(object) = entry else {
        return None;
    };
    if entry.kind() != "message" {
        return None;
    }
    let JsonValue::Object(message) = object.get("message")? else {
        return None;
    };
    Some(SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: entry.id().to_string(),
        parent_id: entry.parent_id().map(str::to_string),
        // 时间戳对消息投影无意义；非字符串值归一为空串。
        timestamp: entry.timestamp().unwrap_or_default().to_string(),
        message: AgentMessage::Custom(Box::new(
            message
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )),
        raw_message: None,
    }))
}

/// 条目贡献的上下文消息（含 Unknown `message` 条目的修复）。
///
/// TS 的 `sessionEntryToContextMessages(entry)` 直接读取原始条目；本函数在
/// kiana-session-core 的 `Unknown` 回退上补回 `type: "message"` 的消息。
///
/// TS: `session-manager.ts#sessionEntryToContextMessages` 的调用点
pub(crate) fn context_messages_for_entry(entry: &SessionEntry) -> Vec<AgentMessage> {
    match recover_unknown_message_entry(entry) {
        Some(recovered) => session_entry_to_context_messages(&recovered),
        None => session_entry_to_context_messages(entry),
    }
}

/// 修复一组路径条目中的 Unknown `message` 条目（供投影使用）。
///
/// 只有存在可修复条目时才克隆；顺序与下标保持不变。
fn recovered_path_entries(entries: &[SessionEntry]) -> Cow<'_, [SessionEntry]> {
    if !entries
        .iter()
        .any(|entry| recover_unknown_message_entry(entry).is_some())
    {
        return Cow::Borrowed(entries);
    }
    Cow::Owned(
        entries
            .iter()
            .map(|entry| recover_unknown_message_entry(entry).unwrap_or_else(|| entry.clone()))
            .collect(),
    )
}

/// 保留显式 `null`：`details?: T` 里省略与 `null` 是两种不同的 JSON 形状。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    JsonValue::deserialize(deserializer).map(Some)
}

/// `compact()` 的返回值；SessionManager 保存时会补 uuid/parentUuid。
///
/// 字段流转：[`compact`] 填好 `summary` / `first_kept_entry_id` / `tokens_before` /
/// `usage` / `details`，`estimated_tokens_after` 留空；调用方追加条目后重新估算上下文并
/// 回填该字段（`kiana-session` 的 `AgentSession` 即如此）。扩展也可以通过
/// `session_before_compact` 直接提供本结构，此时扩展负责 `summary` / `first_kept_entry_id` /
/// `tokens_before` / `usage` / `details`；`estimated_tokens_after` 仍由调用方重新估算并覆盖
/// （`AgentSession` 在自动与手动路径都会用重算值替换扩展传入的值）。
///
/// TS: `compaction.ts#CompactionResult`（TS 的泛型 `<T>` 在 Rust 侧用任意 JSON 表达）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    /// 摘要文本（含文件列表）。
    pub summary: String,
    /// 保留区起点的条目 id。
    pub first_kept_entry_id: String,
    /// 压缩前的上下文 token 数。
    pub tokens_before: u64,
    /// 压缩后的估算 token 数（`compact()` 不设置）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<u64>,
    /// 生成摘要的 LLM 调用用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 扩展私有数据（例如结构化压缩的版本标记）。
    ///
    /// 显式 `null` 会保留（TS 里 `details: null` 与缺省是两种 JSON 形状）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub details: Option<JsonValue>,
}

// ============================================================================
// 类型
// ============================================================================

/// 压缩设置。
///
/// 所有数值的单位都是 token（不是字符）；消息字符到 token 的换算见 [`estimate_tokens`]。
/// [`Default`] 与 [`DEFAULT_COMPACTION_SETTINGS`] 的数值相同，但两者是独立定义，
/// 修改其中一处必须同步另一处，否则“缺省设置”的含义会随入口不同而变化。
///
/// TS: `compaction.ts#CompactionSettings`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    /// 是否启用自动压缩。
    pub enabled: bool,
    /// 为 prompt + 回复保留的 token 数。
    pub reserve_tokens: u64,
    /// 最近需要保留的 token 数。
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        CompactionSettings {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
        }
    }
}

/// 默认压缩设置。
///
/// 这是 `CompactionSettings::default()` 的常量副本；`Default` 实现不是 `const fn`，
/// 无法复用，因此两处数值需手工保持一致。
///
/// TS: `compaction.ts#DEFAULT_COMPACTION_SETTINGS`
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

/// 上下文 token 估算结果。
///
/// 不变量：`tokens == usage_tokens + trailing_tokens`（两处都用饱和加法）。有有效 usage
/// 时，`usage_tokens` 是锚点值、`trailing_tokens` 只统计锚点之后的消息；没有有效 usage
/// 时 `usage_tokens` 为 0，`tokens` 与 `trailing_tokens` 都等于对全部消息的纯字符估算，
/// `last_usage_index` 为 `None`。
///
/// TS: `compaction.ts#ContextUsageEstimate`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextUsageEstimate {
    /// 估算的上下文 token 总数。
    pub tokens: u64,
    /// 来自最后一次 assistant usage 的 token 数。
    pub usage_tokens: u64,
    /// usage 之后的尾随消息估算 token 数。
    pub trailing_tokens: u64,
    /// 最后一次有效 usage 在消息列表中的下标。
    pub last_usage_index: Option<usize>,
}

/// 切点选择结果。
///
/// 所有下标都是调用方传入的 entries 切片下标（不是消息下标）。`turn_start_index` 为
/// `None` 表示未识别到被切开的 turn：切点落在 turn 起点，或区间内找不到 turn 起点
/// （TS 同样用 `-1` 表达这两种情形）；`is_split_turn` 为真时
/// `turn_start_index` 必然为 `Some`（由 [`find_cut_point`] 与 `find_projected_cut_point`
/// 的构造方式保证；后者是私有函数，仅在此说明）。
///
/// TS: `compaction.ts#CutPointResult`（TS 的 `turnStartIndex: -1` 用 `None` 表达）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CutPointResult {
    /// 保留区起点条目下标。
    pub first_kept_entry_index: usize,
    /// 被切开的 turn 的起始条目下标（未切开时为 `None`）。
    pub turn_start_index: Option<usize>,
    /// 切点是否落在 turn 中间。
    pub is_split_turn: bool,
}

/// 压缩前的准备数据（供扩展在 `before_compact` 检查/修改）。
///
/// 由 [`prepare_compaction`] 构造，随后同时交给 `before_compact` 扩展与 [`compact`]。
/// `messages_to_summarize` 与 `turn_prefix_messages` 覆盖两种切点情形：普通切点全部进
/// 前者；切分 turn 时，turn 起点到切点之间的消息进后者、切点之前的历史进前者。两者都
/// 为空时 [`prepare_compaction`] 返回 `None`，因此不会生成空摘要。
///
/// TS: `compaction.ts#CompactionPreparation`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPreparation {
    /// 保留区起点条目 id。
    pub first_kept_entry_id: String,
    /// 将被总结并丢弃的消息。
    pub messages_to_summarize: Vec<AgentMessage>,
    /// 将被总结成 turn prefix 摘要的消息（切分 turn 时）。
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// 切点是否落在 turn 中间。
    pub is_split_turn: bool,
    /// 压缩前的上下文 token 数。
    pub tokens_before: u64,
    /// 上一次压缩的摘要（用于迭代更新）。
    ///
    /// `None` 同时表示 TS 的 `undefined`（条目缺 `summary`）与 `null`（`summary: null`）：
    /// 两者在下游的真值判断与 `??` 里语义相同，`Option<String>` 无法区分；序列化时省略该
    /// 键（TS 对 `undefined` 也省略，对 `null` 会输出 `null`——畸形会话才有的差异）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_summary: Option<String>,
    /// 从 `messages_to_summarize` 提取的文件操作。
    pub file_ops: FileOperations,
    /// 当前压缩设置。
    pub settings: CompactionSettings,
}

/// 摘要生成失败。
///
/// 区分两类失败：软失败是模型正常返回但 `stop_reason` 为 `error`/`length`，或响应里
/// 出现 toolCall；硬失败是流被拒绝、流结果 reject、`uuidv7` 失败。硬失败在
/// [`complete_summarization`] 中不重试；软失败由调用方决定如何呈现。
///
/// `Display` 逐字等于 TS 抛出的 `error.message`。
///
/// `kiana-session` 会把它拼进 `"Compaction failed: {error}"` 的事件文本，因此这里的措辞
/// 属于用户可见输出，修改会同时破坏与 TS 的一致性（文件底部测试锁定这些字符串）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionError {
    /// `` `${label} failed: ${errorMessage || "Unknown error"}` ``
    SummarizationFailed {
        /// TS 的 label（`"Summarization"` / `"Turn prefix summarization"` / `"Branch summarization"`）。
        label: String,
        /// `errorMessage`；缺省或空串时输出 `"Unknown error"`。
        message: Option<String>,
    },
    /// `` `${label} failed: generation hit the token cap and the summary is incomplete` ``
    TokenCap {
        /// TS 的 label。
        label: String,
    },
    /// `` `${label} attempted to call a tool` ``
    ToolCallAttempted {
        /// TS 的 label。
        label: String,
    },
    /// `"First kept entry has no UUID - session may need migration"`
    FirstKeptEntryMissing,
    /// `uuidv7()` 无法生成路由 session id。
    SessionIdUnavailable(String),
    /// 硬失败：`streamFn` 拒绝或流结果 reject。
    ///
    /// TS 的 `completeSummarization` 没有捕获这些 rejection（`retryAssistantCall` 也不会重试
    /// 抛错的 `produce`），错误对象原样冒泡；这里保存 `error.message` 并逐字输出。
    StreamFailed {
        /// 原始错误消息（`error.message`）。
        message: String,
    },
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactionError::SummarizationFailed { label, message } => {
                let message = message
                    .as_deref()
                    .filter(|message| !message.is_empty())
                    .unwrap_or("Unknown error");
                write!(f, "{label} failed: {message}")
            }
            CompactionError::TokenCap { label } => write!(
                f,
                "{label} failed: generation hit the token cap and the summary is incomplete"
            ),
            CompactionError::ToolCallAttempted { label } => {
                write!(f, "{label} attempted to call a tool")
            }
            CompactionError::FirstKeptEntryMissing => {
                f.write_str("First kept entry has no UUID - session may need migration")
            }
            CompactionError::SessionIdUnavailable(message) => f.write_str(message),
            CompactionError::StreamFailed { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for CompactionError {}

// ============================================================================
// Token calculation
// ============================================================================

/// 从 usage 计算上下文 token 总数：`totalTokens` 优先，否则分量求和。
///
/// `total_tokens == 0` 被当作“provider 没给”的哨兵（反序列化缺 `totalTokens` 时正是
/// 填 0），回退到 input + output + cacheRead + cacheWrite 的饱和求和；负值在 `u64` 中
/// 不可表示。该函数的结果决定 usage 是否有效（`> 0`），也是自动压缩阈值的输入。
///
/// TS: `compaction.ts#calculateContextTokens`
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens != 0 {
        usage.total_tokens
    } else {
        usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    }
}

/// 从消息中取有效 usage（跳过 aborted / error / 全零）。
///
/// TS: `compaction.ts#getAssistantUsage`
fn get_assistant_usage(message: &AgentMessage) -> Option<Usage> {
    if message.role() != "assistant" {
        return None;
    }
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            if assistant.stop_reason == StopReason::Aborted
                || assistant.stop_reason == StopReason::Error
            {
                return None;
            }
            if calculate_context_tokens(&assistant.usage) > 0 {
                Some(assistant.usage.clone())
            } else {
                None
            }
        }
        AgentMessage::Custom(object) => {
            // 原始对象回放（带额外键的标准消息）：按 TS 的运行期字段读取。
            let stop_reason = object
                .get("stopReason")
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            if stop_reason == "aborted" || stop_reason == "error" {
                return None;
            }
            let usage_value = object.get("usage")?;
            if !js_truthy(usage_value) {
                return None;
            }
            let usage = usage_from_json_lenient(usage_value)?;
            if calculate_context_tokens(&usage) > 0 {
                Some(usage)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 读取 JSON 数值为 token 计数（忽略缺省、非数值、负数与非有限值）。
fn json_token_count(value: Option<&JsonValue>) -> Option<u64> {
    value
        .and_then(JsonValue::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value as u64)
}

/// JS `Number(value)`：JSON 值 → `f64`（不可转换时为 `NaN`）。
///
/// `calculateContextTokens` 与 `estimateTokens` 都依赖 JS 的隐式数字转换：`null` → 0、
/// 布尔 → 0/1、字符串按 `StringNumericLiteral`、数组 → `Array.prototype.toString` 后再解析、
/// 对象 → `NaN`。与 `kiana-ai` 内部对同类字段使用的 `js_to_number` 保持一致。
fn js_number_value(value: &JsonValue) -> f64 {
    match value {
        JsonValue::Null => 0.0,
        JsonValue::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        JsonValue::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        JsonValue::String(text) => kiana_json::parse_js_number(text),
        JsonValue::Array(_) => {
            kiana_json::parse_js_number(&crate::utils::js_value_to_string(value))
        }
        JsonValue::Object(_) => f64::NAN,
    }
}

/// 读取 JSON 值为 token 计数（JS `Number(value)` 后取整；缺省/非有限/负数按 0）。
///
/// `Usage` 的冻结字段是 `u64`，无法表达 JS 的 `undefined`/`NaN`；只有已经通过
/// `calculateContextTokens > 0` 校验的 usage 才会走到这里。
fn json_token_value(value: Option<&JsonValue>) -> u64 {
    let number = value.map(js_number_value).unwrap_or(0.0);
    if number.is_finite() && number >= 0.0 {
        number as u64
    } else {
        0
    }
}

/// 读取 JSON 数值为 cost（TS 不校验；缺省 / 非数值按 0）。
fn json_cost_number(value: Option<&JsonValue>) -> f64 {
    value.and_then(JsonValue::as_f64).unwrap_or(0.0)
}

/// 从原始 JSON 宽松解析 usage（TS 不校验字段；provider 常常省略 `totalTokens`）。
///
/// TS 的 `getAssistantUsage` 用 `calculateContextTokens(usage) > 0` 判断有效性：
/// `usage.totalTokens || usage.input + usage.output + usage.cacheRead + usage.cacheWrite`。
/// 真值 `totalTokens` 直接参与数值比较；否则四个分量求和，**缺任何一个都是 `undefined`，
/// 求和为 `NaN`，比较恒为 false**（该 usage 被忽略，估算回退到 chars/4）。
/// 负的 `totalTokens`（真值但 `> 0` 为 false）与非数值 `totalTokens` 同样无效。
///
/// TS 的 `getAssistantUsage` 原样返回会话文件里的 usage 对象：`cost`、`reasoning`、
/// `cacheWrite1h` 都必须保留。源对象省略 `totalTokens` 时用 `0` 表示缺省（`Usage` 的
/// 冻结字段是 `u64`，无法表达“键不存在”；`calculateContextTokens` 的 `0 → 分量求和`
/// 回退与 JS 的 `undefined || sum` 等价）。
///
/// 表示能力边界：JS 里字符串分量参与 `+` 会做字符串拼接（`"500" + 0` 得到 `"5000"`），
/// `Usage` 的 `u64` 字段只能按数值保存；这里对每个字段做 `Number()` 转换。
fn usage_from_json_lenient(value: &JsonValue) -> Option<Usage> {
    let object = value.as_object()?;
    let total_value = object.get("totalTokens");
    let input_value = object.get("input");
    let output_value = object.get("output");
    let cache_read_value = object.get("cacheRead");
    let cache_write_value = object.get("cacheWrite");

    // TS 的 `totalTokens || sum` 后接 `> 0`。
    let context_tokens = match total_value {
        Some(total) if js_truthy(total) => js_number_value(total),
        _ => match (
            input_value,
            output_value,
            cache_read_value,
            cache_write_value,
        ) {
            (Some(input), Some(output), Some(cache_read), Some(cache_write)) => {
                js_number_value(input)
                    + js_number_value(output)
                    + js_number_value(cache_read)
                    + js_number_value(cache_write)
            }
            // 缺分量 → `undefined`，JS 求和为 NaN。
            _ => f64::NAN,
        },
    };
    if context_tokens.is_nan() || context_tokens <= 0.0 {
        return None;
    }

    let total_tokens = match total_value {
        Some(total) if js_truthy(total) => json_token_value(Some(total)),
        _ => 0,
    };
    let cost = object.get("cost").and_then(JsonValue::as_object);
    let read_cost = |key: &str| json_cost_number(cost.and_then(|cost| cost.get(key)));
    Some(Usage {
        input: json_token_value(input_value),
        output: json_token_value(output_value),
        cache_read: json_token_value(cache_read_value),
        cache_write: json_token_value(cache_write_value),
        cache_write1h: json_token_count(object.get("cacheWrite1h")),
        reasoning: json_token_count(object.get("reasoning")),
        total_tokens,
        cost: UsageCost {
            input: read_cost("input"),
            output: read_cost("output"),
            cache_read: read_cost("cacheRead"),
            cache_write: read_cost("cacheWrite"),
            total: read_cost("total"),
            total_from_provider: cost
                .and_then(|cost| cost.get("totalFromProvider"))
                .and_then(JsonValue::as_bool),
        },
    })
}

/// 从会话条目中找到最后一个有效的 assistant usage。
///
/// 从后向前扫描，跳过非 `message` 条目；每条消息经 `get_assistant_usage` 过滤
/// （aborted/error stop 与全零 usage 都无效），返回第一条有效 usage 的克隆。该函数也是
/// `kiana-coding-agent` SDK 导出面的一部分。
///
/// TS 只判断 `entry.type === "message"`，不校验结构字段；kiana-session-core 降级成
/// [`SessionEntry::Unknown`] 的 message 条目同样参与（见 `recover_unknown_message_entry`）。
///
/// TS: `compaction.ts#getLastAssistantUsage`
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if entry.kind() != "message" {
            continue;
        }
        let recovered = recover_unknown_message_entry(entry);
        let message = match recovered.as_ref() {
            Some(SessionEntry::Message(message_entry)) => Some(&message_entry.message),
            _ => match entry {
                SessionEntry::Message(message_entry) => Some(&message_entry.message),
                _ => None,
            },
        };
        if let Some(message) = message
            && let Some(usage) = get_assistant_usage(message)
        {
            return Some(usage);
        }
    }
    None
}

/// 找到最后一个有效 usage 及其消息下标。
///
/// TS: `compaction.ts#getLastAssistantUsageInfo`
fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(Usage, usize)> {
    for index in (0..messages.len()).rev() {
        if let Some(usage) = get_assistant_usage(&messages[index]) {
            return Some((usage, index));
        }
    }
    None
}

/// 从消息估算上下文 token：有 usage 时以它为锚，之后的消息单独估算。
///
/// usage 记录的是“产生它的那次请求所看到的上下文”，因此覆盖了该消息之前的全部历史；
/// 只有 `index + 1..` 的尾随消息需要逐条估算。返回的 `last_usage_index` 是 `messages`
/// 的下标（不是会话条目下标），调用方可用它把锚点映射回来源条目。
///
/// TS: `compaction.ts#estimateContextTokens`
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let Some((usage, index)) = get_last_assistant_usage_info(messages) else {
        let estimated: u64 = messages
            .iter()
            .map(estimate_tokens)
            .fold(0, u64::saturating_add);
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(&usage);
    let mut trailing_tokens = 0u64;
    for message in &messages[index + 1..] {
        trailing_tokens = trailing_tokens.saturating_add(estimate_tokens(message));
    }

    ContextUsageEstimate {
        tokens: usage_tokens.saturating_add(trailing_tokens),
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// `getCurrentSystemMessage(messages)` 的 AgentMessage 版本：只回放 system 消息。
///
/// 所有 system 消息会被回放成一条“当前系统提示”（正文按顺序拼接，sections 依次应用
/// 补丁），而不是只取最后一条。
///
/// 原始对象回放（带额外键的标准消息）会先按 [`SystemMessage`] 宽松解析。
fn current_system_message(messages: &[AgentMessage]) -> Option<SystemMessage> {
    let system_messages: Vec<Message> = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::System(system)) => {
                Some(Message::System(system.clone()))
            }
            AgentMessage::Custom(object) => {
                if object.get("role").and_then(JsonValue::as_str) != Some("system") {
                    return None;
                }
                serde_json::from_value::<SystemMessage>(json_object_to_value(object))
                    .ok()
                    .map(Message::System)
            }
            _ => None,
        })
        .collect();
    if system_messages.is_empty() {
        return None;
    }
    get_current_system_message(&system_messages)
}

/// `context_edit` 对目标消息的效果。
enum ContextEditEffect {
    /// `replacement === null`：从模型上下文省略目标。
    Omit,
    /// `replacement.content`。
    Replace(JsonValue),
}

/// 从投影条目收集 `context_edit` 的效果（后出现的覆盖先出现的）。
///
/// 与 `buildSessionProjection` 一样只扫描进入投影的条目：typed 条目按 session-core 的语义
/// 处理，原始对象条目按 TS 的运行期字段读取。
fn projected_context_edits(
    entries: &[ProjectedSessionEntry],
) -> IndexMap<String, ContextEditEffect> {
    let mut edits: IndexMap<String, ContextEditEffect> = IndexMap::new();
    for entry in entries {
        match &entry.source_entry {
            SessionEntry::ContextEdit(edit) => {
                let effect = match &edit.replacement {
                    Some(replacement) => ContextEditEffect::Replace(replacement.content.clone()),
                    None => ContextEditEffect::Omit,
                };
                edits.insert(edit.target_id.clone(), effect);
            }
            SessionEntry::Unknown(object) if entry.source_entry.kind() == "context_edit" => {
                let Some(target_id) = object.get("targetId").and_then(JsonValue::as_str) else {
                    continue;
                };
                match object.get("replacement") {
                    Some(JsonValue::Null) => {
                        edits.insert(target_id.to_string(), ContextEditEffect::Omit);
                    }
                    Some(JsonValue::Object(replacement)) => {
                        if let Some(content) = replacement.get("content") {
                            edits.insert(
                                target_id.to_string(),
                                ContextEditEffect::Replace(content.clone()),
                            );
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    edits
}

/// `{ ...message, content }`（`session-manager.ts#projectContextEntry` 的原始对象分支）。
fn apply_context_replacement_to_custom(
    message: AgentMessage,
    replacement: &JsonValue,
) -> AgentMessage {
    let AgentMessage::Custom(mut object) = message else {
        return message;
    };
    let role = object
        .get("role")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    if !matches!(
        role.as_str(),
        "user" | "assistant" | "toolResult" | "custom"
    ) {
        return AgentMessage::Custom(object);
    }
    // TS：assistant/toolResult 的字符串替换先转成单个 text 块；其余原样。
    let content = if (role == "assistant" || role == "toolResult") && replacement.is_string() {
        JsonValue::Array(vec![serde_json::json!({
            "type": "text",
            "text": replacement.as_str().unwrap_or_default(),
        })])
    } else {
        replacement.clone()
    };
    object.insert("content".to_string(), content);
    AgentMessage::Custom(object)
}

/// TS `buildContextEntries` 永远跳过压缩点之前的 system 消息；kiana-session-core 只识别 typed
/// message 条目，Unknown 的 system 消息会被错误地留在投影里。补回消息时跳过它们。
fn is_pre_compaction_system_message(entry: &SessionEntry, branch_entries: &[SessionEntry]) -> bool {
    let Some(compaction_index) = branch_entries
        .iter()
        .rposition(|entry| entry.kind() == "compaction")
    else {
        return false;
    };
    let Some(entry_index) = branch_entries
        .iter()
        .position(|candidate| candidate.id() == entry.id())
    else {
        return false;
    };
    entry_index < compaction_index
        && matches!(entry, SessionEntry::Unknown(object)
            if object
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(JsonValue::as_str)
                == Some("system"))
}

/// 补回 kiana-session-core 在投影阶段丢掉的 Unknown `message` 消息。
///
/// 外部调用方（kiana-session）的投影由 `buildSessionProjection` 生成，其 `Unknown` 回退会让
/// 结构字段异常的 message 条目丢失消息；TS 不校验字段，消息仍然进入上下文。这里按原始条目
/// 重建消息，并应用 `context_edit` 的省略/替换语义以及压缩点之前 system 消息的排除规则。
///
/// 没有可修复条目时直接借用原投影（零拷贝，正常路径无额外开销）。
///
/// 只有存在“Unknown `message` 且投影为空”的条目时才克隆投影并重算 `messages`。
fn recovered_projection<'a>(
    projection: &'a SessionProjection,
    branch_entries: &[SessionEntry],
) -> Cow<'a, SessionProjection> {
    let needs_recovery = projection.entries.iter().any(|entry| {
        entry.messages.is_empty()
            && entry.source_entry.kind() == "message"
            && matches!(entry.source_entry, SessionEntry::Unknown(_))
            && recover_unknown_message_entry(&entry.source_entry).is_some()
    });
    if !needs_recovery {
        return Cow::Borrowed(projection);
    }

    let edits = projected_context_edits(&projection.entries);
    let mut recovered = projection.clone();
    // 只处理“投影为空且能修复”的条目；其余条目与顺序保持不变。
    for entry in &mut recovered.entries {
        if !entry.messages.is_empty()
            || entry.source_entry.kind() != "message"
            || !matches!(entry.source_entry, SessionEntry::Unknown(_))
        {
            continue;
        }
        if is_pre_compaction_system_message(&entry.source_entry, branch_entries) {
            continue;
        }
        let messages = context_messages_for_entry(&entry.source_entry);
        if messages.is_empty() {
            continue;
        }
        entry.messages = match edits.get(entry.source_entry.id()) {
            Some(ContextEditEffect::Omit) => Vec::new(),
            Some(ContextEditEffect::Replace(replacement)) => messages
                .into_iter()
                .map(|message| apply_context_replacement_to_custom(message, replacement))
                .collect(),
            None => messages,
        };
    }
    // 重建扁平消息列表，保持 projection.messages 与 entries 一致。
    recovered.messages = recovered
        .entries
        .iter()
        .flat_map(|entry| entry.messages.iter().cloned())
        .collect();
    Cow::Owned(recovered)
}

/// 估算投影后的上下文：usage 若早于最近一次 context_edit/compaction 则不再可信。
///
/// 处理顺序：
/// 1. 先补回 session-core 丢失的 Unknown `message` 条目（`recovered_projection`）；
/// 2. 用 [`estimate_context_tokens`] 正常估算。随后把最后一条 usage 所在的消息下标映射到
///    投影条目、再到 `branch_entries`；如果该条目的位置不晚于最近一条
///    `context_edit`/`compaction`（或映射不到），说明 usage 之后上下文被改写/压缩过，
///    锚点作废；
/// 3. 作废时从头估算：把全部 system 消息回放成一条“当前系统提示”计入一次，其余消息
///    逐条计入，避免历史 system 快照被重复计数。
///
/// 该函数是自动压缩阈值判断的输入，只读会话，不产生副作用。
///
/// TS: `compaction.ts#estimateProjectedContextTokens`
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    // kiana-session-core 的投影会丢掉 Unknown message 条目的消息；估算前补回（TS 不校验字段）。
    let recovered = recovered_projection(projection, branch_entries);
    let projection = recovered.as_ref();

    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        // 把投影消息下标映射回来源条目：累计每条目的消息数，第一个覆盖该下标的条目即锚点。
        let mut projected_message_index = 0usize;
        let mut usage_entry_id: Option<String> = None;
        for entry in &projection.entries {
            let next_message_index = projected_message_index + entry.messages.len();
            if last_usage_index < next_message_index {
                usage_entry_id = Some(entry.source_entry.id().to_string());
                break;
            }
            projected_message_index = next_message_index;
        }

        // 把 usage 锚点映射回 branch_entries 下标，再与最近一次 context_edit/compaction
        // 的位置比较：锚点更早说明它记录的上下文已被改写。
        let usage_entry_index = usage_entry_id
            .as_deref()
            .and_then(|id| branch_entries.iter().position(|entry| entry.id() == id));
        let latest_invalidating_entry_index = branch_entries
            .iter()
            .rposition(|entry| entry.kind() == "context_edit" || entry.kind() == "compaction");

        let usage_index = usage_entry_index.map_or(-1, |index| index as i64);
        let invalidating_index = latest_invalidating_entry_index.map_or(-1, |index| index as i64);
        if usage_index > invalidating_index {
            return estimate;
        }
    }

    // 锚点不可用：回放当前 system 提示计一次，其余消息逐条估算（system 消息不重复计入）。
    let current_system = current_system_message(&projection.messages);
    let mut tokens = current_system
        .map(|system| estimate_tokens(&AgentMessage::Standard(Message::System(system))))
        .unwrap_or(0);
    for message in &projection.messages {
        if message.role() != "system" {
            tokens = tokens.saturating_add(estimate_tokens(message));
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// 是否应触发压缩：`contextTokens > contextWindow - reserveTokens`。
///
/// `context_tokens` 通常来自 [`estimate_context_tokens`] 或
/// [`estimate_projected_context_tokens`]，单位 token。返回 `true` 只表示“达到阈值”；
/// 是否真正执行由会话层决定（还受模型可用性、扩展取消等影响）。
///
/// Rust 侧用 `u128` 比较，避免 `reserveTokens > contextWindow` 时无符号下溢
/// （JS 会得到负数，比较结果等价）。
///
/// TS: `compaction.ts#shouldCompact`
pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    u128::from(context_tokens) + u128::from(settings.reserve_tokens) > u128::from(context_window)
}

// ============================================================================
// 切点检测
// ============================================================================

/// 一个图片块估算的字符数。
///
/// TS: `compaction.ts#ESTIMATED_IMAGE_CHARS`
const ESTIMATED_IMAGE_CHARS: u64 = 4800;

/// JS `string.length`（UTF-16 code unit）的 `u64` 版本。
fn utf16_len(text: &str) -> u64 {
    text.encode_utf16().count() as u64
}

/// JS 真值判断。
fn js_truthy(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => false,
        JsonValue::Bool(value) => *value,
        JsonValue::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        JsonValue::String(text) => !text.is_empty(),
        JsonValue::Array(_) | JsonValue::Object(_) => true,
    }
}

/// `JSON.stringify(value).length`（UTF-16）。
fn json_stringify_len(value: &JsonValue) -> u64 {
    utf16_len(&safe_json_stringify(value))
}

/// typed user 内容块的字符数（文本按 UTF-16、图片固定 4800）。
fn user_blocks_chars(blocks: &[UserContentBlock]) -> u64 {
    blocks
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => utf16_len(&text.text),
            UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .fold(0, u64::saturating_add)
}

/// typed `string | (Text|Image)[]` 内容的字符数。
fn user_content_chars(content: &UserContent) -> u64 {
    match content {
        UserContent::Text(text) => utf16_len(text),
        UserContent::Blocks(blocks) => user_blocks_chars(blocks),
    }
}

/// `Math.ceil(chars / 4)`（JS 数值版，保留 `NaN`）。
fn ceil_div4_f64(chars: f64) -> f64 {
    (chars / 4.0).ceil()
}

/// JS `value.length` 按 `ToNumber` 转换后的数值（`estimateTokens` 的 `chars = x.length`）。
///
/// 返回 `NaN` 表示 JS 会在 `null`/`undefined` 上读 `.length` 抛 `TypeError`，或属性值
/// 不可转数字（数字/布尔字面量没有 `length`，普通对象没有 `length` 键）。Rust 侧不复制
/// 崩溃，用 `NaN` 近似；预算比较在 TS 抛错前是 false，与此一致。
fn js_length_number(value: Option<&JsonValue>) -> f64 {
    match value {
        None | Some(JsonValue::Null) => f64::NAN,
        Some(JsonValue::String(text)) => utf16_len(text) as f64,
        Some(JsonValue::Array(items)) => items.len() as f64,
        Some(JsonValue::Object(object)) => match object.get("length") {
            Some(length) => js_number_value(length),
            None => f64::NAN,
        },
        Some(JsonValue::Number(_)) | Some(JsonValue::Bool(_)) => f64::NAN,
    }
}

/// JS `estimateTextAndImageContentChars(content)` 的数值版本（保留 `NaN`）。
///
/// - 字符串 → UTF-16 长度；
/// - 数组 → 每个块：`block.type === "text" && block.text` 时加 `block.text.length`，
///   `block.type === "image"` 时加 [`ESTIMATED_IMAGE_CHARS`]；
/// - 其余（缺省 / `null` / 数字 / 普通对象）→ `for...of` 抛 `TypeError`，这里用 `NaN` 近似。
fn json_content_chars_f64(content: Option<&JsonValue>) -> f64 {
    match content {
        Some(JsonValue::String(text)) => utf16_len(text) as f64,
        Some(JsonValue::Array(blocks)) => {
            let mut chars = 0.0;
            for block in blocks {
                match block {
                    JsonValue::Object(object) => {
                        let kind = object
                            .get("type")
                            .and_then(JsonValue::as_str)
                            .unwrap_or_default();
                        if kind == "text" {
                            if let Some(text) = object.get("text")
                                && js_truthy(text)
                            {
                                chars += js_length_number(Some(text));
                            }
                        } else if kind == "image" {
                            chars += ESTIMATED_IMAGE_CHARS as f64;
                        }
                    }
                    // 数字/字符串/布尔块：`.type` 是 `undefined`，不计数。
                    JsonValue::Null => return f64::NAN, // `block.type` 抛 TypeError
                    _ => {}
                }
            }
            chars
        }
        _ => f64::NAN,
    }
}

/// `system.sections` 的字符数（`if (system.sections) for (Object.values(...)) if (section) chars += section.length`）。
fn add_sections_chars(chars: &mut f64, sections: Option<&JsonValue>) {
    match sections {
        Some(JsonValue::Object(sections)) => {
            for section in sections.values() {
                add_section_chars(chars, section);
            }
        }
        Some(JsonValue::Array(items)) => {
            for section in items {
                add_section_chars(chars, section);
            }
        }
        // `Object.values(string)` 得到每个 UTF-16 code unit 的单字符元素。
        Some(JsonValue::String(text)) => *chars += utf16_len(text) as f64,
        // 真值数字/布尔：`Object.values` 为空；`null`/缺省被真值判断跳过。
        _ => {}
    }
}

/// `if (section) chars += section.length`（JS 数值版）。
fn add_section_chars(chars: &mut f64, section: &JsonValue) {
    if js_truthy(section) {
        *chars += js_length_number(Some(section));
    }
}

/// typed `string | TextContent[]` 的字符数。
fn system_content_chars(content: &SystemContent) -> u64 {
    match content {
        SystemContent::Text(text) => utf16_len(text),
        SystemContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| utf16_len(&block.text))
            .fold(0, u64::saturating_add),
    }
}

/// system 消息的字符数（内容 + sections 的真值项 + `JSON.stringify(toolsAdded)`）。
///
/// 已知边界：typed [`kiana_ai::types::Tool`] 在反序列化时丢弃未声明的键，因此这里的
/// `JSON.stringify` 只包含 `name` / `description` / `parameters` / `constrainedSampling`；
/// TS 的 `system.toolsAdded` 是运行期原始对象，会保留额外键。修复需要 kiana-ai 在 `Tool`
/// 上保留未知键（本 crate 无法从 typed 消息恢复，见 spec §2.20.8）。
fn system_message_chars(system: &SystemMessage) -> u64 {
    let mut chars = system_content_chars(&system.content);
    if let Some(sections) = &system.sections {
        for section in sections.values().flatten() {
            chars = chars.saturating_add(utf16_len(section));
        }
    }
    if let Some(tools) = &system.tools_added {
        let value = serde_json::to_value(tools).unwrap_or(JsonValue::Null);
        chars = chars.saturating_add(json_stringify_len(&value));
    }
    chars
}

/// 估算一条消息的 token 数（chars / 4 上取整）。
///
/// 计入的字符按角色不同：system 计正文 + sections + `toolsAdded` 的 JSON 文本；user /
/// toolResult 计文本块（图片块固定 `ESTIMATED_IMAGE_CHARS`）；assistant 计文本、thinking
/// 与 toolCall 的 `name` + `arguments` JSON 文本；bashExecution 计 `command` + `output`；
/// 摘要消息只计 `summary`。这是压缩阈值的启发式，不是 provider 的真实 tokenizer。
///
/// TS 的估算在畸形消息上可能得到 `NaN`（例如 `branch_summary` 的 `summary` 是数字：
/// `(123).length` 是 `undefined`）。`estimate_tokens_f64` 保留这些 `NaN`；公开的
/// `u64` 接口无法表示 `NaN`，按 0 处理（见其文档）。
///
/// TS: `compaction.ts#estimateTokens`
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    let tokens = estimate_tokens_f64(message);
    // 表示能力边界：`ContextUsageEstimate` / `CompactionPreparation` 的 token 计数是
    // 整数，无法承载 `Math.ceil(NaN / 4)` 的 NaN；畸形消息按 0 计。
    if tokens.is_finite() && tokens >= 0.0 {
        tokens as u64
    } else {
        0
    }
}

/// 单个 tool result 的 token 估算（不构造 `AgentMessage` 副本）。
///
/// pruning / 聚合预算在热路径上遍历整段历史；用 [`estimate_tokens`] 需要先 clone
/// 每条 `ToolResultMessage`，而它的计算公式只依赖 content 块。结果与
/// `estimate_tokens(&AgentMessage::Standard(Message::ToolResult(result.clone())))`
/// 逐位相同。
pub fn estimate_tool_result_tokens(result: &ToolResultMessage) -> u64 {
    let tokens = ceil_div4_f64(user_blocks_chars(&result.content) as f64);
    if tokens.is_finite() && tokens >= 0.0 {
        tokens as u64
    } else {
        0
    }
}

/// [`estimate_tokens`] 的 JS 数值版本：保留 TS 在畸形消息上产生的 `NaN`。
///
/// `prepareBranchEntries` 的预算比较 `totalTokens + tokens > tokenBudget` 在 TS 里遇到
/// NaN 恒为 false（预算失效、更老的条目全部保留）；分支准备使用本函数复现该行为。
///
/// 能力边界：TS 在 `summary` / `command` 等字段缺失时读 `.length` 会抛 `TypeError`；
/// Rust 不复制崩溃，用 `NaN` 近似。
///
/// TS: `compaction.ts#estimateTokens`
pub(crate) fn estimate_tokens_f64(message: &AgentMessage) -> f64 {
    match message.role() {
        "system" => match message {
            AgentMessage::Standard(Message::System(system)) => {
                ceil_div4_f64(system_message_chars(system) as f64)
            }
            AgentMessage::Custom(object) => {
                let mut chars = json_content_chars_f64(object.get("content"));
                add_sections_chars(&mut chars, object.get("sections"));
                if let Some(tools_added) = object.get("toolsAdded")
                    && js_truthy(tools_added)
                {
                    chars += json_stringify_len(tools_added) as f64;
                }
                ceil_div4_f64(chars)
            }
            _ => 0.0,
        },
        "user" => match message {
            AgentMessage::Standard(Message::User(user)) => {
                ceil_div4_f64(user_content_chars(&user.content) as f64)
            }
            AgentMessage::Custom(object) => {
                ceil_div4_f64(json_content_chars_f64(object.get("content")))
            }
            _ => 0.0,
        },
        "assistant" => match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                let mut chars = 0u64;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            chars = chars.saturating_add(utf16_len(&text.text));
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            chars = chars.saturating_add(utf16_len(&thinking.thinking));
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            chars = chars.saturating_add(utf16_len(&call.name));
                            chars = chars.saturating_add(json_stringify_len(
                                &json_object_to_value(&call.arguments),
                            ));
                        }
                    }
                }
                ceil_div4_f64(chars as f64)
            }
            AgentMessage::Custom(object) => {
                // `for (const block of assistant.content)`：数组逐块统计；JS 字符串可迭代，
                // 每个字符没有 `.type`，贡献 0 字符；其余（缺失/null/数字/对象）抛 TypeError。
                let blocks = match object.get("content") {
                    Some(JsonValue::Array(blocks)) => blocks,
                    Some(JsonValue::String(_)) => return 0.0,
                    _ => return f64::NAN,
                };
                let mut chars = 0.0;
                for block in blocks {
                    let JsonValue::Object(block) = block else {
                        // `block.type` 在 null 上抛 TypeError；数字/字符串/布尔无 type。
                        if matches!(block, JsonValue::Null) {
                            return f64::NAN;
                        }
                        continue;
                    };
                    match block.get("type").and_then(JsonValue::as_str) {
                        Some("text") => chars += js_length_number(block.get("text")),
                        Some("thinking") => chars += js_length_number(block.get("thinking")),
                        Some("toolCall") => {
                            chars += js_length_number(block.get("name"));
                            match block.get("arguments") {
                                Some(arguments) => {
                                    chars += json_stringify_len(arguments) as f64;
                                }
                                // `JSON.stringify(undefined)` 是 `undefined`，`.length` 抛 TypeError。
                                None => return f64::NAN,
                            }
                        }
                        _ => {}
                    }
                }
                ceil_div4_f64(chars)
            }
            _ => 0.0,
        },
        "custom" | "toolResult" => match message {
            AgentMessage::Standard(Message::ToolResult(result)) => {
                ceil_div4_f64(user_blocks_chars(&result.content) as f64)
            }
            AgentMessage::Custom(object) => {
                ceil_div4_f64(json_content_chars_f64(object.get("content")))
            }
            _ => 0.0,
        },
        "bashExecution" => match message {
            AgentMessage::Custom(object) => {
                // `message.command.length + message.output.length`：缺失即抛 TypeError。
                ceil_div4_f64(
                    js_length_number(object.get("command"))
                        + js_length_number(object.get("output")),
                )
            }
            _ => 0.0,
        },
        "branchSummary" | "compactionSummary" => match message {
            AgentMessage::Custom(object) => {
                // `chars = message.summary.length`：非字符串数字的 `.length` 是 `undefined`。
                ceil_div4_f64(js_length_number(object.get("summary")))
            }
            _ => 0.0,
        },
        _ => 0.0,
    }
}

/// 是否是合法切点消息（toolResult 必须紧跟其 toolCall，因此不能切）。
///
/// TS: `compaction.ts#isCutPointMessage`
fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "assistant" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

/// 是否是 turn 起始消息。
///
/// TS: `compaction.ts#isTurnStartMessage`
fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

/// 条目是否是 turn 起点（compaction 条目永远不是）。
///
/// TS: `compaction.ts#isTurnStartEntry`
fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if entry.kind() == "compaction" {
        return false;
    }
    context_messages_for_entry(entry)
        .iter()
        .any(is_turn_start_message)
}

/// 找出 `[startIndex, endIndex)` 内所有合法切点。
///
/// 返回下标按升序排列（扫描顺序即条目顺序）；`endIndex <= startIndex` 或区间为空时返回
/// 空 `Vec`。压缩条目被显式跳过：即使它的投影消息属于 `compactionSummary` 角色，也不
/// 作为切点。
///
/// TS: `compaction.ts#findValidCutPoints`
fn find_valid_cut_points(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
) -> Vec<usize> {
    let mut cut_points = Vec::new();
    if end_index <= start_index {
        return cut_points;
    }
    for (index, entry) in entries
        .iter()
        .enumerate()
        .take(end_index.min(entries.len()))
        .skip(start_index)
    {
        if entry.kind() == "compaction" {
            continue;
        }
        if context_messages_for_entry(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(index);
        }
    }
    cut_points
}

/// 找到包含给定条目下标的 turn 起点。
///
/// `entry_index` 会先裁剪到 `entries.len() - 1`；若仍然小于 `start_index`（或 `entries`
/// 为空）就返回 `None`。否则从 `entry_index` 开始向下标递减方向扫描到 `start_index`，
/// 返回最近的 turn 起点条目下标（见 `is_turn_start_entry`）。
///
/// TS: `compaction.ts#findTurnStartIndex`（TS 返回 `-1`，Rust 用 `None`）
pub fn find_turn_start_index(
    entries: &[SessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    let last_index = entries.len().checked_sub(1)?;
    let entry_index = entry_index.min(last_index);
    if entry_index < start_index {
        return None;
    }
    (start_index..=entry_index)
        .rev()
        .find(|&index| is_turn_start_entry(&entries[index]))
}

/// 找出保留约 `keepRecentTokens` 的切点。
///
/// 从最新往回累加消息估算，达到预算后停在最近的有效切点；随后把不影响上下文的元数据条目
/// 一并划入保留区。切在 assistant 消息上时返回被切开的 turn 起点。
///
/// `start_index..end_index` 是条目下标区间，`end_index` 会被裁剪到 `entries.len()`；
/// 正常区间（`start_index < end_index`）下返回的 `first_kept_entry_index` 落在
/// `[start_index, end_index)` 内，区间为空时原样返回 `start_index`。算法分三步，
/// 与 TS 的 `findCutPoint` 一一对应：
///
/// 1. 收集区间内所有合法切点（`is_cut_point_message`）；一个都没有时保留全部，直接返回
///    `start_index`。
/// 2. 从 `end_index - 1` 向前累加每个条目的上下文消息估算 token（估算为 0 的条目跳过，
///    既不累加也不触发预算）。首次达到 `keep_recent_tokens` 时，选“不小于当前下标”的
///    最近切点，使切点尽量靠近预算边界。若尾随 toolResult 自身就超预算、找不到 `>=`
///    当前下标的切点，则退回最后一个切点，避免把 assistant 的 toolCall 与结果拆开。
/// 3. 切点向前回扫连续的无上下文消息条目（模型/thinking 变更、label 等），把它们并入
///    保留区；遇到压缩条目或带消息的条目立即停止。
///
/// 若切点不在 turn 起点，再回找该 turn 起点并标记 `is_split_turn`。
///
/// TS: `compaction.ts#findCutPoint`
pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    // JS 越界读取会抛错；Rust 侧把范围裁剪到实际条目数（调用方应传 entries.len()）。
    let end_index = end_index.min(entries.len());
    // 阶段 1：收集合法切点；区间内没有切点时返回 start_index，等价于“全部保留”。
    let cut_points = find_valid_cut_points(entries, start_index, end_index);
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0u64;
    let mut cut_index = cut_points[0];

    // 阶段 2：从最新条目向前累计估算 token，越过预算后停在最近的合法切点。
    if end_index > start_index {
        for index in (start_index..end_index).rev() {
            let message_tokens = context_messages_for_entry(&entries[index])
                .iter()
                .map(estimate_tokens)
                .fold(0, u64::saturating_add);
            if message_tokens == 0 {
                continue;
            }
            accumulated_tokens = accumulated_tokens.saturating_add(message_tokens);
            if accumulated_tokens >= keep_recent_tokens {
                // 优先取 >= 当前下标的最接近切点；尾随 tool results 自身超预算时退回最后
                // 一个切点（保留它们的 assistant toolCall）。
                cut_index = cut_points
                    .iter()
                    .find(|&&candidate| candidate >= index)
                    .copied()
                    .unwrap_or_else(|| *cut_points.last().unwrap_or(&cut_points[0]));
                break;
            }
        }
    }

    // 阶段 3：把切点前连续的无上下文条目并入保留区。
    // 回扫不影响上下文的相邻元数据条目。
    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if previous.kind() == "compaction" || !context_messages_for_entry(previous).is_empty() {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_turn_start_entry(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
        turn_start_index,
    }
}

// ============================================================================
// Summarization
// ============================================================================

/// 摘要 prompt（逐字）。
///
/// TS: `compaction.ts#SUMMARIZATION_PROMPT`（UTF-16 长度 879）
const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

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
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// 更新摘要的指令（逐字）。
///
/// TS 通过模板插值把它拼进 `UPDATE_SUMMARIZATION_PROMPT`；Rust 侧该常量已是插值后的逐字
/// 文本，本常量保留用于逐字长度校验（TS 源文件里的原始片段）。
///
/// TS: `compaction.ts#UPDATE_SUMMARIZATION_INSTRUCTIONS`（UTF-16 长度 1129）
#[allow(dead_code)]
const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str = r#"Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// 更新摘要 prompt（`UPDATE_SUMMARIZATION_INSTRUCTIONS` 插值后的逐字结果）。
///
/// TS: `compaction.ts#UPDATE_SUMMARIZATION_PROMPT`（插值后 UTF-16 长度 1257）
const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// turn prefix 摘要 prompt（逐字）。
///
/// TS: `compaction.ts#TURN_PREFIX_SUMMARIZATION_PROMPT`（UTF-16 长度 664）
const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

/// 无法安全持久化摘要时返回错误（length stop 只含部分文本）。
///
/// 映射规则：`StopReason::Error` → `"{label} failed: {message}"`（`message` 缺省或空串时
/// 用 `"Unknown error"`）；`StopReason::Length` → token cap 文本；其余 stop reason 返回
/// `None`（调用方还需自行拒绝含 toolCall 的响应）。分支摘要也复用本函数
/// （`label = "Branch summarization"`），所以错误文本属于用户可见契约，改动需同步测试。
///
/// TS: `compaction.ts#getSummarizationFailure`
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    summarization_failure(response, label).map(|error| error.to_string())
}

/// `getSummarizationFailure` 的结构化形式。
pub(crate) fn summarization_failure(
    response: &AssistantMessage,
    label: &str,
) -> Option<CompactionError> {
    match response.stop_reason {
        StopReason::Error => Some(CompactionError::SummarizationFailed {
            label: label.to_string(),
            message: response
                .error_message
                .clone()
                .filter(|message| !message.is_empty()),
        }),
        StopReason::Length => Some(CompactionError::TokenCap {
            label: label.to_string(),
        }),
        _ => None,
    }
}

/// 构造摘要请求的请求选项。
///
/// 把凭证、请求头、provider 环境、取消信号与 routing session 装进 `SimpleStreamOptions`。
/// 只有模型声明支持 reasoning 且 `thinking_level != Off` 时才设置 `reasoning`；`signal`
/// 通过 `AbortScope::token()` 转成底层流可用的 token。
///
/// TS: `compaction.ts#createSummarizationOptions`
pub(crate) fn create_summarization_options(
    model: &Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    signal: Option<AbortScope>,
    thinking_level: Option<ThinkingLevel>,
    session_id: Option<String>,
) -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions {
        base: StreamOptions {
            signal: signal.map(|scope| scope.token()),
            api_key,
            headers,
            env,
            session_id,
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    };
    // TS: `if (model.reasoning && thinkingLevel && thinkingLevel !== "off")`
    if model.reasoning == Some(true)
        && let Some(level) = thinking_level
        && level != ThinkingLevel::Off
    {
        options.reasoning = ai_thinking_level(level);
    }
    options
}

/// 把含 `"off"` 的 `ThinkingLevel` 映射成 `SimpleStreamOptions.reasoning` 的取值集合。
fn ai_thinking_level(level: ThinkingLevel) -> Option<kiana_ai::types::ThinkingLevel> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some(kiana_ai::types::ThinkingLevel::Minimal),
        ThinkingLevel::Low => Some(kiana_ai::types::ThinkingLevel::Low),
        ThinkingLevel::Medium => Some(kiana_ai::types::ThinkingLevel::Medium),
        ThinkingLevel::High => Some(kiana_ai::types::ThinkingLevel::High),
        ThinkingLevel::Xhigh => Some(kiana_ai::types::ThinkingLevel::Xhigh),
        ThinkingLevel::Max => Some(kiana_ai::types::ThinkingLevel::Max),
    }
}

/// `SimpleStreamOptions` 的逐字段克隆（该结构体实现刻意没有提供 `Clone`）。
///
/// 与 `kiana_agent_core::agent_loop::clone_stream_options` 相同：所有字段都是 `Clone`/`Arc`。
fn clone_simple_stream_options(options: &SimpleStreamOptions) -> SimpleStreamOptions {
    // 字段清单集中在 `kiana_ai::types::SimpleStreamOptions::duplicate`，避免多处拷贝漂移。
    options.duplicate()
}

/// 硬失败哨兵消息：记录原始错误，并让重试循环立即结束。
///
/// 冻结的 `retryAssistantCall` 只接受 `Future<Output = AssistantMessage>`，无法表达 TS 里
/// `produce` 抛错导致的 rejection。这里把错误记到旁路，返回一条 `stopReason: "aborted"` 的
/// 消息（aborted 是终态、永不重试、也不会被 `isRetryableAssistantError` 分类），随后由
/// [`complete_summarization`] 把旁路里的原始错误作为 `Err` 返回。
fn hard_error_sentinel(hard_error: &Mutex<Option<String>>, message: String) -> AssistantMessage {
    *hard_error_lock(hard_error) = Some(message);
    AssistantMessage {
        stop_reason: StopReason::Aborted,
        ..AssistantMessage::default()
    }
}

/// 读取硬错误旁路（互斥锁不参与任何 panic 路径，毒化时取回内部值）。
fn hard_error_lock(
    hard_error: &Mutex<Option<String>>,
) -> std::sync::MutexGuard<'_, Option<String>> {
    hard_error
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 包装重试回调：硬失败时抑制 `onRetryFinished`。
///
/// TS 的 `produce` 抛错会让 `retryAssistantCall` 直接 reject，循环结束回调一次也不发；
/// 而 Rust 的哨兵消息会让循环正常返回并触发结束回调。
fn callbacks_suppressing_finished_on_hard_error(
    callbacks: Option<RetryCallbacks>,
    hard_error: &Arc<Mutex<Option<String>>>,
) -> Option<RetryCallbacks> {
    let callbacks = callbacks?;
    let Some(finished) = callbacks.on_retry_finished.clone() else {
        return Some(callbacks);
    };
    let hard_error = Arc::clone(hard_error);
    let wrapped: OnRetryFinished = Arc::new(move |success, attempts, error| {
        if hard_error_lock(&hard_error).is_some() {
            return Box::pin(async {});
        }
        finished(success, attempts, error)
    });
    Some(RetryCallbacks {
        on_retry_scheduled: callbacks.on_retry_scheduled,
        on_retry_attempt_start: callbacks.on_retry_attempt_start,
        on_retry_finished: Some(wrapped),
    })
}

/// 所有压缩/分支摘要共用的 LLM 收敛点。
///
/// 一次性的摘要不写 prompt cache；调用方提供的 routing session 会被复用，没有的（包括
/// 分支摘要）会分配一个新的。整个调用包在 `retryAssistantCall` 里，让瞬时流中断遵循配置的
/// 重试策略。
///
/// 实现上 `cache_retention` 被强制设为 `None`，`session_id` 缺失时用 `uuidv7` 分配。
/// 取消信号经 `AbortScope` 传入；重试循环不用它取消 `produce` future，只用它打断退避，
/// 退避被打断时直接返回 `stopReason: "aborted"` 的响应。同一 signal 随 stream options
/// 传给 provider 层（`StreamOptions.signal`），进行中的请求由 `kiana-ai` 的流实现观察并中止。
///
/// 返回 `Ok` 只表示拿到了 `AssistantMessage`，**不代表摘要成功**：调用方必须继续用
/// [`get_summarization_failure`] 检查 `stop_reason`，并自行拒绝含 toolCall 的响应。
/// 本函数把流级硬失败转换成 [`CompactionError::StreamFailed`]；`uuidv7` 不可用时返回
/// [`CompactionError::SessionIdUnavailable`]。
///
/// 参数：`context` 由调用方构造成单轮摘要请求（见 `build_summarization_context`）；
/// `stream_fn` 为 `None` 时走 `complete_simple`；`retry` / `callbacks` 透传给重试循环。
///
/// 硬失败（`streamFn` 拒绝、流结果 reject）与 TS 一样**不重试**、不走
/// `getSummarizationFailure`，原始错误消息以 [`CompactionError::StreamFailed`] 原样冒泡。
/// 实现上通过一条 `stopReason: "aborted"` 的哨兵消息终止重试循环，原始错误保存在旁路，
/// 循环结束后取出；此时结束回调也会被抑制，以对齐 TS 中 rejection 不触发回调的行为。
///
/// TS: `compaction.ts#completeSummarization`
pub async fn complete_summarization(
    model: &Model,
    context: TranscriptContext,
    options: SimpleStreamOptions,
    stream_fn: Option<StreamFn>,
    retry: Option<RetryPolicy>,
    callbacks: Option<RetryCallbacks>,
) -> Result<AssistantMessage, CompactionError> {
    let mut request_options = options;
    // 一次性摘要不写 prompt cache；routing session 缺失时现分配一个。
    request_options.base.cache_retention = Some(CacheRetention::None);
    request_options.base.session_id = Some(match request_options.base.session_id.clone() {
        Some(session_id) => session_id,
        None => uuidv7(None)
            .map_err(|error| CompactionError::SessionIdUnavailable(error.to_string()))?,
    });

    let abort_scope = request_options.base.signal.clone().map(AbortScope::from);

    let hard_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // produce 只能返回 AssistantMessage（冻结的重试循环签名），流层面的硬失败无法用
    // Result 表达：错误写入旁路，返回一条 aborted 哨兵消息终止重试。
    let produce_options = clone_simple_stream_options(&request_options);
    let produce_model = model.clone();
    let produce_context = context.clone();
    let produce_stream_fn = stream_fn.clone();
    let produce_hard_error = Arc::clone(&hard_error);
    let produce = move || {
        let options = clone_simple_stream_options(&produce_options);
        let model = produce_model.clone();
        let context = produce_context.clone();
        let stream_fn = produce_stream_fn.clone();
        let hard_error = Arc::clone(&produce_hard_error);
        async move {
            match stream_fn {
                Some(stream_fn) => match stream_fn(model, context, Some(options)).await {
                    Ok(stream) => match stream.result().await {
                        Ok(message) => message,
                        Err(error) => hard_error_sentinel(&hard_error, error.to_string()),
                    },
                    Err(error) => hard_error_sentinel(&hard_error, error.to_string()),
                },
                None => complete_simple(&model, context.as_context(), options).await,
            }
        }
    };

    let callbacks = callbacks_suppressing_finished_on_hard_error(callbacks, &hard_error);
    let response = retry_assistant_call(
        produce,
        retry.as_ref(),
        abort_scope.as_ref(),
        callbacks.as_ref(),
    )
    .await;
    if let Some(message) = hard_error_lock(&hard_error).take() {
        // 旁路里有值说明 produce 从未正常返回；硬失败优先于循环给出的哨兵响应。
        return Err(CompactionError::StreamFailed { message });
    }
    Ok(response)
}

/// `generateSummary` 的参数（TS 的位置参数改为具名结构体）。
///
/// 派生 `Default` 只是方便构造，`model` 等必要字段仍需显式设置。`stream_fn` 为 `None`
/// 时摘要走 `kiana_ai::compat::complete_simple` 的默认流实现；`session_id` 为 `None`
/// 时由 [`complete_summarization`] 分配新的路由 id。
///
/// `_reserveTokens` 参数被丢弃（TS 注释：#9512，摘要使用模型输出上限）；其余字段与 TS 一一
/// 对应。
#[derive(Default)]
pub struct GenerateSummaryOptions {
    /// 用于摘要的模型。
    pub model: Model,
    /// API key。
    pub api_key: Option<String>,
    /// 请求头。
    pub headers: Option<ProviderHeaders>,
    /// 取消信号。
    pub signal: Option<AbortScope>,
    /// 追加到 prompt 的自定义焦点。
    pub custom_instructions: Option<String>,
    /// 上一次压缩的摘要（存在时使用更新 prompt）。
    pub previous_summary: Option<String>,
    /// thinking level。
    pub thinking_level: Option<ThinkingLevel>,
    /// 可选的会话 stream 函数。
    pub stream_fn: Option<StreamFn>,
    /// provider 作用域环境变量。
    pub env: Option<ProviderEnv>,
    /// 重试策略。
    pub retry: Option<RetryPolicy>,
    /// 重试回调。
    pub callbacks: Option<RetryCallbacks>,
    /// routing session id。
    pub session_id: Option<String>,
}

/// 摘要结果与其 usage。
///
/// TS: `compaction.ts#generateSummaryWithUsage` 的返回对象 `{ text, usage }`
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryWithUsage {
    /// 摘要文本。
    pub text: String,
    /// 生成摘要的 LLM 调用用量。
    pub usage: Usage,
}

/// 用 LLM 生成对话摘要；有 `previousSummary` 时使用更新 prompt 合并。
///
/// 返回摘要正文，usage 被丢弃（需要用量时用 [`generate_summary_with_usage`]）。失败
/// 条件与后者相同：模型 `error`/`length` stop、响应含 toolCall、流硬失败、session id
/// 生成失败。模型没有文本块时返回空串，本函数不把空摘要视为错误。
///
/// TS: `compaction.ts#generateSummary`
pub async fn generate_summary(
    current_messages: &[AgentMessage],
    options: GenerateSummaryOptions,
) -> Result<String, CompactionError> {
    Ok(generate_summary_with_usage(current_messages, options)
        .await?
        .text)
}

/// 构造摘要请求的 provider 上下文。
///
/// 只有一个 system prompt 和一条承载 `prompt_text` 的 user 消息，然后交给
/// `normalize_context` 做 provider 归一化；摘要请求是单轮的，不携带工具。时间戳取当前
/// 墙钟（`Date.now()` 语义），不参与摘要内容。
///
/// TS: `compaction.ts#buildSummarizationContext`
pub(crate) fn build_summarization_context(prompt_text: &str) -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: prompt_text.to_string(),
                text_signature: None,
            })]),
            timestamp: now_ms(),
        })],
        tools: None,
    })
}

/// 生成或更新对话摘要，并返回 provider usage。
///
/// 流程：
/// 1. 空串与缺省等价（TS 真值判断），据此选择新建摘要 prompt 还是更新摘要 prompt；
///    `custom_instructions` 以 `"\n\nAdditional focus: ..."` 追加。
/// 2. 先用 `convert_to_llm` 把 `AgentMessage` 转成 LLM 消息，再 `serialize_conversation`
///    成纯文本并包进 `<conversation>` 标签；有前次摘要时再包 `<previous-summary>`，
///    避免模型把历史当成需要继续的对话。
/// 3. 调用 [`complete_summarization`]（共享重试/取消），随后检查 stopReason 与 toolCall。
///
/// 错误：模型 `error` stop → [`CompactionError::SummarizationFailed`]；`length` stop →
/// [`CompactionError::TokenCap`]；响应含 toolCall → [`CompactionError::ToolCallAttempted`]；
/// 流硬失败 → [`CompactionError::StreamFailed`]。这些检查都在返回摘要之前完成。
/// 注意 `StopReason::Aborted` 不在失败映射里：取消会被当作普通响应返回（文本可能不完整），
/// 是否采用结果由调用方结合取消信号自行判断。
///
/// TS: `compaction.ts#generateSummaryWithUsage`
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    options: GenerateSummaryOptions,
) -> Result<SummaryWithUsage, CompactionError> {
    let GenerateSummaryOptions {
        model,
        api_key,
        headers,
        signal,
        custom_instructions,
        previous_summary,
        thinking_level,
        stream_fn,
        env,
        retry,
        callbacks,
        session_id,
    } = options;

    // TS 的真值判断：空串等价于缺省。
    let previous_summary = previous_summary.filter(|summary| !summary.is_empty());
    let custom_instructions = custom_instructions.filter(|instructions| !instructions.is_empty());

    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    }
    .to_string();
    if let Some(instructions) = &custom_instructions {
        base_prompt.push_str(&format!("\n\nAdditional focus: {instructions}"));
    }

    // 先把消息转成 LLM 可用的形式（处理 bashExecution/custom 等），再序列化成文本，避免
    // 模型把它当成要继续的对话。
    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);

    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    // 前次摘要放在对话之后、指令之前：更新 prompt 依赖 <previous-summary> 标签定位旧摘要。
    if let Some(previous) = &previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        ));
    }
    prompt_text.push_str(&base_prompt);

    let completion_options = create_summarization_options(
        &model,
        api_key,
        headers,
        env,
        signal,
        thinking_level,
        session_id,
    );

    let response = complete_summarization(
        &model,
        build_summarization_context(&prompt_text),
        completion_options,
        stream_fn,
        retry,
        callbacks,
    )
    .await?;

    if let Some(failure) = summarization_failure(&response, "Summarization") {
        return Err(failure);
    }
    // 摘要请求不允许工具：带 toolCall 的响应视为模型行为异常，不保存。
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(CompactionError::ToolCallAttempted {
            label: "Summarization".to_string(),
        });
    }

    Ok(SummaryWithUsage {
        text: content_text_assistant(&response.content, "\n"),
        usage: response.usage,
    })
}

// ============================================================================
// 压缩准备（供扩展使用）
// ============================================================================

/// 是否是投影条目的 turn 起点。
///
/// TS: `compaction.ts#isProjectedTurnStart`
fn is_projected_turn_start(entry: &ProjectedSessionEntry) -> bool {
    if entry.source_entry.kind() == "compaction" {
        return false;
    }
    entry.messages.iter().any(is_turn_start_message)
}

/// 在投影条目中找到 turn 起点。
///
/// TS: `compaction.ts#findProjectedTurnStartIndex`
fn find_projected_turn_start_index(
    entries: &[ProjectedSessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    let last_index = entries.len().checked_sub(1)?;
    let entry_index = entry_index.min(last_index);
    if entry_index < start_index {
        return None;
    }
    (start_index..=entry_index)
        .rev()
        .find(|&index| is_projected_turn_start(&entries[index]))
}

/// 投影版切点选择（含 recovery-omission-suffix 前移规则）。
///
/// 与 [`find_cut_point`] 的区别：合法切点判断与 token 累计都基于投影后的 `messages`
/// （已应用 `context_edit`），被省略的条目因此不计入预算；预算被越过时还会应用
/// recovery-omission 后缀规则。正常区间下返回下标落在 `[start_index, end_index)` 内，
/// 区间为空时返回 `start_index`。
///
/// TS: `compaction.ts#findProjectedCutPoint`
fn find_projected_cut_point(
    entries: &[ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let end_index = end_index.min(entries.len());
    // 阶段 1：收集投影后仍含合法切点消息的条目。
    let mut cut_points: Vec<usize> = Vec::new();
    if end_index > start_index {
        for (index, entry) in entries
            .iter()
            .enumerate()
            .take(end_index.min(entries.len()))
            .skip(start_index)
        {
            if entry.source_entry.kind() != "compaction"
                && entry.messages.iter().any(is_cut_point_message)
            {
                cut_points.push(index);
            }
        }
    }
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0u64;
    let mut exceeded_budget = false;
    let mut cut_index = cut_points[0];
    // 阶段 2：从最新条目向前累计投影后的 token。`exceeded_budget` 记录是否真的越过预算，
    // 后面的 recovery-omission 前移规则只在越界时生效。
    if end_index > start_index {
        for index in (start_index..end_index).rev() {
            let message_tokens = entries[index]
                .messages
                .iter()
                .map(estimate_tokens)
                .fold(0, u64::saturating_add);
            if message_tokens == 0 {
                continue;
            }
            accumulated_tokens = accumulated_tokens.saturating_add(message_tokens);
            if accumulated_tokens >= keep_recent_tokens {
                exceeded_budget = true;
                cut_index = cut_points
                    .iter()
                    .find(|&&candidate| candidate >= index)
                    .copied()
                    .unwrap_or_else(|| *cut_points.last().unwrap_or(&cut_points[0]));
                break;
            }
        }
    }

    // 恢复尝试及其 omission 编辑在最后一个可见输入之后是上下文不可见的。只有当后缀是一个
    // 封闭的、包含被省略 assistant 尝试的后缀时才前移；任意元数据不得把切点移过未发送的输入。
    //
    // 后缀 = 切点之后的条目。is_intrinsically_visible：原始条目本身会产生上下文消息
    // （context_edit 条目不产生）；is_omitted：本应可见但投影后为空，说明被 context_edit
    // 省略（包括恢复流程写入的 omission）。
    let suffix = slice_range(entries, cut_index + 1, end_index);
    let is_intrinsically_visible = |entry: &ProjectedSessionEntry| {
        entry.source_entry.kind() != "context_edit"
            && !context_messages_for_entry(&entry.source_entry).is_empty()
    };
    let is_omitted = |entry: &ProjectedSessionEntry| {
        is_intrinsically_visible(entry) && entry.messages.is_empty()
    };
    let omitted_suffix_ids: HashSet<&str> = suffix
        .iter()
        .filter(|entry| is_omitted(entry))
        .map(|entry| entry.source_entry.id())
        .collect();
    // 后缀里存在“非省略”的 replacement，且目标不是后缀内被省略的条目：说明改写来自后缀
    // 之外或改写了仍在上下文中的内容，不属于可整体跳过的恢复后缀。
    let has_external_replacement = suffix.iter().any(|entry| {
        let Some((replacement_is_not_null, target_id)) = context_edit_state(&entry.source_entry)
        else {
            return false;
        };
        replacement_is_not_null && !omitted_suffix_ids.contains(target_id.as_str())
    });
    // 四个条件同时成立才前移切点：确实越过预算、没有外部改写、后缀至少包含一个被省略的
    // assistant 尝试、且后缀里其余可见条目也都被省略。这样切点只会跨过“没有发送给模型”
    // 的尾部，不会跨过未发送的输入。
    let is_recovery_omission_suffix = exceeded_budget
        && !has_external_replacement
        && suffix.iter().any(|entry| {
            matches!(&entry.source_entry, SessionEntry::Message(message) if message.message.role() == "assistant")
                && is_omitted(entry)
        })
        && suffix.iter().all(|entry| {
            entry.source_entry.kind() != "compaction"
                && (!is_intrinsically_visible(entry) || is_omitted(entry))
        });
    if is_recovery_omission_suffix {
        cut_index += 1;
    }

    // 与条目版一致的元数据回扫：把切点前连续的无投影消息条目并入保留区。
    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if previous.source_entry.kind() == "compaction" || !previous.messages.is_empty() {
            break;
        }
        cut_index -= 1;
    }

    // 切点落在 turn 中间时记录 turn 起点，供 compact 生成前缀摘要。
    let starts_turn = entries.get(cut_index).is_some_and(is_projected_turn_start);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
        turn_start_index,
    }
}

/// `context_edit` 条目的 `(replacement !== null, targetId)`；非 context_edit 条目返回 `None`。
///
/// TS 的运行期字段读取：`replacement` 缺省（`undefined`）也满足 `!== null`，只有显式 `null`
/// 视为“省略目标”。
fn context_edit_state(entry: &SessionEntry) -> Option<(bool, String)> {
    match entry {
        SessionEntry::ContextEdit(edit) => {
            Some((edit.replacement.is_some(), edit.target_id.clone()))
        }
        SessionEntry::Unknown(object) if entry.kind() == "context_edit" => Some((
            !matches!(object.get("replacement"), Some(JsonValue::Null)),
            object
                .get("targetId")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
        )),
        _ => None,
    }
}

/// TS `array.slice(start, end)` 的 Rust 版本：越界裁剪，`end <= start` 返回空切片。
fn slice_range<T>(items: &[T], start: usize, end: usize) -> &[T] {
    if start >= items.len() || end <= start {
        return &[];
    }
    &items[start..end.min(items.len())]
}

/// 读取压缩条目的摘要文本（typed 或原始对象）。
///
/// TS 直接读原始条目的运行期值：`summary` 为 `null` 或缺失时 `previousSummary` 是
/// `null` / `undefined`，下游的真值判断（选择 prompt）与 `?? "No prior history."`
/// 都把它们当作“没有前次摘要”。typed 字段是 JS `String(value)`（`null` → `"null"`、
/// 缺失 → `""`），因此从文件加载的条目按 `raw_entry` 还原这两种情形。
///
/// TS: `prepareCompaction` 里的 `(projectedEntries[i].sourceEntry as CompactionEntry).summary`
fn compaction_summary_text(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Compaction(compaction) => match &compaction.raw_entry {
            Some(raw) => match raw.get("summary") {
                None | Some(JsonValue::Null) => None,
                Some(_) => Some(compaction.summary.clone()),
            },
            None => Some(compaction.summary.clone()),
        },
        SessionEntry::Unknown(object) => match object.get("summary")? {
            JsonValue::Null => None,
            value => Some(crate::utils::js_value_to_string(value)),
        },
        _ => None,
    }
}

/// 会话历史是否需要压缩；需要时返回压缩准备数据。
///
/// 输入 `path_entries` 是当前分支路径（`kiana-session` 由树导航得到），`settings` 提供
/// `keep_recent_tokens`。返回 `None` 的情形：路径最后一条已是压缩条目（已压缩到底）；
/// 投影为空或切点处取不到条目；切点条目没有 id；待摘要消息与 turn 前缀消息都为空。
///
/// 流程：
/// 1. 先修复 session-core 降级为 `Unknown` 的 `message` 条目，再构建投影；
/// 2. 在投影中找到最近一次压缩（`build_context_entries` 总是把它放在首位），其 `summary`
///    作为 `previous_summary`，切点扫描区间从它之后开始：压缩条目之前的历史不再参与本次
///    摘要（保留区内未被切走的旧消息仍可能被重新总结，以满足 keep_recent_tokens）；
/// 3. `find_projected_cut_point` 选切点；切分 turn 时，turn 起点到切点之间的消息进入
///    `turn_prefix_messages`，切点之前的历史进入 `messages_to_summarize`；
/// 4. 从上一次压缩的 `details` 与待摘要消息的 toolCall 中收集文件操作。
///
/// 本函数只读输入、不产生副作用；调用方拿到 `Some` 后仍可通过 `before_compact` 扩展
/// 取消或替换压缩。
///
/// TS: `compaction.ts#prepareCompaction`
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: &CompactionSettings,
) -> Option<CompactionPreparation> {
    if path_entries
        .last()
        .is_some_and(|entry| entry.kind() == "compaction")
    {
        return None;
    }

    // 阶段 1：修复投影前无法类型化的消息条目。
    // kiana-session-core 把结构字段异常的 `message` 条目降级为 Unknown 并在投影时丢弃消息；
    // TS 不校验字段，这些消息仍参与投影/切点/token 估算。投影前先修复路径条目。
    let recovered_entries = recovered_path_entries(path_entries);
    let path_entries: &[SessionEntry] = recovered_entries.as_ref();

    // 阶段 2：投影会应用压缩裁剪（最新压缩条目排首位），后续切点与消息提取都基于它。
    let projection = build_session_projection(path_entries, LeafId::Unspecified, None);
    let projected_entries = &projection.entries;
    let source_entries: Vec<SessionEntry> = projected_entries
        .iter()
        .map(|entry| entry.source_entry.clone())
        .collect();
    // 最新的压缩最先被投影；更老的压缩条目可能仍在保留的原始区间里，但投影贡献为空。
    let prev_compaction_index = projected_entries
        .iter()
        .position(|entry| entry.source_entry.kind() == "compaction" && !entry.messages.is_empty());

    let mut previous_summary = None;
    let mut boundary_start = 0usize;
    if let Some(index) = prev_compaction_index {
        previous_summary = compaction_summary_text(&projected_entries[index].source_entry);
        boundary_start = index + 1;
    }
    let boundary_end = projected_entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    // 阶段 3：只在压缩边界之后选切点，压缩条目本身与它之前的历史不参与。
    let cut_point = find_projected_cut_point(
        projected_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    let first_kept_entry = projected_entries.get(cut_point.first_kept_entry_index)?;
    let first_kept_entry_id = first_kept_entry.source_entry.id().to_string();
    if first_kept_entry_id.is_empty() {
        return None;
    }

    let history_end = if cut_point.is_split_turn {
        // 切分 turn 时 `turn_start_index` 一定是 `Some`（构造不变式）。
        cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    // 阶段 4：按切点把消息分成“将被摘要的历史”和“被切开 turn 的前缀”两部分。
    let messages_to_summarize: Vec<AgentMessage> =
        slice_range(projected_entries, boundary_start, history_end)
            .iter()
            .flat_map(messages_from_projected_entry_for_compaction)
            .collect();
    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        let turn_start = cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index);
        slice_range(
            projected_entries,
            turn_start,
            cut_point.first_kept_entry_index,
        )
        .iter()
        .flat_map(messages_from_projected_entry_for_compaction)
        .collect()
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    // 阶段 5：文件操作 = 上一次压缩 details + 本次消息中的 toolCall（turn 前缀也计入）。
    let mut file_ops = extract_file_operations(
        &messages_to_summarize,
        &source_entries,
        prev_compaction_index,
    );
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: settings.clone(),
    })
}

// ============================================================================
// 主压缩函数
// ============================================================================

/// 当前 Unix 毫秒（`Date.now()`）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// turn prefix 摘要的内部参数。
///
/// 字段与 [`GenerateSummaryOptions`] 基本对应，但没有 `previous_summary`（turn 前缀摘要
/// 不与旧摘要合并）；`model` / `api_key` 用借用，`compact` 的切分分支不需要再克隆它们。
struct TurnPrefixOptions<'a> {
    model: &'a Model,
    api_key: Option<&'a str>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    signal: Option<AbortScope>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    retry: Option<RetryPolicy>,
    callbacks: Option<RetryCallbacks>,
    session_id: Option<String>,
    custom_instructions: Option<String>,
}

/// 用准备好的数据生成压缩摘要。
///
/// 输入来自 [`prepare_compaction`]；本函数不读会话，只做 LLM 调用与文本拼接。
/// `preparation.settings` 不参与计算（摘要长度由模型输出上限决定）。
///
/// - 普通切点：对 `messages_to_summarize` 调一次摘要；`previous_summary` 存在时使用
///   更新 prompt。
/// - 切分 turn 且存在前缀消息：先摘要历史（没有历史时用前次摘要，仍没有则用
///   `"No prior history."`），再对 `turn_prefix_messages` 单独摘要；两份 usage 用
///   `combine_usage` 合并。最终文本是历史摘要 + 分隔线 + `**Turn Context (split turn):**`
///   + 前缀摘要。前缀消息为空时退化为普通单次摘要。
/// - 最后把文件列表追加到摘要末尾，并把 `CompactionDetails` 放进 `details`。
///
/// 返回值中 `estimated_tokens_after` 恒为 `None`（由调用方回填），`usage` 恒为
/// `Some(...)`。被分类为失败的摘要会立即返回错误，不会产生半成品结果；但 `aborted`
/// 响应不算失败（见 [`generate_summary_with_usage`]），取消时调用方需自行丢弃结果。
/// `first_kept_entry_id` 为空（手工构造的 preparation）时返回
/// [`CompactionError::FirstKeptEntryMissing`]。
///
/// TS 的位置参数保持原顺序（冻结的调用约定），因此显式关闭 `too_many_arguments`。
///
/// TS: `compaction.ts#compact`
#[allow(clippy::too_many_arguments)]
pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    api_key: Option<&str>,
    headers: Option<ProviderHeaders>,
    custom_instructions: Option<&str>,
    signal: Option<AbortScope>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    env: Option<ProviderEnv>,
    retry: Option<RetryPolicy>,
    callbacks: Option<RetryCallbacks>,
    session_id: Option<String>,
) -> Result<CompactionResult, CompactionError> {
    let CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        ..
    } = preparation;

    let summary: String;
    let summary_usage: Usage;

    // 阶段 1：生成摘要文本（普通一次调用；切分 turn 需要历史 + turn 前缀两次）。
    if *is_split_turn && !turn_prefix_messages.is_empty() {
        let mut history_text = previous_summary
            .clone()
            .unwrap_or_else(|| "No prior history.".to_string());
        let mut history_usage: Option<Usage> = None;
        // 切点之前还有历史时才调用模型；否则直接复用前次摘要/占位文本。
        if !messages_to_summarize.is_empty() {
            let history_result = generate_summary_with_usage(
                messages_to_summarize,
                GenerateSummaryOptions {
                    model: model.clone(),
                    api_key: api_key.map(str::to_string),
                    headers: headers.clone(),
                    signal: signal.clone(),
                    custom_instructions: custom_instructions.map(str::to_string),
                    previous_summary: previous_summary.clone(),
                    thinking_level,
                    stream_fn: stream_fn.clone(),
                    env: env.clone(),
                    retry: retry.clone(),
                    callbacks: callbacks.clone(),
                    session_id: session_id.clone(),
                },
            )
            .await?;
            history_text = history_result.text;
            history_usage = Some(history_result.usage);
        }
        // turn 前缀单独摘要：它描述被切开的 turn 的前半段，不能丢。
        let turn_prefix_result = generate_turn_prefix_summary(
            turn_prefix_messages,
            TurnPrefixOptions {
                model,
                api_key,
                headers,
                env,
                signal,
                thinking_level,
                stream_fn,
                retry,
                callbacks,
                session_id,
                custom_instructions: custom_instructions.map(str::to_string),
            },
        )
        .await?;

        summary = format!(
            "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            turn_prefix_result.text
        );
        summary_usage = match history_usage {
            Some(history) => combine_usage(&history, &turn_prefix_result.usage),
            None => turn_prefix_result.usage,
        };
    } else {
        // 普通路径：一次摘要覆盖全部待总结消息。
        let result = generate_summary_with_usage(
            messages_to_summarize,
            GenerateSummaryOptions {
                model: model.clone(),
                api_key: api_key.map(str::to_string),
                headers,
                signal,
                custom_instructions: custom_instructions.map(str::to_string),
                previous_summary: previous_summary.clone(),
                thinking_level,
                stream_fn,
                env,
                retry,
                callbacks,
                session_id,
            },
        )
        .await?;
        summary = result.text;
        summary_usage = result.usage;
    }

    // 阶段 2：把文件列表追加到摘要正文，并把同一份列表作为 details 返回，供下一次压缩
    // 恢复文件跟踪、供会话条目持久化。
    let mut summary = summary;
    let lists = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(
        &lists.read_files,
        &lists.modified_files,
    ));

    // `compact` 是公开 API，调用方可以传入手工构造的 preparation，因此这里再校验一次。
    if first_kept_entry_id.is_empty() {
        return Err(CompactionError::FirstKeptEntryMissing);
    }

    // `to_value` 对纯数据结构的失败理论上不可达；失败时退化为 JSON null，保持与 TS
    // `details: null` 的序列化形状一致。
    let details = serde_json::to_value(CompactionDetails {
        read_files: lists.read_files,
        modified_files: lists.modified_files,
    })
    .unwrap_or(JsonValue::Null);

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: first_kept_entry_id.clone(),
        tokens_before: *tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(details),
    })
}

/// 为切分 turn 生成 turn prefix 摘要。
///
/// prompt 由 `# Conversation` + 序列化消息 + `# Instructions` +
/// `TURN_PREFIX_SUMMARIZATION_PROMPT` 组成，`customInstructions` 非空时追加。与
/// [`generate_summary_with_usage`] 一样，调用后检查 stopReason 与 toolCall，错误 label 为
/// `"Turn prefix summarization"`。
///
/// TS: `compaction.ts#generateTurnPrefixSummary`
async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    options: TurnPrefixOptions<'_>,
) -> Result<SummaryWithUsage, CompactionError> {
    let TurnPrefixOptions {
        model,
        api_key,
        headers,
        env,
        signal,
        thinking_level,
        stream_fn,
        retry,
        callbacks,
        session_id,
        custom_instructions,
    } = options;

    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text = format!(
        "# Conversation\n{conversation_text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    let custom_instructions = custom_instructions.filter(|instructions| !instructions.is_empty());
    if let Some(instructions) = &custom_instructions {
        prompt_text.push_str(&format!("\n\nAdditional focus: {instructions}"));
    }

    let response = complete_summarization(
        model,
        build_summarization_context(&prompt_text),
        create_summarization_options(
            model,
            api_key.map(str::to_string),
            headers,
            env,
            signal,
            thinking_level,
            session_id,
        ),
        stream_fn,
        retry,
        callbacks,
    )
    .await?;

    if let Some(failure) = summarization_failure(&response, "Turn prefix summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(CompactionError::ToolCallAttempted {
            label: "Turn prefix summarization".to_string(),
        });
    }

    Ok(SummaryWithUsage {
        text: content_text_assistant(&response.content, "\n"),
        usage: response.usage,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// `string.length`（UTF-16 code unit）。
    fn units(text: &str) -> usize {
        text.encode_utf16().count()
    }

    #[test]
    fn summarization_prompt_is_byte_exact() {
        assert_eq!(units(SUMMARIZATION_PROMPT), 879);
        assert_eq!(SUMMARIZATION_PROMPT.len(), 879);
        assert!(
            SUMMARIZATION_PROMPT.starts_with("The messages above are a conversation to summarize.")
        );
        assert!(SUMMARIZATION_PROMPT.ends_with("error messages."));
    }

    #[test]
    fn update_prompts_are_byte_exact() {
        assert_eq!(units(UPDATE_SUMMARIZATION_INSTRUCTIONS), 1129);
        assert_eq!(units(UPDATE_SUMMARIZATION_PROMPT), 1257);
        let expected = format!(
            "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}"
        );
        assert_eq!(UPDATE_SUMMARIZATION_PROMPT, expected);
    }

    #[test]
    fn turn_prefix_prompt_is_byte_exact() {
        assert_eq!(units(TURN_PREFIX_SUMMARIZATION_PROMPT), 664);
        assert!(
            TURN_PREFIX_SUMMARIZATION_PROMPT.starts_with("The messages above are earlier context")
        );
    }

    #[test]
    fn error_messages_are_verbatim() {
        let error = CompactionError::SummarizationFailed {
            label: "Summarization".to_string(),
            message: Some("boom".to_string()),
        };
        assert_eq!(error.to_string(), "Summarization failed: boom");
        let error = CompactionError::SummarizationFailed {
            label: "Summarization".to_string(),
            message: None,
        };
        assert_eq!(error.to_string(), "Summarization failed: Unknown error");
        let error = CompactionError::SummarizationFailed {
            label: "Branch summarization".to_string(),
            message: Some(String::new()),
        };
        assert_eq!(
            error.to_string(),
            "Branch summarization failed: Unknown error"
        );
        assert_eq!(
            CompactionError::TokenCap {
                label: "Turn prefix summarization".to_string()
            }
            .to_string(),
            "Turn prefix summarization failed: generation hit the token cap and the summary is incomplete"
        );
        assert_eq!(
            CompactionError::ToolCallAttempted {
                label: "Branch summarization".to_string()
            }
            .to_string(),
            "Branch summarization attempted to call a tool"
        );
        assert_eq!(
            CompactionError::FirstKeptEntryMissing.to_string(),
            "First kept entry has no UUID - session may need migration"
        );
    }

    #[test]
    fn should_compact_uses_u128_to_avoid_underflow() {
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 10_000,
            keep_recent_tokens: 20_000,
        };
        assert!(should_compact(95_000, 100_000, &settings));
        assert!(!should_compact(89_000, 100_000, &settings));
        let disabled = CompactionSettings {
            enabled: false,
            ..settings.clone()
        };
        assert!(!should_compact(95_000, 100_000, &disabled));
        // reserve > window：TS 的 `window - reserve` 是负数，任何非负 context 都触发压缩；
        // u128 比较避免无符号下溢并保持同一结果。
        let oversized = CompactionSettings {
            enabled: true,
            reserve_tokens: 200_000,
            keep_recent_tokens: 20_000,
        };
        assert!(should_compact(0, 100_000, &oversized));
    }
}
