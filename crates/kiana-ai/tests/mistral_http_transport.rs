//! `test/mistral-http-transport.test.ts` 的移植。
//!
//! TS 用可注入的 `fetch` 构造 Response/ReadableStream；Rust 侧实现一个可编程的
//! [`HttpClient`]（返回任意状态/头/字节块，或永不返回）。

mod common;

use std::collections::VecDeque;
#[allow(unused_imports)]
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::api::mistral_conversations::{MistralOptions, stream};
use kiana_ai::http::{ByteStream, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    AssistantContentBlock, Context, Message, Model, ModelInputModality, ProviderResponse,
    StreamOptions, Tool, ToolCall, ToolCallKind, Usage, UsageCost, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use kiana_ai::utils::identity::format_user_agent;
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

enum MockOutcome {
    Stream {
        status: u16,
        headers: Vec<(String, String)>,
        chunks: Vec<Vec<u8>>,
        delay: Option<Duration>,
    },
    Pending,
}

struct MockClient {
    outcome: Box<dyn Fn(&HttpRequest) -> MockOutcome + Send + Sync>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl HttpClient for MockClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        self.requests.lock().unwrap().push(request.clone());
        let outcome = (self.outcome)(&request);
        Box::pin(async move {
            match outcome {
                MockOutcome::Pending => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                MockOutcome::Stream {
                    status,
                    headers,
                    chunks,
                    delay,
                } => {
                    let queue: VecDeque<Vec<u8>> = chunks.into();
                    let body: ByteStream = match delay {
                        None => Box::pin(futures::stream::iter(queue.into_iter().map(Ok))),
                        Some(delay) => Box::pin(futures::stream::unfold(
                            queue,
                            move |mut queue| async move {
                                let item = queue.pop_front()?;
                                tokio::time::sleep(delay).await;
                                Some((Ok(item), queue))
                            },
                        )),
                    };
                    Ok(HttpResponse {
                        response: ProviderResponse {
                            status,
                            headers: headers.into_iter().collect(),
                        },
                        body,
                    })
                }
            }
        })
    }
}

fn mock_client(
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    outcome: impl Fn(&HttpRequest) -> MockOutcome + Send + Sync + 'static,
) -> Arc<dyn HttpClient> {
    Arc::new(MockClient {
        outcome: Box::new(outcome),
        requests,
    })
}

fn sse_body(events: &[Value]) -> String {
    let mut body = events
        .iter()
        .map(|event| format!("data: {}", serde_json::to_string(event).unwrap()))
        .collect::<Vec<_>>()
        .join("\r\n\r\n");
    body.push_str("\r\n\r\ndata: [DONE]\r\n\r\n");
    body
}

fn terminal_event(finish_reason: &str) -> Value {
    json!({
        "id": "mistral-response-id",
        "model": "mistral-large-latest",
        "choices": [{"index": 0, "finish_reason": finish_reason, "delta": {}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    })
}

fn get_model(id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "mistral-conversations".into(),
        provider: "mistral".into(),
        base_url: "https://api.mistral.ai".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: kiana_ai::types::ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            ..Default::default()
        },
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    }
}

fn context_with(context: Context) -> kiana_ai::types::TranscriptContext {
    normalize_context(&context)
}

fn text_content(text: &str) -> UserContentBlock {
    UserContentBlock::Text(kiana_ai::types::TextContent {
        kind_text: kiana_ai::types::TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

#[tokio::test]
async fn serializes_sdk_style_payloads_to_the_mistral_wire_format() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = mock_client(Arc::clone(&requests), |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![
            ("content-type".into(), "text/event-stream".into()),
            ("x-request-id".into(), "request-1".into()),
        ],
        chunks: vec![sse_body(&[terminal_event("stop")]).into_bytes()],
        delay: None,
    });
    let captured_payload: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_payload_cb = Arc::clone(&captured_payload);
    let captured_response: Arc<Mutex<Option<ProviderResponse>>> = Arc::new(Mutex::new(None));
    let captured_response_cb = Arc::clone(&captured_response);

    let model = get_model("mistral-large-latest");
    let context = context_with(Context {
        system_prompt: Some("Be precise".into()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![
                text_content("describe"),
                UserContentBlock::Image(kiana_ai::types::ImageContent {
                    kind_image: kiana_ai::types::ImageKind::Image,
                    data: "aGVsbG8=".into(),
                    mime_type: "image/png".into(),
                }),
            ]),
            timestamp: 1,
        })],
        tools: Some(vec![Tool {
            name: "lookup".into(),
            description: "Look something up".into(),
            parameters: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            constrained_sampling: None,
        }]),
    });
    let message = stream(
        &model,
        &context,
        MistralOptions {
            base: StreamOptions {
                api_key: Some("secret".into()),
                fetch: Some(client),
                headers: Some(
                    [("x-custom".to_string(), Some("value".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                max_tokens: Some(123),
                session_id: Some("session-1".into()),
                on_payload: Some(Arc::new(move |payload, _model| {
                    *captured_payload_cb.lock().unwrap() = Some(payload.clone());
                    let mut object = payload.as_object().cloned().unwrap_or_default();
                    object.insert("topP".into(), json!(0.9));
                    object.insert("randomSeed".into(), json!(42));
                    object.insert(
                        "responseFormat".into(),
                        json!({
                            "type": "json_schema",
                            "jsonSchema": {
                                "name": "result",
                                "schemaDefinition": {
                                    "type": "object",
                                    "properties": {"maxTokens": {"type": "number"}}
                                }
                            }
                        }),
                    );
                    object.insert("presencePenalty".into(), json!(0.1));
                    object.insert("frequencyPenalty".into(), json!(0.2));
                    object.insert("parallelToolCalls".into(), json!(true));
                    object.insert("safePrompt".into(), json!(true));
                    Some(Value::Object(object))
                })),
                on_response: Some(Arc::new(move |response, _model| {
                    *captured_response_cb.lock().unwrap() = Some(response);
                })),
                ..Default::default()
            },
            tool_choice: Some(json!({"type": "function", "function": {"name": "lookup"}})),
            prompt_mode: Some("reasoning".into()),
            reasoning_effort: Some("high".into()),
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        "https://api.mistral.ai/v1/chat/completions"
    );
    let headers: std::collections::HashMap<_, _> = requests[0]
        .headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect();
    assert_eq!(
        headers.get("authorization").map(String::as_str),
        Some("Bearer secret")
    );
    assert_eq!(
        headers.get("accept").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(
        headers.get("x-affinity").map(String::as_str),
        Some("session-1")
    );
    assert_eq!(headers.get("x-custom").map(String::as_str), Some("value"));
    assert_eq!(
        headers.get("user-agent").map(String::as_str),
        Some(format_user_agent(None).as_str())
    );

    let payload = captured_payload.lock().unwrap().clone().expect("payload");
    assert_eq!(payload["maxTokens"], json!(123));
    assert_eq!(payload["promptMode"], json!("reasoning"));
    assert_eq!(payload["promptCacheKey"], json!("session-1"));
    let response = captured_response.lock().unwrap().clone().expect("response");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers.get("x-request-id").map(String::as_str),
        Some("request-1")
    );

    let wire: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).expect("wire");
    assert_eq!(wire["max_tokens"], json!(123));
    assert_eq!(wire["prompt_mode"], json!("reasoning"));
    assert_eq!(wire["reasoning_effort"], json!("high"));
    assert_eq!(
        wire["tool_choice"],
        json!({"type": "function", "function": {"name": "lookup"}})
    );
    assert_eq!(wire["prompt_cache_key"], json!("session-1"));
    assert_eq!(wire["top_p"], json!(0.9));
    assert_eq!(wire["random_seed"], json!(42));
    assert_eq!(wire["presence_penalty"], json!(0.1));
    assert_eq!(wire["frequency_penalty"], json!(0.2));
    assert_eq!(wire["parallel_tool_calls"], json!(true));
    assert_eq!(wire["safe_prompt"], json!(true));
    assert_eq!(
        wire["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "result",
                "schema": {"type": "object", "properties": {"maxTokens": {"type": "number"}}}
            }
        })
    );
    assert!(wire.get("maxTokens").is_none());
    assert!(wire.get("promptMode").is_none());
    assert!(wire.get("promptCacheKey").is_none());
    assert_eq!(
        wire["messages"],
        json!([
            {"role": "system", "content": "Be precise"},
            {"role": "user", "content": [
                {"type": "text", "text": "describe"},
                {"type": "image_url", "image_url": "data:image/png;base64,aGVsbG8="}
            ]}
        ])
    );
}

#[tokio::test]
async fn serializes_assistant_thinking_tool_calls_and_tool_results_for_replay() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = mock_client(Arc::clone(&requests), |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![sse_body(&[terminal_event("stop")]).into_bytes()],
        delay: None,
    });
    let model = get_model("mistral-large-latest");
    let context = context_with(Context {
        messages: vec![
            Message::Assistant(kiana_ai::types::AssistantMessage {
                role: kiana_ai::types::AssistantRole::Assistant,
                content: vec![
                    AssistantContentBlock::Thinking(kiana_ai::types::ThinkingContent {
                        kind_thinking: kiana_ai::types::ThinkingKind::Thinking,
                        thinking: "reason".into(),
                        thinking_signature: None,
                        redacted: None,
                    }),
                    AssistantContentBlock::Text(kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: "answer".into(),
                        text_signature: None,
                    }),
                    AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "abc123456".into(),
                        name: "lookup".into(),
                        arguments: [("query".to_string(), json!("kiana"))]
                            .into_iter()
                            .collect(),
                        ..ToolCall::default()
                    }),
                ],
                api: "mistral-conversations".into(),
                provider: "mistral".into(),
                model: model.id.clone(),
                usage: Usage::default(),
                stop_reason: kiana_ai::types::StopReason::ToolUse,
                timestamp: 1,
                ..Default::default()
            }),
            Message::ToolResult(kiana_ai::types::ToolResultMessage {
                role: kiana_ai::types::ToolResultRole::ToolResult,
                tool_call_id: "abc123456".into(),
                tool_name: "lookup".into(),
                content: vec![
                    text_content("found"),
                    UserContentBlock::Image(kiana_ai::types::ImageContent {
                        kind_image: kiana_ai::types::ImageKind::Image,
                        data: "aGVsbG8=".into(),
                        mime_type: "image/png".into(),
                    }),
                ],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
                duration_ms: None,
            }),
        ],
        ..Context::default()
    });
    let message = stream(
        &model,
        &context,
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    let wire: Value =
        serde_json::from_slice(requests.lock().unwrap()[0].body.as_ref().unwrap()).expect("wire");
    assert_eq!(
        wire["messages"],
        json!([
            {
                "role": "assistant",
                "prefix": false,
                "content": [
                    {"type": "thinking", "thinking": [{"type": "text", "text": "reason"}]},
                    {"type": "text", "text": "answer"}
                ],
                "tool_calls": [
                    {"id": "abc123456", "type": "function",
                     "function": {"name": "lookup", "arguments": "{\"query\":\"kiana\"}"},
                     "index": 0}
                ]
            },
            {
                "role": "tool",
                "tool_call_id": "abc123456",
                "name": "lookup",
                "content": [
                    {"type": "text", "text": "found"},
                    {"type": "image_url", "image_url": "data:image/png;base64,aGVsbG8="}
                ]
            }
        ])
    );
}

#[tokio::test]
async fn parses_native_thinking_text_fragmented_tool_calls_and_cached_token_usage() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let events = vec![
        json!({"id": "response-1", "model": "m", "choices": [{"index": 0, "finish_reason": null,
            "delta": {"content": [{"type": "thinking", "thinking": [{"type": "text", "text": "reason"}]}]}}]}),
        json!({"id": "response-1", "model": "m", "choices": [{"index": 0, "finish_reason": null,
            "delta": {"content": [{"type": "text", "text": "answer"}]}}]}),
        json!({"id": "response-1", "model": "m", "choices": [{"index": 0, "finish_reason": null,
            "delta": {"tool_calls": [{"id": "abc123456", "index": 0,
                "function": {"name": "lookup", "arguments": "{\"query\":"}}]}}]}),
        json!({"id": "response-1", "model": "m", "choices": [{"index": 0, "finish_reason": "tool_calls",
            "delta": {"tool_calls": [{"index": 0, "function": {"name": "", "arguments": "\"kiana\"}"}}]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14,
                      "prompt_tokens_details": {"cached_tokens": 3}}}),
    ];
    let client = mock_client(Arc::clone(&requests), move |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![sse_body(&events).into_bytes()],
        delay: None,
    });
    let message = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(message.response_id.as_deref(), Some("response-1"));
    assert_eq!(message.content.len(), 3);
    assert_eq!(message.usage.input, 7);
    assert_eq!(message.usage.output, 4);
    assert_eq!(message.usage.cache_read, 3);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.total_tokens, 14);
    match &message.content[2] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "abc123456");
            assert_eq!(call.name, "lookup");
            assert_eq!(
                call.arguments.get("query").and_then(Value::as_str),
                Some("kiana")
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn ignores_empty_content_deltas() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let thinking =
        |text: &str| json!({"type": "thinking", "thinking": [{"type": "text", "text": text}]});
    let tool_call = |args: &str, first: bool| {
        let mut value = json!({"index": 0, "function": {"name": if first { "read" } else { "" }, "arguments": args}});
        if first {
            value["id"] = json!("abc123456");
        }
        value
    };
    let deltas = vec![
        json!({"content": ""}),
        json!({"content": [thinking("first part,")]}),
        json!({"content": ""}),
        json!({"content": [{"type": "text", "text": ""}]}),
        json!({"content": [thinking(" second part."), {"type": "text", "text": "Reading."}]}),
        json!({"content": "", "tool_calls": [tool_call("", true)]}),
        json!({"content": "", "tool_calls": [tool_call("{\"path\":", false)]}),
        json!({"content": "", "tool_calls": [tool_call("\"a.txt\"}", false)]}),
        json!({"content": ""}),
    ];
    let events: Vec<Value> = deltas
        .iter()
        .enumerate()
        .map(|(index, delta)| {
            json!({
                "id": "response-1",
                "model": "m",
                "choices": [{
                    "index": 0,
                    "finish_reason": if index == deltas.len() - 1 { json!("tool_calls") } else { Value::Null },
                    "delta": delta,
                }],
            })
        })
        .collect();
    let client = mock_client(Arc::clone(&requests), move |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![sse_body(&events).into_bytes()],
        delay: None,
    });
    let message = stream(
        &get_model("zai-glm-5-3"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(message.content.len(), 3);
    match &message.content[0] {
        AssistantContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking, "first part, second part.")
        }
        other => panic!("expected thinking, got {other:?}"),
    }
    match &message.content[1] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "Reading."),
        other => panic!("expected text, got {other:?}"),
    }
    match &message.content[2] {
        AssistantContentBlock::ToolCall(call) => {
            assert_eq!(call.id, "abc123456");
            assert_eq!(call.name, "read");
            assert_eq!(
                call.arguments.get("path").and_then(Value::as_str),
                Some("a.txt")
            );
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn forwards_each_parsed_sse_payload_before_normalizing_it() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let events = vec![
        json!({"id": "response-1", "provider_metadata": {"request": "test"},
               "choices": [{"index": 0, "finish_reason": null, "delta": {"content": "hello"}}]}),
        json!({"id": "response-1", "choices": [{"index": 0, "finish_reason": "stop", "delta": {}}],
               "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}),
    ];
    let client = mock_client(Arc::clone(&requests), {
        let events = events.clone();
        move |_request| MockOutcome::Stream {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks: vec![sse_body(&events).into_bytes()],
            delay: None,
        }
    });
    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cb = Arc::clone(&received);
    let result = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                on_provider_stream_event: Some(Arc::new(move |event, _model| {
                    received_cb.lock().unwrap().push(event);
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(*received.lock().unwrap(), events);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
}

#[tokio::test]
async fn parses_sse_and_utf8_sequences_split_across_transport_chunks() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let event = json!({
        "id": "response-bytewise",
        "model": "m",
        "choices": [{"index": 0, "finish_reason": "stop", "delta": {"content": "héllo 🌍"}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
    });
    let bytes = sse_body(&[event]).into_bytes();
    let chunks: Vec<Vec<u8>> = bytes.into_iter().map(|byte| vec![byte]).collect();
    let client = mock_client(Arc::clone(&requests), move |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: chunks.clone(),
        delay: None,
    });
    let message = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    match &message.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "héllo 🌍"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn accepts_lone_surrogate_escapes_like_json_parse() {
    // serde_json 会拒绝 `\ud800`；JSON.parse 接受（Rust 侧按 ARCHITECTURE §8 第 31 条落成 U+FFFD）。
    let raw = "data: {\"id\":\"r\",\"model\":\"m\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{\"content\":\"\\ud800\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\r\n\r\ndata: [DONE]\r\n\r\n";
    let client = mock_client(Arc::new(Mutex::new(Vec::new())), move |_request| {
        MockOutcome::Stream {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks: vec![raw.as_bytes().to_vec()],
            delay: None,
        }
    });
    let message = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    match &message.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "\u{fffd}"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn honors_case_insensitive_header_overrides_and_explicit_affinity_suppression() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = mock_client(Arc::clone(&requests), |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![sse_body(&[terminal_event("stop")]).into_bytes()],
        delay: None,
    });
    let mut model = get_model("mistral-large-latest");
    model.headers = Some(
        [
            ("Authorization".to_string(), "Bearer model-key".to_string()),
            ("X-Affinity".to_string(), "model-affinity".to_string()),
        ]
        .into_iter()
        .collect(),
    );
    let _ = stream(
        &model,
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("request-key".into()),
                fetch: Some(client),
                session_id: Some("automatic-affinity".into()),
                headers: Some(
                    [
                        ("authorization".to_string(), None),
                        ("x-affinity".to_string(), None),
                        ("User-Agent".to_string(), Some("custom-agent".to_string())),
                    ]
                    .into_iter()
                    .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    let requests = requests.lock().unwrap();
    let headers: std::collections::HashMap<_, _> = requests[0]
        .headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect();
    assert!(!headers.contains_key("authorization"));
    assert!(!headers.contains_key("x-affinity"));
    assert_eq!(
        headers.get("user-agent").map(String::as_str),
        Some("custom-agent")
    );
}

#[tokio::test]
async fn aborts_while_waiting_for_an_sse_chunk() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    // 永不产出的事件流：模拟“等待下一个 SSE chunk”。
    let client: Arc<dyn HttpClient> = Arc::new(NeverStreamClient {
        requests: Arc::clone(&requests),
    });
    let signal = tokio_util::sync::CancellationToken::new();
    let model = get_model("mistral-large-latest");
    let context = context_with(Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    });
    let event_stream = stream(
        &model,
        &context,
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                signal: Some(signal.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let result = event_stream.result();
    signal.cancel();
    let message = result.await.expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Aborted);
}

struct NeverStreamClient {
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl HttpClient for NeverStreamClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        self.requests.lock().unwrap().push(request);
        Box::pin(async move {
            Ok(HttpResponse {
                response: ProviderResponse {
                    status: 200,
                    headers: [("content-type".to_string(), "text/event-stream".to_string())]
                        .into_iter()
                        .collect(),
                },
                body: Box::pin(futures::stream::pending()),
            })
        })
    }
}

#[tokio::test]
async fn applies_the_request_timeout_while_waiting_for_response_headers() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = mock_client(Arc::clone(&requests), |_request| MockOutcome::Pending);
    let message = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                timeout_ms: Some(5),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Mistral response headers timed out after 5ms")
    );
}

#[tokio::test]
async fn does_not_abort_an_active_stream_that_lasts_longer_than_the_request_timeout() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let thinking_event = json!({"choices": [{"index": 0,
        "delta": {"content": [{"type": "thinking", "thinking": [{"type": "text", "text": "x"}]}]}}]});
    let mut body = String::new();
    for _ in 0..5 {
        body.push_str(&format!(
            "data: {}\n\n",
            serde_json::to_string(&thinking_event).unwrap()
        ));
    }
    body.push_str(&format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&terminal_event("stop")).unwrap()
    ));
    let client = mock_client(Arc::clone(&requests), move |_request| MockOutcome::Stream {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        chunks: vec![body.clone().into_bytes()],
        delay: Some(Duration::from_millis(10)),
    });
    let message = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                timeout_ms: Some(20),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    match &message.content[0] {
        AssistantContentBlock::Thinking(thinking) => assert_eq!(thinking.thinking, "xxxxx"),
        other => panic!("expected thinking, got {other:?}"),
    }
}

#[tokio::test]
async fn preserves_http_status_and_response_bodies_in_errors() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let client = mock_client(Arc::clone(&requests), |_request| MockOutcome::Stream {
        status: 403,
        headers: vec![("content-type".into(), "application/json".into())],
        chunks: vec![b"{\"message\":\"blocked by gateway\"}".to_vec()],
        delay: None,
    });
    let message = stream(
        &get_model("mistral-large-latest"),
        &context_with(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Context::default()
        }),
        MistralOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(client),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Mistral API error (403): {\"message\":\"blocked by gateway\"}")
    );
}

// 保持 `Usage`/`UsageCost` 引用，避免 import 未使用。
#[allow(dead_code)]
fn usage(_: Usage, _: UsageCost) {}

// 计数器避免未使用告警（部分用例共用）。
#[allow(dead_code)]
static _COUNTER: AtomicUsize = AtomicUsize::new(0);
