//! 会话恢复：先分类，再修复。
//!
//! 一次会话可能在任何一步被杀掉。恢复的第一步必须是**只读分类**，而不是先改
//! transcript：
//!
//! - [`RecoveryClass::Clean`]：最后一条消息正常结束，不需要任何修复；
//! - [`RecoveryClass::InterruptedPrompt`]：尾部是 user 消息（prompt 已发出、模型
//!   回复从未落盘）。恢复时把这条 user **重放一次**，不新增 user entry；
//! - [`RecoveryClass::InterruptedTurn`]：尾部 assistant 的 tool_call 没有对应
//!   tool_result（在 tool_calls 与 tool_result 之间被杀）。恢复时为每个悬挂调用
//!   合成一个 failed tool result，**绝不重新执行**任何工具（尤其是 mutating 工具），
//!   并在 canonical transcript 里留下 turn 标记；
//! - [`RecoveryClass::DegradedHistory`]：历史里有孤儿 tool result（没有对应
//!   tool_call）或非尾部悬挂调用。只修**请求视图**，canonical transcript 保持原样。
//!
//! 分类顺序与参考实现一致：先看尾部 user（`InterruptedPrompt`），再看尾部
//! assistant/其 tool_result 的悬挂调用（`InterruptedTurn`），最后才把历史异常归为
//! `DegradedHistory`。分类函数只读，[`RecoveryPlan::apply`] 才会写 canonical
//! transcript，且只对 `InterruptedTurn` 写入合成结果与标记。
//!
//! 合成结果的两个稳定性约束：
//!
//! 1. `tool_call_id` 原样复用被中断调用的 id（provider 靠它配对；重试恢复时不会
//!    生成新的配对）；
//! 2. 错误文本有长度上限（[`DEFAULT_RECOVERY_ERROR_MAX_CHARS`]），截断按字符而不是
//!    字节，避免在多字节边界切开。
//!
//! 参考设计：
//! - `reference_project/qwen-code/packages/core/src/core/turn-interruption.ts`
//!   的 `detectTurnInterruption` / `buildSyntheticToolResponseParts`；
//! - `reference_project/qwen-code/packages/core/src/core/session-recovery.ts`
//!   的 `buildSessionRecoveryPlanFromApiHistory`（分类先于修复、repair 与 continuation
//!   分离）；
//! - `reference_project/ClaudeCode/src/utils/conversationRecovery.ts` 的
//!   `deserializeMessagesWithInterruptDetection`（尾部 user 重放与 interrupted turn）。

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, Message, TextContent, TextKind, ToolResultMessage, ToolResultRole,
    UserContentBlock,
};
use serde::{Deserialize, Serialize};

use crate::session_manager::{SessionEntry, SessionManager, SessionManagerError};

/// 合成错误文本的默认长度上限（字符数）。
pub const DEFAULT_RECOVERY_ERROR_MAX_CHARS: usize = 512;

/// `InterruptedTurn` 写入 canonical transcript 时使用的自定义条目类型。
///
/// `custom` 条目不进入 LLM 上下文，只用于 UI/诊断与“这个 turn 被恢复过”的审计。
pub const INTERRUPTED_TURN_CUSTOM_TYPE: &str = "kiana.recovery.interrupted_turn";

/// 会话恢复分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryClass {
    /// 干净结束。
    Clean,
    /// 尾部 user prompt 没有模型回复。
    InterruptedPrompt,
    /// 尾部 assistant 的 tool_call 没有 tool_result。
    InterruptedTurn,
    /// 历史结构损坏（孤儿结果 / 非尾部悬挂调用）。
    DegradedHistory,
}

impl RecoveryClass {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::InterruptedPrompt => "interrupted_prompt",
            Self::InterruptedTurn => "interrupted_turn",
            Self::DegradedHistory => "degraded_history",
        }
    }
}

impl std::fmt::Display for RecoveryClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一个被记录但没有结果的工具调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRef {
    /// provider 侧的调用 id。
    pub call_id: String,
    /// 工具名。
    pub tool_name: String,
}

/// 历史结构问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedIssueKind {
    /// tool_result 没有对应的 tool_call。
    OrphanToolResult,
    /// 同一 tool_call 的重复 tool_result。
    DuplicateToolResult,
    /// 非尾部位置的 tool_call 没有结果。
    DanglingToolCall,
}

/// 一条历史结构问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DegradedIssue {
    /// 问题类型。
    pub kind: DegradedIssueKind,
    /// 出问题的条目 id（tool_call 所在 assistant 或 tool_result 自身）。
    pub entry_id: String,
    /// 相关调用 id。
    pub tool_call_id: String,
    /// 工具名（tool_result 没有工具名时取 `None`）。
    pub tool_name: Option<String>,
}

/// `interrupted_turn` 的细节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedTurn {
    /// 含悬挂调用的 assistant 条目 id。
    pub assistant_entry_id: String,
    /// 悬挂调用（按 assistant 内容块顺序）。
    pub dangling_calls: Vec<ToolCallRef>,
}

/// `interrupted_prompt` 的细节：尾部 user 消息。
#[derive(Debug, Clone, PartialEq)]
pub struct InterruptedPrompt {
    /// 尾部 user 的条目 id。
    pub entry_id: String,
    /// 原消息（重放时原样提交一次）。
    pub message: AgentMessage,
}

/// 只读分类结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryClassification {
    /// 主分类。
    pub class: RecoveryClass,
    /// `InterruptedTurn` 细节；其它分类为 `None`。
    pub interrupted_turn: Option<InterruptedTurn>,
    /// `InterruptedPrompt` 细节；其它分类为 `None`。
    pub interrupted_prompt: Option<InterruptedPrompt>,
    /// 同时检测到的历史结构问题（即使主分类不是 `DegradedHistory` 也可能非空）。
    pub degraded: Vec<DegradedIssue>,
    /// 最后一条 message 条目的 id（跳过 custom / label 等非消息条目）。
    pub last_message_entry_id: Option<String>,
    /// 最后一条 message 条目的角色（标准消息角色或自定义 role）。
    pub last_message_role: Option<String>,
}

impl RecoveryClassification {
    /// 是否需要在请求视图里做任何修复。
    pub fn needs_request_view_repair(&self) -> bool {
        !self.degraded.is_empty()
            || self
                .interrupted_turn
                .as_ref()
                .is_some_and(|turn| !turn.dangling_calls.is_empty())
    }

    /// 分类是否允许自动继续（调用方仍应遵守自己的确认策略）。
    pub fn can_continue(&self) -> bool {
        matches!(
            self.class,
            RecoveryClass::InterruptedPrompt | RecoveryClass::InterruptedTurn
        )
    }
}

/// 一类 canonical transcript 修复（只会由 `InterruptedTurn` 产生）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CanonicalRepair {
    /// 为悬挂调用追加一个 failed tool result。
    AppendSyntheticToolResult {
        /// 原调用 id。
        tool_call_id: String,
        /// 原工具名。
        tool_name: String,
    },
    /// 追加一个“turn 被恢复”的 custom 条目（不进入 LLM 上下文）。
    AppendInterruptedTurnMarker {
        /// 被恢复的 assistant 条目 id。
        assistant_entry_id: String,
        /// 被合成的调用 id。
        call_ids: Vec<String>,
    },
}

/// 一类请求视图修复（不改 canonical transcript）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestViewRepair {
    /// 从请求视图移除孤儿 / 重复 tool result。
    RemoveOrphanToolResult {
        /// tool result 所属条目 id。
        entry_id: String,
        /// 调用 id。
        tool_call_id: String,
    },
    /// 在指定 assistant 消息之后插入合成的 failed tool result。
    InsertSyntheticToolResult {
        /// assistant 条目 id。
        after_entry_id: String,
        /// 调用 id。
        tool_call_id: String,
        /// 工具名。
        tool_name: String,
    },
}

/// 恢复后的继续动作。
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryContinuation {
    /// `InterruptedPrompt`：把尾部 user 原样重放一次。
    ReplayUser {
        /// 尾部 user 的条目 id；调用方用它做去重（同一个 id 只允许重放一次）。
        entry_id: String,
        /// 原消息。
        message: AgentMessage,
    },
    /// `InterruptedTurn`：把合成的 failed tool result 作为下一轮输入。
    SyntheticToolResults {
        /// 合成结果（顺序与悬挂调用一致）。
        messages: Vec<AgentMessage>,
    },
}

/// 完整的恢复计划。
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryPlan {
    /// 只读分类结果。
    pub classification: RecoveryClassification,
    /// canonical transcript 修复。
    pub canonical_repairs: Vec<CanonicalRepair>,
    /// 请求视图修复。
    pub request_view_repairs: Vec<RequestViewRepair>,
}

impl RecoveryPlan {
    /// 恢复后的继续动作；`Clean` / `DegradedHistory` 返回 `None`。
    pub fn continuation(&self) -> Option<RecoveryContinuation> {
        match self.classification.class {
            RecoveryClass::InterruptedPrompt => {
                let prompt = self.classification.interrupted_prompt.as_ref()?;
                Some(RecoveryContinuation::ReplayUser {
                    entry_id: prompt.entry_id.clone(),
                    message: prompt.message.clone(),
                })
            }
            RecoveryClass::InterruptedTurn => {
                let turn = self.classification.interrupted_turn.as_ref()?;
                let messages = turn
                    .dangling_calls
                    .iter()
                    .map(|call| {
                        synthetic_failed_tool_result(call, DEFAULT_RECOVERY_ERROR_MAX_CHARS)
                    })
                    .collect();
                Some(RecoveryContinuation::SyntheticToolResults { messages })
            }
            RecoveryClass::Clean | RecoveryClass::DegradedHistory => None,
        }
    }

    /// 应用 canonical 修复。返回实际写入的条目。
    ///
    /// 只写 `InterruptedTurn` 的合成结果与标记；`InterruptedPrompt` /
    /// `DegradedHistory` / `Clean` 不写 canonical transcript。函数是幂等的：如果
    /// 已有同 id 的 tool result（例如上一次恢复已写入），不会重复追加。
    pub fn apply(
        &self,
        manager: &mut SessionManager,
    ) -> Result<AppliedRecovery, SessionManagerError> {
        let existing_results = existing_tool_result_ids(manager.get_branch(None).iter());
        let existing_markers = existing_interrupted_turn_markers(manager.get_branch(None).iter());
        let mut applied = AppliedRecovery {
            class: self.classification.class,
            appended_entry_ids: Vec::new(),
            appended_tool_results: Vec::new(),
            reused_existing_results: Vec::new(),
        };

        for repair in &self.canonical_repairs {
            match repair {
                CanonicalRepair::AppendSyntheticToolResult {
                    tool_call_id,
                    tool_name,
                } => {
                    if existing_results.contains(tool_call_id) {
                        applied.reused_existing_results.push(tool_call_id.clone());
                        continue;
                    }
                    let call = ToolCallRef {
                        call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                    };
                    let message =
                        synthetic_failed_tool_result(&call, DEFAULT_RECOVERY_ERROR_MAX_CHARS);
                    let entry_id = manager.append_message(message)?;
                    applied.appended_entry_ids.push(entry_id);
                    applied.appended_tool_results.push(tool_call_id.clone());
                }
                CanonicalRepair::AppendInterruptedTurnMarker {
                    assistant_entry_id,
                    call_ids,
                } => {
                    if call_ids.is_empty() {
                        continue;
                    }
                    // 同一次恢复可能被重试：已有相同 assistant/call 集合的标记时
                    // 不再追加，保证 canonical transcript 不因重试而膨胀。
                    let marker_key = interrupted_turn_marker_key(assistant_entry_id, call_ids);
                    if existing_markers.contains(&marker_key) {
                        continue;
                    }
                    let data = serde_json::json!({
                        "assistantEntryId": assistant_entry_id,
                        "callIds": call_ids,
                        "synthesized": call_ids.len(),
                    });
                    let entry_id =
                        manager.append_custom_entry(INTERRUPTED_TURN_CUSTOM_TYPE, Some(data))?;
                    applied.appended_entry_ids.push(entry_id);
                }
            }
        }
        Ok(applied)
    }
}

/// `apply` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedRecovery {
    /// 分类。
    pub class: RecoveryClass,
    /// 新写入的条目 id（按写入顺序）。
    pub appended_entry_ids: Vec<String>,
    /// 实际合成的 tool_call id。
    pub appended_tool_results: Vec<String>,
    /// 已存在结果、未重复追加的调用 id。
    pub reused_existing_results: Vec<String>,
}

/// 只做分类（不计算修复、不修改任何内容）。
pub fn classify_recovery(entries: &[SessionEntry]) -> RecoveryClassification {
    let messages: Vec<MessageRef<'_>> = entries.iter().filter_map(MessageRef::from_entry).collect();
    let issues = detect_structural_issues(&messages);

    let Some(last) = messages.last() else {
        return RecoveryClassification {
            class: RecoveryClass::Clean,
            interrupted_turn: None,
            interrupted_prompt: None,
            degraded: issues,
            last_message_entry_id: None,
            last_message_role: None,
        };
    };

    let last_role = last.message.role().to_string();
    let mut classification = RecoveryClassification {
        class: RecoveryClass::Clean,
        interrupted_turn: None,
        interrupted_prompt: None,
        degraded: issues,
        last_message_entry_id: Some(last.entry_id.clone()),
        last_message_role: Some(last_role.clone()),
    };

    if last_role == "user" {
        // 尾部 user：prompt 已入 transcript，但没有模型回复。
        classification.class = RecoveryClass::InterruptedPrompt;
        classification.interrupted_prompt = Some(InterruptedPrompt {
            entry_id: last.entry_id.clone(),
            message: last.message.clone(),
        });
        return classification;
    }

    // 找最后一条 assistant，判断它是否仍在“未完成 turn”的尾部：assistant 之后
    // 没有 user（只有 toolResult / 非消息条目）。
    let last_assistant = messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| message.message.role() == "assistant");
    if let Some((assistant_index, assistant)) = last_assistant {
        let call_ids = tool_call_ids(assistant.message);
        if !call_ids.is_empty() {
            let answered: HashSet<String> = messages[assistant_index + 1..]
                .iter()
                .filter_map(|message| tool_result_id(message.message))
                .map(str::to_string)
                .collect();
            let dangling: Vec<ToolCallRef> = call_ids
                .iter()
                .filter(|call| !answered.contains(&call.call_id))
                .cloned()
                .collect();
            let has_user_after = messages[assistant_index + 1..]
                .iter()
                .any(|message| message.message.role() == "user");
            if !dangling.is_empty() && !has_user_after {
                // 尾部的悬挂调用由 InterruptedTurn 处理，不再重复记为 degraded。
                let dangling_ids: HashSet<&str> =
                    dangling.iter().map(|call| call.call_id.as_str()).collect();
                classification.degraded.retain(|issue| {
                    !(issue.kind == DegradedIssueKind::DanglingToolCall
                        && issue.entry_id == assistant.entry_id
                        && dangling_ids.contains(issue.tool_call_id.as_str()))
                });
                classification.class = RecoveryClass::InterruptedTurn;
                classification.interrupted_turn = Some(InterruptedTurn {
                    assistant_entry_id: assistant.entry_id.clone(),
                    dangling_calls: dangling,
                });
                return classification;
            }
        }
    }

    if !classification.degraded.is_empty() {
        classification.class = RecoveryClass::DegradedHistory;
    }
    classification
}

/// 在分类之后计算修复。
///
/// 分类与修复是分开的：调用方可以先看 [`RecoveryPlan::classification`] 决定是否
/// 恢复，修复列表不会在分类阶段产生任何副作用。
pub fn plan_recovery(entries: &[SessionEntry]) -> RecoveryPlan {
    let classification = classify_recovery(entries);
    let mut canonical_repairs = Vec::new();
    let mut request_view_repairs = Vec::new();

    for issue in &classification.degraded {
        if issue.kind == DegradedIssueKind::OrphanToolResult
            || issue.kind == DegradedIssueKind::DuplicateToolResult
        {
            request_view_repairs.push(RequestViewRepair::RemoveOrphanToolResult {
                entry_id: issue.entry_id.clone(),
                tool_call_id: issue.tool_call_id.clone(),
            });
        }
    }

    if let Some(turn) = &classification.interrupted_turn {
        for call in &turn.dangling_calls {
            canonical_repairs.push(CanonicalRepair::AppendSyntheticToolResult {
                tool_call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
            });
            request_view_repairs.push(RequestViewRepair::InsertSyntheticToolResult {
                after_entry_id: turn.assistant_entry_id.clone(),
                tool_call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
            });
        }
        canonical_repairs.push(CanonicalRepair::AppendInterruptedTurnMarker {
            assistant_entry_id: turn.assistant_entry_id.clone(),
            call_ids: turn
                .dangling_calls
                .iter()
                .map(|call| call.call_id.clone())
                .collect(),
        });
    }

    // 非尾部的悬挂调用：只在请求视图里补，canonical 保持不变。
    for issue in &classification.degraded {
        if issue.kind != DegradedIssueKind::DanglingToolCall {
            continue;
        }
        request_view_repairs.push(RequestViewRepair::InsertSyntheticToolResult {
            after_entry_id: issue.entry_id.clone(),
            tool_call_id: issue.tool_call_id.clone(),
            tool_name: issue.tool_name.clone().unwrap_or_default(),
        });
    }

    RecoveryPlan {
        classification,
        canonical_repairs,
        request_view_repairs,
    }
}

/// 按需构造合成结果（`tool_call_id` 稳定、错误文本限长）。
pub fn synthetic_failed_tool_result(call: &ToolCallRef, max_error_chars: usize) -> AgentMessage {
    let error = truncate_chars(
        "Tool execution was interrupted before a result was recorded; \
         the call was not retried to avoid repeating a mutating operation.",
        max_error_chars,
    );
    AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: call.call_id.clone(),
        tool_name: call.tool_name.clone(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: error,
            text_signature: None,
        })],
        details: Some(serde_json::json!({
            "kianaRecovery": {
                "synthesized": true,
                "reason": "interrupted_turn",
            }
        })),
        usage: None,
        nested_calls: None,
        is_error: true,
        timestamp: now_millis(),
        duration_ms: None,
    }))
}

/// 修复后的请求视图。
#[derive(Debug, Clone, PartialEq)]
pub struct RepairedRequestView {
    /// 修复后的消息列表（可直接发给 provider）。
    pub messages: Vec<AgentMessage>,
    /// 被移除的孤儿 / 重复 tool result 的调用 id。
    pub removed_orphan_results: Vec<String>,
    /// 被插入的合成结果调用 id。
    pub synthesized_tool_results: Vec<String>,
}

/// 只修请求视图：移除孤儿/重复 tool result，为悬挂调用补 failed tool result。
///
/// `messages` 通常来自 `SessionManager::build_session_projection().messages`（已经
/// 过压缩/context edit 处理）。函数不读取也不修改 canonical transcript；孤儿结果
/// 只是在这里被跳过，会话文件仍然保留它们。
pub fn repair_request_view(messages: &[AgentMessage], plan: &RecoveryPlan) -> RepairedRequestView {
    let synthesize: HashMap<String, String> = plan
        .request_view_repairs
        .iter()
        .filter_map(|repair| match repair {
            RequestViewRepair::InsertSyntheticToolResult {
                tool_call_id,
                tool_name,
                ..
            } => Some((tool_call_id.clone(), tool_name.clone())),
            _ => None,
        })
        .collect();
    let remove: HashSet<String> = plan
        .request_view_repairs
        .iter()
        .filter_map(|repair| match repair {
            RequestViewRepair::RemoveOrphanToolResult { tool_call_id, .. } => {
                Some(tool_call_id.clone())
            }
            _ => None,
        })
        .collect();

    // “已应答”只统计会保留在请求视图里的结果（被移除的孤儿/重复不参与配对）。
    let mut answered: HashSet<String> = messages
        .iter()
        .filter_map(|message| tool_result_id(message))
        .filter(|result_id| !remove.contains(*result_id))
        .map(str::to_string)
        .collect();
    let mut seen_results: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(messages.len());
    let mut removed_orphan_results = Vec::new();
    let mut synthesized_tool_results = Vec::new();

    for message in messages {
        if let Some(result_id) = tool_result_id(message) {
            let duplicate = !seen_results.insert(result_id.to_string());
            if remove.contains(result_id) || duplicate {
                removed_orphan_results.push(result_id.to_string());
                continue;
            }
        }
        out.push(message.clone());
        if message.role() != "assistant" {
            continue;
        }
        for call_id in tool_call_ids(message) {
            if answered.contains(&call_id.call_id) {
                continue;
            }
            let Some(tool_name) = synthesize.get(&call_id.call_id) else {
                continue;
            };
            out.push(synthetic_failed_tool_result(
                &ToolCallRef {
                    call_id: call_id.call_id.clone(),
                    tool_name: tool_name.clone(),
                },
                DEFAULT_RECOVERY_ERROR_MAX_CHARS,
            ));
            answered.insert(call_id.call_id.clone());
            synthesized_tool_results.push(call_id.call_id);
        }
    }

    RepairedRequestView {
        messages: out,
        removed_orphan_results,
        synthesized_tool_results,
    }
}

/// `SessionManager::classify_recovery` 的返回：分类 + 分支条目。
pub fn classify_manager_recovery(manager: &SessionManager) -> RecoveryClassification {
    classify_recovery(&manager.get_branch(None))
}

/// `SessionManager::recovery_plan` 的返回：分类 + 修复。
pub fn plan_manager_recovery(manager: &SessionManager) -> RecoveryPlan {
    plan_recovery(&manager.get_branch(None))
}

// ---------------------------------------------------------------------------
// 内部
// ---------------------------------------------------------------------------

struct MessageRef<'a> {
    entry_id: String,
    message: &'a AgentMessage,
}

impl<'a> MessageRef<'a> {
    fn from_entry(entry: &'a SessionEntry) -> Option<Self> {
        match entry {
            SessionEntry::Message(message_entry) => Some(Self {
                entry_id: message_entry.id.clone(),
                message: &message_entry.message,
            }),
            _ => None,
        }
    }
}

fn standard_message(message: &AgentMessage) -> Option<&Message> {
    match message {
        AgentMessage::Standard(message) => Some(message),
        AgentMessage::Custom(_) => None,
    }
}

fn tool_call_ids(message: &AgentMessage) -> Vec<ToolCallRef> {
    let Some(Message::Assistant(assistant)) = standard_message(message) else {
        return Vec::new();
    };
    assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(ToolCallRef {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn tool_result_id(message: &AgentMessage) -> Option<&str> {
    match standard_message(message) {
        Some(Message::ToolResult(result)) => Some(result.tool_call_id.as_str()),
        _ => None,
    }
}

fn existing_tool_result_ids<'a>(
    entries: impl Iterator<Item = &'a SessionEntry>,
) -> HashSet<String> {
    entries
        .filter_map(|entry| entry.message())
        .filter_map(tool_result_id)
        .map(str::to_string)
        .collect()
}

fn interrupted_turn_marker_key(assistant_entry_id: &str, call_ids: &[String]) -> String {
    let mut key = String::with_capacity(assistant_entry_id.len() + call_ids.len() * 16);
    key.push_str(assistant_entry_id);
    for call_id in call_ids {
        key.push('\u{1f}');
        key.push_str(call_id);
    }
    key
}

fn existing_interrupted_turn_markers<'a>(
    entries: impl Iterator<Item = &'a SessionEntry>,
) -> HashSet<String> {
    entries
        .filter_map(|entry| match entry {
            SessionEntry::Custom(custom) if custom.custom_type == INTERRUPTED_TURN_CUSTOM_TYPE => {
                Some(custom)
            }
            _ => None,
        })
        .filter_map(|custom| {
            let data = custom.data.as_ref()?;
            let assistant_entry_id = data.get("assistantEntryId")?.as_str()?;
            let call_ids: Vec<String> = data
                .get("callIds")?
                .as_array()?
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            Some(interrupted_turn_marker_key(assistant_entry_id, &call_ids))
        })
        .collect()
}

fn detect_structural_issues(messages: &[MessageRef<'_>]) -> Vec<DegradedIssue> {
    // 先收集全部分 tool_call，再配对 tool_result：结果出现在调用之前（乱序历史）仍然
    // 算“已应答”，避免把正常配对误判成孤儿 + 悬挂。
    let mut calls: HashMap<String, (String, String)> = HashMap::new(); // id -> (name, entry id)
    for message in messages {
        if let Some(Message::Assistant(assistant)) = standard_message(message.message) {
            for block in &assistant.content {
                let AssistantContentBlock::ToolCall(call) = block else {
                    continue;
                };
                calls
                    .entry(call.id.clone())
                    .or_insert_with(|| (call.name.clone(), message.entry_id.clone()));
            }
        }
    }

    let mut result_counts: HashMap<String, usize> = HashMap::new();
    let mut issues = Vec::new();
    // 按出现顺序标记孤儿与重复；第一次出现保留。
    for message in messages {
        let Some(Message::ToolResult(result)) = standard_message(message.message) else {
            continue;
        };
        if !calls.contains_key(&result.tool_call_id) {
            issues.push(DegradedIssue {
                kind: DegradedIssueKind::OrphanToolResult,
                entry_id: message.entry_id.clone(),
                tool_call_id: result.tool_call_id.clone(),
                tool_name: Some(result.tool_name.clone()),
            });
            continue;
        }
        let count = result_counts
            .entry(result.tool_call_id.clone())
            .or_insert(0);
        *count += 1;
        if *count > 1 {
            issues.push(DegradedIssue {
                kind: DegradedIssueKind::DuplicateToolResult,
                entry_id: message.entry_id.clone(),
                tool_call_id: result.tool_call_id.clone(),
                tool_name: Some(result.tool_name.clone()),
            });
        }
    }

    for (call_id, (tool_name, entry_id)) in calls {
        if !result_counts.contains_key(&call_id) {
            issues.push(DegradedIssue {
                kind: DegradedIssueKind::DanglingToolCall,
                entry_id,
                tool_call_id: call_id,
                tool_name: Some(tool_name),
            });
        }
    }
    issues
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
