//! 压缩前的 tool-result pruning（确定性、无 LLM）。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/oh-my-pi/packages/agent/src/compaction/pruning.ts`：
//!   `protectTokens = 40_000`、`minimumSavings = 20_000`、`MIN_PRUNE_TOKENS = 50`、
//!   superseded read 优先、skill/plan 保护、精确 placeholder、原地 blank；
//! - `reference_project/oh-my-pi/packages/agent/src/compaction/tool-protection.ts`：
//!   工具调用配对查找与 `skill://` / active plan 的 read 保护；
//! - `reference_project/ClaudeCode/src/services/compact/microCompact.ts`：
//!   `[Old tool result content cleared]` 占位符与“只改内容、不删消息”的 microcompact
//!   形态。
//!
//! # 不变量
//!
//! 1. **只原地改内容**：entry 的 id、parentId、顺序、`toolCallId`、`toolName` 都不变，
//!    assistant(tool_calls) 与 tool_result 的配对始终合法；
//! 2. **保护窗口**：最新 [`DEFAULT_PROTECT_TOKENS`] 个 tool-output token 原样保留；
//!    superseded / useless 结果可以穿透窗口（它们在任何年龄都是死重量）；
//! 3. **回收门槛**：估算节省 < [`DEFAULT_MINIMUM_SAVINGS`] 时返回原始历史（`applied=false`），
//!    避免为一点收益破坏 prompt cache；
//! 4. **小结果不动**：token 数 < [`MIN_PRUNE_TOKENS`] 的结果 blank 后反而变大；
//! 5. **错误结果不动**：失败结果是证据，只有显式 useless 标记才允许清除。
//!
//! # 与调用方的关系
//!
//! `AgentSession` 在 `prepare_compaction` 之前调用 [`prune_tool_outputs`]，把返回的
//! `entries`（纯函数结果，不修改入参）写回会话；若写回失败，原历史仍在调用方手里，
//! 不需要 undo。聚合预算由 [`enforce_aggregate_tool_result_budget`] 在构建请求前执行。

use std::collections::{HashMap, HashSet};

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, JsonValue, Message, TextContent, TextKind, ToolCall, ToolResultMessage,
    UserContent, UserContentBlock,
};
use kiana_core::artifact_store::ArtifactStore;
use kiana_session_core::session_manager::{SessionEntry, SessionMessageEntry};

use crate::compaction::estimate_tool_result_tokens;

/// 默认保护的最新 tool-output token 数。
pub const DEFAULT_PROTECT_TOKENS: u64 = 40_000;
/// 默认最小回收量（token）；低于它不做任何改动。
pub const DEFAULT_MINIMUM_SAVINGS: u64 = 20_000;
/// 小于该 token 数的结果不 blank（占位符本身有成本）。
pub const MIN_PRUNE_TOKENS: u64 = 50;

/// 被更新一次同文件读取覆盖时的精确占位符。
pub const SUPERSEDED_NOTICE: &str = "[Superseded by a newer read of this file]";
/// 零结果 grep / 等待类结果的精确占位符。
pub const USELESS_NOTICE: &str = "[Uneventful result elided]";
/// 聚合预算清除结果、但没有 artifact 时的占位符。
pub const CLEARED_NOTICE: &str = "[Old tool result content cleared]";

/// 聚合预算清除结果、完整内容已落盘时的占位符。
pub fn cleared_with_artifact_notice(path: &str) -> String {
    format!("[Old tool result content cleared; artifact: {path}]")
}

/// 年龄裁剪的占位符（保留原 token 数，便于诊断）。
pub fn truncated_notice(tokens: u64) -> String {
    format!("[Output truncated - {tokens} tokens]")
}

/// tool-result 保护匹配器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectedToolMatcher {
    /// 按工具名保护（例如 `skill`）。
    ToolName(String),
    /// 保护 `read` 的 skill 文档（`skill://` URL、`/skills/…/SKILL.md`）。
    SkillRead,
    /// 保护 active plan 的 read 结果（路径精确匹配，忽略 `local:/` 与读取器后缀）。
    PlanRead {
        /// 当前会话的 plan 路径；空集合表示没有 active plan。
        paths: Vec<String>,
    },
    /// 保护对 artifact 自身的回读（清除它只会产生新的 artifact，可能无限循环）。
    ArtifactRead,
}

/// pruning 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneConfig {
    /// 保护的最新 tool-output token 数。
    pub protect_tokens: u64,
    /// 最小回收量；不足时整次 prune 回滚为 no-op。
    pub minimum_savings: u64,
    /// 保护匹配器。
    pub protected_tools: Vec<ProtectedToolMatcher>,
    /// 是否清除工具标记为 useless 的结果（零结果 grep / wait）。
    pub prune_useless: bool,
    /// 最新压缩条目的 `firstKeptEntryId`；它之前的条目不会被发送，跳过。
    pub keep_boundary_id: Option<String>,
    /// 是否启用 superseded read 检测（默认启用）。
    pub prune_superseded: bool,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            protect_tokens: DEFAULT_PROTECT_TOKENS,
            minimum_savings: DEFAULT_MINIMUM_SAVINGS,
            protected_tools: vec![
                ProtectedToolMatcher::SkillRead,
                ProtectedToolMatcher::ArtifactRead,
                ProtectedToolMatcher::PlanRead { paths: Vec::new() },
                // 新版 `skill` 工具的结果不带 `skill://` / `SKILL.md` 路径，必须按工具名保护，
                // 否则活跃技能正文会在输出超过保护窗口后被当作旧结果清除。
                ProtectedToolMatcher::ToolName("skill".to_string()),
            ],
            prune_useless: true,
            keep_boundary_id: None,
            prune_superseded: true,
        }
    }
}

impl PruneConfig {
    /// 设置 active plan 路径。
    pub fn with_plan_paths(mut self, paths: Vec<String>) -> Self {
        for matcher in &mut self.protected_tools {
            if let ProtectedToolMatcher::PlanRead { paths: configured } = matcher {
                *configured = paths.clone();
            }
        }
        if !self
            .protected_tools
            .iter()
            .any(|matcher| matches!(matcher, ProtectedToolMatcher::PlanRead { .. }))
        {
            self.protected_tools
                .push(ProtectedToolMatcher::PlanRead { paths });
        }
        self
    }

    /// 设置压缩边界。
    pub fn with_boundary(mut self, first_kept_entry_id: Option<String>) -> Self {
        self.keep_boundary_id = first_kept_entry_id;
        self
    }
}

/// 一次 prune 的产物。
#[derive(Debug, Clone, PartialEq)]
pub struct PruneOutcome {
    /// 处理后的条目（纯函数结果；入参未被修改）。
    pub entries: Vec<SessionEntry>,
    /// 被 blank 的结果。
    pub pruned: Vec<PruneRecord>,
    /// 估算节省的 token。
    pub tokens_saved: u64,
    /// 是否真正应用（false 表示收益不足或没有候选）。
    pub applied: bool,
}

impl PruneOutcome {
    fn untouched(entries: &[SessionEntry]) -> Self {
        Self {
            entries: entries.to_vec(),
            pruned: Vec::new(),
            tokens_saved: 0,
            applied: false,
        }
    }

    /// 是否没有任何改动。
    pub fn is_noop(&self) -> bool {
        !self.applied
    }
}

/// 单条被 blank 的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneRecord {
    /// 会话条目 id。
    pub entry_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 占位符。
    pub notice: String,
}

/// 一次 result 的“无用”分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UselessKind {
    /// grep 零匹配。
    ZeroResultGrep,
    /// 成功的等待/睡眠。
    Wait,
    /// 工具在 details 里显式标记 `useless: true`。
    Flagged,
}

/// 判断结果是否可以在保护窗口内直接清除。
///
/// - `grep` 的零结果：内容只有 “No matches found” 之类；
/// - `wait` / `sleep` 的成功结果：短文本；
/// - `details.useless == true`：工具自己声明。
pub fn classify_useless(
    tool_name: &str,
    content: &str,
    details: Option<&JsonValue>,
) -> Option<UselessKind> {
    if details
        .and_then(|details| details.get("useless"))
        .and_then(JsonValue::as_bool)
        == Some(true)
    {
        return Some(UselessKind::Flagged);
    }
    let trimmed = content.trim();
    let lower = trimmed.to_ascii_lowercase();
    if matches!(tool_name, "grep" | "grep_search" | "search") {
        let zero = lower.is_empty()
            || lower == "no matches found"
            || lower == "no matches found."
            || lower == "no matches"
            || lower == "no matches."
            || lower == "found 0 matches"
            || lower == "0 matches"
            || lower == "0 results";
        if zero {
            return Some(UselessKind::ZeroResultGrep);
        }
    }
    if matches!(tool_name, "wait" | "sleep" | "task_wait" | "wait_for")
        && !lower.contains("error")
        && !lower.contains("failed")
        && trimmed.chars().count() <= 200
    {
        return Some(UselessKind::Wait);
    }
    None
}

/// `read` 的 supersede key。
///
/// - 无 offset/limit 的整文件读取：key = 路径（父键）；
/// - 带 offset/limit 的范围读取：key = `路径\0offset:limit`（子键）；
/// - `skill://` / `artifact://` 等内部 URL 不参与 supersede。
///
/// 父键的**完整**读取可以覆盖子键范围读取；同 key 的新读取覆盖旧读取。
pub fn read_supersede_key(tool_name: &str, args: &JsonValue) -> Option<String> {
    if tool_name != "read" && tool_name != "read_file" {
        return None;
    }
    let path = args
        .get("path")
        .or_else(|| args.get("file_path"))
        .and_then(JsonValue::as_str)?;
    if path.is_empty() || path.contains("://") {
        return None;
    }
    let offset = json_number(args.get("offset"));
    let limit = json_number(args.get("limit"));
    if offset.is_none() && limit.is_none() {
        Some(path.to_string())
    } else {
        Some(format!("{path}\u{0}{offset:?}:{limit:?}"))
    }
}

/// 读取数值参数，兼容字符串形态（`{"limit": "20"}`）。
///
/// 这里看的是**持久化的原始** tool-call 参数：执行前会按 JSON schema 做类型强转，
/// 但强转结果不会写回消息，所以模型传字符串时 `as_f64()` 得不到值。若按“无
/// offset/limit”处理，范围读取会被当成整文件读取，supersede 关系就错了。
fn json_number(value: Option<&JsonValue>) -> Option<f64> {
    match value? {
        JsonValue::Number(number) => number.as_f64(),
        JsonValue::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// 读取结果是否覆盖整个文件（可证明完整）。
///
/// Kiana 的 `read` 只在截断时写 `details.truncation`；没有该字段且结果是**结构上**
/// 未截断的形态时视为完整。截断形态有三种：
/// - 输出预算的硬截断：标记追加在结果的最后一行（`[output truncated: original N chars]`）；
/// - 落盘后的 `<persisted-output>` 预览：整条结果以包装标签开头，中间是
///   `… [middle elided: N chars] …`；
/// - 本模块自己的年龄占位符：整条结果就是一行 `[Output truncated - N tokens]`。
///
/// 只看“全文是否包含某个标记”会把恰好提到这些字样的普通文件读成截断（本模块源码
/// 就是一例），让完整读取无法 retire 旧的冗余读取。
pub fn is_complete_read_result(result: &ToolResultMessage) -> bool {
    if result.is_error {
        return false;
    }
    if let Some(details) = &result.details
        && details
            .get("truncation")
            .is_some_and(|truncation| !truncation.is_null())
    {
        return false;
    }
    !looks_truncated(&tool_result_text(result))
}

/// 结果文本是否是截断形态（见 [`is_complete_read_result`] 的三种标记）。
fn looks_truncated(text: &str) -> bool {
    if text.trim_start().starts_with("<persisted-output>") {
        return true;
    }
    let compact = text.trim();
    if compact.starts_with("[Output truncated - ")
        && compact.ends_with(" tokens]")
        && !compact.contains('\n')
    {
        return true;
    }
    text.trim_end()
        .rsplit('\n')
        .next()
        .is_some_and(|line| line.starts_with("[output truncated: original "))
}

/// 把 tool result 的文本块拼起来（图片块忽略）。
pub fn tool_result_text(result: &ToolResultMessage) -> String {
    let mut out = String::new();
    for block in &result.content {
        if let UserContentBlock::Text(text) = block {
            out.push_str(&text.text);
        }
    }
    out
}

/// 工具调用表：`toolCallId -> ToolCall`。
pub fn collect_tool_calls(entries: &[SessionEntry]) -> HashMap<String, ToolCall> {
    let mut map = HashMap::new();
    for entry in entries {
        let SessionEntry::Message(entry) = entry else {
            continue;
        };
        let AgentMessage::Standard(Message::Assistant(assistant)) = &entry.message else {
            continue;
        };
        for block in &assistant.content {
            if let AssistantContentBlock::ToolCall(call) = block {
                map.insert(call.id.clone(), call.clone());
            }
        }
    }
    map
}

/// 工具结果消息。
pub fn tool_result_message(entry: &SessionEntry) -> Option<&ToolResultMessage> {
    let SessionEntry::Message(entry) = entry else {
        return None;
    };
    match &entry.message {
        AgentMessage::Standard(Message::ToolResult(result)) => Some(result),
        _ => None,
    }
}

/// active plan 路径归一化：去掉 `local:/`/`local://` 与首尾空白。
fn normalize_plan_path(path: &str) -> String {
    let trimmed = path.trim();
    let trimmed = trimmed.strip_prefix("local://").unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix("local:/").unwrap_or(trimmed);
    trimmed.trim_start_matches("./").to_string()
}

fn read_path_from_call(call: &ToolCall) -> Option<&str> {
    call.arguments
        .get("path")
        .or_else(|| call.arguments.get("file_path"))
        .and_then(JsonValue::as_str)
}

/// 判断一个 result 是否受保护。
pub fn is_protected_result(
    result: &ToolResultMessage,
    call: Option<&ToolCall>,
    matchers: &[ProtectedToolMatcher],
) -> bool {
    for matcher in matchers {
        match matcher {
            ProtectedToolMatcher::ToolName(name) => {
                if &result.tool_name == name {
                    return true;
                }
            }
            ProtectedToolMatcher::SkillRead => {
                if result.tool_name != "read" && result.tool_name != "read_file" {
                    continue;
                }
                if let Some(path) = call.and_then(read_path_from_call) {
                    if path.starts_with("skill://")
                        || (path.contains("/skills/") && path.ends_with("SKILL.md"))
                    {
                        return true;
                    }
                }
            }
            ProtectedToolMatcher::PlanRead { paths } => {
                if result.tool_name != "read" && result.tool_name != "read_file" {
                    continue;
                }
                if let Some(path) = call.and_then(read_path_from_call) {
                    let normalized = normalize_plan_path(path);
                    for plan in paths {
                        let plan = normalize_plan_path(plan);
                        if normalized == plan || normalized.starts_with(&format!("{plan}:")) {
                            return true;
                        }
                    }
                }
            }
            ProtectedToolMatcher::ArtifactRead => {
                if result.tool_name != "read" && result.tool_name != "read_file" {
                    continue;
                }
                if let Some(path) = call.and_then(read_path_from_call)
                    && path.starts_with("artifact://")
                {
                    return true;
                }
            }
        }
    }
    false
}

fn resolve_boundary_index(entries: &[SessionEntry], boundary_id: Option<&str>) -> usize {
    let Some(boundary_id) = boundary_id else {
        return 0;
    };
    entries
        .iter()
        .rposition(|entry| entry.id() == boundary_id)
        .unwrap_or(0)
}

fn notice_tokens(notice: &str) -> u64 {
    (notice.chars().count() as u64).div_ceil(4)
}

fn blank_result(entry: &mut SessionEntry, notice: &str) -> bool {
    let SessionEntry::Message(message_entry) = entry else {
        return false;
    };
    let AgentMessage::Standard(Message::ToolResult(result)) = &mut message_entry.message else {
        return false;
    };
    result.content = vec![UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: notice.to_string(),
        text_signature: None,
    })];
    // 原始消息对象会绕过 typed 视图回放旧内容，必须清掉。
    message_entry.raw_message = None;
    true
}

/// 压缩前的 tool-result pruning。
///
/// 返回处理后的 entries（入参不变）。收益不足时 `applied == false` 且 entries
/// 与入参逐项相等。
pub fn prune_tool_outputs(entries: &[SessionEntry], config: &PruneConfig) -> PruneOutcome {
    let boundary = resolve_boundary_index(entries, config.keep_boundary_id.as_deref());
    let calls = collect_tool_calls(entries);

    // 第一遍：标记 superseded（从新到旧）。
    let mut superseded_ids: HashSet<String> = HashSet::new();
    if config.prune_superseded {
        #[derive(Default)]
        struct NewerRead {
            success: bool,
            complete: bool,
        }
        let mut newer_by_key: HashMap<String, NewerRead> = HashMap::new();
        for index in (boundary..entries.len()).rev() {
            let entry = &entries[index];
            let Some(result) = tool_result_message(entry) else {
                continue;
            };
            let Some(call) = calls.get(&result.tool_call_id) else {
                continue;
            };
            if is_protected_result(result, Some(call), &config.protected_tools) {
                continue;
            }
            let arguments = serde_json::to_value(&call.arguments).unwrap_or(JsonValue::Null);
            let Some(key) = read_supersede_key(&call.name, &arguments) else {
                continue;
            };
            let separator = key.find('\u{0}');
            let same_key = newer_by_key.get(&key);
            let parent = separator.and_then(|index| newer_by_key.get(&key[..index]));
            let superseded = if result.is_error {
                same_key.is_some() || parent.is_some_and(|newer| newer.success)
            } else {
                same_key.is_some_and(|newer| newer.success)
                    || parent.is_some_and(|newer| newer.complete)
            };
            let newer = newer_by_key.entry(key).or_default();
            if !result.is_error && !newer.success {
                newer.success = true;
                newer.complete = is_complete_read_result(result);
            }
            if superseded {
                superseded_ids.insert(entry.id().to_string());
            }
        }
    }

    // 第二遍：从新到旧累计 tool-output token，选候选。
    let mut accumulated: u64 = 0;
    let mut candidates: Vec<(usize, String, String)> = Vec::new();
    for index in (boundary..entries.len()).rev() {
        let entry = &entries[index];
        let Some(result) = tool_result_message(entry) else {
            continue;
        };
        let tokens = estimate_tool_result_tokens(result);
        let call = calls.get(&result.tool_call_id);
        let protected = is_protected_result(result, call, &config.protected_tools);
        let already_cleared = tool_result_text(result)
            .starts_with("[Old tool result content cleared")
            || tool_result_text(result) == SUPERSEDED_NOTICE
            || tool_result_text(result) == USELESS_NOTICE;

        let superseded = superseded_ids.contains(entry.id());
        let useless = config.prune_useless
            && !result.is_error
            && classify_useless(
                &result.tool_name,
                &tool_result_text(result),
                result.details.as_ref(),
            )
            .is_some();

        if already_cleared {
            accumulated = accumulated.saturating_add(tokens);
            continue;
        }

        // `MIN_PRUNE_TOKENS` 必须对所有清除分支生效：superseded / useless 的小结果换成
        // 占位符后反而更大（模块文档第 4 条不变量）。保护匹配器命中的结果任何分支都不动。
        let notice = if !protected && superseded && tokens >= MIN_PRUNE_TOKENS {
            Some(SUPERSEDED_NOTICE.to_string())
        } else if !protected && useless && tokens >= MIN_PRUNE_TOKENS {
            Some(USELESS_NOTICE.to_string())
        } else if !protected
            && !result.is_error
            && accumulated >= config.protect_tokens
            && tokens >= MIN_PRUNE_TOKENS
        {
            Some(truncated_notice(tokens))
        } else {
            None
        };

        if let Some(notice) = notice {
            candidates.push((index, entry.id().to_string(), notice));
        }
        accumulated = accumulated.saturating_add(tokens);
    }

    let mut tokens_saved = 0u64;
    for (index, _, notice) in &candidates {
        let Some(result) = tool_result_message(&entries[*index]) else {
            continue;
        };
        let tokens = estimate_tool_result_tokens(result);
        tokens_saved = tokens_saved.saturating_add(tokens.saturating_sub(notice_tokens(notice)));
    }

    if candidates.is_empty() || tokens_saved < config.minimum_savings {
        return PruneOutcome::untouched(entries);
    }

    let mut pruned_entries = entries.to_vec();
    let mut records = Vec::new();
    for (index, entry_id, notice) in candidates {
        let entry = &mut pruned_entries[index];
        let tool_name = tool_result_message(entry)
            .map(|result| result.tool_name.clone())
            .unwrap_or_default();
        if blank_result(entry, &notice) {
            records.push(PruneRecord {
                entry_id,
                tool_name,
                notice,
            });
        }
    }

    PruneOutcome {
        entries: pruned_entries,
        pruned: records,
        tokens_saved,
        applied: true,
    }
}

// ============================================================================
// 聚合预算
// ============================================================================

/// 聚合 tool-result 预算的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateBudgetOutcome {
    /// 处理后的条目。
    pub entries: Vec<SessionEntry>,
    /// 被清除的条目及其占位符（可直接用于 `append_context_edit`）。
    pub cleared: Vec<PruneRecord>,
    /// 清除后的估算 tool-result token 总数。
    pub tokens_after: u64,
    /// 因没有 artifact 而只能使用无路径占位符的条目数。
    pub cleared_without_artifact: usize,
}

/// 聚合 tool-result token 超限时，从最旧的结果开始清除。
///
/// 原内容在 `store` 存在时完整落盘，占位符写入 `artifact: <path>`；清空只改
/// `content`，不删除消息、不拆 tool pair。skill/plan/artifact 回读受保护。
///
/// 每次请求前都会跑一次；只需要清除计划的调用方用
/// [`plan_aggregate_tool_result_budget`] 可以避免整段历史的深拷贝。
pub fn enforce_aggregate_tool_result_budget(
    entries: &[SessionEntry],
    max_tokens: u64,
    store: Option<&ArtifactStore>,
    config: &PruneConfig,
) -> AggregateBudgetOutcome {
    let plan = plan_aggregate_tool_result_budget(entries, max_tokens, store, config);
    if plan.cleared.is_empty() {
        return AggregateBudgetOutcome {
            entries: entries.to_vec(),
            cleared: plan.cleared,
            tokens_after: plan.tokens_after,
            cleared_without_artifact: plan.cleared_without_artifact,
        };
    }
    let mut cleared_entries = entries.to_vec();
    for record in &plan.cleared {
        if let Some(index) = entries
            .iter()
            .position(|entry| entry.id() == record.entry_id)
        {
            blank_result(&mut cleared_entries[index], &record.notice);
        }
    }
    AggregateBudgetOutcome {
        entries: cleared_entries,
        cleared: plan.cleared,
        tokens_after: plan.tokens_after,
        cleared_without_artifact: plan.cleared_without_artifact,
    }
}

/// 聚合预算的清除计划（不复制 `entries`）。
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateClearPlan {
    /// 需要清除的条目及其占位符（可直接用于 `append_context_edit`）。
    pub cleared: Vec<PruneRecord>,
    /// 清除后的估算 tool-result token 总数。
    pub tokens_after: u64,
    /// 因没有 artifact 而只能使用无路径占位符的条目数。
    pub cleared_without_artifact: usize,
}

/// 计算聚合预算需要清除哪些条目。
///
/// 两个不变量：
/// - **清除最新结果必须换来预算达标**：错误/图片等不可清除的结果本身也计入预算，
///   当它们单独就超过 `max_tokens` 时，继续清到模型刚拿到的输出只会让模型看不到
///   本轮工具结果，预算却仍然超标；这种情况下保留最新结果（它前面可清除的结果
///   照常清除）。
/// - 计划阶段不复制历史：生产路径每次 provider 请求前都会调用，`entries.to_vec()`
///   会把整段上下文深拷贝一遍。
///
/// 已经通过 `context_edit` 清除过的条目请用
/// [`plan_aggregate_tool_result_budget_with_replacements`]：`branch` 里的原始内容
/// 仍是旧文本，不传替换内容会把它重新选为候选、重复落盘并追加重复编辑。
pub fn plan_aggregate_tool_result_budget(
    entries: &[SessionEntry],
    max_tokens: u64,
    store: Option<&ArtifactStore>,
    config: &PruneConfig,
) -> AggregateClearPlan {
    plan_aggregate_tool_result_budget_with_replacements(
        entries,
        &HashMap::new(),
        max_tokens,
        store,
        config,
    )
}

/// [`plan_aggregate_tool_result_budget`] 的投影感知版本。
///
/// `replacements` 是分支上 `context_edit` 生效后的替换内容（`target_id -> content`，
/// 后出现的覆盖先出现的）。替换过的条目按替换内容计 token，并且不再是清除候选：
/// 投影里的模型消息已经是占位符，再清一次只会重复写 artifact / 追加编辑。
pub fn plan_aggregate_tool_result_budget_with_replacements(
    entries: &[SessionEntry],
    replacements: &HashMap<String, JsonValue>,
    max_tokens: u64,
    store: Option<&ArtifactStore>,
    config: &PruneConfig,
) -> AggregateClearPlan {
    let calls = collect_tool_calls(entries);
    let boundary = resolve_boundary_index(entries, config.keep_boundary_id.as_deref());
    let newest = (boundary..entries.len())
        .rev()
        .find(|index| tool_result_message(&entries[*index]).is_some());

    let mut candidates: Vec<(usize, u64)> = Vec::new();
    let mut total = 0u64;
    for (index, entry) in entries.iter().enumerate().skip(boundary) {
        let Some(result) = tool_result_message(entry) else {
            continue;
        };
        // 已被 `context_edit` 替换的条目：模型看到的是替换内容，按它计 token，且不再
        // 作为清除候选（否则每个请求都会重复落盘并追加一条编辑）。
        if let Some(replacement) = replacements.get(entry.id()) {
            total = total.saturating_add(replacement_tokens(replacement));
            continue;
        }
        let tokens = estimate_tool_result_tokens(result);
        total = total.saturating_add(tokens);
        let text = tool_result_text(result);
        if text.starts_with("[Old tool result content cleared") || text == SUPERSEDED_NOTICE {
            continue;
        }
        if result.is_error {
            continue;
        }
        if is_protected_result(
            result,
            calls.get(&result.tool_call_id),
            &config.protected_tools,
        ) {
            continue;
        }
        candidates.push((index, tokens));
    }

    let mut cleared = Vec::new();
    let mut cleared_without_artifact = 0usize;
    let mut tokens_after = total;
    for (index, tokens) in candidates {
        if tokens_after <= max_tokens {
            break;
        }
        // 最新结果只在“清除后预算能达标”时才清除；不可清除的结果本身已超预算时，
        // 牺牲本轮输出也换不来达标，保留它。占位符本身也占 token，用保守上界计入。
        if newest == Some(index) {
            let remaining = tokens_after
                .saturating_sub(tokens)
                .saturating_add(notice_cost_upper_bound());
            if remaining > max_tokens {
                continue;
            }
        }
        let Some(result) = tool_result_message(&entries[index]) else {
            continue;
        };
        let original = tool_result_text(result);
        if original.trim().is_empty() {
            continue;
        }
        let notice = match store {
            Some(store) => match store.save(&result.tool_name, &original) {
                Ok(handle) => cleared_with_artifact_notice(&handle.path.display().to_string()),
                Err(_) => {
                    cleared_without_artifact += 1;
                    CLEARED_NOTICE.to_string()
                }
            },
            None => {
                cleared_without_artifact += 1;
                CLEARED_NOTICE.to_string()
            }
        };
        tokens_after = tokens_after
            .saturating_sub(tokens)
            .saturating_add(plain_text_tokens(&notice));
        cleared.push(PruneRecord {
            entry_id: entries[index].id().to_string(),
            tool_name: result.tool_name.clone(),
            notice,
        });
    }

    AggregateClearPlan {
        cleared,
        tokens_after,
        cleared_without_artifact,
    }
}

/// 纯文本内容的 token 估算（与 [`estimate_tool_result_tokens`] 对单个 text 块一致）。
fn plain_text_tokens(text: &str) -> u64 {
    let chars = text.encode_utf16().count() as f64;
    (chars / 4.0).ceil() as u64
}

/// 清除占位符 token 的保守上界（artifact 路径按 512 字符计）。
///
/// 只用于“清掉最新结果能否让预算达标”的前置判断：上界成立时实际占位符一定更短。
fn notice_cost_upper_bound() -> u64 {
    let path = "a".repeat(512);
    plain_text_tokens(&cleared_with_artifact_notice(&path))
}

/// `context_edit` 替换内容的 token 估算。
///
/// `null` 表示目标从模型上下文省略（0 token）；字符串按文本计；其它内容块数组按
/// 序列化后的近似长度计。
fn replacement_tokens(content: &JsonValue) -> u64 {
    match content {
        JsonValue::Null => 0,
        JsonValue::String(text) => plain_text_tokens(text),
        other => plain_text_tokens(&other.to_string()),
    }
}

// ============================================================================
// 配对校验
// ============================================================================

/// tool call/result 配对违规（用于 split-turn 与压缩后自检）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingViolation {
    /// 条目下标。
    pub index: usize,
    /// 说明。
    pub message: String,
}

/// 校验所有 tool result 都有配对的 assistant tool call，且同一批 result 不跨
/// assistant 边界。
pub fn tool_pairing_violations(entries: &[SessionEntry]) -> Vec<PairingViolation> {
    let mut violations = Vec::new();
    let mut known_calls: HashSet<String> = HashSet::new();
    let mut pending: HashSet<String> = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let SessionEntry::Message(message_entry) = entry else {
            continue;
        };
        match &message_entry.message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                // 新的 assistant 意味着上一批 tool call 已结束；仍有 pending 说明缺少结果，
                // 但这在流式/中断场景合法，不作为违规。
                pending.clear();
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        known_calls.insert(call.id.clone());
                        pending.insert(call.id.clone());
                    }
                }
            }
            AgentMessage::Standard(Message::ToolResult(result)) => {
                if !known_calls.contains(&result.tool_call_id) {
                    violations.push(PairingViolation {
                        index,
                        message: format!(
                            "tool result {} has no matching tool call {}",
                            entry.id(),
                            result.tool_call_id
                        ),
                    });
                }
                pending.remove(&result.tool_call_id);
            }
            _ => {}
        }
    }
    violations
}

/// split-turn 的前缀/历史是否保持 tool pair 完整。
pub fn messages_pairing_violations(messages: &[AgentMessage]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut known_calls: HashSet<String> = HashSet::new();
    for message in messages {
        match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        known_calls.insert(call.id.clone());
                    }
                }
            }
            AgentMessage::Standard(Message::ToolResult(result)) => {
                if !known_calls.contains(&result.tool_call_id) {
                    violations.push(format!(
                        "tool result references unknown tool call {}",
                        result.tool_call_id
                    ));
                }
            }
            _ => {}
        }
    }
    violations
}

/// 提取 typed user 文本（custom 消息返回 `None`）。
pub fn typed_user_text(message: &AgentMessage) -> Option<String> {
    let AgentMessage::Standard(Message::User(user)) = message else {
        return None;
    };
    match &user.content {
        UserContent::Text(text) => Some(text.clone()),
        UserContent::Blocks(blocks) => {
            let mut out = String::new();
            for block in blocks {
                if let UserContentBlock::Text(text) = block {
                    out.push_str(&text.text);
                }
            }
            Some(out)
        }
    }
}

/// 消息是否是 tool result。
pub fn is_tool_result(message: &AgentMessage) -> bool {
    matches!(message, AgentMessage::Standard(Message::ToolResult(_)))
}

/// 供调用方构造 `SessionMessageEntry` 的辅助（测试/恢复路径）。
pub fn message_entry(
    id: &str,
    parent_id: Option<&str>,
    message: AgentMessage,
) -> SessionMessageEntry {
    SessionMessageEntry {
        kind: kiana_session_core::session_manager::SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: String::new(),
        message,
        raw_message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_is_precise() {
        assert_eq!(truncated_notice(1234), "[Output truncated - 1234 tokens]");
        assert_eq!(
            cleared_with_artifact_notice("/tmp/a.log"),
            "[Old tool result content cleared; artifact: /tmp/a.log]"
        );
    }

    #[test]
    fn aggregate_budget_keeps_the_newest_result_when_clearing_it_cannot_help() {
        let entries = vec![
            tool_result("old", "bash", &"a".repeat(40_000)),
            tool_result("new", "bash", "fresh output"),
        ];
        let plan = plan_aggregate_tool_result_budget(&entries, 1, None, &PruneConfig::default());
        assert_eq!(plan.cleared.len(), 1, "{plan:?}");
        assert_eq!(plan.cleared[0].entry_id, "old");

        let outcome =
            enforce_aggregate_tool_result_budget(&entries, 1, None, &PruneConfig::default());
        assert_eq!(outcome.cleared, plan.cleared);
        assert_eq!(
            tool_result_text(tool_result_message(&outcome.entries[1]).unwrap()),
            "fresh output",
            "the newest result must survive the aggregate budget"
        );

        // 不可清除的结果自己就超预算：清掉最新结果也达不到预算，保留它。
        let blocked = vec![
            error_tool_result("err", &"x".repeat(400_000)),
            tool_result("new", "bash", "fresh output"),
        ];
        let plan = plan_aggregate_tool_result_budget(&blocked, 1, None, &PruneConfig::default());
        assert!(plan.cleared.is_empty(), "{plan:?}");

        // 对照：只有一条可清除结果、且清除后（含占位符成本）能达标时必须清除
        // （`skill_paths_plan_wiring` 的 control 用例锁定同一行为）。
        let only = vec![tool_result("only", "bash", &"b".repeat(40_000))];
        let plan = plan_aggregate_tool_result_budget(&only, 1_000, None, &PruneConfig::default());
        assert_eq!(plan.cleared.len(), 1, "{plan:?}");
        assert_eq!(plan.cleared[0].entry_id, "only");

        // 预算小到连占位符都放不下：清掉也超预算，保留。
        let plan = plan_aggregate_tool_result_budget(&only, 1, None, &PruneConfig::default());
        assert!(plan.cleared.is_empty(), "{plan:?}");
    }

    #[test]
    fn edited_results_are_not_cleared_again() {
        let entries = vec![tool_result("old", "bash", &"a".repeat(40_000))];
        let mut replacements = HashMap::new();
        replacements.insert(
            "old".to_string(),
            serde_json::json!("[Old tool result content cleared]"),
        );
        let plan = plan_aggregate_tool_result_budget_with_replacements(
            &entries,
            &replacements,
            1,
            None,
            &PruneConfig::default(),
        );
        assert!(
            plan.cleared.is_empty(),
            "an already-edited result must not be selected again: {plan:?}"
        );
        assert!(
            plan.tokens_after < 50,
            "the projection counts the replacement, not the original: {}",
            plan.tokens_after
        );

        // 对照：没有替换内容时同一条目仍会被清除（占位符成本计入后仍达标）。
        let plan =
            plan_aggregate_tool_result_budget(&entries, 1_000, None, &PruneConfig::default());
        assert_eq!(plan.cleared.len(), 1);
    }

    #[test]
    fn supersede_key_accepts_string_encoded_numbers() {
        // 持久化的原始参数可能把数字写成字符串（执行前才强转，且不回写消息）。
        // 若按“无 offset/limit”处理，范围读取会被当成整文件读取。
        let bare = serde_json::json!({"path": "src/lib.rs"});
        let string_range = serde_json::json!({"path": "src/lib.rs", "offset": "10", "limit": "20"});
        let numeric_range = serde_json::json!({"path": "src/lib.rs", "offset": 10, "limit": 20});
        assert_eq!(
            read_supersede_key("read", &string_range),
            read_supersede_key("read", &numeric_range)
        );
        assert_ne!(
            read_supersede_key("read", &string_range).unwrap(),
            read_supersede_key("read", &bare).unwrap()
        );
    }

    #[test]
    fn spilled_and_pruned_results_are_not_complete_reads() {
        let complete = tool_result("a", "read", "line 1\nline 2");
        assert!(is_complete_read_result(
            tool_result_message(&complete).unwrap()
        ));

        let spilled = tool_result(
            "b",
            "read",
            "<persisted-output>\nOutput too large (45000 chars).\nPreview (middle elided, up to 40000 chars):\nhead\n… [middle elided: 5000 chars] …\ntail\n</persisted-output>",
        );
        assert!(!is_complete_read_result(
            tool_result_message(&spilled).unwrap()
        ));

        let age_pruned = tool_result("c", "read", "[Output truncated - 7500 tokens]");
        assert!(!is_complete_read_result(
            tool_result_message(&age_pruned).unwrap()
        ));

        let hard_truncated = tool_result(
            "d",
            "read",
            "head\n[output truncated: original 90000 chars]",
        );
        assert!(!is_complete_read_result(
            tool_result_message(&hard_truncated).unwrap()
        ));

        // 完整读取的文件正文里**恰好**包含这些标记（例如本模块自己的源码）不能误判。
        let mentions_markers = tool_result(
            "e",
            "read",
            "docs:\n<persisted-output> is a wrapper\nthe preview says middle elided\n[output truncated: original 1 chars] appears in prose\nlast line",
        );
        assert!(
            is_complete_read_result(tool_result_message(&mentions_markers).unwrap()),
            "markers inside ordinary file content must not be treated as truncation"
        );
    }

    #[test]
    fn supersede_key_distinguishes_ranges() {
        let bare = serde_json::json!({"path": "src/lib.rs"});
        let range = serde_json::json!({"path": "src/lib.rs", "offset": 10, "limit": 20});
        assert_eq!(read_supersede_key("read", &bare).unwrap(), "src/lib.rs");
        assert_ne!(
            read_supersede_key("read", &range).unwrap(),
            read_supersede_key("read", &bare).unwrap()
        );
        assert!(read_supersede_key("read", &serde_json::json!({"path": "skill://x"})).is_none());
    }

    #[test]
    fn useless_classification() {
        assert_eq!(
            classify_useless("grep", "No matches found", None),
            Some(UselessKind::ZeroResultGrep)
        );
        assert_eq!(classify_useless("bash", "No matches found", None), None);
        assert_eq!(
            classify_useless("wait", "Waited 2s, process finished", None),
            Some(UselessKind::Wait)
        );
        assert_eq!(classify_useless("grep", "src/a.rs:1:match", None), None);
        let flagged = serde_json::json!({"useless": true});
        assert_eq!(
            classify_useless("anything", "x", Some(&flagged)),
            Some(UselessKind::Flagged)
        );
    }

    fn message_entry(id: &str, message: AgentMessage) -> SessionEntry {
        SessionEntry::Message(SessionMessageEntry {
            kind: kiana_session_core::session_manager::SessionMessageType::Marker,
            id: id.to_string(),
            parent_id: None,
            timestamp: String::new(),
            message,
            raw_message: None,
        })
    }

    fn tool_result(id: &str, tool: &str, text: &str) -> SessionEntry {
        message_entry(
            id,
            AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
                role: kiana_ai::types::ToolResultRole::ToolResult,
                tool_call_id: format!("call-{id}"),
                tool_name: tool.to_string(),
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: text.to_string(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 0,
                duration_ms: None,
            })),
        )
    }

    fn error_tool_result(id: &str, text: &str) -> SessionEntry {
        let mut entry = tool_result(id, "bash", text);
        if let SessionEntry::Message(message) = &mut entry
            && let AgentMessage::Standard(Message::ToolResult(result)) = &mut message.message
        {
            result.is_error = true;
        }
        entry
    }

    fn read_call_entry(id: &str, call_id: &str, path: &str) -> SessionEntry {
        message_entry(
            id,
            AgentMessage::Standard(Message::Assistant(kiana_ai::types::AssistantMessage {
                content: vec![AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: Default::default(),
                    id: call_id.to_string(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({ "path": path })
                        .as_object()
                        .map(|object| {
                            object
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                })],
                ..Default::default()
            })),
        )
    }

    #[test]
    fn small_superseded_and_useless_results_are_not_blanked() {
        // 小结果的占位符比原文更长：即使 protect_tokens / minimum_savings 都是 0，
        // superseded 与 useless 分支也必须保持原样。
        let useless = tool_result("u", "grep", "No matches found");
        let config = PruneConfig {
            protect_tokens: 0,
            minimum_savings: 0,
            ..PruneConfig::default()
        };
        let outcome = prune_tool_outputs(&[useless.clone()], &config);
        assert!(!outcome.applied);
        assert_eq!(outcome.entries, vec![useless.clone()]);

        let entries = vec![
            read_call_entry("a1", "call-a1", "src/lib.rs"),
            tool_result("r1", "read", "x"),
            read_call_entry("a2", "call-a2", "src/lib.rs"),
            tool_result("r2", "read", "x"),
        ];
        let outcome = prune_tool_outputs(&entries, &config);
        // 不能因为“后面有更新的读取”就把 <50 token 的结果换成占位符。
        assert!(!outcome.applied, "{outcome:?}");
        assert_eq!(outcome.entries, entries);
    }

    #[test]
    fn skill_tool_results_are_protected_from_age_pruning() {
        let big = "skill body ".repeat(10_000);
        let entries = vec![tool_result("s", "skill", &big)];
        let config = PruneConfig {
            protect_tokens: 0,
            minimum_savings: 0,
            ..PruneConfig::default()
        };
        assert!(
            PruneConfig::default()
                .protected_tools
                .iter()
                .any(|matcher| matcher == &ProtectedToolMatcher::ToolName("skill".to_string()))
        );
        let outcome = prune_tool_outputs(&entries, &config);
        assert!(!outcome.applied, "skill result must never be pruned");
        assert_eq!(outcome.entries, entries);
    }
}
