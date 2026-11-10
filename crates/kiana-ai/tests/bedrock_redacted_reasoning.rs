//! `test/bedrock-redacted-reasoning.test.ts` 的移植。

use serde_json::{Value, json};

use kiana_ai::api::bedrock_converse_stream::{
    BedrockOptions, build_command_input, stream_with_items,
};
use kiana_ai::types::{
    AssistantContentBlock, CacheRetention, Context, Message, Model, ModelInputModality,
    StreamOptions, UserContent, UserMessage, UserRole,
};

const REDACTED_BASE64: &str = "cnNuXzVaVnJpZjRKMGJYSXFtV2RsZWRqN1FJRmVOaWtSUWJF";

fn redacted_bytes() -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(REDACTED_BASE64)
        .expect("base64")
}

fn gpt_model() -> Model {
    Model {
        id: "global.openai.gpt-5.6-terra".into(),
        name: "GPT-5.6 Terra (Global)".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.ap-northeast-1.amazonaws.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: kiana_ai::types::ModelCost {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 0.0,
            ..Default::default()
        },
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    kiana_ai::utils::transcript::normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

fn options() -> BedrockOptions {
    BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn run(items: Vec<Value>) -> kiana_ai::types::AssistantMessage {
    let model = gpt_model();
    stream_with_items(
        &model,
        &context(),
        options(),
        None,
        items.into_iter().map(Ok).collect(),
    )
    .result()
    .await
    .expect("result")
}

fn redacted_events() -> Vec<Value> {
    vec![
        json!({"messageStart": {"role": "assistant"}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0,
            "delta": {"reasoningContent": {"redactedContent": redacted_bytes()}}}}),
        json!({"contentBlockStop": {"contentBlockIndex": 0}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 1, "delta": {"text": "done"}}}),
        json!({"contentBlockStop": {"contentBlockIndex": 1}}),
        json!({"messageStop": {"stopReason": "end_turn"}}),
    ]
}

#[tokio::test]
async fn does_not_fail_the_stream_when_reasoning_arrives_as_redacted_content() {
    let response = run(redacted_events()).await;
    assert_ne!(
        response.stop_reason,
        kiana_ai::types::StopReason::Error,
        "{:?}",
        response.error_message
    );
    let kinds: Vec<&str> = response
        .content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Thinking(_) => "thinking",
            AssistantContentBlock::Text(_) => "text",
            AssistantContentBlock::ToolCall(_) => "toolCall",
        })
        .collect();
    assert_eq!(kinds, vec!["thinking", "text"]);
    match &response.content[1] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "done"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn preserves_the_encrypted_reasoning_payload_on_the_assistant_message() {
    let response = run(redacted_events()).await;
    let thinking = response
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("thinking");
    assert_eq!(thinking.redacted, Some(true));
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(REDACTED_BASE64)
    );
    // 流式 scratch state 不会留在块上（类型化结构没有该字段）。
    let serialized =
        serde_json::to_value(AssistantContentBlock::Thinking(thinking.clone())).unwrap();
    assert!(serialized.get("redactedChunks").is_none());
}

#[tokio::test]
async fn encodes_the_payload_when_the_stream_never_sends_content_block_stop() {
    let response = run(vec![
        json!({"messageStart": {"role": "assistant"}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0,
            "delta": {"reasoningContent": {"redactedContent": redacted_bytes()}}}}),
        json!({"messageStop": {"stopReason": "end_turn"}}),
    ])
    .await;
    let thinking = response
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("thinking");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(REDACTED_BASE64)
    );
}

#[tokio::test]
async fn joins_encrypted_reasoning_split_across_deltas() {
    let bytes = redacted_bytes();
    let (head, tail) = bytes.split_at(7);
    let response = run(vec![
        json!({"messageStart": {"role": "assistant"}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0,
            "delta": {"reasoningContent": {"redactedContent": head}}}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0,
            "delta": {"reasoningContent": {"redactedContent": tail}}}}),
        json!({"contentBlockStop": {"contentBlockIndex": 0}}),
        json!({"messageStop": {"stopReason": "end_turn"}}),
    ])
    .await;
    let thinking = response
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("thinking");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(REDACTED_BASE64)
    );
    assert_eq!(thinking.thinking, "[Reasoning redacted]");
}

fn assistant_message(
    model: &Model,
    content: Vec<AssistantContentBlock>,
    stop: kiana_ai::types::StopReason,
) -> Message {
    Message::Assistant(kiana_ai::types::AssistantMessage {
        role: kiana_ai::types::AssistantRole::Assistant,
        content,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: kiana_ai::types::Usage::default(),
        stop_reason: stop,
        timestamp: 1,
        ..Default::default()
    })
}

fn payload_for(context: Context, model: &Model) -> Value {
    build_command_input(
        model,
        &kiana_ai::utils::transcript::normalize_context(&context),
        CacheRetention::None,
        &options(),
        false,
    )
    .expect("payload")
}

#[test]
fn replays_redacted_reasoning_as_reasoning_content_redacted_content() {
    let model = gpt_model();
    let context = Context {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            }),
            assistant_message(
                &model,
                vec![
                    AssistantContentBlock::Thinking(kiana_ai::types::ThinkingContent {
                        kind_thinking: kiana_ai::types::ThinkingKind::Thinking,
                        thinking: String::new(),
                        thinking_signature: Some(REDACTED_BASE64.into()),
                        redacted: Some(true),
                    }),
                    AssistantContentBlock::Text(kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "done".into(),
                        text_signature: None,
                    }),
                ],
                kiana_ai::types::StopReason::Stop,
            ),
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("continue".into()),
                timestamp: 1,
            }),
        ],
        ..Context::default()
    };
    let payload = payload_for(context, &model);
    let assistant = payload["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == json!("assistant"))
        .cloned()
        .expect("assistant");
    assert_eq!(
        assistant["content"],
        json!([
            {"reasoningContent": {"redactedContent": REDACTED_BASE64}},
            {"text": "done"}
        ])
    );
}

#[test]
fn replays_redacted_reasoning_before_the_tool_use_block_it_belongs_to() {
    let model = gpt_model();
    let context = Context {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("read the file".into()),
                timestamp: 1,
            }),
            assistant_message(
                &model,
                vec![
                    AssistantContentBlock::Thinking(kiana_ai::types::ThinkingContent {
                        kind_thinking: kiana_ai::types::ThinkingKind::Thinking,
                        thinking: String::new(),
                        thinking_signature: Some(REDACTED_BASE64.into()),
                        redacted: Some(true),
                    }),
                    AssistantContentBlock::ToolCall(kiana_ai::types::ToolCall {
                        kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
                        id: "tool-1".into(),
                        name: "read".into(),
                        arguments: [("path".to_string(), json!("/tmp/a.txt"))]
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    }),
                ],
                kiana_ai::types::StopReason::ToolUse,
            ),
            Message::ToolResult(kiana_ai::types::ToolResultMessage {
                role: kiana_ai::types::ToolResultRole::ToolResult,
                tool_call_id: "tool-1".into(),
                tool_name: "read".into(),
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "file body".into(),
                        text_signature: None,
                    },
                )],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 1,
                duration_ms: None,
            }),
        ],
        ..Context::default()
    };
    let payload = payload_for(context, &model);
    let assistant = payload["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == json!("assistant"))
        .cloned()
        .expect("assistant");
    assert_eq!(
        assistant["content"],
        json!([
            {"reasoningContent": {"redactedContent": REDACTED_BASE64}},
            {"toolUse": {"toolUseId": "tool-1", "name": "read", "input": {"path": "/tmp/a.txt"}}}
        ])
    );
}
