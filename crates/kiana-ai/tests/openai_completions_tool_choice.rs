//! `openai-completions-tool-choice.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_completions::{OpenAICompletionsOptions, convert_messages};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, Model,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingContent, ThinkingLevel, Tool, ToolCall,
    ToolResultMessage, TranscriptContext,
};
use serde_json::{Value, json};

fn arguments_json(call: &ToolCall) -> Value {
    serde_json::to_value(&call.arguments).expect("arguments serialize")
}

fn success_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    })])
}

fn openai_completions_model() -> Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.compat = None;
    model
}

fn ping_tool() -> Tool {
    Tool {
        name: "ping".into(),
        description: "Ping tool".into(),
        parameters: json!({
            "type": "object",
            "properties": { "ok": { "type": "boolean" } },
            "required": ["ok"],
        }),
        constrained_sampling: None,
    }
}

fn context_with_tools(messages: Vec<Message>, tools: Vec<Tool>) -> TranscriptContext {
    mock_http::context_with(&Context {
        messages,
        tools: Some(tools),
        ..Context::default()
    })
}

fn full_options(mock: &mock_http::MockHttp, extra: Value) -> OpenAICompletionsOptions {
    let mut base = mock_http::stream_options(mock, "test");
    base.extra = extra
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    OpenAICompletionsOptions::from_stream_options(base)
}

fn simple_with(
    mock: &mock_http::MockHttp,
    reasoning: Option<ThinkingLevel>,
) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(Arc::new(mock.clone())),
            ..StreamOptions::default()
        },
        reasoning,
        ..SimpleStreamOptions::default()
    }
}

async fn capture_simple(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let mock = mock_http::MockHttp::single(success_sse());
    let _ = mock_http::run_completions_simple(
        model,
        &mock_http::text_context("Hi"),
        simple_with(&mock, reasoning),
    )
    .await;
    mock.last_body()
}

async fn capture_full(model: &Model, extra: Value) -> Value {
    let mock = mock_http::MockHttp::single(success_sse());
    let _ = mock_http::run_completions(
        model,
        &mock_http::text_context("Hi"),
        full_options(&mock, extra),
    )
    .await;
    mock.last_body()
}

async fn drain_events(
    stream: kiana_ai::event_stream::AssistantMessageEventStream,
) -> (Vec<AssistantMessageEvent>, AssistantMessage) {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let message = stream.result().await.expect("result");
    (events, message)
}

#[tokio::test]
async fn forwards_tool_choice_from_options_to_payload() {
    let mock = mock_http::MockHttp::single(success_sse());
    let context = context_with_tools(
        vec![mock_http::user("Call ping with ok=true")],
        vec![ping_tool()],
    );
    let _ = mock_http::run_completions(
        &openai_completions_model(),
        &context,
        full_options(&mock, json!({ "toolChoice": "required" })),
    )
    .await;
    let params = mock.last_body();
    assert_eq!(params["tool_choice"], json!("required"));
    assert_eq!(params["tools"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn includes_tool_choice_when_no_tools_are_provided() {
    let mock = mock_http::MockHttp::single(success_sse());
    let _ = mock_http::run_completions(
        &openai_completions_model(),
        &mock_http::text_context("Summarize the conversation"),
        full_options(&mock, json!({ "toolChoice": "none" })),
    )
    .await;
    let params = mock.last_body();
    assert_eq!(params["tool_choice"], json!("none"));
    assert!(params.get("tools").is_none());
}

#[tokio::test]
async fn omits_falsy_tool_choice() {
    // TS: `if (options?.toolChoice) params.tool_choice = options.toolChoice;`
    for tool_choice in [Value::Null, json!(false), json!("")] {
        let mock = mock_http::MockHttp::single(success_sse());
        let _ = mock_http::run_completions(
            &openai_completions_model(),
            &mock_http::text_context("Summarize the conversation"),
            full_options(&mock, json!({ "toolChoice": tool_choice })),
        )
        .await;
        let params = mock.last_body();
        assert!(
            params.get("tool_choice").is_none(),
            "toolChoice {tool_choice} must be omitted: {params}"
        );
    }
}

#[tokio::test]
async fn omits_strict_when_compat_disables_strict_mode() {
    let mut model = openai_completions_model();
    model.compat = Some(json!({ "supportsStrictMode": false }));
    let mock = mock_http::MockHttp::single(success_sse());
    let context = context_with_tools(vec![mock_http::user("Call ping")], vec![ping_tool()]);
    let _ = mock_http::run_completions(&model, &context, full_options(&mock, json!({}))).await;
    let params = mock.last_body();
    assert!(params["tools"][0]["function"].get("strict").is_none());
}

#[tokio::test]
async fn defaults_unknown_openai_compatible_endpoints_to_non_strict_tools() {
    let model = Model {
        id: "local-model".into(),
        name: "Local Model".into(),
        api: "openai-completions".into(),
        provider: "local-vllm".into(),
        base_url: "http://localhost:8000/v1".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        ..Model::default()
    };
    let mut tool = ping_tool();
    tool.parameters = json!({
        "type": "object",
        "properties": { "required": { "type": "string" }, "optional": { "type": "string" } },
        "required": ["required"],
    });
    tool.constrained_sampling =
        Some(serde_json::from_value(json!({ "type": "json_schema", "strict": "prefer" })).unwrap());
    let mock = mock_http::MockHttp::single(success_sse());
    let context = context_with_tools(vec![mock_http::user("Call ping")], vec![tool]);
    let _ = mock_http::run_completions(&model, &context, full_options(&mock, json!({}))).await;
    let params = mock.last_body();
    assert!(params["tools"][0]["function"].get("strict").is_none());
    assert_eq!(
        params["tools"][0]["function"]["parameters"]["required"],
        json!(["required"])
    );
}

#[tokio::test]
async fn preserves_strict_tools_for_capable_builtin_chat_completions_models() {
    let model = mock_http::catalog_model("groq", "openai/gpt-oss-20b");
    assert_eq!(
        model
            .compat
            .as_ref()
            .and_then(|c| c.get("supportsStrictMode"))
            .and_then(Value::as_bool),
        Some(true)
    );
    let mut tool = ping_tool();
    tool.parameters = json!({
        "type": "object",
        "properties": { "required": { "type": "string" }, "optional": { "type": "string" } },
        "required": ["required"],
    });
    tool.constrained_sampling =
        Some(serde_json::from_value(json!({ "type": "json_schema", "strict": "prefer" })).unwrap());
    let mock = mock_http::MockHttp::single(success_sse());
    let context = context_with_tools(vec![mock_http::user("Call ping")], vec![tool]);
    let _ = mock_http::run_completions(&model, &context, full_options(&mock, json!({}))).await;
    let params = mock.last_body();
    assert_eq!(params["tools"][0]["function"]["strict"], json!(true));
    assert_eq!(
        params["tools"][0]["function"]["parameters"]["required"],
        json!(["required", "optional"])
    );
}

#[tokio::test]
async fn maps_groq_qwen_reasoning_levels_to_default_reasoning_effort() {
    let model = mock_http::catalog_model("groq", "qwen/qwen3.6-27b");
    let params = capture_simple(&model, Some(ThinkingLevel::Medium)).await;
    assert_eq!(params["reasoning_effort"], json!("default"));
}

#[tokio::test]
async fn keeps_normal_reasoning_effort_for_groq_models_without_compat_mapping() {
    let model = mock_http::catalog_model("groq", "openai/gpt-oss-20b");
    let params = capture_simple(&model, Some(ThinkingLevel::Medium)).await;
    assert_eq!(params["reasoning_effort"], json!("medium"));
}

#[tokio::test]
async fn omits_reasoning_effort_for_opencode_grok_build() {
    // TS: `omits reasoning effort for OpenCode Grok Build`
    let model = mock_http::catalog_model("opencode", "grok-build-0.1");
    let params = capture_simple(&model, Some(ThinkingLevel::High)).await;
    assert!(params.get("reasoning_effort").is_none(), "{params}");
}

#[tokio::test]
async fn enables_tool_stream_for_supported_zai_models_with_tools() {
    let model = mock_http::catalog_model("zai", "glm-5.2");
    let mock = mock_http::MockHttp::single(success_sse());
    let context = context_with_tools(vec![mock_http::user("Call ping")], vec![ping_tool()]);
    let _ = mock_http::run_completions(&model, &context, full_options(&mock, json!({}))).await;
    assert_eq!(mock.last_body()["tool_stream"], json!(true));
}

#[tokio::test]
async fn omits_tool_stream_when_no_tools_are_provided() {
    let model = mock_http::catalog_model("zai", "glm-5.2");
    let params = capture_full(&model, json!({})).await;
    assert!(params.get("tool_stream").is_none());
}

#[tokio::test]
async fn maps_non_standard_provider_finish_reason_values_to_stop_reason_error() {
    let model = mock_http::catalog_model("zai", "glm-5.2");
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "choices": [{ "index": 0, "delta": { "content": "partial" }, "finish_reason": null }] }),
        json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "network_error" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ]));
    let message = mock_http::run_completions_simple(
        &model,
        &mock_http::text_context("Hi"),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider finish_reason: network_error")
    );
}

#[tokio::test]
async fn ignores_null_stream_chunks_from_openai_compatible_providers() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse_raw(
        "data: null\n\n".to_string()
            + &mock_http::sse_body_without_done(&[
                json!({ "id": "chatcmpl-test", "choices": [{ "index": 0, "delta": { "content": "OK" }, "finish_reason": null }] }),
                json!({
                    "id": "chatcmpl-test",
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                    "usage": {
                        "prompt_tokens": 3,
                        "completion_tokens": 1,
                        "prompt_tokens_details": { "cached_tokens": 0 },
                        "completion_tokens_details": { "reasoning_tokens": 0 },
                    },
                }),
            ])
            + "data: [DONE]\n\n",
    ));
    let message = mock_http::run_completions_simple(
        &openai_completions_model(),
        &mock_http::text_context("Reply with exactly OK"),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.error_message, None);
    assert_eq!(message.response_id.as_deref(), Some("chatcmpl-test"));
    assert_eq!(message.usage.total_tokens, 4);
    assert_eq!(message.content.len(), 1);
}

#[tokio::test]
async fn errors_when_a_stream_ends_after_only_null_finish_reason_chunks() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "id": "chatcmpl-truncated", "choices": [{ "index": 0, "delta": { "content": "partial answer" }, "finish_reason": null }] }),
        json!({ "id": "chatcmpl-truncated", "choices": [{ "index": 0, "delta": { "content": "partial answer" }, "finish_reason": null }] }),
    ]));
    let message = mock_http::run_completions_simple(
        &openai_completions_model(),
        &mock_http::text_context("Reply with a longer sentence"),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Stream ended without finish_reason")
    );
}

#[tokio::test]
async fn accepts_streams_without_finish_reason_when_compat_disables_it() {
    let mut model = openai_completions_model();
    model.compat = Some(json!({ "supportsFinishReason": false }));
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-no-finish-reason",
        "choices": [{ "index": 0, "delta": { "content": "complete answer" }, "finish_reason": null }],
    })]));
    let message = mock_http::run_completions_simple(
        &model,
        &mock_http::text_context("Reply with a complete answer"),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.error_message, None);
    assert_eq!(message.content.len(), 1);
}

#[tokio::test]
async fn ignores_empty_custom_objects_on_function_tool_call_deltas() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-empty-custom",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read", "arguments": "{\"path\":\"README.md\"}" },
                    "custom": {},
                }],
            },
            "finish_reason": "tool_calls",
        }],
    })]));
    let mut tool = ping_tool();
    tool.name = "read".into();
    let context = context_with_tools(vec![mock_http::user("Read README.md")], vec![tool]);
    let message = mock_http::run_completions(
        &openai_completions_model(),
        &context,
        full_options(&mock, json!({})),
    )
    .await;
    assert_eq!(message.content.len(), 1);
    let AssistantContentBlock::ToolCall(tool_call) = &message.content[0] else {
        panic!("expected tool call");
    };
    assert_eq!(tool_call.id, "call_1");
    assert_eq!(tool_call.name, "read");
    assert_eq!(tool_call.arguments["path"], json!("README.md"));
}

#[tokio::test]
async fn coalesces_tool_call_deltas_by_stable_index_when_provider_mutates_ids_mid_stream() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{
                "delta": { "tool_calls": [{
                    "index": 0,
                    "id": "functions.read:0",
                    "type": "function",
                    "function": { "name": "read", "arguments": "" },
                }] },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{
                "delta": { "tool_calls": [{
                    "index": 0,
                    "id": "chatcmpl-tool-a",
                    "type": "function",
                    "function": { "name": null, "arguments": "{\"path\":\"README" },
                }] },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-kimi-bad-stream",
            "choices": [{
                "delta": { "tool_calls": [{
                    "index": 0,
                    "id": "chatcmpl-tool-b",
                    "type": "function",
                    "function": { "name": null, "arguments": ".md\"}" },
                }] },
                "finish_reason": "tool_calls",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ]));
    let mut tool = ping_tool();
    tool.name = "read".into();
    tool.parameters = json!({ "type": "object", "properties": { "path": { "type": "string" } } });
    let context = context_with_tools(vec![mock_http::user("Read README.md")], vec![tool]);
    let stream = kiana_ai::api::openai_completions::stream(
        &openai_completions_model(),
        &context,
        full_options(&mock, json!({})),
    );
    let (events, message) = drain_events(stream).await;
    let indexes: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ToolCallStart { content_index, .. }
            | AssistantMessageEvent::ToolCallDelta { content_index, .. }
            | AssistantMessageEvent::ToolCallEnd { content_index, .. } => Some(*content_index),
            _ => None,
        })
        .collect();
    assert_eq!(indexes, vec![0, 0, 0, 0, 0]);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.content.len(), 1);
    let AssistantContentBlock::ToolCall(tool_call) = &message.content[0] else {
        panic!("expected tool call");
    };
    assert_eq!(tool_call.id, "functions.read:0");
    assert_eq!(tool_call.name, "read");
    assert_eq!(arguments_json(tool_call), json!({ "path": "README.md" }));
    assert!(tool_call.arguments_error.is_none());
}

fn mixed_chunks() -> Vec<Value> {
    vec![
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": "answer 1",
                    "reasoning_content": "think 1",
                    "tool_calls": [
                        { "index": 0, "id": "tc_read_initial", "type": "function", "function": { "name": "read", "arguments": "{\"path\":\"README" } },
                        { "index": 1, "id": "tc_grep_initial", "type": "function", "function": { "name": "grep", "arguments": "{\"pattern\":\"TODO" } },
                        { "id": "tc_list_no_index", "type": "function", "function": { "name": "list", "arguments": "{\"path\":\"packages" } },
                        { "id": "tc_write_no_index", "type": "function", "function": { "name": "write", "arguments": "{\"path\":\"out" } },
                    ],
                },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": " answer 2",
                    "tool_calls": [
                        { "index": 1, "id": "tc_grep_changed", "type": "function", "function": { "arguments": "\",\"path\":\"src" } },
                        { "id": "tc_write_no_index", "type": "function", "function": { "arguments": ".txt\",\"content\":\"ok\"}" } },
                        { "id": "tc_list_no_index", "type": "function", "function": { "arguments": "/ai\"}" } },
                    ],
                },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": "\n",
                    "reasoning_content": " think 2",
                    "tool_calls": [
                        { "index": 0, "id": "tc_read_changed", "type": "function", "function": { "arguments": ".md\"}" } },
                        { "index": 1, "type": "function", "function": { "arguments": "\"}" } },
                    ],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 2 },
            },
        }),
    ]
}

#[tokio::test]
async fn accumulates_mixed_content_reasoning_and_parallel_tool_call_deltas_independently() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&mixed_chunks()));
    let tools = vec![
        Tool {
            name: "read".into(),
            description: "Read a file".into(),
            parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            constrained_sampling: None,
        },
        Tool {
            name: "grep".into(),
            description: "Search a file".into(),
            parameters: json!({ "type": "object", "properties": { "pattern": { "type": "string" }, "path": { "type": "string" } } }),
            constrained_sampling: None,
        },
        Tool {
            name: "list".into(),
            description: "List a directory".into(),
            parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            constrained_sampling: None,
        },
        Tool {
            name: "write".into(),
            description: "Write a file".into(),
            parameters: json!({ "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } } }),
            constrained_sampling: None,
        },
    ];
    let context = context_with_tools(
        vec![mock_http::user("Think, answer, and use tools.")],
        tools,
    );
    let stream = kiana_ai::api::openai_completions::stream(
        &openai_completions_model(),
        &context,
        full_options(&mock, json!({})),
    );
    let (events, message) = drain_events(stream).await;
    let count = |kind: &str| events.iter().filter(|event| event.kind() == kind).count();
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(count("text_start"), 1);
    assert_eq!(count("text_delta"), 3);
    assert_eq!(count("text_end"), 1);
    assert_eq!(count("thinking_start"), 1);
    assert_eq!(count("thinking_delta"), 2);
    assert_eq!(count("thinking_end"), 1);
    assert_eq!(count("toolcall_start"), 4);
    assert_eq!(count("toolcall_delta"), 9);
    assert_eq!(count("toolcall_end"), 4);

    let mut per_index: std::collections::BTreeMap<usize, Vec<&str>> = Default::default();
    for event in &events {
        if let AssistantMessageEvent::ToolCallStart { content_index, .. }
        | AssistantMessageEvent::ToolCallDelta { content_index, .. }
        | AssistantMessageEvent::ToolCallEnd { content_index, .. } = event
        {
            per_index
                .entry(*content_index)
                .or_default()
                .push(event.kind());
        }
    }
    assert_eq!(
        per_index.get(&2).unwrap(),
        &[
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end"
        ]
    );
    assert_eq!(
        per_index.get(&3).unwrap(),
        &[
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end"
        ]
    );
    assert_eq!(
        per_index.get(&4).unwrap(),
        &[
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end"
        ]
    );
    assert_eq!(
        per_index.get(&5).unwrap(),
        &[
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end"
        ]
    );

    assert_eq!(message.content.len(), 6);
    let AssistantContentBlock::Thinking(thinking) = &message.content[0] else {
        panic!()
    };
    assert_eq!(thinking.thinking, "think 1 think 2");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some("reasoning_content")
    );
    let AssistantContentBlock::Text(text) = &message.content[1] else {
        panic!()
    };
    assert_eq!(text.text, "answer 1 answer 2\n");
    let calls: Vec<&ToolCall> = message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].id, "tc_read_initial");
    assert_eq!(arguments_json(calls[0]), json!({ "path": "README.md" }));
    assert_eq!(calls[1].id, "tc_grep_initial");
    assert_eq!(
        arguments_json(calls[1]),
        json!({ "pattern": "TODO", "path": "src" })
    );
    assert_eq!(calls[2].id, "tc_list_no_index");
    assert_eq!(arguments_json(calls[2]), json!({ "path": "packages/ai" }));
    assert_eq!(calls[3].id, "tc_write_no_index");
    assert_eq!(
        arguments_json(calls[3]),
        json!({ "path": "out.txt", "content": "ok" })
    );
}

async fn capture_with_system_prompt(model: &Model, system_prompt: &str) -> Value {
    let mock = mock_http::MockHttp::single(success_sse());
    let context = mock_http::context_with(&Context {
        system_prompt: Some(system_prompt.into()),
        messages: vec![mock_http::user("Hi")],
        tools: None,
    });
    let _ = mock_http::run_completions_simple(model, &context, simple_with(&mock, None)).await;
    mock.last_body()
}

#[tokio::test]
async fn uses_system_messages_for_non_openai_anthropic_openrouter_reasoning_model_instructions() {
    let model = mock_http::catalog_model("openrouter", "deepseek/deepseek-v4-pro");
    let params = capture_with_system_prompt(&model, "Follow instructions.").await;
    assert_eq!(params["messages"][0]["role"], json!("system"));
}

#[tokio::test]
async fn keeps_developer_messages_for_openai_and_anthropic_openrouter_batch_instructions() {
    for (provider, model_id) in [
        ("openrouter", "openai/gpt-5.2-codex"),
        ("openrouter", "anthropic/claude-fable-5.1:batch"),
    ] {
        let model = mock_http::catalog_model(provider, model_id);
        let params = capture_with_system_prompt(&model, "Follow instructions.").await;
        assert_eq!(
            params["messages"][0]["role"],
            json!("developer"),
            "{model_id}"
        );
    }
}

#[tokio::test]
async fn keeps_developer_messages_for_openai_reasoning_model_instructions() {
    let mut model = mock_http::catalog_model("openai", "gpt-5.5");
    model.api = "openai-completions".into();
    let params = capture_with_system_prompt(&model, "Follow instructions.").await;
    assert_eq!(params["messages"][0]["role"], json!("developer"));
}

#[test]
fn stores_reasoning_replay_compat_in_builtin_metadata() {
    let kimi = mock_http::catalog_model("openrouter", "moonshotai/kimi-k2.6");
    let compat = kimi.compat.as_ref().unwrap();
    assert_eq!(compat.get("supportsDeveloperRole"), Some(&json!(false)));
    assert_eq!(
        compat.get("requiresReasoningContentOnAssistantMessages"),
        Some(&json!(true))
    );

    for provider in [
        "xiaomi",
        "xiaomi-token-plan-cn",
        "xiaomi-token-plan-ams",
        "xiaomi-token-plan-sgp",
    ] {
        let model = mock_http::catalog_model(provider, "mimo-v2.6-pro");
        let compat = model.compat.as_ref().unwrap();
        assert_eq!(
            compat.get("requiresReasoningContentOnAssistantMessages"),
            Some(&json!(true))
        );
        assert_eq!(compat.get("thinkingFormat"), Some(&json!("deepseek")));
        assert!(compat.get("maxTokensField").is_none());
        assert!(compat.get("supportsDeveloperRole").is_none());
    }

    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
    ] {
        let model = mock_http::catalog_model(provider, "qwen3.7-max");
        let compat = model.compat.as_ref().unwrap();
        assert_eq!(compat.get("thinkingFormat"), Some(&json!("qwen")));
        assert!(
            compat
                .get("requiresReasoningContentOnAssistantMessages")
                .is_none()
        );
        assert_eq!(compat.get("supportsDeveloperRole"), Some(&json!(false)));
        assert_eq!(compat.get("supportsStore"), Some(&json!(false)));
    }
}

fn assistant_with_content(
    content: Vec<AssistantContentBlock>,
    provider: &str,
    model_id: &str,
) -> AssistantMessage {
    AssistantMessage {
        role: Default::default(),
        content,
        api: "openai-completions".into(),
        provider: provider.into(),
        model: model_id.into(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    }
}

fn tool_result(tool_call_id: &str) -> ToolResultMessage {
    ToolResultMessage {
        role: Default::default(),
        tool_call_id: tool_call_id.into(),
        tool_name: "read".into(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                text: "contents".into(),
                ..Default::default()
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    }
}

async fn capture_replay(
    model: &Model,
    messages: Vec<Message>,
    reasoning: Option<ThinkingLevel>,
) -> Value {
    let mock = mock_http::MockHttp::single(success_sse());
    let context = mock_http::context_with(&Context {
        messages,
        tools: None,
        ..Context::default()
    });
    let _ = mock_http::run_completions_simple(model, &context, simple_with(&mock, reasoning)).await;
    mock.last_body()
}

fn assistant_message_in(params: &Value) -> &Value {
    params["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == json!("assistant"))
        .expect("assistant message")
}

#[tokio::test]
async fn replays_xiaomi_mimo_assistant_tool_calls_with_empty_reasoning_content_when_thinking_is_missing()
 {
    let model = mock_http::catalog_model("xiaomi", "mimo-v2.6-pro");
    let assistant = assistant_with_content(
        vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "read".into(),
            arguments: [("path".to_string(), json!("README.md"))]
                .into_iter()
                .collect(),
            ..Default::default()
        })],
        "xiaomi",
        "mimo-v2.6-pro",
    );
    let params = capture_replay(
        &model,
        vec![
            mock_http::user("Read README.md"),
            Message::Assistant(assistant),
            Message::ToolResult(tool_result("call_1")),
        ],
        Some(ThinkingLevel::High),
    )
    .await;
    let replayed = assistant_message_in(&params);
    assert_eq!(replayed["reasoning_content"], json!(""));
    assert_eq!(params["thinking"], json!({ "type": "enabled" }));
    assert!(params.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn normalizes_opencode_go_reasoning_deltas_to_reasoning_content_for_replay() {
    let model = mock_http::catalog_model("opencode-go", "kimi-k3");
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-opencode-go-reasoning",
        "choices": [{ "index": 0, "delta": { "reasoning": "think" }, "finish_reason": "stop" }],
    })]));
    let message = mock_http::run_completions_simple(
        &model,
        &mock_http::text_context("Use reasoning."),
        simple_with(&mock, None),
    )
    .await;
    let AssistantContentBlock::Thinking(thinking) = &message.content[0] else {
        panic!()
    };
    assert_eq!(thinking.thinking, "think");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some("reasoning_content")
    );
}

#[tokio::test]
async fn keeps_non_opencode_go_reasoning_deltas_on_the_original_reasoning_field() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-reasoning",
        "choices": [{ "index": 0, "delta": { "reasoning": "think" }, "finish_reason": "stop" }],
    })]));
    let message = mock_http::run_completions_simple(
        &openai_completions_model(),
        &mock_http::text_context("Use reasoning."),
        simple_with(&mock, None),
    )
    .await;
    let AssistantContentBlock::Thinking(thinking) = &message.content[0] else {
        panic!()
    };
    assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning"));
}

#[test]
fn replays_opencode_go_reasoning_thinking_blocks_as_reasoning_content() {
    let model = mock_http::catalog_model("opencode-go", "kimi-k3");
    let assistant = assistant_with_content(
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "think".into(),
                thinking_signature: Some("reasoning".into()),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: Default::default(),
                ..Default::default()
            }),
        ],
        "opencode-go",
        "kimi-k3",
    );
    let context = mock_http::context_with(&Context {
        messages: vec![Message::Assistant(assistant)],
        tools: None,
        ..Context::default()
    });
    let messages = convert_messages(
        &model,
        &context,
        &kiana_ai::api::openai_completions::get_compat(&model),
        None,
    )
    .expect("convert");
    assert_eq!(messages[0]["reasoning_content"], json!("think"));
    assert!(messages[0].get("reasoning").is_none());
}

#[tokio::test]
async fn sends_thinking_disabled_or_enabled_for_opencode_kimi_k2_6() {
    let model = mock_http::catalog_model("opencode", "kimi-k2.6");
    let params = capture_simple(&model, None).await;
    assert_eq!(params["thinking"], json!({ "type": "disabled" }));
    assert!(params.get("reasoning_effort").is_none());

    let params = capture_simple(&model, Some(ThinkingLevel::High)).await;
    assert_eq!(params["thinking"], json!({ "type": "enabled" }));
    assert!(params.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn omits_disabled_thinking_for_moonshot_kimi_k2_7_code_models() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        let model = mock_http::catalog_model(provider, "kimi-k2.7-code");
        let params = capture_simple(&model, None).await;
        assert!(params.get("thinking").is_none(), "{provider}");
        assert!(params.get("reasoning_effort").is_none(), "{provider}");
    }
}

#[tokio::test]
async fn keeps_disabled_thinking_for_moonshot_kimi_k2_6_when_thinking_is_off() {
    let model = mock_http::catalog_model("moonshotai-cn", "kimi-k2.6");
    let params = capture_simple(&model, None).await;
    assert_eq!(params["thinking"], json!({ "type": "disabled" }));
    assert!(params.get("reasoning_effort").is_none());
}

fn simple_with_max_tokens(mock: &mock_http::MockHttp, max_tokens: u64) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(Arc::new(mock.clone())),
            max_tokens: Some(max_tokens),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    }
}

async fn capture_max_tokens(model: &Model) -> Value {
    let mock = mock_http::MockHttp::single(success_sse());
    let _ = mock_http::run_completions_simple(
        model,
        &mock_http::text_context("Hi"),
        simple_with_max_tokens(&mock, 123),
    )
    .await;
    mock.last_body()
}

#[tokio::test]
async fn sends_max_tokens_for_opencode_deepseek_and_zai_models() {
    for (provider, model_id) in [
        ("opencode-go", "kimi-k3"),
        ("opencode", "kimi-k2.6"),
        ("deepseek", "deepseek-flash"),
        ("deepseek", "deepseek-v4-pro"),
        ("zai", "glm-5-turbo"),
        ("zai", "glm-5.2"),
    ] {
        let model = mock_http::catalog_model(provider, model_id);
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|c| c.get("maxTokensField"))
                .and_then(Value::as_str),
            Some("max_tokens"),
            "{model_id}"
        );
        let params = capture_max_tokens(&model).await;
        assert_eq!(params["max_tokens"], json!(123), "{model_id}");
        assert!(params.get("max_completion_tokens").is_none(), "{model_id}");
    }

    for base_url in ["https://api.deepseek.com", "https://API.DeepSeek.COM"] {
        let model = Model {
            id: "custom-deepseek-model".into(),
            name: "Custom DeepSeek Model".into(),
            api: "openai-completions".into(),
            provider: "custom-deepseek".into(),
            base_url: base_url.into(),
            reasoning: Some(true),
            input: vec![kiana_ai::types::ModelInputModality::Text],
            context_window: Some(128_000),
            max_tokens: Some(8192),
            ..Model::default()
        };
        let params = capture_max_tokens(&model).await;
        assert_eq!(params["max_tokens"], json!(123), "{base_url}");
        assert!(params.get("max_completion_tokens").is_none(), "{base_url}");
    }
}

#[tokio::test]
async fn does_not_double_count_reasoning_tokens_in_completion_usage() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "id": "chatcmpl-reasoning-usage",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 33,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 21 },
        },
    })]));
    let message = mock_http::run_completions_simple(
        &openai_completions_model(),
        &mock_http::text_context("Use reasoning."),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.usage.input, 10);
    assert_eq!(message.usage.output, 33);
    assert_eq!(message.usage.total_tokens, 43);
}

#[tokio::test]
async fn preserves_prompt_tokens_details_cache_read_write_fields() {
    let chunk_usage = json!({
        "id": "chatcmpl-cache-write",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 5,
            "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 30 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    });
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[chunk_usage]));
    let message = mock_http::run_completions_simple(
        &openai_completions_model(),
        &mock_http::text_context("Reply with exactly OK"),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.usage.input, 20);
    assert_eq!(message.usage.cache_read, 50);
    assert_eq!(message.usage.cache_write, 30);
    assert_eq!(message.usage.total_tokens, 105);

    let choice_usage = json!({
        "id": "chatcmpl-cache-write-choice",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop",
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_tokens_details": { "cached_tokens": 50, "cache_write_tokens": 30 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }],
    });
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[choice_usage]));
    let message = mock_http::run_completions_simple(
        &openai_completions_model(),
        &mock_http::text_context("Reply with exactly OK"),
        simple_with(&mock, None),
    )
    .await;
    assert_eq!(message.usage.input, 20);
    assert_eq!(message.usage.cache_read, 50);
    assert_eq!(message.usage.cache_write, 30);
    assert_eq!(message.usage.total_tokens, 105);
}

#[tokio::test]
async fn uses_openrouter_reasoning_object_instead_of_reasoning_effort() {
    let model = mock_http::catalog_model("openrouter", "deepseek/deepseek-r1-0528");
    let params = capture_simple(&model, Some(ThinkingLevel::High)).await;
    assert_eq!(params["reasoning"], json!({ "effort": "high" }));
    assert!(params.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn uses_configurable_chat_template_boolean_thinking_kwargs() {
    let model = Model {
        id: "deepseek-ai/DeepSeek-V3.1".into(),
        name: "DeepSeek V3.1 via vLLM".into(),
        api: "openai-completions".into(),
        provider: "local-vllm".into(),
        base_url: "http://localhost:8000/v1".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        compat: Some(json!({
            "thinkingFormat": "chat-template",
            "supportsReasoningEffort": false,
            "chatTemplateKwargs": { "thinking": { "$var": "thinking.enabled" } },
        })),
        ..Model::default()
    };
    for (reasoning, expected) in [(Some(ThinkingLevel::High), true), (None, false)] {
        let params = capture_simple(&model, reasoning).await;
        assert_eq!(
            params["chat_template_kwargs"],
            json!({ "thinking": expected })
        );
        assert!(params.get("thinking").is_none());
        assert!(params.get("reasoning_effort").is_none());
    }
}

#[tokio::test]
async fn uses_qwen_chat_template_thinking_kwargs() {
    let model = Model {
        id: "Qwen/Qwen3-Coder".into(),
        name: "Qwen3 Coder via vLLM".into(),
        api: "openai-completions".into(),
        provider: "local-vllm".into(),
        base_url: "http://localhost:8000/v1".into(),
        reasoning: Some(true),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        compat: Some(json!({
            "thinkingFormat": "qwen-chat-template",
            "supportsReasoningEffort": false,
        })),
        ..Model::default()
    };
    for (reasoning, expected) in [(Some(ThinkingLevel::High), true), (None, false)] {
        let params = capture_simple(&model, reasoning).await;
        assert_eq!(
            params["chat_template_kwargs"],
            json!({ "enable_thinking": expected, "preserve_thinking": true })
        );
        assert!(params.get("reasoning_effort").is_none());
    }
}

#[tokio::test]
async fn uses_configurable_chat_template_effort_kwargs_with_static_kwargs() {
    let model = Model {
        id: "unsloth/gpt-oss-120b-GGUF".into(),
        name: "GPT OSS via vLLM".into(),
        api: "openai-completions".into(),
        provider: "local-vllm".into(),
        base_url: "http://localhost:8000/v1".into(),
        reasoning: Some(true),
        thinking_level_map: Some(serde_json::from_value(json!({ "xhigh": "max" })).unwrap()),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        compat: Some(json!({
            "thinkingFormat": "chat-template",
            "supportsReasoningEffort": false,
            "chatTemplateKwargs": {
                "preserve_thinking": true,
                "reasoning_effort": { "$var": "thinking.effort", "omitWhenOff": true },
            },
        })),
        ..Model::default()
    };
    let params = capture_simple(&model, Some(ThinkingLevel::Xhigh)).await;
    assert_eq!(
        params["chat_template_kwargs"],
        json!({ "preserve_thinking": true, "reasoning_effort": "max" })
    );
    assert!(params.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn uses_ant_ling_compatibility_metadata() {
    let model = mock_http::catalog_model("ant-ling", "Ring-2.6-1T");
    let compat = model.compat.as_ref().unwrap();
    assert_eq!(compat.get("supportsStore"), Some(&json!(false)));
    assert_eq!(compat.get("supportsDeveloperRole"), Some(&json!(false)));
    assert_eq!(compat.get("supportsReasoningEffort"), Some(&json!(false)));
    assert_eq!(compat.get("maxTokensField"), Some(&json!("max_tokens")));
    assert_eq!(compat.get("thinkingFormat"), Some(&json!("ant-ling")));
    assert_eq!(
        compat.get("supportsLongCacheRetention"),
        Some(&json!(false))
    );
    assert_eq!(compat.get("supportsStrictMode"), Some(&json!(true)));
    assert!(
        compat
            .get("requiresReasoningContentOnAssistantMessages")
            .is_none()
    );

    let mock = mock_http::MockHttp::single(success_sse());
    let context = mock_http::context_with(&Context {
        system_prompt: Some("Follow instructions.".into()),
        messages: vec![mock_http::user("Hi")],
        tools: None,
    });
    let mut options = full_options(&mock, json!({ "reasoningEffort": "high" }));
    options.base.max_tokens = Some(123);
    options.base.cache_retention = Some(kiana_ai::types::CacheRetention::Long);
    options.base.session_id = Some("ant-ling-session".into());
    let _ = mock_http::run_completions(&model, &context, options).await;
    let params = mock.last_body();
    assert_eq!(params["max_tokens"], json!(123));
    assert!(params.get("max_completion_tokens").is_none());
    assert_eq!(params["messages"][0]["role"], json!("system"));
    assert_eq!(params["reasoning"], json!({ "effort": "high" }));
    assert!(params.get("reasoning_effort").is_none());
    assert!(params.get("store").is_none());
    assert!(params.get("prompt_cache_key").is_none());
    assert!(params.get("prompt_cache_retention").is_none());
}

#[tokio::test]
async fn omits_ant_ling_reasoning_for_unmapped_direct_reasoning_efforts_and_non_reasoning_models() {
    let ring = mock_http::catalog_model("ant-ling", "Ring-2.6-1T");
    let params = capture_full(&ring, json!({ "reasoningEffort": "medium" })).await;
    assert!(params.get("reasoning").is_none());

    let ling = mock_http::catalog_model("ant-ling", "Ling-2.6-flash");
    let params = capture_simple(&ling, Some(ThinkingLevel::High)).await;
    assert!(params.get("reasoning").is_none());
}

#[test]
fn stores_zai_tool_stream_and_effort_metadata() {
    for model_id in ["glm-4.7", "glm-5-turbo", "glm-5.2"] {
        let model = mock_http::catalog_model("zai", model_id);
        assert_eq!(
            model.compat.as_ref().and_then(|c| c.get("zaiToolStream")),
            Some(&json!(true)),
            "{model_id}"
        );
    }
    for model_id in ["glm-5.2", "glm-5.2-highspeed"] {
        let model = mock_http::catalog_model("zai", model_id);
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|c| c.get("supportsReasoningEffort")),
            Some(&json!(true)),
            "{model_id}"
        );
        assert_eq!(
            model.thinking_level_map,
            Some(
                serde_json::from_value(json!({
                    "off": "none",
                    "minimal": null,
                    "low": null,
                    "medium": null,
                    "high": "high",
                    "xhigh": null,
                    "max": "max",
                }))
                .unwrap()
            )
        );
    }
    for provider in ["zai", "zai-coding-cn"] {
        let model = mock_http::catalog_model(provider, "glm-5.3");
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|c| c.get("supportsReasoningEffort")),
            Some(&json!(true))
        );
        assert_eq!(
            model.thinking_level_map,
            Some(
                serde_json::from_value(json!({
                    "off": null,
                    "minimal": null,
                    "low": "low",
                    "medium": null,
                    "high": "high",
                    "xhigh": null,
                    "max": "max",
                }))
                .unwrap()
            )
        );
    }
}

#[tokio::test]
async fn maps_zai_glm_5_2_thinking_levels_to_reasoning_effort() {
    let model = mock_http::catalog_model("zai", "glm-5.2");
    for (reasoning, effort) in [
        (ThinkingLevel::Low, "high"),
        (ThinkingLevel::Medium, "high"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Max, "max"),
    ] {
        let params = capture_simple(&model, Some(reasoning)).await;
        assert_eq!(
            params["thinking"],
            json!({ "type": "enabled", "clear_thinking": false }),
            "{reasoning:?}"
        );
        assert_eq!(params["reasoning_effort"], json!(effort), "{reasoning:?}");
    }
}

#[tokio::test]
async fn preserves_zai_thinking_when_replaying_reasoning_content_and_omits_effort_when_off() {
    let model = mock_http::catalog_model("zai", "glm-5.2");
    let assistant = assistant_with_content(
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "prior reasoning".into(),
                thinking_signature: Some("reasoning_content".into()),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: [("path".to_string(), json!("README.md"))]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }),
        ],
        "zai",
        "glm-5.2",
    );
    let params = capture_replay(
        &model,
        vec![
            mock_http::user("Read README.md"),
            Message::Assistant(assistant),
            Message::ToolResult(tool_result("call_1")),
            mock_http::user("Continue"),
        ],
        Some(ThinkingLevel::High),
    )
    .await;
    assert_eq!(
        assistant_message_in(&params)["reasoning_content"],
        json!("prior reasoning")
    );
    assert_eq!(
        params["thinking"],
        json!({ "type": "enabled", "clear_thinking": false })
    );

    let params = capture_simple(&model, None).await;
    assert_eq!(params["thinking"], json!({ "type": "disabled" }));
    assert!(params.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn respects_explicit_zai_tool_stream_override() {
    let mut model = mock_http::catalog_model("zai", "glm-5.2");
    model.compat = Some(json!({ "zaiToolStream": true }));
    let mock = mock_http::MockHttp::single(success_sse());
    let context = context_with_tools(vec![mock_http::user("Call ping")], vec![ping_tool()]);
    let _ = mock_http::run_completions(&model, &context, full_options(&mock, json!({}))).await;
    assert_eq!(mock.last_body()["tool_stream"], json!(true));
}

#[tokio::test]
async fn streams_grammar_custom_tool_input_as_json_arguments() {
    let model = Model {
        id: "grammar-model".into(),
        name: "Grammar Model".into(),
        api: "openai-completions".into(),
        provider: "local-vllm".into(),
        base_url: "http://localhost:8000/v1".into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(8192),
        compat: Some(json!({ "supportsOpenAIGrammarTools": true })),
        ..Model::default()
    };
    let mut tool = ping_tool();
    tool.name = "query".into();
    tool.description = "Grammar tool".into();
    tool.parameters = json!({ "type": "object", "properties": { "input": { "type": "string" } }, "required": ["input"] });
    tool.constrained_sampling = Some(
        serde_json::from_value(
            json!({ "type": "grammar", "variants": { "openai_lark": "start: /.+/" } }),
        )
        .unwrap(),
    );
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({
            "id": "chatcmpl-grammar",
            "choices": [{
                "delta": { "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "custom",
                    "custom": { "name": "query", "input": "abc" },
                }] },
                "finish_reason": null,
            }],
        }),
        json!({
            "id": "chatcmpl-grammar",
            "choices": [{
                "delta": { "tool_calls": [{ "index": 0, "custom": { "input": "def" } }] },
                "finish_reason": "tool_calls",
            }],
        }),
    ]));
    let context = context_with_tools(vec![mock_http::user("Query")], vec![tool]);
    let (events, message) = drain_events(kiana_ai::api::openai_completions::stream(
        &model,
        &context,
        full_options(&mock, json!({})),
    ))
    .await;
    let AssistantContentBlock::ToolCall(tool_call) = &message.content[0] else {
        panic!()
    };
    assert_eq!(tool_call.name, "query");
    assert_eq!(
        serde_json::to_value(&tool_call.arguments).unwrap(),
        json!({ "input": "abcdef" })
    );
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["{\"input\":\"abc", "def", "\"}"]);
}
