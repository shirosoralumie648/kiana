//! `openai-completions-thinking-as-text.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::openai_completions::{convert_messages, detect_compat};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, Model,
    ModelInputModality, StopReason, TextContent, ThinkingContent, Usage, UserContent, UserMessage,
    UserRole,
};
use serde_json::{Value, json};

fn build_model(base_url: &str) -> Model {
    Model {
        id: "repro-model".into(),
        name: "Repro Model".into(),
        api: "openai-completions".into(),
        provider: "repro-provider".into(),
        base_url: base_url.into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: Default::default(),
        context_window: Some(128_000),
        max_tokens: Some(4096),
        compat: Some(json!({ "requiresThinkingAsText": true })),
        ..Model::default()
    }
}

fn compat(model: &Model) -> kiana_ai::api::openai_completions::ResolvedOpenAICompletionsCompat {
    let mut compat = detect_compat(model);
    compat.supports_strict_mode = true;
    compat.requires_thinking_as_text = true;
    compat
}

fn build_assistant(content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "openai-completions".into(),
        provider: "repro-provider".into(),
        model: "repro-model".into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        timestamp: 2,
        ..AssistantMessage::default()
    }
}

fn build_context(assistant: AssistantMessage) -> kiana_ai::types::TranscriptContext {
    kiana_ai::types::TranscriptContext::from_normalized(Context {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            }),
            Message::Assistant(assistant),
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("continue".into()),
                timestamp: 3,
            }),
        ],
        ..Context::default()
    })
}

#[test]
fn serializes_same_model_thinking_plus_text_replay_as_assistant_text_parts() {
    let model = build_model("http://127.0.0.1:1");
    let messages = convert_messages(
        &model,
        &build_context(build_assistant(vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "internal reasoning".into(),
                ..Default::default()
            }),
            AssistantContentBlock::Text(TextContent {
                text: "visible answer".into(),
                ..Default::default()
            }),
        ])),
        &compat(&model),
        None,
    )
    .expect("convert");
    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" },
            ],
        })
    );
}

#[test]
fn serializes_same_model_thinking_only_replay_as_assistant_text_parts() {
    let model = build_model("http://127.0.0.1:1");
    let messages = convert_messages(
        &model,
        &build_context(build_assistant(vec![AssistantContentBlock::Thinking(
            ThinkingContent {
                thinking: "internal reasoning".into(),
                ..Default::default()
            },
        )])),
        &compat(&model),
        None,
    )
    .expect("convert");
    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "internal reasoning" }],
        })
    );
}

#[tokio::test]
async fn reaches_the_endpoint_when_replay_contains_both_thinking_and_text() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({
            "id": "chatcmpl-repro",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "repro-model",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" }, "finish_reason": null }],
        }),
        json!({
            "id": "chatcmpl-repro",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "repro-model",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        }),
    ]));
    let model = build_model("http://127.0.0.1:1");
    let context = build_context(build_assistant(vec![
        AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "internal reasoning".into(),
            ..Default::default()
        }),
        AssistantContentBlock::Text(TextContent {
            text: "visible answer".into(),
            ..Default::default()
        }),
    ]));
    let message = mock_http::run_completions(
        &model,
        &context,
        kiana_ai::api::openai_completions::OpenAICompletionsOptions {
            base: mock_http::stream_options(&mock, "test-key"),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);

    let body: Value = mock.last_body();
    assert_eq!(
        body["messages"][1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" },
            ],
        })
    );
}
