//! 畸形会话输入的回归测试：会话文件不做字段校验，`null`/缺省值会以
//! `null`/`undefined`/`NaN` 的形式影响压缩逻辑。这里锁定当前稳定边界行为，改动需同步测试。
//!
//! 期望值由 TS 参考实现（`packages/coding-agent/src/core/compaction`）生成：
//! - `compaction.summary = null` → `previousSummary === null`，按“无前次摘要”处理；
//! - `compaction.summary` 缺失 → `previousSummary === undefined`，同上；
//! - `compaction.summary = ""` → `previousSummary === ""`（`??` 不触发回退）；
//! - assistant `usage` 缺分量 / `totalTokens` 为负或非数值 → usage 无效（回退 chars/4）；
//! - `branch_summary.summary = 123` → `estimateTokens` 得到 `NaN`，token 预算失效。
//!
//! TS: `packages/coding-agent/src/core/compaction/compaction.ts`

use std::sync::{Arc, Mutex};

use kiana_agent_core::types::{AgentMessage, StreamFn};
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, DoneReason,
    JsonObject, JsonValue, Message, Model, ModelCost, ModelInputModality, StopReason, TextContent,
    TextKind, TranscriptContext, Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_compaction::{
    CompactionPreparation, CompactionSettings, ContextUsageEstimate, DEFAULT_COMPACTION_SETTINGS,
    FileOperations, compact, estimate_context_tokens, prepare_branch_entries, prepare_compaction,
};
use kiana_session_core::session_manager::{FileEntry, SessionEntry, parse_session_entries};

fn parse_entries(jsonl: &str) -> Vec<SessionEntry> {
    parse_session_entries(jsonl)
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect()
}

fn custom_message(value: JsonValue) -> AgentMessage {
    let object: JsonObject = serde_json::from_value(value).expect("JSON object");
    AgentMessage::Custom(Box::new(object))
}

fn assistant_with_usage(usage: JsonValue) -> AgentMessage {
    custom_message(serde_json::json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "hi" }],
        "api": "a",
        "provider": "p",
        "model": "m",
        "usage": usage,
        "stopReason": "stop",
        "timestamp": 1,
    }))
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 1,
    }))
}

fn create_model() -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(8192),
        ..Model::default()
    }
}

fn mock_summary_response(text: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "test-model".to_string(),
        usage: Usage {
            input: 10,
            output: 10,
            total_tokens: 20,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp: 0,
        ..AssistantMessage::default()
    }
}

/// 记录每个摘要请求的上下文；同步推入一条 `done` 事件。
fn recording_stream_fn(
    response: AssistantMessage,
) -> (StreamFn, Arc<Mutex<Vec<TranscriptContext>>>) {
    let contexts: Arc<Mutex<Vec<TranscriptContext>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&contexts);
    let stream_fn: StreamFn = Arc::new(move |_model, context, _options| {
        sink.lock().expect("contexts").push(context.clone());
        let stream = create_assistant_message_event_stream();
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(response.clone()),
        });
        Box::pin(async move { Ok(stream) })
    });
    (stream_fn, contexts)
}

fn captured_prompts(contexts: &Arc<Mutex<Vec<TranscriptContext>>>) -> Vec<String> {
    contexts
        .lock()
        .expect("contexts")
        .iter()
        .map(|context| {
            let context = context.as_context();
            match context.messages.last() {
                Some(Message::User(user)) => match &user.content {
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
                _ => String::new(),
            }
        })
        .collect()
}

/// 带一条 `compaction` 条目的单分支会话；`compaction` 行由调用方给出。
fn compaction_session(compaction_line: &str) -> Vec<SessionEntry> {
    let jsonl = [
        r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2025-01-01T00:00:00.000Z","message":{"role":"user","content":"one","timestamp":1}}"#,
        compaction_line,
        r#"{"type":"message","id":"u2","parentId":"c1","timestamp":"2025-01-01T00:00:02.000Z","message":{"role":"user","content":"two","timestamp":2}}"#,
        r#"{"type":"message","id":"a2","parentId":"u2","timestamp":"2025-01-01T00:00:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"answer"}],"api":"a","provider":"p","model":"m","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":3}}"#,
    ]
    .join("\n");
    parse_entries(&jsonl)
}

fn prepare_with_keep_one(entries: &[SessionEntry]) -> Option<CompactionPreparation> {
    prepare_compaction(
        entries,
        &CompactionSettings {
            keep_recent_tokens: 1,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
}

/// TS: `previousSummary = (sourceEntry as CompactionEntry).summary`；`null` → `null`。
#[test]
fn compaction_entry_with_null_summary_has_no_previous_summary() {
    let entries = compaction_session(
        r#"{"type":"compaction","id":"c1","parentId":"u1","timestamp":"2025-01-01T00:00:01.000Z","summary":null,"firstKeptEntryId":"u2","tokensBefore":10}"#,
    );
    let preparation = prepare_with_keep_one(&entries).expect("preparation");
    assert_eq!(preparation.previous_summary, None);
}

/// TS: 缺省 `summary` 是 `undefined`；与 `null` 一样按“无前次摘要”处理。
#[test]
fn compaction_entry_with_missing_summary_has_no_previous_summary() {
    let entries = compaction_session(
        r#"{"type":"compaction","id":"c1","parentId":"u1","timestamp":"2025-01-01T00:00:01.000Z","firstKeptEntryId":"u2","tokensBefore":10}"#,
    );
    let preparation = prepare_with_keep_one(&entries).expect("preparation");
    assert_eq!(preparation.previous_summary, None);
}

/// 合法空串保持 `Some("")`：TS 的 `??` 不把空串当作缺省（split-turn 的历史文本为空）。
#[test]
fn compaction_entry_with_empty_summary_keeps_empty_previous_summary() {
    let entries = compaction_session(
        r#"{"type":"compaction","id":"c1","parentId":"u1","timestamp":"2025-01-01T00:00:01.000Z","summary":"","firstKeptEntryId":"u2","tokensBefore":10}"#,
    );
    let preparation = prepare_with_keep_one(&entries).expect("preparation");
    assert_eq!(preparation.previous_summary.as_deref(), Some(""));
}

/// TS: `previousSummary ? UPDATE : SUMMARIZATION` —— null 是无前次摘要，用初始 prompt，
/// 且不拼 `<previous-summary>` 块。
#[tokio::test]
async fn null_previous_summary_uses_initial_summarization_prompt() {
    let preparation = CompactionPreparation {
        first_kept_entry_id: "keep".to_string(),
        messages_to_summarize: vec![user_message("hi")],
        turn_prefix_messages: Vec::new(),
        is_split_turn: false,
        tokens_before: 10,
        previous_summary: None,
        file_ops: FileOperations::default(),
        settings: DEFAULT_COMPACTION_SETTINGS,
    };
    let (stream_fn, contexts) = recording_stream_fn(mock_summary_response("TP"));

    compact(
        &preparation,
        &create_model(),
        Some("key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    let prompts = captured_prompts(&contexts);
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains(
            "The messages above are a conversation to summarize. Create a structured context checkpoint summary"
        ),
        "expected the initial summarization prompt, got: {}",
        prompts[0]
    );
    assert!(!prompts[0].contains("<previous-summary>"));
    assert!(!prompts[0].contains("NEW conversation messages"));
}

/// TS: `let historyText = previousSummary ?? "No prior history."` —— null 触发回退。
#[tokio::test]
async fn split_turn_with_null_summary_uses_no_prior_history() {
    let entries = compaction_session(
        r#"{"type":"compaction","id":"c1","parentId":"u1","timestamp":"2025-01-01T00:00:01.000Z","summary":null,"firstKeptEntryId":"u2","tokensBefore":10}"#,
    );
    let preparation = prepare_with_keep_one(&entries).expect("preparation");
    assert_eq!(preparation.previous_summary, None);
    assert!(preparation.is_split_turn);
    assert!(preparation.messages_to_summarize.is_empty());
    assert!(!preparation.turn_prefix_messages.is_empty());

    let (stream_fn, _contexts) = recording_stream_fn(mock_summary_response("TP"));
    let result = compact(
        &preparation,
        &create_model(),
        Some("key"),
        None,
        None,
        None,
        None,
        Some(stream_fn),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compaction");

    assert!(
        result
            .summary
            .starts_with("No prior history.\n\n---\n\n**Turn Context (split turn):**\n\nTP"),
        "got: {}",
        result.summary
    );
}

/// TS: `calculateContextTokens(usage) > 0` 才把 assistant usage 当作锚点。缺分量的求和
/// 是 `NaN`；真值但为负/非数值的 `totalTokens` 比较为 false。四种畸形 usage 都必须被
/// 忽略（估算回退到 chars/4）。
#[test]
fn assistant_usage_with_missing_or_invalid_total_is_ignored() {
    let malformed = [
        serde_json::json!({ "input": 500, "totalTokens": 0 }),
        serde_json::json!({ "input": 500 }),
        serde_json::json!({ "input": 1, "output": 1, "cacheRead": 1, "cacheWrite": 1, "totalTokens": -5 }),
        serde_json::json!({ "input": 1, "output": 1, "cacheRead": 1, "cacheWrite": 1, "totalTokens": "abc" }),
    ];
    for usage in malformed {
        let estimate = estimate_context_tokens(&[
            assistant_with_usage(usage.clone()),
            user_message("hello world!"),
        ]);
        assert_eq!(
            estimate,
            ContextUsageEstimate {
                tokens: 4,
                usage_tokens: 0,
                trailing_tokens: 4,
                last_usage_index: None,
            },
            "usage {usage} must be ignored"
        );
    }
}

/// 对照：`totalTokens: 0` 但四个分量齐全时回退到分量求和，usage 仍有效。
#[test]
fn assistant_usage_with_all_components_still_anchors() {
    let estimate = estimate_context_tokens(&[
        assistant_with_usage(
            serde_json::json!({ "input": 1, "output": 1, "cacheRead": 1, "cacheWrite": 1, "totalTokens": 0 }),
        ),
        user_message("hello world!"),
    ]);
    assert_eq!(
        estimate,
        ContextUsageEstimate {
            tokens: 7,
            usage_tokens: 4,
            trailing_tokens: 3,
            last_usage_index: Some(0),
        }
    );
}

/// 带数字 `summary` 的 `branch_summary` 条目。
fn numeric_summary_session(summary: &str) -> Vec<SessionEntry> {
    let jsonl = [
        r#"{"type":"message","id":"u1","parentId":null,"timestamp":"2025-01-01T00:00:00.000Z","message":{"role":"user","content":"hello","timestamp":1}}"#,
        summary,
        r#"{"type":"message","id":"u2","parentId":"b1","timestamp":"2025-01-01T00:00:02.000Z","message":{"role":"user","content":"next","timestamp":2}}"#,
    ]
    .join("\n");
    parse_entries(&jsonl)
}

/// TS: `chars = message.summary.length` —— 数字 `summary` 的 `.length` 是 `undefined`，
/// 估算得到 `NaN`；`totalTokens + NaN > tokenBudget` 恒为 false，预算失效，所有条目保留。
#[test]
fn branch_summary_with_non_string_summary_disables_token_budget() {
    let entries = numeric_summary_session(
        r#"{"type":"branch_summary","id":"b1","parentId":"u1","timestamp":"2025-01-01T00:00:01.000Z","summary":123,"fromId":"x"}"#,
    );
    for budget in [0, 1, 99] {
        let preparation = prepare_branch_entries(&entries, budget);
        assert_eq!(
            preparation
                .messages
                .iter()
                .map(AgentMessage::role)
                .collect::<Vec<_>>(),
            vec!["user", "branchSummary", "user"],
            "budget {budget}"
        );
        assert!(
            preparation.total_tokens.is_nan(),
            "budget {budget}: expected NaN, got {}",
            preparation.total_tokens
        );
    }
}

/// 对照：字符串摘要按 UTF-16 长度计 token，预算正常生效（只保留最新的 user 消息）。
#[test]
fn branch_summary_with_string_summary_still_applies_token_budget() {
    let entries = numeric_summary_session(
        r#"{"type":"branch_summary","id":"b1","parentId":"u1","timestamp":"2025-01-01T00:00:01.000Z","summary":"123","fromId":"x"}"#,
    );
    let preparation = prepare_branch_entries(&entries, 1);
    assert_eq!(
        preparation
            .messages
            .iter()
            .map(AgentMessage::role)
            .collect::<Vec<_>>(),
        vec!["user"]
    );
    assert_eq!(preparation.total_tokens, 1.0);
}
