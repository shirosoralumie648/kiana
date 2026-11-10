//! `test/cloudflare-ai-binding.test.ts` 的 Rust 移植（三条全部覆盖）。
//!
//! 第 3 条通过 `openai-completions` 适配器端到端验证
//! 「`cf-aig-authorization` + `Authorization: null` + `x-api-key: null`」不会把 SDK 的
//! 占位鉴权头带上线。同目录的 `test/cloudflare-stream.test.ts` 由 `tests/cloudflare_stream.rs`
//! 覆盖（`providers/cloudflare_stream.rs`）。

#[path = "fixtures/api-classifier-images/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;
use kiana_ai::api::cloudflare::{
    AiBinding, CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL, create_ai_binding_fetch,
};
use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::http::{ByteStream, HttpRequest, HttpResponse};
use kiana_ai::types::{
    Context, Model, ModelInputModality, ProviderHeaders, ProviderResponse, StopReason,
    StreamOptions, UserContent, UserMessage, UserRole,
};
use tokio_util::sync::CancellationToken;

const BINDING_PREFIX: &str = "https://workers-binding.ai/ai-gateway/gateways/my-gateway";

#[tokio::test]
async fn passes_requests_to_the_binding_untouched() {
    let captured: Arc<Mutex<Vec<HttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_binding = Arc::clone(&captured);
    let binding = AiBinding {
        ai_gateway_log_id: None,
        fetch: Some(Arc::new(
            move |request: HttpRequest, _signal: Option<CancellationToken>| {
                captured_for_binding
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(request);
                Box::pin(async {
                    let mut headers = IndexMap::new();
                    headers.insert("content-type".to_string(), "text/event-stream".to_string());
                    headers.insert("cf-aig-log-id".to_string(), "log-1".to_string());
                    let body: ByteStream = Box::pin(futures::stream::once(async {
                        Ok(b"data: {}\n\n".to_vec())
                    }));
                    Ok(HttpResponse {
                        response: ProviderResponse {
                            status: 200,
                            headers,
                        },
                        body,
                    })
                })
            },
        )),
    };
    let fetch = create_ai_binding_fetch(binding).expect("binding exposes fetch");
    let body =
        serde_json::json!({"model": "claude", "messages": [{"role": "user", "content": "hi"}]});
    let request = HttpRequest {
        method: "POST".to_string(),
        url: format!("{BINDING_PREFIX}/anthropic/v1/messages?beta=true"),
        headers: vec![
            ("content-type".to_string(), "application/json".to_string()),
            (
                "cf-aig-authorization".to_string(),
                format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}"),
            ),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ],
        body: Some(kiana_json::js_object::stringify(&body).into_bytes()),
        timeout_ms: None,
    };

    let response = fetch
        .execute(request.clone(), None)
        .await
        .expect("response");

    let captured = captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].url,
        format!("{BINDING_PREFIX}/anthropic/v1/messages?beta=true")
    );
    assert_eq!(captured[0].method, "POST");
    assert_eq!(
        captured[0]
            .headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["content-type", "cf-aig-authorization", "anthropic-version"]
    );
    assert_eq!(
        captured[0]
            .headers
            .iter()
            .find(|(name, _)| name == "cf-aig-authorization")
            .map(|(_, value)| value.as_str()),
        Some("Bearer cloudflare-gateway-binding")
    );
    assert_eq!(
        captured[0].body.as_deref(),
        Some(kiana_json::js_object::stringify(&body).as_bytes())
    );
    assert_eq!(response.response.status, 200);
    assert_eq!(
        response
            .response
            .headers
            .get("cf-aig-log-id")
            .map(String::as_str),
        Some("log-1")
    );
    let body = response.body.collect::<Vec<_>>().await;
    assert_eq!(body.len(), 1);
    assert_eq!(
        String::from_utf8_lossy(body[0].as_ref().expect("chunk")),
        "data: {}\n\n"
    );
}

#[tokio::test]
async fn keeps_sdk_placeholder_auth_off_the_wire_when_paired_with_null_auth_headers() {
    // 模块文档里的完整头契约，端到端穿过真实 API 实现：sentinel 满足 kiana 的请求鉴权检查，
    // 显式 null 让 OpenAI SDK 在派发前删掉它自己的 `Authorization: Bearer unused` 占位。
    let captured: Arc<Mutex<Vec<HttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_binding = Arc::clone(&captured);
    let binding = AiBinding {
        ai_gateway_log_id: None,
        fetch: Some(Arc::new(
            move |request: HttpRequest, _signal: Option<CancellationToken>| {
                captured_for_binding
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(request);
                Box::pin(async {
                    let mut headers = IndexMap::new();
                    headers.insert("content-type".to_string(), "application/json".to_string());
                    Ok(HttpResponse {
                        response: ProviderResponse {
                            status: 400,
                            headers,
                        },
                        body: Box::pin(futures::stream::once(async {
                            Ok(br#"{"error":{"type":"bad_request","message":"stubbed"}}"#.to_vec())
                        })) as ByteStream,
                    })
                })
            },
        )),
    };
    let fetch = create_ai_binding_fetch(binding).expect("binding exposes fetch");
    let model = Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: "openai-completions".into(),
        provider: "openai".into(),
        base_url: format!("{BINDING_PREFIX}/openai"),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(10_000),
        max_tokens: Some(1_000),
        ..Default::default()
    };
    let mut headers = ProviderHeaders::new();
    headers.insert(
        "cf-aig-authorization".into(),
        Some(format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}")),
    );
    headers.insert("Authorization".into(), None);
    headers.insert("x-api-key".into(), None);
    let context = kiana_ai::utils::transcript::normalize_context(&Context {
        messages: vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    });

    let result = kiana_ai::api::openai_completions::stream(
        &model,
        &context,
        OpenAICompletionsOptions {
            base: StreamOptions {
                headers: Some(headers),
                fetch: Some(fetch),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("stream result");

    assert_eq!(result.stop_reason, StopReason::Error);
    let captured = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0].url,
        format!("{BINDING_PREFIX}/openai/chat/completions")
    );
    let header_names: Vec<String> = captured[0]
        .headers
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    assert!(
        !header_names.iter().any(|name| name == "authorization"),
        "{header_names:?}"
    );
    assert!(
        !header_names.iter().any(|name| name == "x-api-key"),
        "{header_names:?}"
    );
}

#[test]
fn rejects_a_binding_with_no_fetch_at_construction_not_on_first_request() {
    // A well-typed binding on a runtime that predates `Ai#fetch`: `fetch` is optional on the
    // type, so the check has to happen at runtime — early, with a message that names the cause.
    let error = create_ai_binding_fetch(AiBinding {
        ai_gateway_log_id: None,
        fetch: None,
    })
    .err()
    .expect("missing fetch");
    assert!(
        error.message.contains("does not expose fetch()"),
        "{}",
        error.message
    );
    assert_eq!(error.name, "TypeError");
}

#[test]
fn exposes_the_sentinel_value() {
    assert_eq!(
        CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL,
        "cloudflare-gateway-binding"
    );
}
