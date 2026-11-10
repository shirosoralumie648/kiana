//! `compaction.test.ts` 的 Rust 移植。
//!
//! TS: `packages/coding-agent/test/compaction.test.ts`

use indexmap::IndexMap;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonValue, Message, StopReason,
    SystemContent, SystemMessage, SystemRole, TextContent, TextKind, ToolCall, ToolCallKind, Usage,
    UserContent, UserMessage, UserRole,
};
use kiana_compaction::{
    CompactionSettings, CutPointResult, DEFAULT_COMPACTION_SETTINGS, calculate_context_tokens,
    compact, compute_file_lists, estimate_context_tokens, estimate_projected_context_tokens,
    find_cut_point, format_file_operations, get_last_assistant_usage, prepare_compaction,
    should_compact,
};
use kiana_session_core::session_manager::{
    CompactionEntry, CompactionType, ContextEditEntry, ContextEditReplacement, ContextEditType,
    CustomMessageEntry, CustomMessageType, FileEntry, LabelEntry, LabelType, LeafId,
    ModelChangeEntry, ModelChangeType, ModelRef, SessionEntry, SessionMessageEntry,
    SessionMessageType, ThinkingLevelChangeEntry, ThinkingLevelChangeType, build_session_context,
    build_session_projection, migrate_session_entries, parse_session_entries,
};

// ============================================================================
// Test fixtures
// ============================================================================

fn load_large_session_entries() -> Vec<SessionEntry> {
    let content = include_str!("fixtures/large-session.jsonl");
    let mut entries = parse_session_entries(content);
    migrate_session_entries(&mut entries); // Add id/parentId for v1 fixtures
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            // TS: `entries.filter(e => e.type !== "session")`
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
            FileEntry::Entry(entry) => Some(entry),
        })
        .collect()
}

fn create_mock_usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        ..Usage::default()
    }
}

/// 测试用的固定时间戳（TS 用 `Date.now()`；这里断言不依赖墙钟）。
fn test_timestamp() -> i64 {
    1_767_225_600_000
}

fn test_iso_timestamp() -> String {
    "2026-01-01T00:00:00.000Z".to_string()
}

fn create_user_message(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: test_timestamp(),
    }))
}

fn create_assistant_message(text: &str, usage: Option<Usage>) -> AgentMessage {
    assistant_message_with_content(
        vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        usage,
        StopReason::Stop,
    )
}

fn assistant_message_with_content(
    content: Vec<AssistantContentBlock>,
    usage: Option<Usage>,
    stop_reason: StopReason,
) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        usage: usage.unwrap_or_else(|| create_mock_usage(100, 50, 0, 0)),
        stop_reason,
        timestamp: test_timestamp(),
        ..AssistantMessage::default()
    }))
}

/// 条目构造器：递增的 `test-id-N` 与线性 parent 链（对应 TS 的模块级计数器）。
struct EntryFactory {
    counter: usize,
    last_id: Option<String>,
}

impl EntryFactory {
    fn new() -> Self {
        EntryFactory {
            counter: 0,
            last_id: None,
        }
    }

    fn next_id(&mut self) -> String {
        let id = format!("test-id-{}", self.counter);
        self.counter += 1;
        id
    }

    fn message(&mut self, message: AgentMessage) -> SessionEntry {
        let id = self.next_id();
        let entry = SessionEntry::Message(SessionMessageEntry {
            kind: SessionMessageType::Marker,
            id: id.clone(),
            parent_id: self.last_id.clone(),
            timestamp: test_iso_timestamp(),
            message,
            raw_message: None,
        });
        self.last_id = Some(id);
        entry
    }

    fn compaction(&mut self, summary: &str, first_kept_entry_id: &str) -> SessionEntry {
        let id = self.next_id();
        let entry = SessionEntry::Compaction(CompactionEntry {
            kind: CompactionType::Marker,
            id: id.clone(),
            parent_id: self.last_id.clone(),
            timestamp: test_iso_timestamp(),
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id.to_string(),
            tokens_before: 10_000.0,
            details: None,
            usage: None,
            from_hook: None,
            system_message: None,
            first_kept_entry_index: None,
            raw_entry: None,
        });
        self.last_id = Some(id);
        entry
    }

    fn model_change(&mut self, provider: &str, model_id: &str) -> SessionEntry {
        let id = self.next_id();
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            kind: ModelChangeType::Marker,
            id: id.clone(),
            parent_id: self.last_id.clone(),
            timestamp: test_iso_timestamp(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        });
        self.last_id = Some(id);
        entry
    }

    fn thinking_level(&mut self, thinking_level: &str) -> SessionEntry {
        let id = self.next_id();
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            kind: ThinkingLevelChangeType::Marker,
            id: id.clone(),
            parent_id: self.last_id.clone(),
            timestamp: test_iso_timestamp(),
            thinking_level: thinking_level.to_string(),
        });
        self.last_id = Some(id);
        entry
    }

    fn custom_message(&mut self, content: &str) -> SessionEntry {
        let id = self.next_id();
        let entry = SessionEntry::CustomMessage(CustomMessageEntry {
            kind: CustomMessageType::Marker,
            custom_type: "test".to_string(),
            content: JsonValue::String(content.to_string()),
            display: true,
            details: None,
            id: id.clone(),
            parent_id: self.last_id.clone(),
            timestamp: test_iso_timestamp(),
            raw_entry: None,
        });
        self.last_id = Some(id);
        entry
    }
}

fn custom_field<'a>(message: &'a AgentMessage, key: &str) -> Option<&'a JsonValue> {
    match message {
        AgentMessage::Custom(object) => object.get(key),
        AgentMessage::Standard(_) => None,
    }
}

fn custom_text(message: &AgentMessage, key: &str) -> String {
    custom_field(message, key)
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string()
}

fn content_text_of(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                    kiana_ai::types::UserContentBlock::Image(_) => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        },
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        AgentMessage::Standard(Message::ToolResult(result)) => result
            .content
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                kiana_ai::types::UserContentBlock::Image(_) => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        AgentMessage::Custom(object) => match object.get("content") {
            Some(JsonValue::String(text)) => text.clone(),
            Some(JsonValue::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(JsonValue::as_str) == Some("text") {
                        block
                            .get("text")
                            .and_then(JsonValue::as_str)
                            .map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        },
        AgentMessage::Standard(Message::System(_)) => String::new(),
    }
}

fn extract_text(messages: &[AgentMessage]) -> String {
    messages
        .iter()
        .map(|message| match message.role() {
            "user" | "assistant" | "custom" | "toolResult" => content_text_of(message),
            "branchSummary" | "compactionSummary" => custom_text(message, "summary"),
            "bashExecution" => format!(
                "{}\n{}",
                custom_text(message, "command"),
                custom_text(message, "output")
            ),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn entry_role(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Message(message) => Some(message.message.role().to_string()),
        _ => None,
    }
}

fn entry_id(entry: &SessionEntry) -> &str {
    entry.id()
}

// ============================================================================
// Unit tests
// ============================================================================

/// TS: it("should calculate total context tokens from usage")
#[test]
fn should_calculate_total_context_tokens_from_usage() {
    let usage = create_mock_usage(1000, 500, 200, 100);
    assert_eq!(calculate_context_tokens(&usage), 1800);
}

/// TS: it("should handle zero values")
#[test]
fn should_handle_zero_values() {
    let usage = create_mock_usage(0, 0, 0, 0);
    assert_eq!(calculate_context_tokens(&usage), 0);
}

/// TS: it("should find the last non-aborted assistant message usage")
#[test]
fn should_find_the_last_non_aborted_assistant_message_usage() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("Hello")),
        factory.message(create_assistant_message(
            "Hi",
            Some(create_mock_usage(100, 50, 0, 0)),
        )),
        factory.message(create_user_message("How are you?")),
        factory.message(create_assistant_message(
            "Good",
            Some(create_mock_usage(200, 100, 0, 0)),
        )),
    ];

    let usage = get_last_assistant_usage(&entries).expect("usage");
    assert_eq!(usage.input, 200);
}

/// TS: it("should skip aborted messages")
#[test]
fn should_skip_aborted_messages() {
    let mut factory = EntryFactory::new();
    let aborted = assistant_message_with_content(
        vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "Aborted".to_string(),
            text_signature: None,
        })],
        Some(create_mock_usage(300, 150, 0, 0)),
        StopReason::Aborted,
    );

    let entries = vec![
        factory.message(create_user_message("Hello")),
        factory.message(create_assistant_message(
            "Hi",
            Some(create_mock_usage(100, 50, 0, 0)),
        )),
        factory.message(create_user_message("How are you?")),
        factory.message(aborted),
    ];

    let usage = get_last_assistant_usage(&entries).expect("usage");
    assert_eq!(usage.input, 100);
}

/// TS: it("should skip all-zero assistant usage")
#[test]
fn should_skip_all_zero_assistant_usage() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("Hello")),
        factory.message(create_assistant_message(
            "Hi",
            Some(create_mock_usage(100, 50, 0, 0)),
        )),
        factory.message(create_user_message("continue")),
        factory.message(create_assistant_message(
            "Partial",
            Some(create_mock_usage(0, 0, 0, 0)),
        )),
    ];

    let usage = get_last_assistant_usage(&entries).expect("usage");
    assert_eq!(usage.input, 100);
}

/// TS: it("should return undefined if no assistant messages")
#[test]
fn should_return_undefined_if_no_assistant_messages() {
    let mut factory = EntryFactory::new();
    let entries = vec![factory.message(create_user_message("Hello"))];
    assert!(get_last_assistant_usage(&entries).is_none());
}

/// Rust-only regression：usage 省略 `totalTokens`（provider 常见形态）时整条 assistant
/// 消息回放为 `AgentMessage::Custom`；TS 的 `getAssistantUsage` 原样返回 usage 对象，
/// 因此 `cost` / `reasoning` / `cacheWrite1h` 必须保留，而不是清零。
///
/// `Usage.total_tokens` 是冻结的 `u64`，无法表达“键不存在”，缺省时固定为 `0`
/// （`calculateContextTokens` 的 `0 → 分量求和` 回退与 JS 的 `undefined || sum` 等价）。
#[test]
fn get_last_assistant_usage_preserves_replayed_usage_metadata() {
    let entry: SessionEntry = serde_json::from_value(serde_json::json!({
        "type": "message",
        "id": "m1",
        "parentId": null,
        "timestamp": test_iso_timestamp(),
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": "hi" }],
            "api": "anthropic-messages",
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "usage": {
                "input": 1,
                "output": 2,
                "cacheRead": 3,
                "cacheWrite": 4,
                "cacheWrite1h": 2,
                "reasoning": 1,
                "cost": {
                    "input": 0.1,
                    "output": 0.2,
                    "cacheRead": 0.3,
                    "cacheWrite": 0.4,
                    "total": 1.0,
                    "totalFromProvider": true
                }
            },
            "stopReason": "stop",
            "timestamp": 1
        }
    }))
    .expect("entry");

    // 无 `totalTokens` → typed 解析失败 → 原始对象回放。
    let SessionEntry::Message(message_entry) = &entry else {
        panic!("expected message entry");
    };
    assert!(matches!(message_entry.message, AgentMessage::Custom(_)));

    let usage = get_last_assistant_usage(&[entry]).expect("usage");
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write
        ),
        (1, 2, 3, 4)
    );
    assert_eq!(usage.cache_write1h, Some(2));
    assert_eq!(usage.reasoning, Some(1));
    assert_eq!(usage.total_tokens, 0);
    assert_eq!(calculate_context_tokens(&usage), 10);
    assert_eq!(usage.cost.input, 0.1);
    assert_eq!(usage.cost.output, 0.2);
    assert_eq!(usage.cost.cache_read, 0.3);
    assert_eq!(usage.cost.cache_write, 0.4);
    assert_eq!(usage.cost.total, 1.0);
    assert_eq!(usage.cost.total_from_provider, Some(true));
}

/// Rust-only regression：大型 fixture 的最后一条 usage 带有真实 cost（TS 原样返回）。
#[test]
fn get_last_assistant_usage_on_large_session_preserves_cost() {
    let entries = load_large_session_entries();

    let usage = get_last_assistant_usage(&entries).expect("last assistant usage");
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write
        ),
        (0, 53, 176_585, 1019)
    );
    assert_eq!(usage.total_tokens, 0);
    // TS: { cost: { input: 0, output: 0.000795, cacheRead: 0.052975499999999995,
    //             cacheWrite: 0.00382125, total: 0.05759174999999999 } }
    assert_eq!(usage.cost.input, 0.0);
    assert_eq!(usage.cost.output, 0.000_795);
    assert_eq!(usage.cost.cache_read, 0.052_975_499_999_999_995);
    assert_eq!(usage.cost.cache_write, 0.003_821_25);
    assert_eq!(usage.cost.total, 0.057_591_749_999_999_99);
    assert_eq!(usage.cost.total_from_provider, None);
}

/// `fromHook` 按 JS 真值判断：任意真值（`1`、`"yes"`……）都表示扩展生成，跳过 details。
///
/// Rust-only regression；TS 见 `compaction.ts#extractFileOperations`（`!prevCompaction.fromHook`）。
#[test]
fn prepare_compaction_treats_truthy_non_boolean_from_hook_as_hook_generated() {
    fn hook_entries(from_hook: Option<serde_json::Value>) -> Vec<SessionEntry> {
        let mut compaction = serde_json::json!({
            "type": "compaction",
            "id": "c0",
            "parentId": null,
            "timestamp": test_iso_timestamp(),
            "summary": "old summary",
            "firstKeptEntryId": "u1",
            "tokensBefore": 100,
            "details": {
                "readFiles": ["prev-read.ts"],
                "modifiedFiles": ["prev-mod.ts"]
            }
        });
        if let Some(from_hook) = from_hook {
            compaction["fromHook"] = from_hook;
        }
        let user = |id: &str, parent: &str, text: &str| {
            serde_json::json!({
                "type": "message",
                "id": id,
                "parentId": parent,
                "timestamp": test_iso_timestamp(),
                "message": { "role": "user", "content": text, "timestamp": 1 }
            })
        };
        let assistant = |id: &str, parent: &str, text: &str| {
            serde_json::json!({
                "type": "message",
                "id": id,
                "parentId": parent,
                "timestamp": test_iso_timestamp(),
                "message": {
                    "role": "assistant",
                    "content": [{ "type": "text", "text": text }],
                    "api": "a",
                    "provider": "p",
                    "model": "m",
                    "usage": {
                        "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0,
                        "totalTokens": 2,
                        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
                    },
                    "stopReason": "stop",
                    "timestamp": 1
                }
            })
        };
        serde_json::from_value(serde_json::json!([
            compaction,
            user("u1", "c0", &"two ".repeat(30)),
            assistant("a1", "u1", &"answer two ".repeat(10)),
            user("u2", "a1", &"three ".repeat(30)),
            assistant("a2", "u2", "answer three"),
        ]))
        .expect("entries")
    }

    let settings = CompactionSettings {
        keep_recent_tokens: 20,
        ..DEFAULT_COMPACTION_SETTINGS
    };

    for (from_hook, collects_details) in [
        (Some(serde_json::json!(true)), false),
        (Some(serde_json::json!(1)), false),
        (Some(serde_json::json!("yes")), false),
        (Some(serde_json::json!(false)), true),
        (Some(serde_json::json!(0)), true),
        (Some(serde_json::Value::Null), true),
        (None, true),
    ] {
        let entries = hook_entries(from_hook.clone());
        let preparation = prepare_compaction(&entries, &settings).expect("preparation");
        assert_eq!(
            preparation.file_ops.read.contains("prev-read.ts"),
            collects_details,
            "fromHook={from_hook:?}"
        );
        assert_eq!(
            preparation.file_ops.edited.contains("prev-mod.ts"),
            collects_details,
            "fromHook={from_hook:?}"
        );
    }
}

/// Rust-only regression：kiana-session-core 把结构字段异常的 `type: "message"` 条目降级为
/// [`SessionEntry::Unknown`]（例如数字 `timestamp`）；TS 加载时不校验字段，消息仍参与切点与
/// 投影。期望值由 TS 参考实现生成（`prepareCompaction` / `findCutPoint` /
/// `getLastAssistantUsage` 对同一组条目）。
#[test]
fn prepare_compaction_includes_messages_from_entries_kiana_session_core_cannot_type() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "u1", "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "hello" }
        },
        {
            "type": "message", "id": "a1", "parentId": "u1",
            "timestamp": 123457,
            "message": {
                "role": "assistant",
                "content": [{ "type": "toolCall", "id": "t", "name": "read", "arguments": { "path": "x.ts" } }],
                "stopReason": "toolUse"
            }
        },
        {
            "type": "message", "id": "r1", "parentId": "a1",
            "timestamp": "2025-01-01T00:00:02.000Z",
            "message": {
                "role": "toolResult", "toolCallId": "t", "toolName": "read",
                "content": [{ "type": "text", "text": "data" }], "isError": false, "timestamp": 3
            }
        },
        {
            "type": "message", "id": "u2", "parentId": "r1",
            "timestamp": "2025-01-01T00:00:03.000Z",
            "message": { "role": "user", "content": "next" }
        }
    ]))
    .expect("entries");
    // 确认 kiana-session-core 的降级路径确实被触发。
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));

    let settings = |keep_recent_tokens| CompactionSettings {
        enabled: true,
        reserve_tokens: 16_384,
        keep_recent_tokens,
    };

    let preparation = prepare_compaction(&entries, &settings(1)).expect("preparation");
    assert_eq!(preparation.first_kept_entry_id, "u2");
    assert!(!preparation.is_split_turn);
    assert_eq!(preparation.tokens_before, 9);
    assert_eq!(
        preparation
            .messages_to_summarize
            .iter()
            .map(AgentMessage::role)
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "toolResult"]
    );
    assert!(preparation.turn_prefix_messages.is_empty());
    assert!(preparation.file_ops.read.contains("x.ts"));

    let preparation = prepare_compaction(&entries, &settings(3)).expect("preparation");
    assert_eq!(preparation.first_kept_entry_id, "a1");
    assert!(preparation.is_split_turn);
    assert!(preparation.messages_to_summarize.is_empty());
    assert_eq!(
        preparation
            .turn_prefix_messages
            .iter()
            .map(AgentMessage::role)
            .collect::<Vec<_>>(),
        vec!["user"]
    );
}

/// Rust-only regression：`findCutPoint` 必须把 Unknown `message` 条目的消息计入 token。
///
/// TS 参考（`findCutPoint(entries, 0, entries.length, 1)`）：
/// `{ firstKeptEntryIndex: 1, turnStartIndex: 0, isSplitTurn: true }`。
#[test]
fn find_cut_point_counts_messages_from_entries_kiana_session_core_cannot_type() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "u1", "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "hello world" }
        },
        {
            "type": "message", "id": "a1", "parentId": "u1",
            "timestamp": 123456,
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hi there" }],
                "stopReason": "stop"
            }
        }
    ]))
    .expect("entries");
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));

    let cut = find_cut_point(&entries, 0, entries.len(), 1);

    assert_eq!(cut.first_kept_entry_index, 1);
    assert_eq!(cut.turn_start_index, Some(0));
    assert!(cut.is_split_turn);
}

/// Rust-only regression：`getLastAssistantUsage` 读取 Unknown `message` 条目的 usage。
#[test]
fn get_last_assistant_usage_reads_entries_kiana_session_core_cannot_type() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "a1", "parentId": null,
            "timestamp": 123456,
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hi" }],
                "usage": {
                    "input": 100, "output": 50, "cacheRead": 0, "cacheWrite": 0,
                    "totalTokens": 150,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
                },
                "stopReason": "stop"
            }
        }
    ]))
    .expect("entries");
    assert!(matches!(entries[0], SessionEntry::Unknown(_)));

    let usage = get_last_assistant_usage(&entries).expect("usage");

    assert_eq!(usage.total_tokens, 150);
    assert_eq!(calculate_context_tokens(&usage), 150);
}

/// Rust-only regression：`details.readFiles` / `modifiedFiles` 的非字符串元素保留原始 JSON。
///
/// TS 只检查 `Array.isArray`，元素原样进 `Set`，渲染时按 JS 规则转换：`null` 的排序键是
/// `"null"`、`join` 却是空串；数字 `123` 与字符串 `"123"` 是两个元素。期望值由 TS 参考实现
/// 生成（`prepareCompaction` -> `computeFileLists` -> `formatFileOperations`）。
#[test]
fn prepare_compaction_keeps_non_string_file_details_raw_like_js() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "compaction", "id": "c0", "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "summary": "old summary", "firstKeptEntryId": "u1", "tokensBefore": 100,
            "details": {
                "readFiles": [null, 123, "123", true, ["a", null], { "x": 1 }, "a.ts"],
                "modifiedFiles": [null, 123, "123"]
            }
        },
        {
            "type": "message", "id": "u1", "parentId": "c0",
            "timestamp": "2025-01-01T00:00:01.000Z",
            "message": { "role": "user", "content": "two ".repeat(30) }
        },
        {
            "type": "message", "id": "a1", "parentId": "u1",
            "timestamp": "2025-01-01T00:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "answer ".repeat(10) }],
                "stopReason": "stop"
            }
        },
        {
            "type": "message", "id": "u2", "parentId": "a1",
            "timestamp": "2025-01-01T00:00:03.000Z",
            "message": { "role": "user", "content": "three ".repeat(30) }
        },
        {
            "type": "message", "id": "a2", "parentId": "u2",
            "timestamp": "2025-01-01T00:00:04.000Z",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "done" }],
                "stopReason": "stop"
            }
        }
    ]))
    .expect("entries");

    let settings = CompactionSettings {
        keep_recent_tokens: 20,
        ..DEFAULT_COMPACTION_SETTINGS
    };
    let preparation = prepare_compaction(&entries, &settings).expect("preparation");

    assert_eq!(
        preparation
            .file_ops
            .read
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            JsonValue::Null,
            serde_json::json!(123),
            serde_json::json!("123"),
            serde_json::json!(true),
            serde_json::json!(["a", null]),
            serde_json::json!({ "x": 1 }),
            serde_json::json!("a.ts"),
        ]
    );
    assert_eq!(
        preparation
            .file_ops
            .edited
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            JsonValue::Null,
            serde_json::json!(123),
            serde_json::json!("123"),
        ]
    );

    let lists = compute_file_lists(&preparation.file_ops);
    assert_eq!(
        lists.read_files,
        vec![
            serde_json::json!({ "x": 1 }),
            serde_json::json!(["a", null]),
            serde_json::json!("a.ts"),
            serde_json::json!(true),
        ]
    );
    assert_eq!(
        lists.modified_files,
        vec![
            serde_json::json!(123),
            serde_json::json!("123"),
            JsonValue::Null,
        ]
    );
    assert_eq!(
        format_file_operations(&lists.read_files, &lists.modified_files),
        "\n\n<read-files>\n[object Object]\na,\na.ts\ntrue\n</read-files>\n\n<modified-files>\n123\n123\n\n</modified-files>"
    );
}

/// Rust-only regression：外部（kiana-session）用 `buildSessionProjection` 构建的投影同样要补回
/// Unknown `message` 条目的消息。TS 参考：投影消息 [user, assistant]，tokens 5。
#[test]
fn estimate_projected_context_tokens_recovers_entries_kiana_session_core_cannot_type() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "u1", "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "hello world" }
        },
        {
            "type": "message", "id": "a1", "parentId": "u1",
            "timestamp": 123456,
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hi there" }],
                "stopReason": "stop"
            }
        }
    ]))
    .expect("entries");
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));

    let projection = build_session_projection(&entries, LeafId::Unspecified, None);
    // kiana-session-core 的投影丢掉了 Unknown 条目的消息；修复发生在估算函数内部。
    assert_eq!(projection.messages.len(), 1);

    let estimate = estimate_projected_context_tokens(&projection, &entries);

    assert_eq!(estimate.tokens, 5);
    assert_eq!(estimate.usage_tokens, 0);
    assert_eq!(estimate.trailing_tokens, 5);
    assert_eq!(estimate.last_usage_index, None);
}

/// Rust-only regression：压缩点之前的 system 消息永远不进入上下文（TS `buildContextEntries`）；
/// kiana-session-core 只识别 typed message 条目，这里补回消息时必须保持该排除规则。
///
/// TS 参考：投影消息 [compactionSummary, user, user]，tokens 6（不含 4 token 的 system）。
#[test]
fn estimate_projected_context_tokens_skips_pre_compaction_system_unknown_messages() {
    let entries: Vec<SessionEntry> = serde_json::from_value(serde_json::json!([
        {
            "type": "message", "id": "u0", "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": "start" }
        },
        {
            "type": "message", "id": "s1", "parentId": "u0",
            "timestamp": 111,
            "message": { "role": "system", "content": "system prompt" }
        },
        {
            "type": "message", "id": "u1", "parentId": "s1",
            "timestamp": "2025-01-01T00:00:01.000Z",
            "message": { "role": "user", "content": "hello" }
        },
        {
            "type": "compaction", "id": "c1", "parentId": "u1",
            "timestamp": "2025-01-01T00:00:02.000Z",
            "summary": "summary!", "firstKeptEntryId": "s1", "tokensBefore": 10
        },
        {
            "type": "message", "id": "u2", "parentId": "c1",
            "timestamp": "2025-01-01T00:00:03.000Z",
            "message": { "role": "user", "content": "after" }
        }
    ]))
    .expect("entries");
    assert!(matches!(entries[1], SessionEntry::Unknown(_)));

    let projection = build_session_projection(&entries, LeafId::Unspecified, None);
    let estimate = estimate_projected_context_tokens(&projection, &entries);

    assert_eq!(estimate.tokens, 6);
}

/// Rust-only regression：`context_edit` 对受修复条目的省略/替换语义。
///
/// TS 参考：`replacement: null` -> 0 token；`content: "replaced"` 与单 text 块 -> 2 token。
#[test]
fn estimate_projected_context_tokens_applies_edits_to_recovered_entries() {
    let entries_for = |replacement: serde_json::Value| {
        serde_json::from_value::<Vec<SessionEntry>>(serde_json::json!([
            {
                "type": "message", "id": "u1", "parentId": null,
                "timestamp": 111,
                "message": { "role": "user", "content": "hello world" }
            },
            {
                "type": "context_edit", "id": "e1", "parentId": "u1",
                "timestamp": "2025-01-01T00:00:01.000Z",
                "targetId": "u1", "replacement": replacement
            }
        ]))
        .expect("entries")
    };

    for replacement in [
        serde_json::Value::Null,
        serde_json::json!({ "content": "replaced" }),
        serde_json::json!({ "content": [{ "type": "text", "text": "blocks" }] }),
    ] {
        let entries = entries_for(replacement.clone());
        assert!(matches!(entries[0], SessionEntry::Unknown(_)));
        let projection = build_session_projection(&entries, LeafId::Unspecified, None);
        let estimate = estimate_projected_context_tokens(&projection, &entries);
        let expected = if replacement.is_null() { 0 } else { 2 };
        assert_eq!(estimate.tokens, expected, "replacement={replacement}");
    }
}

/// TS: it("uses the last non-zero assistant usage as the context anchor")
#[test]
fn uses_the_last_non_zero_assistant_usage_as_the_context_anchor() {
    let messages = vec![
        create_user_message("Hello"),
        create_assistant_message("Hi", Some(create_mock_usage(100, 50, 0, 0))),
        create_user_message("continue"),
        create_assistant_message("Partial thinking", Some(create_mock_usage(0, 0, 0, 0))),
    ];

    let estimate = estimate_context_tokens(&messages);

    assert_eq!(estimate.usage_tokens, 150);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 150 + estimate.trailing_tokens);
}

/// TS: it("should return true when context exceeds threshold")
#[test]
fn should_return_true_when_context_exceeds_threshold() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10_000,
        keep_recent_tokens: 20_000,
    };

    assert!(should_compact(95_000, 100_000, &settings));
    assert!(!should_compact(89_000, 100_000, &settings));
}

/// TS: it("should return false when disabled")
#[test]
fn should_return_false_when_disabled() {
    let settings = CompactionSettings {
        enabled: false,
        reserve_tokens: 10_000,
        keep_recent_tokens: 20_000,
    };

    assert!(!should_compact(95_000, 100_000, &settings));
}

/// TS: it("should find cut point based on actual token differences")
#[test]
fn should_find_cut_point_based_on_actual_token_differences() {
    let mut factory = EntryFactory::new();
    let mut entries = Vec::new();
    for index in 0..10 {
        entries.push(factory.message(create_user_message(&format!("User {index}"))));
        entries.push(factory.message(create_assistant_message(
            &format!("Assistant {index}"),
            Some(create_mock_usage(0, 100, (index + 1) * 1000, 0)),
        )));
    }

    let result = find_cut_point(&entries, 0, entries.len(), 2500);

    assert_eq!(entries[result.first_kept_entry_index].kind(), "message");
    let role = entry_role(&entries[result.first_kept_entry_index]).expect("message entry");
    assert!(role == "user" || role == "assistant");
}

/// TS: it("should return startIndex if no valid cut points in range")
#[test]
fn should_return_start_index_if_no_valid_cut_points_in_range() {
    let mut factory = EntryFactory::new();
    let entries = vec![factory.message(create_assistant_message("a", None))];
    let result = find_cut_point(&entries, 0, entries.len(), 1000);
    assert_eq!(result.first_kept_entry_index, 0);
}

/// TS: it("should keep everything if all messages fit within budget")
#[test]
fn should_keep_everything_if_all_messages_fit_within_budget() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("1")),
        factory.message(create_assistant_message(
            "a",
            Some(create_mock_usage(0, 50, 500, 0)),
        )),
        factory.message(create_user_message("2")),
        factory.message(create_assistant_message(
            "b",
            Some(create_mock_usage(0, 50, 1000, 0)),
        )),
    ];

    let result = find_cut_point(&entries, 0, entries.len(), 50_000);
    assert_eq!(result.first_kept_entry_index, 0);
}

/// TS: it("should indicate split turn when cutting at assistant message")
#[test]
fn should_indicate_split_turn_when_cutting_at_assistant_message() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("Turn 1")),
        factory.message(create_assistant_message(
            "A1",
            Some(create_mock_usage(0, 100, 1000, 0)),
        )),
        factory.message(create_user_message("Turn 2")),
        factory.message(create_assistant_message(
            "A2-1",
            Some(create_mock_usage(0, 100, 5000, 0)),
        )),
        factory.message(create_assistant_message(
            "A2-2",
            Some(create_mock_usage(0, 100, 8000, 0)),
        )),
        factory.message(create_assistant_message(
            "A2-3",
            Some(create_mock_usage(0, 100, 10000, 0)),
        )),
    ];

    let result = find_cut_point(&entries, 0, entries.len(), 3000);

    let cut_role = entry_role(&entries[result.first_kept_entry_index]);
    if cut_role.as_deref() == Some("assistant") {
        assert!(result.is_split_turn);
        assert_eq!(result.turn_start_index, Some(2));
    }
}

/// TS: it("should budget context-visible custom message entries")
#[test]
fn should_budget_context_visible_custom_message_entries() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("hi")),
        factory.message(create_assistant_message("hello", None)),
        factory.custom_message(&"x".repeat(4000)),
        factory.message(create_assistant_message("ok", None)),
    ];

    let tiny_budget = find_cut_point(&entries, 0, entries.len(), 1);
    assert_eq!(tiny_budget.first_kept_entry_index, 3);
    assert!(tiny_budget.is_split_turn);
    assert_eq!(tiny_budget.turn_start_index, Some(2));

    let custom_fits_budget = find_cut_point(&entries, 0, entries.len(), 2);
    assert_eq!(custom_fits_budget.first_kept_entry_index, 2);
    assert!(!custom_fits_budget.is_split_turn);
    assert_eq!(custom_fits_budget.turn_start_index, None);
}

// Regression test for #9740.
/// TS: it("should fall back to the latest valid cut point before oversized trailing tool results")
#[test]
fn should_fall_back_to_the_latest_valid_cut_point_before_oversized_trailing_tool_results() {
    let mut factory = EntryFactory::new();
    let old_user_message = create_user_message("old history");
    let old_assistant_message = create_assistant_message("old answer", None);
    let current_user_message = create_user_message("read the large file");
    let tool_call_message = assistant_message_with_content(
        vec![AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: "call-1".to_string(),
            name: "read".to_string(),
            arguments: IndexMap::from([(
                "path".to_string(),
                JsonValue::String("big.txt".to_string()),
            )]),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        })],
        None,
        StopReason::ToolUse,
    );
    let tool_result_message =
        AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
            role: kiana_ai::types::ToolResultRole::ToolResult,
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            content: vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "x".repeat(8000),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: test_timestamp(),
            duration_ms: None,
        }));

    let old_user = factory.message(old_user_message.clone());
    let old_assistant = factory.message(old_assistant_message.clone());
    let current_user = factory.message(current_user_message.clone());
    let tool_call = factory.message(tool_call_message);
    let tool_result = factory.message(tool_result_message);
    let entries = vec![
        old_user,
        old_assistant,
        current_user,
        tool_call,
        tool_result,
    ];

    let result = find_cut_point(&entries, 0, entries.len(), 1000);
    assert_eq!(
        result,
        kiana_compaction::CutPointResult {
            first_kept_entry_index: 3,
            turn_start_index: Some(2),
            is_split_turn: true,
        }
    );

    let preparation = prepare_compaction(
        &entries,
        &CompactionSettings {
            keep_recent_tokens: 1000,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .expect("preparation");
    assert_eq!(preparation.first_kept_entry_id, entry_id(&entries[3]));
    assert_eq!(
        preparation.messages_to_summarize,
        vec![old_user_message, old_assistant_message]
    );
    assert_eq!(preparation.turn_prefix_messages, vec![current_user_message]);
}

/// TS: it("should load all messages when no compaction")
#[test]
fn should_load_all_messages_when_no_compaction() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("1")),
        factory.message(create_assistant_message("a", None)),
        factory.message(create_user_message("2")),
        factory.message(create_assistant_message("b", None)),
    ];

    let loaded = build_session_context(&entries, LeafId::Unspecified, None);
    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.thinking_level, "off");
    assert_eq!(
        loaded.model,
        Some(kiana_session_core::session_manager::ModelRef {
            provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-5".to_string(),
        })
    );
}

/// TS: it("should handle single compaction")
#[test]
fn should_handle_single_compaction() {
    let mut factory = EntryFactory::new();
    let u1 = factory.message(create_user_message("1"));
    let a1 = factory.message(create_assistant_message("a", None));
    let u2 = factory.message(create_user_message("2"));
    let a2 = factory.message(create_assistant_message("b", None));
    let u2_id = entry_id(&u2).to_string();
    let compaction = factory.compaction("Summary of 1,a,2,b", &u2_id);
    let u3 = factory.message(create_user_message("3"));
    let a3 = factory.message(create_assistant_message("c", None));

    let entries = vec![u1, a1, u2, a2, compaction, u3, a3];

    let loaded = build_session_context(&entries, LeafId::Unspecified, None);
    assert_eq!(loaded.messages.len(), 5);
    assert_eq!(loaded.messages[0].role(), "compactionSummary");
    assert!(custom_text(&loaded.messages[0], "summary").contains("Summary of 1,a,2,b"));
}

/// TS: it("should handle multiple compactions (only latest matters)")
#[test]
fn should_handle_multiple_compactions_only_latest_matters() {
    let mut factory = EntryFactory::new();
    let u1 = factory.message(create_user_message("1"));
    let a1 = factory.message(create_assistant_message("a", None));
    let u1_id = entry_id(&u1).to_string();
    let compact1 = factory.compaction("First summary", &u1_id);
    let u2 = factory.message(create_user_message("2"));
    let b = factory.message(create_assistant_message("b", None));
    let u3 = factory.message(create_user_message("3"));
    let c = factory.message(create_assistant_message("c", None));
    let u3_id = entry_id(&u3).to_string();
    let compact2 = factory.compaction("Second summary", &u3_id);
    let u4 = factory.message(create_user_message("4"));
    let d = factory.message(create_assistant_message("d", None));

    let entries = vec![u1, a1, compact1, u2, b, u3, c, compact2, u4, d];

    let loaded = build_session_context(&entries, LeafId::Unspecified, None);
    assert_eq!(loaded.messages.len(), 5);
    assert!(custom_text(&loaded.messages[0], "summary").contains("Second summary"));
}

/// TS: it("should keep all messages when firstKeptEntryId is first entry")
#[test]
fn should_keep_all_messages_when_first_kept_entry_id_is_first_entry() {
    let mut factory = EntryFactory::new();
    let u1 = factory.message(create_user_message("1"));
    let a1 = factory.message(create_assistant_message("a", None));
    let u1_id = entry_id(&u1).to_string();
    let compact1 = factory.compaction("First summary", &u1_id);
    let u2 = factory.message(create_user_message("2"));
    let b = factory.message(create_assistant_message("b", None));

    let entries = vec![u1, a1, compact1, u2, b];

    let loaded = build_session_context(&entries, LeafId::Unspecified, None);
    assert_eq!(loaded.messages.len(), 5);
}

/// TS: it("should track model and thinking level changes")
#[test]
fn should_track_model_and_thinking_level_changes() {
    let mut factory = EntryFactory::new();
    let entries = vec![
        factory.message(create_user_message("1")),
        factory.model_change("openai", "gpt-4"),
        factory.message(create_assistant_message("a", None)),
        factory.thinking_level("high"),
    ];

    let loaded = build_session_context(&entries, LeafId::Unspecified, None);
    assert_eq!(
        loaded.model,
        Some(kiana_session_core::session_manager::ModelRef {
            provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-5".to_string(),
        })
    );
    assert_eq!(loaded.thinking_level, "high");
}

/// TS: it("does not treat system messages as conversation history")
#[test]
fn does_not_treat_system_messages_as_conversation_history() {
    let mut factory = EntryFactory::new();
    let system_message = AgentMessage::Standard(Message::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: Some(IndexMap::from([(
            "preamble".to_string(),
            Some("current prompt".to_string()),
        )])),
        tools_added: None,
        tools_removed: None,
        timestamp: test_timestamp(),
    }));
    let user_message = create_user_message("one long turn");
    let assistant_message = create_assistant_message("assistant suffix", None);

    let system = factory.message(system_message);
    let user = factory.message(user_message.clone());
    let assistant = factory.message(assistant_message);
    let assistant_id = entry_id(&assistant).to_string();

    let preparation = prepare_compaction(
        &[system, user, assistant],
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .expect("preparation");

    assert_eq!(preparation.first_kept_entry_id, assistant_id);
    assert!(preparation.is_split_turn);
    assert!(preparation.messages_to_summarize.is_empty());
    assert_eq!(preparation.turn_prefix_messages, vec![user_message]);
}

/// TS: it("should skip repeated compactions when kept messages still fit")
#[test]
fn should_skip_repeated_compactions_when_kept_messages_still_fit() {
    let mut factory = EntryFactory::new();
    let u1 = factory.message(create_user_message(
        "user msg 1 (summarized by compaction1)",
    ));
    let a1 = factory.message(create_assistant_message("assistant msg 1", None));
    let u2 = factory.message(create_user_message("user msg 2 - kept by compaction1"));
    let a2 = factory.message(create_assistant_message("assistant msg 2", None));
    let u3 = factory.message(create_user_message("user msg 3 - kept by compaction1"));
    let a3 = factory.message(create_assistant_message(
        "assistant msg 3",
        Some(create_mock_usage(5000, 1000, 0, 0)),
    ));
    let u2_id = entry_id(&u2).to_string();
    let compaction1 = factory.compaction("First summary", &u2_id);
    let u4 = factory.message(create_user_message("user msg 4 (new after compaction1)"));
    let a4 = factory.message(create_assistant_message(
        "assistant msg 4",
        Some(create_mock_usage(8000, 2000, 0, 0)),
    ));

    let path_entries = vec![u1, a1, u2, a2, u3, a3, compaction1, u4, a4];
    let preparation = prepare_compaction(&path_entries, &DEFAULT_COMPACTION_SETTINGS);

    assert!(preparation.is_none());
}

/// spec §2.20.20 第 1 步：路径最后一条是压缩条目时不再压缩（已压缩到底）。
///
/// TS: `prepareCompaction` 的 `pathEntries[pathEntries.length - 1].type === "compaction"` 分支
#[test]
fn prepare_compaction_returns_none_when_path_ends_with_compaction() {
    let mut factory = EntryFactory::new();
    let user = factory.message(create_user_message("one"));
    let user_id = entry_id(&user).to_string();
    let compaction = factory.compaction("summary", &user_id);

    assert!(prepare_compaction(&[user, compaction], &DEFAULT_COMPACTION_SETTINGS).is_none());
}

/// TS: it("should re-summarize previously kept messages when the recent window moves past them")
#[test]
fn should_re_summarize_previously_kept_messages_when_the_recent_window_moves_past_them() {
    let mut factory = EntryFactory::new();
    let u1 = factory.message(create_user_message(
        &"user msg 1 (summarized by compaction1)".repeat(4),
    ));
    let a1 = factory.message(create_assistant_message(&"assistant msg 1".repeat(4), None));
    let u2 = factory.message(create_user_message(
        &"user msg 2 - kept by compaction1 ".repeat(12),
    ));
    let a2 = factory.message(create_assistant_message(
        &"assistant msg 2 ".repeat(12),
        None,
    ));
    let u3 = factory.message(create_user_message(
        &"user msg 3 - kept by compaction1 ".repeat(12),
    ));
    let a3 = factory.message(create_assistant_message(
        &"assistant msg 3 ".repeat(12),
        Some(create_mock_usage(5000, 1000, 0, 0)),
    ));
    let u2_id = entry_id(&u2).to_string();
    let compaction1 = factory.compaction("First summary", &u2_id);
    let u4 = factory.message(create_user_message(
        &"user msg 4 (new after compaction1) ".repeat(12),
    ));
    let a4 = factory.message(create_assistant_message(
        &"assistant msg 4 ".repeat(12),
        Some(create_mock_usage(8000, 2000, 0, 0)),
    ));

    let settings = CompactionSettings {
        keep_recent_tokens: 100,
        ..DEFAULT_COMPACTION_SETTINGS
    };
    let preparation = prepare_compaction(&[u1, a1, u2, a2, u3, a3, compaction1, u4, a4], &settings)
        .expect("preparation");

    let summarized_text = extract_text(&preparation.messages_to_summarize);
    assert!(summarized_text.contains("user msg 2 - kept by compaction1"));
    assert!(summarized_text.contains("user msg 3 - kept by compaction1"));
    assert!(!summarized_text.contains("First summary"));
    assert_eq!(
        preparation.previous_summary,
        Some("First summary".to_string())
    );
}

// ============================================================================
// Integration tests with real session data
// ============================================================================

/// TS: it("should parse the large session")
#[test]
fn should_parse_the_large_session() {
    let entries = load_large_session_entries();
    assert!(entries.len() > 100);

    let message_count = entries
        .iter()
        .filter(|entry| entry.kind() == "message")
        .count();
    assert!(message_count > 100);
}

/// TS: it("should find cut point in large session")
#[test]
fn should_find_cut_point_in_large_session() {
    let entries = load_large_session_entries();
    let result = find_cut_point(
        &entries,
        0,
        entries.len(),
        DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens,
    );

    assert_eq!(entries[result.first_kept_entry_index].kind(), "message");
    let role = entry_role(&entries[result.first_kept_entry_index]).expect("message entry");
    assert!(role == "user" || role == "assistant");
}

/// 黄金值由 TS 参考实现生成（`vitest` 跑同一 fixture；见交付说明）。任何切点/估算的
/// off-by-one 都会让这些断言失败。
#[test]
fn matches_ts_reference_golden_values_for_the_large_session() {
    let entries = load_large_session_entries();
    assert_eq!(entries.len(), 1018);

    let context = build_session_context(&entries, LeafId::Unspecified, None);
    assert_eq!(context.messages.len(), 914);
    assert_eq!(context.thinking_level, "off");
    assert_eq!(
        context.model,
        Some(ModelRef {
            provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-5".to_string(),
        })
    );

    let cut = find_cut_point(
        &entries,
        0,
        entries.len(),
        DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens,
    );
    assert_eq!(
        cut,
        CutPointResult {
            first_kept_entry_index: 837,
            turn_start_index: Some(834),
            is_split_turn: true,
        }
    );

    let preparation = prepare_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS)
        .expect("preparation for large session");
    assert_eq!(preparation.first_kept_entry_id, entry_id(&entries[837]));
    assert_eq!(preparation.tokens_before, 177_657);
    assert_eq!(preparation.messages_to_summarize.len(), 730);
    assert_eq!(preparation.turn_prefix_messages.len(), 3);
    assert!(preparation.is_split_turn);
    assert_eq!(preparation.previous_summary, None);
    assert_eq!(
        preparation
            .file_ops
            .read
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "packages/coding-agent/docs/theme.md".to_string(),
            "packages/coding-agent/src/theme/theme.ts".to_string(),
            "packages/coding-agent/src/tui/oauth-selector.ts".to_string(),
            "packages/coding-agent/src/tui/theme-selector.ts".to_string(),
            "packages/coding-agent/src/tui/model-selector.ts".to_string(),
            "packages/coding-agent/src/tui/user-message-selector.ts".to_string(),
            "packages/coding-agent/src/tui/tui-renderer.ts".to_string(),
            "packages/coding-agent/src/tui/custom-editor.ts".to_string(),
            "packages/tui/test/chat-simple.ts".to_string(),
            "packages/tui/test/editor.test.ts".to_string(),
            "packages/tui/test/markdown.test.ts".to_string(),
            "packages/tui/test/wrap-ansi.test.ts".to_string(),
            "packages/coding-agent/src/theme/dark.json".to_string(),
            "packages/coding-agent/src/theme/light.json".to_string(),
            "packages/tui/src/components/truncated-text.ts".to_string(),
            "packages/coding-agent/src/tui/user-message.ts".to_string(),
            "packages/coding-agent/src/tui/tool-execution.ts".to_string(),
            "packages/coding-agent/src/tui/footer.ts".to_string(),
            "packages/coding-agent/README.md".to_string(),
            "AGENTS.md".to_string(),
            "~/.kiana/agent/themes/nord.json".to_string(),
        ]
    );
    assert_eq!(
        preparation
            .file_ops
            .written
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "packages/tui/test/test-themes.ts".to_string(),
            "packages/tui/test/truncated-text.test.ts".to_string(),
            "packages/coding-agent/test/test-theme-colors.ts".to_string(),
        ]
    );
    assert_eq!(
        preparation
            .file_ops
            .edited
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "packages/coding-agent/src/tui/user-message-selector.ts".to_string(),
            "packages/coding-agent/src/tui/tui-renderer.ts".to_string(),
            "packages/coding-agent/src/theme/theme.ts".to_string(),
            "packages/tui/test/chat-simple.ts".to_string(),
            "packages/tui/test/editor.test.ts".to_string(),
            "packages/tui/test/markdown.test.ts".to_string(),
            "packages/tui/test/wrap-ansi.test.ts".to_string(),
            "packages/tui/test/test-themes.ts".to_string(),
            "packages/coding-agent/docs/theme.md".to_string(),
            "packages/coding-agent/src/theme/dark.json".to_string(),
            "packages/coding-agent/src/theme/light.json".to_string(),
            "packages/tui/src/components/truncated-text.ts".to_string(),
            "packages/tui/test/truncated-text.test.ts".to_string(),
            "packages/tui/src/components/text.ts".to_string(),
            "packages/coding-agent/test/test-theme-colors.ts".to_string(),
            "packages/coding-agent/src/tui/footer.ts".to_string(),
            "packages/tui/src/components/markdown.ts".to_string(),
            "packages/coding-agent/README.md".to_string(),
            "packages/coding-agent/src/main.ts".to_string(),
            "~/.kiana/agent/themes/nord.json".to_string(),
        ]
    );

    // 最后一条 assistant 消息的 usage 省略了 `totalTokens`（provider 常见形态）：
    // 必须走分量求和路径（TS: `totalTokens || input+output+cacheRead+cacheWrite`）。
    let usage = get_last_assistant_usage(&entries).expect("last assistant usage");
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write
        ),
        (0, 53, 176_585, 1019)
    );
    assert_eq!(calculate_context_tokens(&usage), 177_657);

    let estimate = estimate_context_tokens(&context.messages);
    assert_eq!(estimate.tokens, 177_657);
    assert_eq!(estimate.usage_tokens, 177_657);
    assert_eq!(estimate.trailing_tokens, 0);
    assert_eq!(estimate.last_usage_index, Some(913));
}

/// TS: it("should load session correctly")
#[test]
fn should_load_session_correctly() {
    let entries = load_large_session_entries();
    let loaded = build_session_context(&entries, LeafId::Unspecified, None);

    assert!(loaded.messages.len() > 100);
    assert!(loaded.model.is_some());
}

// ============================================================================
// 投影切点黄金测试（由 TS 参考实现生成）
// ============================================================================

fn entry_with_message(id: &str, parent: Option<&str>, message: AgentMessage) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        timestamp: test_iso_timestamp(),
        message,
        raw_message: None,
    })
}

fn assistant_with_usage(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
) -> AgentMessage {
    assistant_message_with_content(content, Some(create_mock_usage(10, 5, 0, 0)), stop_reason)
}

fn tool_call_block(id: &str, name: &str, path: &str) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: IndexMap::from([("path".to_string(), JsonValue::String(path.to_string()))]),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })
}

fn tool_result_message(tool_call_id: &str, tool_name: &str, text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
        role: kiana_ai::types::ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        content: vec![kiana_ai::types::UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: test_timestamp(),
        duration_ms: None,
    }))
}

fn context_edit_entry(
    id: &str,
    parent: Option<&str>,
    target_id: &str,
    replacement: Option<JsonValue>,
) -> SessionEntry {
    SessionEntry::ContextEdit(ContextEditEntry {
        kind: ContextEditType::Marker,
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        timestamp: test_iso_timestamp(),
        target_id: target_id.to_string(),
        replacement: replacement.map(|content| ContextEditReplacement { content }),
        raw_entry: None,
    })
}

fn label_entry(id: &str, parent: Option<&str>, target_id: &str) -> SessionEntry {
    SessionEntry::Label(LabelEntry {
        kind: LabelType::Marker,
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        timestamp: test_iso_timestamp(),
        target_id: target_id.to_string(),
        label: Some("mark".to_string()),
    })
}

fn compaction_entry_with_details(
    id: &str,
    parent: Option<&str>,
    first_kept_entry_id: &str,
    details: Option<JsonValue>,
) -> SessionEntry {
    SessionEntry::Compaction(CompactionEntry {
        kind: CompactionType::Marker,
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        timestamp: test_iso_timestamp(),
        summary: "old summary".to_string(),
        first_kept_entry_id: first_kept_entry_id.to_string(),
        tokens_before: 100.0,
        details,
        usage: None,
        from_hook: None,
        system_message: None,
        first_kept_entry_index: None,
        raw_entry: None,
    })
}

/// 场景结果的可比较视图（与 TS 黄金 JSON 的字段对应）。
#[derive(Debug, PartialEq)]
struct ScenarioResult {
    first_kept_entry_id: Option<String>,
    tokens_before: Option<u64>,
    message_roles: Vec<String>,
    turn_prefix_roles: Vec<String>,
    is_split_turn: Option<bool>,
    previous_summary: Option<String>,
    read: Vec<String>,
    written: Vec<String>,
    edited: Vec<String>,
}

/// 差分夹具里的文件操作元素都是字符串；取原始字符串（非字符串不会出现在夹具里）。
fn file_value_text(value: kiana_compaction::FileValue) -> String {
    match value.into_json() {
        JsonValue::String(text) => text,
        other => other.to_string(),
    }
}

fn summarize(entries: &[SessionEntry], keep_recent_tokens: u64) -> ScenarioResult {
    let preparation = prepare_compaction(
        entries,
        &CompactionSettings {
            keep_recent_tokens,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    );
    match preparation {
        Some(preparation) => ScenarioResult {
            first_kept_entry_id: Some(preparation.first_kept_entry_id),
            tokens_before: Some(preparation.tokens_before),
            message_roles: preparation
                .messages_to_summarize
                .iter()
                .map(|message| message.role().to_string())
                .collect(),
            turn_prefix_roles: preparation
                .turn_prefix_messages
                .iter()
                .map(|message| message.role().to_string())
                .collect(),
            is_split_turn: Some(preparation.is_split_turn),
            previous_summary: preparation.previous_summary,
            read: preparation
                .file_ops
                .read
                .into_iter()
                .map(file_value_text)
                .collect(),
            written: preparation
                .file_ops
                .written
                .into_iter()
                .map(file_value_text)
                .collect(),
            edited: preparation
                .file_ops
                .edited
                .into_iter()
                .map(file_value_text)
                .collect(),
        },
        None => ScenarioResult {
            first_kept_entry_id: None,
            tokens_before: None,
            message_roles: Vec::new(),
            turn_prefix_roles: Vec::new(),
            is_split_turn: None,
            previous_summary: None,
            read: Vec::new(),
            written: Vec::new(),
            edited: Vec::new(),
        },
    }
}

fn base_entries() -> Vec<SessionEntry> {
    vec![
        entry_with_message("u1", None, create_user_message("one")),
        entry_with_message(
            "a1",
            Some("u1"),
            assistant_with_usage(
                vec![AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "answer one".to_string(),
                    text_signature: None,
                })],
                StopReason::Stop,
            ),
        ),
        entry_with_message("ubig", Some("a1"), create_user_message(&"x".repeat(4000))),
        entry_with_message(
            "abad",
            Some("ubig"),
            assistant_with_usage(
                vec![tool_call_block("call-1", "read", "bad.ts")],
                StopReason::ToolUse,
            ),
        ),
        entry_with_message(
            "tres",
            Some("abad"),
            tool_result_message("call-1", "read", "result"),
        ),
    ]
}

fn empty_files() -> (Vec<String>, Vec<String>, Vec<String>) {
    (Vec::new(), Vec::new(), Vec::new())
}

#[test]
fn projected_cut_point_scenarios_match_ts_reference_golden() {
    // 黄金值来自 TS 参考实现的 `prepareCompaction`（见交付说明里的 probe）。
    let (read, written, edited) = empty_files();

    // 1. 被 Omission 编辑删除的 assistant 尝试：recovery-omission-suffix 前移一位。
    let mut entries = base_entries();
    entries.push(context_edit_entry("edit1", Some("tres"), "abad", None));
    entries.push(context_edit_entry("edit2", Some("edit1"), "tres", None));
    assert_eq!(
        summarize(&entries, 1),
        ScenarioResult {
            first_kept_entry_id: Some("abad".to_string()),
            tokens_before: Some(1004),
            message_roles: vec!["user".to_string(), "assistant".to_string()],
            turn_prefix_roles: vec!["user".to_string()],
            is_split_turn: Some(true),
            previous_summary: None,
            read: read.clone(),
            written: written.clone(),
            edited: edited.clone(),
        }
    );

    // 2. 有外部 replacement 时不触发 recovery 规则（切点仍落在 replacement 后的 assistant 上）。
    let mut entries = base_entries();
    entries.push(context_edit_entry(
        "edit1",
        Some("tres"),
        "abad",
        Some(serde_json::json!([{ "type": "text", "text": "replacement" }])),
    ));
    entries.push(context_edit_entry("edit2", Some("edit1"), "tres", None));
    assert_eq!(
        summarize(&entries, 1),
        ScenarioResult {
            first_kept_entry_id: Some("abad".to_string()),
            tokens_before: Some(1007),
            message_roles: vec!["user".to_string(), "assistant".to_string()],
            turn_prefix_roles: vec!["user".to_string()],
            is_split_turn: Some(true),
            previous_summary: None,
            read: read.clone(),
            written: written.clone(),
            edited: edited.clone(),
        }
    );

    // 3. 只省略 toolResult：后缀没有 omitted assistant，不前移。
    let mut entries = base_entries();
    entries.push(context_edit_entry("edit2", Some("tres"), "tres", None));
    assert_eq!(
        summarize(&entries, 1),
        ScenarioResult {
            first_kept_entry_id: Some("abad".to_string()),
            tokens_before: Some(1010),
            message_roles: vec!["user".to_string(), "assistant".to_string()],
            turn_prefix_roles: vec!["user".to_string()],
            is_split_turn: Some(true),
            previous_summary: None,
            read: read.clone(),
            written: written.clone(),
            edited: edited.clone(),
        }
    );

    // 4. 元数据回扫：cut 落在 a2，向前回扫跳过不可见 label。
    let entries = vec![
        entry_with_message("u1", None, create_user_message("one")),
        entry_with_message(
            "a1",
            Some("u1"),
            assistant_with_usage(
                vec![AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "answer one".to_string(),
                    text_signature: None,
                })],
                StopReason::Stop,
            ),
        ),
        label_entry("lab", Some("a1"), "u1"),
        entry_with_message("u2", Some("lab"), create_user_message("two")),
        entry_with_message(
            "a2",
            Some("u2"),
            assistant_with_usage(
                vec![AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "answer two".to_string(),
                    text_signature: None,
                })],
                StopReason::Stop,
            ),
        ),
    ];
    assert_eq!(
        summarize(&entries, 2),
        ScenarioResult {
            first_kept_entry_id: Some("a2".to_string()),
            tokens_before: Some(15),
            message_roles: vec!["user".to_string(), "assistant".to_string()],
            turn_prefix_roles: vec!["user".to_string()],
            is_split_turn: Some(true),
            previous_summary: None,
            read: read.clone(),
            written: written.clone(),
            edited: edited.clone(),
        }
    );

    // 5. cut 落在 u2 后回扫到 label：firstKeptEntryId 变为 label（不可见条目不贡献消息）。
    let entries = vec![
        entry_with_message("u1", None, create_user_message("one")),
        entry_with_message(
            "a1",
            Some("u1"),
            assistant_with_usage(
                vec![AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "answer one".to_string(),
                    text_signature: None,
                })],
                StopReason::Stop,
            ),
        ),
        label_entry("lab", Some("a1"), "u1"),
        entry_with_message("u2", Some("lab"), create_user_message("two")),
        entry_with_message(
            "a2",
            Some("u2"),
            assistant_with_usage(Vec::new(), StopReason::Stop),
        ),
    ];
    assert_eq!(
        summarize(&entries, 1),
        ScenarioResult {
            first_kept_entry_id: Some("lab".to_string()),
            tokens_before: Some(15),
            message_roles: Vec::new(),
            turn_prefix_roles: vec!["user".to_string(), "assistant".to_string()],
            is_split_turn: Some(true),
            previous_summary: None,
            read: read.clone(),
            written: written.clone(),
            edited: edited.clone(),
        }
    );

    // 6. 前一次压缩：只总结边界之后的保留消息。
    let previous_compaction_entries = |details: Option<JsonValue>, with_tool_call: bool| {
        vec![
            entry_with_message("u1", None, create_user_message(&"one ".repeat(30))),
            entry_with_message(
                "a1",
                Some("u1"),
                assistant_with_usage(
                    vec![AssistantContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: "answer one".to_string(),
                        text_signature: None,
                    })],
                    StopReason::Stop,
                ),
            ),
            compaction_entry_with_details("c1", Some("a1"), "u2", details),
            entry_with_message("u2", Some("c1"), create_user_message(&"two ".repeat(30))),
            entry_with_message(
                "a2",
                Some("u2"),
                if with_tool_call {
                    assistant_with_usage(
                        vec![tool_call_block("call-9", "edit", "history.ts")],
                        StopReason::ToolUse,
                    )
                } else {
                    assistant_with_usage(
                        vec![AssistantContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text: "answer two".to_string(),
                            text_signature: None,
                        })],
                        StopReason::Stop,
                    )
                },
            ),
            entry_with_message("u3", Some("a2"), create_user_message(&"three ".repeat(30))),
            entry_with_message(
                "a3",
                Some("u3"),
                assistant_with_usage(
                    vec![AssistantContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: "answer three".to_string(),
                        text_signature: None,
                    })],
                    StopReason::Stop,
                ),
            ),
        ]
    };
    assert_eq!(
        summarize(&previous_compaction_entries(None, false), 20),
        ScenarioResult {
            first_kept_entry_id: Some("u3".to_string()),
            tokens_before: Some(15),
            message_roles: vec!["user".to_string(), "assistant".to_string()],
            turn_prefix_roles: Vec::new(),
            is_split_turn: Some(false),
            previous_summary: Some("old summary".to_string()),
            read: read.clone(),
            written: written.clone(),
            edited: edited.clone(),
        }
    );

    // 7. 前一次压缩的 details：readFiles -> read，modifiedFiles -> edited；
    //    保留消息里的 edit toolCall 也进入 edited。
    assert_eq!(
        summarize(
            &previous_compaction_entries(
                Some(serde_json::json!({
                    "readFiles": ["prev-read.ts"],
                    "modifiedFiles": ["prev-modified.ts"],
                })),
                true,
            ),
            20
        ),
        ScenarioResult {
            first_kept_entry_id: Some("u3".to_string()),
            tokens_before: Some(15),
            message_roles: vec!["user".to_string(), "assistant".to_string()],
            turn_prefix_roles: Vec::new(),
            is_split_turn: Some(false),
            previous_summary: Some("old summary".to_string()),
            read: vec!["prev-read.ts".to_string()],
            written: Vec::new(),
            edited: vec!["prev-modified.ts".to_string(), "history.ts".to_string()],
        }
    );
}

// ============================================================================
// LLM integration tests (skipped without API key)
// ============================================================================

/// TS: it("should generate a compaction result for the large session")
#[tokio::test]
#[ignore = "requires live ANTHROPIC_OAUTH_TOKEN"]
async fn should_generate_a_compaction_result_for_the_large_session() {
    let entries = load_large_session_entries();
    let model = kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5").expect("model");
    let api_key = std::env::var("ANTHROPIC_OAUTH_TOKEN").expect("ANTHROPIC_OAUTH_TOKEN");

    let preparation =
        prepare_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS).expect("preparation");

    let compaction_result = compact(
        &preparation,
        &model,
        Some(&api_key),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert!(compaction_result.summary.len() > 100);
    assert!(!compaction_result.first_kept_entry_id.is_empty());
    assert!(compaction_result.tokens_before > 0);
}

/// TS: it("should produce valid session after compaction")
#[tokio::test]
#[ignore = "requires live ANTHROPIC_OAUTH_TOKEN"]
async fn should_produce_valid_session_after_compaction() {
    let entries = load_large_session_entries();
    let loaded = build_session_context(&entries, LeafId::Unspecified, None);
    let model = kiana_ai::compat::get_model("anthropic", "claude-sonnet-4-5").expect("model");
    let api_key = std::env::var("ANTHROPIC_OAUTH_TOKEN").expect("ANTHROPIC_OAUTH_TOKEN");

    let preparation =
        prepare_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS).expect("preparation");

    let compaction_result = compact(
        &preparation,
        &model,
        Some(&api_key),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    let last_entry = entries.last().expect("last entry");
    let compaction_entry = SessionEntry::Compaction(CompactionEntry {
        kind: CompactionType::Marker,
        id: "compaction-test-id".to_string(),
        parent_id: Some(last_entry.id().to_string()),
        timestamp: test_iso_timestamp(),
        summary: compaction_result.summary.clone(),
        first_kept_entry_id: compaction_result.first_kept_entry_id.clone(),
        tokens_before: compaction_result.tokens_before as f64,
        details: compaction_result.details.clone(),
        usage: compaction_result.usage.clone(),
        from_hook: None,
        system_message: None,
        first_kept_entry_index: None,
        raw_entry: None,
    });
    let mut new_entries = entries.clone();
    new_entries.push(compaction_entry);
    let reloaded = build_session_context(&new_entries, LeafId::Unspecified, None);

    assert!(reloaded.messages.len() < loaded.messages.len());
    assert_eq!(reloaded.messages[0].role(), "compactionSummary");
    assert!(custom_text(&reloaded.messages[0], "summary").contains(&compaction_result.summary));
}
