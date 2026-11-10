//! `test/mistral-tool-schema.test.ts` 的移植。
//!
//! TS 断言 TypeBox 对象上的 symbol 键不会进入 payload（Rust 的 `Tool.parameters` 是纯 JSON，
//! 天然没有 symbol）；这里保留其余断言：`strict: true` 与 schema 结构。

use std::sync::{Arc, Mutex};

use kiana_ai::api::mistral_conversations::stream;
use kiana_ai::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StreamOptions, Tool, UserContent, UserMessage,
    UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

struct FailingClient;

impl HttpClient for FailingClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> kiana_ai::http::BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move { Err(HttpError::new("connection refused")) })
    }
}

#[tokio::test]
async fn serializes_tool_schemas_with_strict_mode() {
    let model = Model {
        id: "devstral-medium-latest".into(),
        name: "Devstral Medium".into(),
        api: "mistral-conversations".into(),
        provider: "mistral".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    };
    let context = normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hi".into()),
            timestamp: 1,
        })],
        tools: Some(vec![Tool {
            name: "inspect_schema".into(),
            description: "Inspect the schema".into(),
            parameters: json!({
                "type": "object",
                "properties": {"nested": {"type": "object", "properties": {"value": {"type": "string"}}}},
            }),
            constrained_sampling: Some(kiana_ai::types::ConstrainedSampling::Config(
                kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
                    strict: kiana_ai::types::StrictMode::Require,
                },
            )),
        }]),
        ..Context::default()
    });

    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_cb = Arc::clone(&captured);
    let response = stream(
        &model,
        &context,
        kiana_ai::api::mistral_conversations::MistralOptions {
            base: StreamOptions {
                api_key: Some("fake-key".into()),
                fetch: Some(Arc::new(FailingClient)),
                on_payload: Some(Arc::new(move |payload, _model| {
                    *captured_cb.lock().unwrap() = Some(payload.clone());
                    None
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    let payload = captured.lock().unwrap().clone().expect("payload");
    let tools = payload["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], json!("function"));
    assert_eq!(tools[0]["function"]["name"], json!("inspect_schema"));
    assert_eq!(tools[0]["function"]["strict"], json!(true));
    // TS 只断言 payload 里没有 symbol 键、strict === true；收紧后的 schema 结构由
    // constrained-sampling 单元负责，这里只确认参数对象存在且是对象。
    assert!(tools[0]["function"]["parameters"].is_object());
    assert!(tools[0]["function"]["parameters"]["properties"]["nested"].is_object());
    assert_eq!(response.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        !response
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("Input validation failed")
    );
}
