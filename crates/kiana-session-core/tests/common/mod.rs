//! 测试共用助手。
//!
//! 与 TS 的 `test/utilities.ts` 对应（只含本 crate 用得到的部分）。每个集成测试二进制都会
//! 编译本模块，未用到的助手在每个二进制里都可能触发 `dead_code`；这里整体允许并注明原因。
#![allow(dead_code)]

use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, SystemContent,
    SystemMessage, SystemRole, TextContent, TextKind, ToolResultMessage, ToolResultRole, Usage,
    UsageCost, UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_session_core::session_manager::{
    BranchSummaryEntry, BranchSummaryType, CompactionEntry, CompactionType, CustomEntry,
    CustomEntryType, FileEntry, LabelEntry, LabelType, ModelChangeEntry, ModelChangeType,
    SessionEntry, SessionHeader, SessionInfoEntry, SessionInfoType, SessionMessageEntry,
    SessionMessageType, SessionTypeMarker, ThinkingLevelChangeEntry, ThinkingLevelChangeType,
};

/// 零成本（与 TS 测试里的 `zeroCost` 相同）。
pub fn zero_cost() -> UsageCost {
    UsageCost::default()
}

/// `usage(input, output, totalTokens, cost)`。
pub fn usage_with(input: u64, output: u64, total_tokens: u64, cost: f64) -> Usage {
    Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost {
            total: cost,
            ..Default::default()
        },
    }
}

/// `userMsg(text)`（`test/utilities.ts`）：字符串 content。
pub fn user_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp,
    }))
}

/// 块状 user 消息。
pub fn user_blocks_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::Text(text_block(text))]),
        timestamp,
    }))
}

/// `assistantMsg(text)`（`test/utilities.ts`）：usage 1/1/2，成本 0。
pub fn assistant_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(text_block(text))],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "test".to_string(),
        usage: usage_with(1, 1, 2, 0.0),
        stop_reason: StopReason::Stop,
        timestamp,
        ..Default::default()
    }))
}

/// 带完整字段的 assistant 消息（build-context 测试里的 `msg()`）。
pub fn assistant_message_full(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(text_block(text))],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-test".to_string(),
        usage: Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 2,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        timestamp,
        ..Default::default()
    }))
}

/// toolResult 消息。
pub fn tool_result_message(
    tool_call_id: &str,
    tool_name: &str,
    text: &str,
    timestamp: i64,
) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        content: vec![UserContentBlock::Text(text_block(text))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    }))
}

/// system 消息。
pub fn system_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(text.to_string()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp,
    }))
}

/// `TextContent`。
pub fn text_block(text: &str) -> TextContent {
    TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    }
}

/// 固定 timestamp 的 `message` 条目。
pub fn message_entry(
    id: &str,
    parent_id: Option<&str>,
    timestamp: &str,
    message: AgentMessage,
) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: timestamp.to_string(),
        message,
        raw_message: None,
    })
}

/// `compaction` 条目。
pub fn compaction_entry(
    id: &str,
    parent_id: Option<&str>,
    summary: &str,
    first_kept_entry_id: &str,
) -> SessionEntry {
    compaction_entry_with(id, parent_id, summary, first_kept_entry_id, 1000.0)
}

/// 带 `tokensBefore` 的 `compaction` 条目。
pub fn compaction_entry_with(
    id: &str,
    parent_id: Option<&str>,
    summary: &str,
    first_kept_entry_id: &str,
    tokens_before: f64,
) -> SessionEntry {
    SessionEntry::Compaction(CompactionEntry {
        kind: CompactionType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        summary: summary.to_string(),
        first_kept_entry_id: first_kept_entry_id.to_string(),
        tokens_before,
        details: None,
        usage: None,
        from_hook: None,
        system_message: None,
        first_kept_entry_index: None,
        raw_entry: None,
    })
}

/// `branch_summary` 条目。
pub fn branch_summary_entry(
    id: &str,
    parent_id: Option<&str>,
    summary: &str,
    from_id: &str,
) -> SessionEntry {
    SessionEntry::BranchSummary(BranchSummaryEntry {
        kind: BranchSummaryType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        from_id: from_id.to_string(),
        summary: summary.to_string(),
        details: None,
        usage: None,
        from_hook: None,
        raw_entry: None,
    })
}

/// `custom` 条目。
pub fn custom_entry(
    id: &str,
    parent_id: Option<&str>,
    custom_type: &str,
    data: Option<serde_json::Value>,
) -> SessionEntry {
    SessionEntry::Custom(CustomEntry {
        kind: CustomEntryType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        custom_type: custom_type.to_string(),
        data,
    })
}

/// `thinking_level_change` 条目。
pub fn thinking_level_entry(id: &str, parent_id: Option<&str>, level: &str) -> SessionEntry {
    SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
        kind: ThinkingLevelChangeType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        thinking_level: level.to_string(),
    })
}

/// `model_change` 条目。
pub fn model_change_entry(
    id: &str,
    parent_id: Option<&str>,
    provider: &str,
    model_id: &str,
) -> SessionEntry {
    SessionEntry::ModelChange(ModelChangeEntry {
        kind: ModelChangeType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        provider: provider.to_string(),
        model_id: model_id.to_string(),
    })
}

/// `label` 条目。
pub fn label_entry(
    id: &str,
    parent_id: Option<&str>,
    target_id: &str,
    label: Option<&str>,
) -> SessionEntry {
    SessionEntry::Label(LabelEntry {
        kind: LabelType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        target_id: target_id.to_string(),
        label: label.map(str::to_string),
    })
}

/// `session_info` 条目。
pub fn session_info_entry(id: &str, parent_id: Option<&str>, name: Option<&str>) -> SessionEntry {
    SessionEntry::SessionInfo(SessionInfoEntry {
        kind: SessionInfoType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        name: name.map(str::to_string),
    })
}

/// 会话文件头（v3）。
pub fn session_header(id: &str, cwd: Option<&str>, parent_session: Option<&str>) -> SessionHeader {
    SessionHeader {
        kind: SessionTypeMarker::Marker,
        version: Some(3.0),
        id: id.to_string(),
        timestamp: Some("2025-01-01T00:00:00Z".to_string()),
        cwd: cwd.map(str::to_string),
        parent_session: parent_session.map(str::to_string),
    }
}

/// 预置条目列表（header 在前）。
pub fn file_entries(header_id: &str, cwd: &str, entries: Vec<SessionEntry>) -> Vec<FileEntry> {
    let mut file_entries = vec![FileEntry::Header(session_header(
        header_id,
        Some(cwd),
        None,
    ))];
    file_entries.extend(entries.into_iter().map(FileEntry::Entry));
    file_entries
}

/// 读出会话文件的逐行角色（`test/utilities.ts#readSessionFileRoles`）。
pub fn read_session_file_roles(path: &str) -> Vec<String> {
    let content = std::fs::read_to_string(path).expect("read session file");
    content
        .trim()
        .split('\n')
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("json line");
            value
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    value
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default()
        })
        .collect()
}

/// `AgentMessage` → `serde_json::Value`（JS 语义键序/数字格式）。
pub fn message_json(message: &AgentMessage) -> serde_json::Value {
    serde_json::to_value(message).expect("message to json")
}

/// `SessionEntry` → `serde_json::Value`。
pub fn entry_json(entry: &SessionEntry) -> serde_json::Value {
    serde_json::to_value(entry).expect("entry to json")
}

/// 创建一个唯一的临时目录（测试结束后由调用方清理）。
pub fn temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("create temp dir")
}

/// 取错误文本；`Ok` 视为测试失败。
pub fn error_text<T>(
    result: Result<T, kiana_session_core::session_manager::SessionManagerError>,
) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.to_string(),
    }
}

/// 当前工作目录（等价 TS `process.cwd()`）。
pub fn current_cwd() -> String {
    std::env::current_dir()
        .expect("current dir")
        .to_string_lossy()
        .into_owned()
}

/// `/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/`
/// （与 `custom-session-id.test.ts` 的 `UUID_V7_RE` 等价）。
pub fn is_uuid_v7(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            14 => {
                if *byte != b'7' {
                    return false;
                }
            }
            19 => {
                if !matches!(*byte, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase() {
                    return false;
                }
            }
        }
    }
    true
}

/// 会话文件名形如 `YYYY-MM-DDTHH-MM-SS-mmmZ_<id>.jsonl`（TS 测试里的正则）。
pub fn is_session_file_name(file: &str, id: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let suffix = format!("_{id}.jsonl");
    let Some(prefix) = name.strip_suffix(&suffix) else {
        return false;
    };
    let bytes = prefix.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let expected = match index {
            4 | 7 | 13 | 16 | 19 => b'-',
            10 => b'T',
            23 => b'Z',
            _ => {
                if !byte.is_ascii_digit() {
                    return false;
                }
                continue;
            }
        };
        if *byte != expected {
            return false;
        }
    }
    true
}
