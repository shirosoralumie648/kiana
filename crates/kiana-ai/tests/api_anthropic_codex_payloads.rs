//! 黄金向量：与 TS 参照实现（`packages/ai/src/api/anthropic-messages.ts` /
//! `openai-codex-responses.ts`）用同一组输入捕获的 `onPayload` 请求体逐字节比对。
//!
//! fixture 由 `/home/ctyun/kiana-work/upstream-reference` 的 node 直接运行 TS 源生成
//! （`JSON.stringify(payload)` 的结果），因此同时校验键序与数字格式。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use harness::{MockHttpClient, MockResponse};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream as anthropic_stream};
use kiana_ai::api::openai_codex_responses::{OpenAICodexResponsesOptions, stream as codex_stream};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, CacheRetention, ImageContent, Model,
    ModelCost, ModelInputModality, ModelThinkingLevel, StopReason, TextContent, ThinkingContent,
    ThinkingLevelMap, Tool, ToolCall, ToolResultMessage, Transport, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/api-anthropic-codex/payloads.json"))
        .expect("fixture parses")
}

fn user_text(text: &str, timestamp: i64) -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp,
    })
}

fn user_blocks(blocks: Vec<UserContentBlock>, timestamp: i64) -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(blocks),
        timestamp,
    })
}

fn text(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        text: text.into(),
        ..Default::default()
    })
}

fn image(data: &str) -> UserContentBlock {
    UserContentBlock::Image(ImageContent {
        data: data.into(),
        mime_type: "image/png".into(),
        ..Default::default()
    })
}

fn assistant(
    api: &str,
    provider: &str,
    model: &str,
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
    timestamp: i64,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: api.into(),
        provider: provider.into(),
        model: model.into(),
        usage: Default::default(),
        stop_reason,
        timestamp,
        ..Default::default()
    }
}

fn tool_result(
    tool_call_id: &str,
    tool_name: &str,
    content: Vec<UserContentBlock>,
) -> kiana_ai::types::Message {
    kiana_ai::types::Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: tool_call_id.into(),
        tool_name: tool_name.into(),
        content,
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    })
}

fn capture_options(
    payloads: Arc<Mutex<Option<Value>>>,
    fetch: Arc<MockHttpClient>,
) -> AnthropicOptions {
    let mut base = harness::stream_options(fetch);
    base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *payloads.lock().unwrap() = Some(payload);
        None
    }));
    AnthropicOptions {
        base,
        ..Default::default()
    }
}

fn assert_payload(name: &str, actual: Option<Value>) {
    let actual = actual.expect("payload captured");
    let expected = fixture()[name].clone();
    let actual_text = kiana_json::js_object::stringify(&actual);
    let expected_text = kiana_json::js_object::stringify(&expected);
    assert_eq!(actual_text, expected_text, "payload {name} differs");
}

#[tokio::test]
async fn anthropic_basic_payload_matches_the_ts_reference() {
    let model = Model {
        id: "claude-test".into(),
        name: "Claude Test".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write: 6.25,
            ..Default::default()
        },
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        compat: Some(json!({ "supportsStrictTools": true, "forceAdaptiveThinking": true })),
        ..Default::default()
    };
    let tools = vec![
        Tool {
            name: "lookup".into(),
            description: "Look up a value".into(),
            parameters: json!({
                "type": "object",
                "properties": { "value": { "type": "string" }, "optional": { "type": "number" } },
                "required": ["value"],
            }),
            constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Config(
                kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: kiana_ai::types::StrictMode::Prefer,
                },
            )),
        },
        Tool {
            name: "ordinary".into(),
            description: "Ordinary".into(),
            parameters: json!({ "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] }),
            constrained_sampling: None,
        },
    ];
    let context = harness::context_with_tools(
        Some("You are helpful."),
        vec![
            user_text("first", 1),
            kiana_ai::types::Message::Assistant(assistant(
                "anthropic-messages",
                "anthropic",
                "claude-test",
                vec![
                    AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: "reason".into(),
                        thinking_signature: Some("sig".into()),
                        ..Default::default()
                    }),
                    AssistantContentBlock::Text(TextContent {
                        text: "answer".into(),
                        ..Default::default()
                    }),
                    AssistantContentBlock::ToolCall(ToolCall {
                        id: "call-1".into(),
                        name: "lookup".into(),
                        arguments: json!({ "value": "x" })
                            .as_object()
                            .unwrap()
                            .clone()
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    }),
                ],
                StopReason::ToolUse,
                2,
            )),
            tool_result("call-1", "lookup", vec![text("value"), image("AAAA")]),
            user_blocks(vec![text("second"), image("BBBB")], 4),
        ],
        tools,
    );
    let payloads = Arc::new(Mutex::new(None));
    let fetch = MockHttpClient::fixed(MockResponse::json(
        400,
        "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"stop\"}}",
    ));
    let mut options = capture_options(payloads.clone(), fetch);
    options.base.cache_retention = Some(CacheRetention::Long);
    options.thinking_enabled = Some(true);
    let _ = anthropic_stream(&model, &context, options).result().await;
    let captured = payloads.lock().unwrap().clone();
    assert_payload("anthropic_basic", captured);
}

#[tokio::test]
async fn anthropic_redacted_payload_matches_the_ts_reference() {
    let model = Model {
        id: "claude-test".into(),
        name: "Claude Test".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write: 6.25,
            ..Default::default()
        },
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        compat: Some(json!({ "supportsStrictTools": true, "forceAdaptiveThinking": true })),
        ..Default::default()
    };
    let context = harness::context(
        None,
        vec![
            user_text("hi", 1),
            kiana_ai::types::Message::Assistant(assistant(
                "anthropic-messages",
                "anthropic",
                "claude-test",
                vec![
                    AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: "[Reasoning redacted]".into(),
                        thinking_signature: Some("opaque".into()),
                        redacted: Some(true),
                        ..Default::default()
                    }),
                    AssistantContentBlock::Thinking(ThinkingContent {
                        thinking: String::new(),
                        thinking_signature: Some(String::new()),
                        ..Default::default()
                    }),
                    AssistantContentBlock::Text(TextContent {
                        text: "  ".into(),
                        ..Default::default()
                    }),
                ],
                StopReason::Stop,
                2,
            )),
            user_text("again", 3),
        ],
    );
    let payloads = Arc::new(Mutex::new(None));
    let fetch = MockHttpClient::fixed(MockResponse::json(
        400,
        "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"stop\"}}",
    ));
    let mut options = capture_options(payloads.clone(), fetch);
    options.base.cache_retention = Some(CacheRetention::None);
    let _ = anthropic_stream(&model, &context, options).result().await;
    let captured = payloads.lock().unwrap().clone();
    assert_payload("anthropic_redacted", captured);
}

#[tokio::test]
async fn anthropic_mid_convo_payload_matches_the_ts_reference() {
    let model = Model {
        id: "claude-fable-5-1".into(),
        name: "Claude Test".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write: 6.25,
            ..Default::default()
        },
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        thinking_level_map: Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Off, None),
            (ModelThinkingLevel::Minimal, Some("low".into())),
            (ModelThinkingLevel::Low, Some("low".into())),
            (ModelThinkingLevel::Medium, Some("medium".into())),
            (ModelThinkingLevel::High, Some("high".into())),
            (ModelThinkingLevel::Max, Some("max".into())),
        ])),
        compat: Some(json!({
            "forceAdaptiveThinking": true,
            "supportsMidConvoEffort": true,
            "supportsStrictTools": true,
        })),
        ..Default::default()
    };
    let mut assistant_message = assistant(
        "anthropic-messages",
        "anthropic",
        "claude-fable-5-1",
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "reason".into(),
                thinking_signature: Some("sig".into()),
                ..Default::default()
            }),
            AssistantContentBlock::Text(TextContent {
                text: "answer".into(),
                ..Default::default()
            }),
        ],
        StopReason::Stop,
        2,
    );
    assistant_message.provider_thinking_level = Some("low".into());
    let context = harness::context(
        None,
        vec![
            user_text("one", 1),
            kiana_ai::types::Message::Assistant(assistant_message),
            user_text("two", 3),
        ],
    );
    let payloads = Arc::new(Mutex::new(None));
    let fetch = MockHttpClient::fixed(MockResponse::json(
        400,
        "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"stop\"}}",
    ));
    let mut options = capture_options(payloads.clone(), fetch);
    options.base.cache_retention = Some(CacheRetention::Short);
    options.thinking_enabled = Some(true);
    options.effort = Some("high".into());
    let _ = anthropic_stream(&model, &context, options).result().await;
    let captured = payloads.lock().unwrap().clone();
    assert_payload("anthropic_mid_convo", captured);
}

#[tokio::test]
async fn codex_body_matches_the_ts_reference() {
    let model = Model {
        id: "gpt-5.1-codex".into(),
        name: "GPT-5.1 Codex".into(),
        api: "openai-codex-responses".into(),
        provider: "openai-codex".into(),
        base_url: "https://chatgpt.com/backend-api".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            ..Default::default()
        },
        context_window: Some(400_000),
        max_tokens: Some(128_000),
        thinking_level_map: Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Xhigh, Some("xhigh".into())),
            (ModelThinkingLevel::Max, Some("max".into())),
        ])),
        ..Default::default()
    };
    let reasoning_signature = json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [],
        "encrypted_content": "enc",
    })
    .to_string();
    let text_signature = json!({ "v": 1, "id": "msg_1", "phase": "final_answer" }).to_string();
    let mut assistant_message = assistant(
        "openai-codex-responses",
        "openai-codex",
        "gpt-5.1-codex",
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "reason".into(),
                thinking_signature: Some(reasoning_signature),
                ..Default::default()
            }),
            AssistantContentBlock::Text(TextContent {
                text: "answer".into(),
                text_signature: Some(text_signature),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call-1|fc_1".into(),
                name: "lookup".into(),
                arguments: json!({ "value": "x" })
                    .as_object()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .collect(),
                namespace: Some("ns".into()),
                ..Default::default()
            }),
        ],
        StopReason::ToolUse,
        2,
    );
    assistant_message.response_id = Some("resp_prev".into());
    let tools = vec![
        Tool {
            name: "lookup".into(),
            description: "Look up a value".into(),
            parameters: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
            }),
            constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Config(
                kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: kiana_ai::types::StrictMode::Prefer,
                },
            )),
        },
        Tool {
            name: "ordinary".into(),
            description: "Ordinary".into(),
            parameters: json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "required": ["q"],
            }),
            constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Disabled(false)),
        },
    ];
    let context = harness::context_with_tools(
        Some("You are a helpful assistant."),
        vec![
            user_text("first", 1),
            kiana_ai::types::Message::Assistant(assistant_message),
            tool_result("call-1|fc_1", "lookup", vec![text("value")]),
            user_text("next", 4),
        ],
        tools,
    );
    let payloads = Arc::new(Mutex::new(None));
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let payloads_for_callback = payloads.clone();
    let token_payload = base64::engine::general_purpose::STANDARD.encode(
        json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc_test" } }).to_string(),
    );
    let token = format!("aaa.{token_payload}.bbb");
    let mut base = harness::stream_options(fetch);
    base.api_key = Some(token);
    base.transport = Some(Transport::Sse);
    base.session_id = Some("session-1".into());
    base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *payloads_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let options = OpenAICodexResponsesOptions {
        base,
        reasoning_effort: Some("xhigh".into()),
        ..Default::default()
    };
    let _ = codex_stream(&model, &context, options).result().await;
    let captured = payloads.lock().unwrap().clone();
    assert_payload("codex_body", captured);
}
