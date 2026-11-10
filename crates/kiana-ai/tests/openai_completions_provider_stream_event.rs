//! `openai-completions-provider-stream-event.test.ts`。
//!
//! 断言 provider chunk（含 OpenRouter 计费元数据）按顺序透传给
//! `onProviderStreamEvent`，以及 OpenRouter 上报成本的处理。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_ai::http::{BoxFuture, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    AssistantContentBlock, Context, Message, Model, ProviderResponse, SimpleStreamOptions,
    StreamOptions, TranscriptContext, UserContent, UserMessage, UserRole,
};
use serde_json::{Value, json};

mod mock {
    use super::*;

    /// 记录请求并返回预设响应的 `HttpClient`。
    type ResponseQueue = VecDeque<(u16, IndexMap<String, String>, Vec<u8>)>;

    #[derive(Clone, Default)]
    pub struct MockHttp {
        pub requests: Arc<Mutex<Vec<HttpRequest>>>,
        pub responses: Arc<Mutex<ResponseQueue>>,
    }

    impl MockHttp {
        pub fn new(responses: Vec<(u16, IndexMap<String, String>, Vec<u8>)>) -> Self {
            MockHttp {
                requests: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    impl HttpClient for MockHttp {
        fn execute<'a>(
            &'a self,
            request: HttpRequest,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request);
                let (status, headers, body) =
                    self.responses.lock().unwrap().pop_front().unwrap_or((
                        500,
                        IndexMap::new(),
                        b"{\"error\":\"no response\"}".to_vec(),
                    ));
                let stream: kiana_ai::http::ByteStream =
                    Box::pin(futures::stream::iter(vec![Ok(body)]));
                Ok(HttpResponse {
                    response: ProviderResponse { status, headers },
                    body: stream,
                })
            })
        }
    }
}

fn openrouter_model(provider: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model("openrouter", "openrouter/auto")
        .expect("openrouter/auto in catalog")
        .clone();
    let mut model: Model = serde_json::from_value(value).expect("model");
    model.provider = provider.to_string();
    model
}

fn context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hi".into()),
            timestamp: 1_700_000_000_000,
        })],
        ..Context::default()
    })
}

fn sse(chunks: &[Value]) -> Vec<u8> {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&kiana_json::js_object::stringify(chunk));
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

fn options(mock: &mock::MockHttp, events: Arc<Mutex<Vec<Value>>>) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(Arc::new(mock.clone())),
            on_provider_stream_event: Some(Arc::new(move |event, _model| {
                events.lock().unwrap().push(event);
            })),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    }
}

#[tokio::test]
async fn exposes_provider_chunks_including_openrouter_metadata() {
    let first_chunk = json!({
        "id": "chatcmpl-1",
        "model": "anthropic/claude-sonnet-4.6",
        "choices": [{ "index": 0, "delta": { "content": "hello" } }],
    });
    let final_chunk = json!({
        "id": "chatcmpl-1",
        "model": "anthropic/claude-sonnet-4.6",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12,
            "cost": 0.0012,
            "is_byok": false,
        },
        "openrouter_metadata": { "strategy": "direct", "region": "iad" },
    });
    let mock = mock::MockHttp::new(vec![(
        200,
        IndexMap::new(),
        sse(&[first_chunk.clone(), final_chunk.clone()]),
    )]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let message = kiana_ai::api::openai_completions::stream_simple(
        &openrouter_model("openrouter"),
        &context(),
        options(&mock, events.clone()),
    )
    .expect("stream")
    .result()
    .await
    .expect("result");

    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(kiana_ai::types::TextContent {
            text: "hello".into(),
            ..Default::default()
        })]
    );
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], first_chunk);
    assert_eq!(events[1], final_chunk);
}

#[tokio::test]
async fn uses_openrouter_reported_total_without_double_counting_non_byok_upstream_cost() {
    let chunk = json!({
        "id": "chatcmpl-cost",
        "model": "openrouter/auto",
        "choices": [{ "index": 0, "delta": { "content": "done" }, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "cost": 0.004,
            "is_byok": false,
            "cost_details": { "upstream_inference_cost": 0.003 },
        },
    });
    let mock = mock::MockHttp::new(vec![(200, IndexMap::new(), sse(&[chunk]))]);
    let message = kiana_ai::api::openai_completions::stream_simple(
        &openrouter_model("openrouter-work"),
        &context(),
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock)),
                ..StreamOptions::default()
            },
            ..SimpleStreamOptions::default()
        },
    )
    .expect("stream")
    .result()
    .await
    .expect("result");

    assert_eq!(message.usage.cost.total, 0.004);
    assert_eq!(message.usage.cost.total_from_provider, Some(true));
}

#[tokio::test]
async fn includes_upstream_inference_cost_for_openrouter_byok_requests() {
    let chunk = json!({
        "id": "chatcmpl-byok",
        "model": "openrouter/auto",
        "choices": [{ "index": 0, "delta": { "content": "done" }, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "cost": 0.0002,
            "is_byok": true,
            "cost_details": { "upstream_inference_cost": 0.003 },
        },
    });
    let mock = mock::MockHttp::new(vec![(200, IndexMap::new(), sse(&[chunk]))]);
    let message = kiana_ai::api::openai_completions::stream_simple(
        &openrouter_model("openrouter"),
        &context(),
        SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock)),
                ..StreamOptions::default()
            },
            ..SimpleStreamOptions::default()
        },
    )
    .expect("stream")
    .result()
    .await
    .expect("result");

    assert!((message.usage.cost.total - 0.0032).abs() < 1e-12);
    assert_eq!(message.usage.cost.total_from_provider, Some(true));
}
