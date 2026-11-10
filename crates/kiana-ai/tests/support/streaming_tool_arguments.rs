//! `streaming-tool-arguments` 测试的共享夹具：六条 provider 路径的假 SSE 流。
//!
//! 行为用例在 `tests/streaming_tool_arguments.rs`，内存量级/释放用例在
//! `tests/streaming_tool_arguments_memory.rs`（两者都要构造同样的流）。

#![allow(dead_code)]

#[path = "../fixtures/api-openai/mock_http.rs"]
pub mod mock_http;

use std::sync::Arc;

use futures::StreamExt;
use kiana_ai::api::anthropic_messages::AnthropicOptions;
use kiana_ai::api::kiana_messages::KianaMessagesOptions;
use kiana_ai::api::mistral_conversations::MistralOptions;
use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::http::{BoxFuture, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxContent, FauxTokenSize, FauxToolCallOptions,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider_with, faux_tool_call_with,
};
use kiana_ai::types::{
    AssistantContentBlock, Context, JsonObject, Model, ModelInputModality, ProviderResponse,
    SimpleStreamOptions, StopReason, StreamOptions, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

/// `content`: 20 000 行脚本，`writeCall(size)` 从中取前 `size` 个字符。
pub fn content() -> String {
    (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\\\ done\n"))
        .collect()
}

/// `write` 参数（`size` 个字符的文件内容）及其 JSON 文本。
pub fn write_call(size: usize) -> (JsonObject, String) {
    let full = content();
    let mut args = JsonObject::new();
    args.insert("path".into(), json!("/src/file.ts"));
    args.insert("content".into(), json!(full[..size].to_string()));
    let value = Value::Object(
        args.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let text = kiana_json::js_object::stringify(&value);
    (args, text)
}

pub fn deltas_of(text: &str, size: usize) -> Vec<String> {
    text.as_bytes()
        .chunks(size)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect()
}

pub fn base_model(id: &str, name: &str, api: &str, provider: &str, base_url: &str) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: api.into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Default::default()
    }
}

pub fn context() -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("write".into()),
            timestamp: 0,
        })],
        ..Context::default()
    })
}

pub fn stream_options(mock: &mock_http::MockHttp, api_key: &str) -> StreamOptions {
    StreamOptions {
        api_key: Some(api_key.into()),
        fetch: Some(Arc::new(mock.clone())),
        max_retries: Some(0),
        ..Default::default()
    }
}

/// 把 SSE body 按事件切成小块、每块之间 `yield_now` 的假客户端。
///
/// TS 的 `sseFetch` 用 `ReadableStream` 的 `pull` 每次只产出一小批事件，生产/消费因此交错；
/// Rust 适配器的生产任务若一次拿到整包 body，会在一轮 poll 里把所有 partial 事件塞进队列
/// （内存是队列长度 × partial 大小）。内存测量用例需要同样的交错。
pub struct ChunkedHttp {
    chunks: Vec<Vec<u8>>,
}

impl ChunkedHttp {
    pub fn new(body: &str) -> Self {
        let mut chunks = Vec::new();
        let mut start = 0;
        while let Some(index) = body[start..].find("\n\n") {
            let end = start + index + 2;
            chunks.push(body.as_bytes()[start..end].to_vec());
            start = end;
        }
        if start < body.len() {
            chunks.push(body.as_bytes()[start..].to_vec());
        }
        ChunkedHttp { chunks }
    }
}

impl HttpClient for ChunkedHttp {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move {
            let mut headers = indexmap::IndexMap::new();
            headers.insert("content-type".to_string(), "text/event-stream".to_string());
            let chunks = self.chunks.clone();
            let body: kiana_ai::http::ByteStream =
                Box::pin(futures::stream::iter(chunks).then(|chunk| async move {
                    tokio::task::yield_now().await;
                    Ok(chunk)
                }));
            Ok(HttpResponse {
                response: ProviderResponse {
                    status: 200,
                    headers,
                },
                body,
            })
        })
    }
}

/// `chunked=false` 用整包 [`mock_http::MockHttp`]；`chunked=true` 用 [`ChunkedHttp`]。
pub fn http_options(body: &str, chunked: bool, api_key: &str) -> StreamOptions {
    let fetch: Arc<dyn HttpClient> = if chunked {
        Arc::new(ChunkedHttp::new(body))
    } else {
        Arc::new(mock_http::MockHttp::single(
            mock_http::MockResponse::sse_raw(body),
        ))
    };
    StreamOptions {
        api_key: Some(api_key.into()),
        fetch: Some(fetch),
        max_retries: Some(0),
        ..Default::default()
    }
}

pub fn sse(events: &[(&str, Value)], done: bool) -> String {
    let mut body = String::new();
    for (event, data) in events {
        body.push_str("event: ");
        body.push_str(event);
        body.push_str("\ndata: ");
        body.push_str(&kiana_json::js_object::stringify(data));
        body.push_str("\n\n");
    }
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    body
}

pub fn anthropic_body(deltas: &[String]) -> String {
    let mut events: Vec<(&str, Value)> = vec![(
        "message_start",
        json!({
            "type": "message_start",
            "message": { "id": "msg_1", "model": "claude-test", "usage": { "input_tokens": 1, "output_tokens": 1 } },
        }),
    )];
    events.push((
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "write", "input": {} },
        }),
    ));
    for delta in deltas {
        events.push((
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "input_json_delta", "partial_json": delta },
            }),
        ));
    }
    events.push((
        "content_block_stop",
        json!({ "type": "content_block_stop", "index": 0 }),
    ));
    events.push((
        "message_delta",
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 10 } }),
    ));
    events.push(("message_stop", json!({ "type": "message_stop" })));
    sse(&events, false)
}

pub fn chat_body(deltas: &[String], with_id_every_chunk: bool) -> String {
    let base =
        json!({ "id": "chat_1", "object": "chat.completion.chunk", "created": 1, "model": "test" });
    let mut first_chunk = base.as_object().cloned().unwrap_or_default();
    first_chunk.insert(
        "choices".into(),
        json!([{ "index": 0, "delta": { "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": { "name": "write", "arguments": "" },
        }] } }]),
    );
    let mut events: Vec<(&str, Value)> = vec![("", Value::Object(first_chunk))];
    for delta in deltas {
        let mut chunk = base.as_object().cloned().unwrap_or_default();
        let tool_call = if with_id_every_chunk {
            json!({
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "write", "arguments": delta },
            })
        } else {
            json!({
                "index": 0,
                "function": { "name": "write", "arguments": delta },
            })
        };
        chunk.insert(
            "choices".into(),
            json!([{ "index": 0, "delta": { "tool_calls": [tool_call] } }]),
        );
        events.push(("", Value::Object(chunk)));
    }
    let mut final_chunk = base.as_object().cloned().unwrap_or_default();
    final_chunk.insert(
        "choices".into(),
        json!([{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]),
    );
    final_chunk.insert(
        "usage".into(),
        json!({ "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }),
    );
    events.push(("", Value::Object(final_chunk)));
    sse(&events, true)
}

pub fn kiana_messages_body(deltas: &[String], args: &JsonObject) -> String {
    let mut events: Vec<(&str, Value)> = vec![
        ("", json!({ "type": "start" })),
        (
            "",
            json!({ "type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "write" }),
        ),
    ];
    for delta in deltas {
        events.push((
            "",
            json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": delta }),
        ));
    }
    events.push((
        "",
        json!({
            "type": "toolcall_end",
            "contentIndex": 0,
            "toolCall": { "type": "toolCall", "id": "call_1", "name": "write", "arguments": args },
        }),
    ));
    events.push((
        "",
        json!({
            "type": "done",
            "reason": "toolUse",
            "usage": {
                "input": 1,
                "output": 1,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 2,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
            },
        }),
    ));
    sse(&events, false)
}

pub fn responses_body(deltas: &[String], text: &str) -> String {
    let item = json!({
        "type": "function_call",
        "id": "fc_1",
        "call_id": "call_1",
        "name": "write",
        "arguments": "",
        "status": "in_progress",
    });
    let mut completed = item.as_object().cloned().unwrap_or_default();
    completed.insert("arguments".into(), json!(text));
    completed.insert("status".into(), json!("completed"));
    let completed_item = Value::Object(completed);
    let mut events: Vec<(&str, Value)> = vec![(
        "",
        json!({ "type": "response.output_item.added", "output_index": 0, "sequence_number": 0, "item": item }),
    )];
    for delta in deltas {
        events.push((
            "",
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "item_id": "fc_1",
                "sequence_number": 0,
                "delta": delta,
            }),
        ));
    }
    events.push((
        "",
        json!({
            "type": "response.function_call_arguments.done",
            "output_index": 0,
            "item_id": "fc_1",
            "sequence_number": 0,
            "arguments": text,
        }),
    ));
    events.push((
        "",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "sequence_number": 0,
            "item": completed_item,
        }),
    ));
    events.push((
        "",
        json!({
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "status": "completed",
                "output": [],
                "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 },
            },
        }),
    ));
    sse(&events, false)
}

/// 六条 provider 路径。
#[derive(Clone, Copy, Debug)]
pub enum Path {
    Anthropic,
    Completions,
    Responses,
    Mistral,
    KianaMessages,
    Faux,
}

impl Path {
    pub fn name(self) -> &'static str {
        match self {
            Path::Anthropic => "anthropic-messages",
            Path::Completions => "openai-completions",
            Path::Responses => "openai-responses",
            Path::Mistral => "mistral-conversations",
            Path::KianaMessages => "kiana-messages",
            Path::Faux => "faux",
        }
    }

    pub fn stream(
        self,
        deltas: &[String],
        args: &JsonObject,
        text: &str,
    ) -> AssistantMessageEventStream {
        self.stream_with(deltas, args, text, false)
    }

    /// 事件之间让出执行权的版本（内存测量用例；见 [`ChunkedHttp`]）。
    pub fn stream_chunked(
        self,
        deltas: &[String],
        args: &JsonObject,
        text: &str,
    ) -> AssistantMessageEventStream {
        self.stream_with(deltas, args, text, true)
    }

    fn stream_with(
        self,
        deltas: &[String],
        args: &JsonObject,
        text: &str,
        chunked: bool,
    ) -> AssistantMessageEventStream {
        let context = context();
        match self {
            Path::Anthropic => {
                let body = anthropic_body(deltas);
                let model = base_model(
                    "claude-test",
                    "c",
                    "anthropic-messages",
                    "anthropic",
                    "https://api.anthropic.com",
                );
                kiana_ai::api::anthropic_messages::stream(
                    &model,
                    &context,
                    AnthropicOptions {
                        base: http_options(&body, chunked, "sk-ant-test"),
                        ..Default::default()
                    },
                )
            }
            Path::Completions => {
                let body = chat_body(deltas, false);
                let model = base_model(
                    "gpt-test",
                    "g",
                    "openai-completions",
                    "openai",
                    "https://api.openai.com/v1",
                );
                kiana_ai::api::openai_completions::stream(
                    &model,
                    &context,
                    OpenAICompletionsOptions {
                        base: http_options(&body, chunked, "sk-test"),
                        ..Default::default()
                    },
                )
            }
            Path::Responses => {
                let body = responses_body(deltas, text);
                let model = base_model(
                    "gpt-test",
                    "g",
                    "openai-responses",
                    "openai",
                    "https://api.openai.com/v1",
                );
                kiana_ai::api::openai_responses::stream(
                    &model,
                    &context,
                    OpenAIResponsesOptions {
                        base: http_options(&body, chunked, "sk-test"),
                        ..Default::default()
                    },
                )
            }
            Path::Mistral => {
                let body = chat_body(deltas, true);
                let model = base_model(
                    "devstral",
                    "d",
                    "mistral-conversations",
                    "mistral",
                    "https://api.mistral.ai",
                );
                kiana_ai::api::mistral_conversations::stream(
                    &model,
                    &context,
                    MistralOptions {
                        base: http_options(&body, chunked, "test"),
                        ..Default::default()
                    },
                )
            }
            Path::KianaMessages => {
                let body = kiana_messages_body(deltas, args);
                let model = base_model(
                    "auto",
                    "a",
                    "kiana-messages",
                    "radius",
                    "https://example.test/v1",
                );
                kiana_ai::api::kiana_messages::stream(
                    &model,
                    &context,
                    KianaMessagesOptions {
                        base: http_options(&body, chunked, "test"),
                        ..Default::default()
                    },
                )
            }
            Path::Faux => {
                let faux = faux_provider_with(RegisterFauxProviderOptions {
                    token_size: Some(FauxTokenSize {
                        min: Some(8),
                        max: Some(8),
                    }),
                    ..Default::default()
                });
                faux.set_responses(vec![
                    faux_assistant_message(
                        FauxContent::Block(AssistantContentBlock::ToolCall(faux_tool_call_with(
                            "write",
                            args.clone(),
                            FauxToolCallOptions {
                                id: Some("call_1".into()),
                            },
                        ))),
                        FauxAssistantMessageOptions {
                            stop_reason: Some(StopReason::ToolUse),
                            ..Default::default()
                        },
                    )
                    .into(),
                ]);
                let model = faux.get_model(None).expect("faux model").clone();
                faux.provider
                    .stream_simple(&model, &context, SimpleStreamOptions::default())
            }
        }
    }
}
