//! `openai-completions-reasoning-details.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelInputModality, Tool,
};
use serde_json::{Value, json};

fn reasoning_detail() -> Value {
    json!({ "type": "reasoning.encrypted", "id": "call_1", "data": "encrypted-signature" })
}

fn signed_reasoning_text_detail() -> Value {
    json!({
        "type": "reasoning.text",
        "text": "I should call the read tool.",
        "signature": "sha256:signed-text",
        "id": "reasoning-text-1",
        "format": "anthropic-claude-v1",
        "index": 0,
    })
}

fn reasoning_summary_detail() -> Value {
    json!({
        "type": "reasoning.summary",
        "summary": "Decided to inspect the requested file.",
        "id": "reasoning-summary-1",
        "format": "anthropic-claude-v1",
        "index": 1,
    })
}

fn read_tool() -> Tool {
    Tool {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
        constrained_sampling: None,
    }
}

fn model() -> Model {
    Model {
        id: "google/gemini-test".into(),
        name: "Gemini Test".into(),
        api: "openai-completions".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(100_000),
        max_tokens: Some(4096),
        ..Model::default()
    }
}

fn chunk(delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-test",
        "model": "google/gemini-test",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
    })
}

fn tool_call_chunk() -> Value {
    chunk(
        json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "read", "arguments": "{\"path\":\"README.md\"}" },
            }],
        }),
        None,
    )
}

async fn run(mock: &mock_http::MockHttp, messages: Vec<Message>) -> AssistantMessage {
    let context = mock_http::context_with(&Context {
        messages,
        tools: Some(vec![read_tool()]),
        ..Context::default()
    });
    mock_http::run_completions(
        &model(),
        &context,
        OpenAICompletionsOptions {
            base: kiana_ai::types::StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
}

fn assistant_payload(payload: &Value) -> Option<&Value> {
    payload["messages"]
        .as_array()?
        .iter()
        .find(|message| message["role"] == json!("assistant"))
}

#[tokio::test]
async fn streaming_prefers_reasoning_content_over_reasoning_in_the_same_delta() {
    // TS 的流式搜索顺序是 ["reasoning_content", "reasoning", "reasoning_text"]
    // （`openai-completions.ts:582`），chutes.ai 会同时返回两个字段；签名决定回放字段。
    let mock = mock_http::MockHttp::new(vec![
        mock_http::MockResponse::sse(&[
            chunk(
                json!({
                    "reasoning_content": "from reasoning_content",
                    "reasoning": "from reasoning",
                }),
                None,
            ),
            chunk(json!({ "content": "answer" }), None),
            chunk(json!({}), Some("stop")),
        ]),
        mock_http::MockResponse::sse(&[
            chunk(json!({ "content": "ok" }), None),
            chunk(json!({}), Some("stop")),
        ]),
    ]);
    let message = run(&mock, Vec::new()).await;
    let thinking = message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("thinking block");
    assert_eq!(thinking.thinking, "from reasoning_content");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some("reasoning_content")
    );

    let _ = run(&mock, vec![Message::Assistant(message)]).await;
    let body = mock.last_body();
    let payload = assistant_payload(&body).unwrap();
    assert_eq!(
        payload["reasoning_content"],
        json!("from reasoning_content")
    );
    assert!(payload.get("reasoning").is_none());
}

#[tokio::test]
async fn preserves_reasoning_details_in_the_thinking_signature() {
    let mock = mock_http::MockHttp::new(vec![
        mock_http::MockResponse::sse(&[
            chunk(json!({ "reasoning_details": [reasoning_detail()] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ]),
        mock_http::MockResponse::sse(&[
            chunk(json!({ "content": "ok" }), None),
            chunk(json!({}), Some("stop")),
        ]),
    ]);
    let message = run(&mock, Vec::new()).await;
    let thinking = message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .expect("thinking block");
    assert_eq!(thinking.thinking, "");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(
            serde_json::to_string(&vec![reasoning_detail()])
                .unwrap()
                .as_str()
        )
    );
    let tool_call = message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool_call.id, "call_1");
    assert_eq!(tool_call.arguments["path"], json!("README.md"));

    let _ = run(&mock, vec![Message::Assistant(message)]).await;
    assert_eq!(
        assistant_payload(&mock.last_body()).unwrap()["reasoning_details"],
        json!([reasoning_detail()])
    );
}

#[tokio::test]
async fn falls_back_to_encrypted_tool_call_signatures_for_older_stored_assistant_messages() {
    let mock = mock_http::MockHttp::new(vec![
        mock_http::MockResponse::sse(&[
            chunk(json!({ "reasoning_details": [reasoning_detail()] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ]),
        mock_http::MockResponse::sse(&[
            chunk(json!({ "content": "ok" }), None),
            chunk(json!({}), Some("stop")),
        ]),
    ]);
    let mut message = run(&mock, Vec::new()).await;
    message
        .content
        .retain(|block| !matches!(block, AssistantContentBlock::Thinking(_)));
    let tool_signature = serde_json::to_string(&reasoning_detail()).unwrap();
    for block in &mut message.content {
        if let AssistantContentBlock::ToolCall(tool_call) = block {
            tool_call.thought_signature = Some(tool_signature.clone());
        }
    }

    let _ = run(&mock, vec![Message::Assistant(message)]).await;
    assert_eq!(
        assistant_payload(&mock.last_body()).unwrap()["reasoning_details"],
        json!([reasoning_detail()])
    );
}

#[tokio::test]
async fn preserves_signed_text_and_summary_reasoning_details_in_their_original_sequence() {
    let mock = mock_http::MockHttp::new(vec![
        mock_http::MockResponse::sse(&[
            chunk(
                json!({
                    "reasoning": "I should call the read tool.",
                    "reasoning_details": [signed_reasoning_text_detail()],
                }),
                None,
            ),
            chunk(
                json!({ "reasoning_details": [reasoning_detail(), reasoning_summary_detail()] }),
                None,
            ),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ]),
        mock_http::MockResponse::sse(&[
            chunk(json!({ "content": "ok" }), None),
            chunk(json!({}), Some("stop")),
        ]),
    ]);
    let message = run(&mock, Vec::new()).await;
    let expected = vec![
        signed_reasoning_text_detail(),
        reasoning_detail(),
        reasoning_summary_detail(),
    ];
    let thinking = message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .unwrap();
    assert_eq!(thinking.thinking, "I should call the read tool.");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(serde_json::to_string(&expected).unwrap().as_str())
    );

    let _ = run(&mock, vec![Message::Assistant(message)]).await;
    let body = mock.last_body();
    let payload = assistant_payload(&body).unwrap();
    assert_eq!(payload["reasoning_details"], json!(expected));
    assert!(payload.get("reasoning").is_none());
}

#[tokio::test]
async fn merges_consecutive_text_and_summary_reasoning_details_deltas_before_replay() {
    let text_delta = json!({ "type": "reasoning.text", "text": "The", "index": 0 });
    let text_delta_with_signature = json!({
        "type": "reasoning.text",
        "text": " user wants the time.",
        "signature": "sha256:text-signature",
        "format": "openai-responses-v1",
        "index": 0,
    });
    let summary_delta = json!({ "type": "reasoning.summary", "summary": "Looked", "index": 0 });
    let summary_delta_with_format = json!({
        "type": "reasoning.summary",
        "summary": " up time.",
        "format": "openai-responses-v1",
        "index": 0,
    });
    let later_summary_delta = json!({
        "type": "reasoning.summary",
        "summary": "After encrypted block.",
        "format": "openai-responses-v1",
        "index": 0,
    });
    let expected = json!([
        {
            "type": "reasoning.text",
            "text": "The user wants the time.",
            "index": 0,
            "signature": "sha256:text-signature",
            "format": "openai-responses-v1",
        },
        {
            "type": "reasoning.summary",
            "summary": "Looked up time.",
            "index": 0,
            "format": "openai-responses-v1",
        },
        reasoning_detail(),
        later_summary_delta,
    ]);
    let mock = mock_http::MockHttp::new(vec![
        mock_http::MockResponse::sse(&[
            chunk(json!({ "reasoning_details": [text_delta] }), None),
            chunk(
                json!({ "reasoning_details": [text_delta_with_signature] }),
                None,
            ),
            chunk(json!({ "reasoning_details": [summary_delta] }), None),
            chunk(
                json!({ "reasoning_details": [summary_delta_with_format] }),
                None,
            ),
            chunk(json!({ "reasoning_details": [reasoning_detail()] }), None),
            chunk(json!({ "reasoning_details": [later_summary_delta] }), None),
            tool_call_chunk(),
            chunk(json!({}), Some("tool_calls")),
        ]),
        mock_http::MockResponse::sse(&[
            chunk(json!({ "content": "ok" }), None),
            chunk(json!({}), Some("stop")),
        ]),
    ]);
    let message = run(&mock, Vec::new()).await;
    let thinking = message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some(serde_json::to_string(&expected).unwrap().as_str())
    );

    let _ = run(&mock, vec![Message::Assistant(message)]).await;
    assert_eq!(
        assistant_payload(&mock.last_body()).unwrap()["reasoning_details"],
        expected
    );
}

#[tokio::test]
async fn places_thinking_before_text_when_one_delta_carries_reasoning_content_and_content() {
    let mock = mock_http::MockHttp::new(vec![mock_http::MockResponse::sse(&[chunk(
        json!({ "role": "assistant", "reasoning_content": "thinking", "content": "answer" }),
        Some("stop"),
    )])]);
    let message = run(&mock, Vec::new()).await;
    assert_eq!(message.content.len(), 2);
    assert!(matches!(
        message.content[0],
        AssistantContentBlock::Thinking(_)
    ));
    assert!(matches!(message.content[1], AssistantContentBlock::Text(_)));
}

#[tokio::test]
async fn places_thinking_before_text_when_one_delta_carries_reasoning_details_and_content() {
    let mock = mock_http::MockHttp::new(vec![mock_http::MockResponse::sse(&[chunk(
        json!({ "reasoning_details": [reasoning_detail()], "content": "answer" }),
        Some("stop"),
    )])]);
    let message = run(&mock, Vec::new()).await;
    let kinds: Vec<&str> = message
        .content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(_) => "text",
            AssistantContentBlock::Thinking(_) => "thinking",
            AssistantContentBlock::ToolCall(_) => "toolCall",
        })
        .collect();
    assert_eq!(kinds, ["thinking", "text"]);
}
