//! core/compaction/branch-summarization.ts。
//!
//! 树导航时对即将离开的分支生成摘要，避免上下文丢失。
//!
//! # 这个文件解决什么问题
//!
//! 用户从一条对话分支切走（或回到更早的祖先）后，被放弃的那段历史不再出现在 leaf 的
//! 祖先链上。如果不做摘要，期间读过的文件、试过的方案和失败原因会从模型上下文中消失。
//! 本模块把这段分支压缩成一段结构化文本，由调用方写入 `branch_summary` 会话条目。
//!
//! # 边界与上下游
//!
//! - 上游：`kiana-session` 的 `AgentSession::navigate_tree`
//!   （`crates/kiana-session/src/agent_session.rs`）在树导航前调用
//!   [`collect_entries_for_branch_summary`] 与 [`generate_branch_summary`]，并把结果写进
//!   会话；本模块自身不读写会话文件，也不移动 leaf。
//! - 会话访问通过 `kiana-session-core` 的只读视图 `ReadonlySessionManager`（取路径与条目，
//!   返回克隆快照）。
//! - 唯一的副作用是通过注入的 `stream_fn` 发起摘要请求（失败的流请求可能按 `retry`
//!   策略重试）；usage 原样随结果返回。
//!
//! # 主要流程
//!
//! 1. [`collect_entries_for_branch_summary`]：从旧 leaf 沿 `parentId` 上溯到与目标路径的
//!    共同祖先（不含祖先本身），再反转为时间顺序；
//! 2. [`prepare_branch_entries`]：第一遍从 `branch_summary.details` 累积文件操作，第二遍
//!    从最新到最旧把消息装进 `token_budget`（`compaction`/`branch_summary` 条目在已用
//!    token 低于预算 90% 时强制保留）；
//! 3. [`generate_branch_summary`]：`convert_to_llm` 后序列化成纯文本，套上
//!    `<conversation>` 与固定 prompt，经 `complete_summarization` 发起请求；
//! 4. 按 `stop_reason` 分流；成功后先在模型文本之前加 preamble（`BRANCH_SUMMARY_PREAMBLE`
//!    前置到 `summary` 开头），再把文件列表追加到摘要末尾。
//!
//! # 失败语义
//!
//! 与 TS 的 Promise 语义对应，分三层：
//! - 中止（`stopReason: "aborted"`）→ `Ok({ aborted: true })`，调用方按“已取消”处理；
//! - 软失败（`stopReason` 为 `error`/`length`，或响应含 toolCall）→ `Ok({ error })`，
//!   调用方把它转成会话层错误；
//! - 硬失败（`stream_fn` 拒绝、流结果 reject、session id 生成失败）→
//!   `Err(CompactionError)`，错误文本原样冒泡；`complete_summarization` 不会重试这类
//!   错误。
//!
//! # 保真约束
//!
//! `BRANCH_SUMMARY_PREAMBLE` / `BRANCH_SUMMARY_PROMPT` 属于当前稳定契约（UTF-16 长度
//! 103 / 689，由文件末尾测试锁定）；它们直接进入模型上下文，改动会改变模型输出，需
//! 同步更新测试。条目到消息的转换还需按当前约定处理畸形历史数据（空摘要仍生成消息、
//! 非法时间戳的兜底等），细节见各函数的文档注释。

use kiana_agent_core::types::{AgentMessage, StreamFn};
use kiana_ai::types::{
    AssistantContentBlock, JsonObject, JsonValue, Model, ProviderEnv, ProviderHeaders,
    SimpleStreamOptions, StopReason, StreamOptions, Usage,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::retry::{RetryCallbacks, RetryPolicy};
use kiana_ai::utils::text::content_text_assistant;
use kiana_session_core::messages::{convert_to_llm, create_branch_summary_message};
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, session_entry_to_context_messages,
};

use crate::compaction::{
    CompactionError, build_summarization_context, complete_summarization,
    context_messages_for_entry, entry_from_hook_truthy, estimate_tokens_f64,
    get_summarization_failure,
};
use crate::utils::{
    FileOperations, FileValue, compute_file_lists, create_file_ops, extract_file_ops_from_message,
    format_file_operations, serialize_conversation,
};

// ============================================================================
// Types
// ============================================================================

/// 分支摘要结果。软失败（error stopReason / length / toolCall）以字段表达；硬失败
/// （`streamFn` 拒绝、流结果 reject、uuidv7 失败）与 TS 的 Promise rejection 一样以 `Err`
/// 传播（见 [`generate_branch_summary`]）。
///
/// TS: `branch-summarization.ts#BranchSummaryResult`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BranchSummaryResult {
    /// 摘要文本（含 preamble 与文件列表）。
    pub summary: Option<String>,
    /// 生成摘要的 LLM 调用用量。
    pub usage: Option<Usage>,
    /// 只读文件。
    pub read_files: Option<Vec<FileValue>>,
    /// 修改过的文件。
    pub modified_files: Option<Vec<FileValue>>,
    /// 是否因中止而取消。
    pub aborted: Option<bool>,
    /// 失败文本（用户可见契约；当前稳定，改动需同步测试）。
    pub error: Option<String>,
}

/// `BranchSummaryEntry.details` 中保存的文件跟踪信息。
///
/// TS: `branch-summarization.ts#BranchSummaryDetails`
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    /// 只读文件。
    pub read_files: Vec<FileValue>,
    /// 修改过的文件。
    pub modified_files: Vec<FileValue>,
}

/// 分支摘要的准备数据。
///
/// TS: `branch-summarization.ts#BranchPreparation`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BranchPreparation {
    /// 按时间顺序提取的消息。
    pub messages: Vec<AgentMessage>,
    /// 从 toolCall 提取的文件操作。
    pub file_ops: FileOperations,
    /// 消息的估算 token 总数；畸形摘要条目会得到 `NaN`（以 f64 承载），使预算比较自然失效。
    pub total_tokens: f64,
}

/// 分支条目收集结果。
///
/// TS: `branch-summarization.ts#CollectEntriesResult`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectEntriesResult {
    /// 按时间顺序排列的待总结条目。
    pub entries: Vec<SessionEntry>,
    /// 旧位置与新位置的共同祖先（若有）。
    pub common_ancestor_id: Option<String>,
}

/// `generateBranchSummary` 的参数。
///
/// TS: `branch-summarization.ts#GenerateBranchSummaryOptions`（`signal` 在 TS 里必填）。
pub struct GenerateBranchSummaryOptions {
    /// 用于摘要的模型。
    pub model: Model,
    /// API key。
    pub api_key: Option<String>,
    /// 请求头。
    pub headers: Option<ProviderHeaders>,
    /// provider 作用域环境变量。
    pub env: Option<ProviderEnv>,
    /// 取消信号（TS 必填）。
    pub signal: AbortScope,
    /// 可选的自定义摘要指令（空串按缺省处理，与 TS 的真值判断一致）。
    pub custom_instructions: Option<String>,
    /// 为 true 时 `custom_instructions` 替换默认 prompt，而不是追加；自定义指令为空或
    /// 缺失时仍使用默认 prompt。
    pub replace_instructions: Option<bool>,
    /// 选择分支历史时保留的 token 数（默认 16384）。
    pub reserve_tokens: Option<u64>,
    /// 可选的会话 stream 函数；`None` 时由 `complete_summarization` 走真实 provider 调用。
    pub stream_fn: Option<StreamFn>,
    /// 重试策略。
    pub retry: Option<RetryPolicy>,
    /// 重试回调。
    pub callbacks: Option<RetryCallbacks>,
}

// ============================================================================
// Entry Collection
// ============================================================================

/// 收集从一个位置导航到另一个位置时需要总结的条目。
///
/// 从 `old_leaf_id` 沿 `parentId` 上溯到与 `target_id` 的共同祖先；**不**在压缩边界停止
/// （压缩摘要作为上下文一起被总结）。
///
/// # 参数与返回
///
/// - `old_leaf_id`：导航前的 leaf；`None` 与空串都是 TS 的假值，直接返回默认空结果。
/// - `target_id`：导航目标；只用于求共同祖先，不会出现在返回的 `entries` 里。
/// - `common_ancestor_id`：两条路径上最深的公共条目 id；没有交集时为 `None`，此时会一直
///   收集到根。
///
/// `entries` 是**时间顺序**（根→叶）的克隆快照，调用方持有期间会话继续变化不会影响它。
/// 取不到某条父链条目（悬空 `parentId`）时静默停止，已收集部分照常返回，不产生错误。
///
/// TS: `branch-summarization.ts#collectEntriesForBranchSummary`
pub fn collect_entries_for_branch_summary(
    session: &dyn ReadonlySessionManager,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> CollectEntriesResult {
    // TS: `if (!oldLeafId)` —— `null` 与空串都是假值。
    let Some(old_leaf_id) = old_leaf_id.filter(|id| !id.is_empty()) else {
        return CollectEntriesResult::default();
    };

    let old_path: std::collections::HashSet<String> = session
        .get_branch(Some(old_leaf_id))
        .into_iter()
        .map(|entry| entry.id().to_string())
        .collect();
    let target_path = session.get_branch(Some(target_id));

    // targetPath 是根在前，从尾部向头找最深的共同祖先。
    let common_ancestor_id = target_path
        .iter()
        .rev()
        .find(|entry| old_path.contains(entry.id()))
        .map(|entry| entry.id().to_string());

    // 待总结部分 = 旧 leaf 到共同祖先之间（不含共同祖先）；从叶向根回溯收集。
    let mut entries: Vec<SessionEntry> = Vec::new();
    let mut current = Some(old_leaf_id.to_string());
    while let Some(id) = current.clone() {
        if Some(id.as_str()) == common_ancestor_id.as_deref() {
            break;
        }
        let Some(entry) = session.get_entry(&id) else {
            break;
        };
        let parent_id = entry.parent_id().map(str::to_string);
        entries.push(entry.clone());
        // TS 的 `while (current && ...)`：空 parentId 也是假值。
        current = parent_id.filter(|parent| !parent.is_empty());
    }

    // 回溯得到的是叶→根顺序，反转成时间顺序（后续序列化与预算遍历都依赖它）。
    entries.reverse();
    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

// ============================================================================
// Entry to Message Conversion
// ============================================================================

/// 从一个会话条目提取 AgentMessage。
///
/// 与 `sessionEntryToContextMessages` 的区别：`branch_summary` 的空摘要仍会生成消息，
/// `compaction` 只返回摘要消息（不带 `systemMessage`），`toolResult` 被跳过。
///
/// TS: `branch-summarization.ts#getMessageFromEntry`
fn get_message_from_entry(entry: &SessionEntry) -> Option<AgentMessage> {
    match entry.kind() {
        "message" => {
            let message = context_messages_for_entry(entry).into_iter().next()?;
            // TS 跳过 toolResult（上下文已在 assistant 的 toolCall 里）。
            if message.role() == "toolResult" {
                return None;
            }
            Some(message)
        }
        "custom_message" => session_entry_to_context_messages(entry).into_iter().next(),
        "branch_summary" => {
            let message = session_entry_to_context_messages(entry)
                .into_iter()
                .find(|message| message.role() == "branchSummary");
            match message {
                Some(message) => Some(message),
                None => match entry {
                    SessionEntry::BranchSummary(branch) => Some(
                        create_branch_summary_message(
                            &branch.summary,
                            &branch.from_id,
                            &branch.timestamp,
                        )
                        .to_agent_message(),
                    ),
                    // TS 的 case `"branch_summary"` 无条件构造消息：空摘要条目同样贡献一个
                    // `summary: ""` 的 branchSummary 消息。kiana-session-core 对假值摘要返回空，
                    // 这里按原始字段补回。
                    SessionEntry::Unknown(object) => Some(branch_summary_message_from_raw(object)),
                    _ => None,
                },
            }
        }
        // compaction 条目可能同时贡献 systemMessage 与 compactionSummary，TS 只取摘要消息。
        "compaction" => session_entry_to_context_messages(entry)
            .into_iter()
            .find(|message| message.role() == "compactionSummary"),
        // TS 的 switch 没有 default：thinking_level_change / model_change / custom / label /
        // session_info / usage / context_edit 以及任何未来条目类型都不贡献消息。
        _ => None,
    }
}

/// 原始（[`SessionEntry::Unknown`]）`branch_summary` 条目的消息。
///
/// TS: `messages.ts#createBranchSummaryMessage(entry.summary, entry.fromId, entry.timestamp)`
/// 按原始字段构造对象；缺省（`undefined`）的键在 `JSON.stringify` 中省略，但空字符串
/// `summary` 仍然保留。`timestamp` 走 `new Date(value).getTime()`（无效为 `NaN`，JSON 中为
/// `null`）。
///
/// 能力边界：`summary` 缺省/`null` 时 TS 的 `estimateTokens` 会在 `message.summary.length`
/// 上抛 `TypeError`；Rust 用 `NaN` 近似（不复制崩溃）。`summary` 是数字/布尔时 TS 得到
/// `undefined`（`chars` 变 `NaN`），`estimate_tokens_f64` 同样返回 `NaN`（见
/// [`crate::compaction::estimate_tokens`]）。
fn branch_summary_message_from_raw(object: &JsonObject) -> AgentMessage {
    let mut message = JsonObject::new();
    message.insert(
        "role".to_string(),
        JsonValue::String("branchSummary".to_string()),
    );
    if let Some(summary) = object.get("summary") {
        message.insert("summary".to_string(), summary.clone());
    }
    if let Some(from_id) = object.get("fromId") {
        message.insert("fromId".to_string(), from_id.clone());
    }
    message.insert(
        "timestamp".to_string(),
        json_number_or_null(js_date_constructor_time(object.get("timestamp"))),
    );
    AgentMessage::Custom(Box::new(message))
}

/// V8 `kMaxTimeInMs`（±8.64e15）；`TimeClip` 之外的日期为 Invalid Date。
const MAX_TIME_IN_MS: f64 = 864_000_000.0 * 10_000_000.0;

/// JS `TimeClip(value)`：非有限或超出 ±8.64e15 → `NaN`，否则截断为整数毫秒。
fn time_clip(value: f64) -> f64 {
    if !value.is_finite() || value.abs() > MAX_TIME_IN_MS {
        f64::NAN
    } else {
        value.trunc()
    }
}

/// `new Date(value).getTime()`：按 `Date` 构造器的强制转换规则处理任意 JSON 值。
///
/// - 缺省（`undefined`）→ `NaN`；`null` → 0；
/// - 布尔 → 0/1；数字 → 毫秒（TimeClip）；
/// - 字符串 → `Date.parse`；
/// - 数组 → `Array.prototype.toString` 后再 `Date.parse`；对象 → `NaN`。
fn js_date_constructor_time(value: Option<&JsonValue>) -> f64 {
    match value {
        None => f64::NAN,
        Some(JsonValue::Null) => 0.0,
        Some(JsonValue::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(JsonValue::Number(number)) => time_clip(number.as_f64().unwrap_or(f64::NAN)),
        Some(JsonValue::String(text)) => kiana_ai::model_catalog::js_date_parse(text)
            .map(time_clip)
            .unwrap_or(f64::NAN),
        Some(JsonValue::Array(items)) => {
            let text = items
                .iter()
                .map(|item| match item {
                    // `Array.prototype.join`：null/undefined 元素写成空串。
                    JsonValue::Null => String::new(),
                    other => crate::utils::js_value_to_string(other),
                })
                .collect::<Vec<_>>()
                .join(",");
            kiana_ai::model_catalog::js_date_parse(&text)
                .map(time_clip)
                .unwrap_or(f64::NAN)
        }
        Some(JsonValue::Object(_)) => f64::NAN,
    }
}

/// `f64` → JSON 数字；`NaN`/`±Infinity` → `null`（与 `JSON.stringify` 一致）。
fn json_number_or_null(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

/// 读取 `branch_summary` 条目的 `fromHook` / `details`（typed 或原始对象）。
fn branch_summary_details(entry: &SessionEntry) -> Option<(bool, Option<&JsonValue>)> {
    match entry {
        SessionEntry::BranchSummary(branch) => {
            Some((entry_from_hook_truthy(entry), branch.details.as_ref()))
        }
        SessionEntry::Unknown(object) => {
            // TS 只在 `entry.type === "branch_summary"` 时读取 details；未知 / 未来条目
            // （例如带 details 的 session_info、custom）不参与文件跟踪。
            if object.get("type").and_then(JsonValue::as_str) != Some("branch_summary") {
                return None;
            }
            Some((
                entry_from_hook_truthy(entry),
                object.get("details").filter(|details| js_truthy(details)),
            ))
        }
        _ => None,
    }
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

/// 按 token 预算准备分支条目。
///
/// 从**最新到最旧**加入消息直到预算耗尽，保证分支过长时保留最近的上下文。文件操作从所有
/// 条目收集（即使不进预算）：assistant toolCall 的调用，以及既有 `branch_summary` 条目的
/// `details`（累积跟踪）。
///
/// 注意：第二遍会调用 `get_message_from_entry`，而 `toolResult` 消息在那里被跳过，因此
/// toolResult 上记录的 `nestedCalls` 文件操作不会进入分支摘要的 `file_ops`（这是两遍扫描下的既有语义，改动需同步测试）。
///
/// `token_budget <= 0` 表示不限制（TS 的 `contextWindow - reserveTokens` 可以为负）。
///
/// TS: `branch-summarization.ts#prepareBranchEntries`
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: i64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens = 0.0f64;

    // 第一遍：从所有条目收集文件操作（即使它们不进 token 预算）。
    // 只处理 kiana 生成的摘要（`fromHook` 为 JS 假值）。
    for entry in entries {
        let Some((from_hook, details)) = branch_summary_details(entry) else {
            continue;
        };
        if from_hook {
            continue;
        }
        let Some(details) = details else {
            continue;
        };
        if let Some(read_files) = details.get("readFiles").and_then(JsonValue::as_array) {
            for path in read_files {
                // TS 的 Set 接受任意元素（不做 `typeof === "string"` 检查）：原始值入集合，
                // 只在渲染时按 JS 规则转换（见 [`FileValue`]）。
                file_ops.read.insert(FileValue::from(path.clone()));
            }
        }
        if let Some(modified_files) = details.get("modifiedFiles").and_then(JsonValue::as_array) {
            for path in modified_files {
                // TS 注释说同时进 edited 与 written，但代码只加 edited；
                // `computeFileLists` 的 modified 是 edited ∪ written，因此等效。
                file_ops.edited.insert(FileValue::from(path.clone()));
            }
        }
    }

    // 第二遍：从新到旧加入消息直到预算耗尽。
    // 头插（等价 TS 的 `unshift`）把遍历顺序反回时间顺序。
    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry) else {
            continue;
        };

        extract_file_ops_from_message(&message, &mut file_ops);
        // 畸形摘要条目（如 `summary: 123`）估算得到 NaN；NaN 与预算比较恒为 false，
        // 因此预算失效、更老的条目全部保留。
        let tokens = estimate_tokens_f64(&message);

        if token_budget > 0 && total_tokens + tokens > token_budget as f64 {
            // 摘要类条目是重要上下文：已用 token 仍低于预算的 90% 时强行放入。
            // 比较是严格小于，已用 token 恰好等于 90% 时不放行（测试锁定该边界）。
            if (entry.kind() == "compaction" || entry.kind() == "branch_summary")
                && total_tokens < (token_budget as f64) * 0.9
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

// ============================================================================
// Summary Generation
// ============================================================================

/// 分支摘要前缀（结尾有一个空行）。
///
/// TS: `branch-summarization.ts#BRANCH_SUMMARY_PREAMBLE`（UTF-16 长度 103）
const BRANCH_SUMMARY_PREAMBLE: &str = r#"The user explored a different conversation branch before returning here.
Summary of that exploration:

"#;

/// 分支摘要 prompt（逐字）。
///
/// TS: `branch-summarization.ts#BRANCH_SUMMARY_PROMPT`（UTF-16 长度 689）
const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// 生成被放弃分支的摘要。
///
/// 中止返回 `{ aborted: true }`，软失败/工具调用返回 `{ error }`；硬失败（与 TS 的 Promise
/// rejection 对应）返回 `Err`。
///
/// TS: `branch-summarization.ts#generateBranchSummary`
pub async fn generate_branch_summary(
    entries: &[SessionEntry],
    options: &GenerateBranchSummaryOptions,
) -> Result<BranchSummaryResult, CompactionError> {
    let reserve_tokens = options.reserve_tokens.unwrap_or(16384);
    // TS: `model.contextWindow || 128000`（0 与 undefined 都回退）。
    let context_window = match options.model.context_window {
        Some(window) if window != 0 => window,
        _ => 128_000,
    };
    // 预算可能为负（TS 的数值语义），`prepare_branch_entries` 把 `<= 0` 当作不限制。
    // `i64::try_from` 仅在 u64 超过 i64::MAX 时失败，此时按 i64::MAX 计算。
    let token_budget = i64::try_from(context_window).unwrap_or(i64::MAX)
        - i64::try_from(reserve_tokens).unwrap_or(i64::MAX);

    let preparation = prepare_branch_entries(entries, token_budget);
    // 没有可总结的消息时不发起 LLM 调用：TS 返回固定文案，且不带 usage/readFiles。
    if preparation.messages.is_empty() {
        return Ok(BranchSummaryResult {
            summary: Some("No content to summarize".to_string()),
            ..BranchSummaryResult::default()
        });
    }

    // 先转成 LLM 消息再序列化，避免模型把它当成要继续的对话。
    let llm_messages = convert_to_llm(&preparation.messages);
    let conversation_text = serialize_conversation(&llm_messages);

    // TS 的真值判断：空串 customInstructions 等价于缺省。
    let custom_instructions = options
        .custom_instructions
        .as_deref()
        .filter(|instructions| !instructions.is_empty());
    // replaceInstructions 只有在提供了非空自定义指令时才替换默认 prompt；否则自定义指令
    // 追加为 `Additional focus:`。空串按缺省处理（JS 真值）。
    let instructions =
        if options.replace_instructions == Some(true) && custom_instructions.is_some() {
            custom_instructions.unwrap_or_default().to_string()
        } else if let Some(custom_instructions) = custom_instructions {
            format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom_instructions}")
        } else {
            BRANCH_SUMMARY_PROMPT.to_string()
        };
    let prompt_text =
        format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");

    // 只设置 apiKey/headers/env/signal，不设置 reasoning、sessionId 或
    // toolChoice；`complete_summarization` 会补 cacheRetention:"none" 与新的 session id
    // （测试 `does_not_override_tool_choice_for_branch_summaries` 锁定这些默认值）。
    let request_options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: options.api_key.clone(),
            headers: options.headers.clone(),
            env: options.env.clone(),
            signal: Some(options.signal.token()),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    };

    // 硬失败（streamFn 拒绝/流 reject/uuid 失败）与 TS 的 Promise rejection 一样向上传播。
    let response = complete_summarization(
        &options.model,
        build_summarization_context(&prompt_text),
        request_options,
        options.stream_fn.clone(),
        options.retry.clone(),
        options.callbacks.clone(),
    )
    .await?;

    // 分流顺序：aborted 优先（已取消的请求不应被当作错误），其次 error/length，最后才是 toolCall。
    if response.stop_reason == StopReason::Aborted {
        return Ok(BranchSummaryResult {
            aborted: Some(true),
            ..BranchSummaryResult::default()
        });
    }
    if let Some(failure) = get_summarization_failure(&response, "Branch summarization") {
        return Ok(BranchSummaryResult {
            error: Some(failure),
            ..BranchSummaryResult::default()
        });
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Ok(BranchSummaryResult {
            error: Some("Branch summarization attempted to call a tool".to_string()),
            ..BranchSummaryResult::default()
        });
    }

    let mut summary = content_text_assistant(&response.content, "\n");
    // 加 preamble 说明这是被放弃分支的摘要。
    summary = format!("{BRANCH_SUMMARY_PREAMBLE}{summary}");

    // 文件列表 = 第一遍收集的全部 `branch_summary.details` + 第二遍已遍历消息（含触发
    // 预算上限的那一条）的 toolCall；预算 break 之后更旧的条目不再遍历，其 toolCall 不进
    // 列表。列表追加在模型文本之后。
    let lists = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(
        &lists.read_files,
        &lists.modified_files,
    ));

    Ok(BranchSummaryResult {
        // preamble 让 summary 永远非空，`|| "No summary generated"` 实际不可达（照抄 TS）。
        summary: Some(if summary.is_empty() {
            "No summary generated".to_string()
        } else {
            summary
        }),
        usage: Some(response.usage),
        read_files: Some(lists.read_files),
        modified_files: Some(lists.modified_files),
        aborted: None,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> usize {
        text.encode_utf16().count()
    }

    #[test]
    fn branch_summary_preamble_is_byte_exact() {
        assert_eq!(units(BRANCH_SUMMARY_PREAMBLE), 103);
        assert_eq!(BRANCH_SUMMARY_PREAMBLE.len(), 103);
        assert!(BRANCH_SUMMARY_PREAMBLE.ends_with("\n\n"));
    }

    #[test]
    fn branch_summary_prompt_is_byte_exact() {
        assert_eq!(units(BRANCH_SUMMARY_PROMPT), 689);
        assert_eq!(BRANCH_SUMMARY_PROMPT.len(), 689);
        assert!(
            BRANCH_SUMMARY_PROMPT
                .starts_with("Create a structured summary of this conversation branch")
        );
        assert!(BRANCH_SUMMARY_PROMPT.ends_with("error messages."));
    }
}
