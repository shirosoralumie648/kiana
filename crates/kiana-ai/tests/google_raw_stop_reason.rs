//! `test/google-raw-stop-reason.test.ts` 的移植（wiremock 直接提供 SSE chunk）。

mod common;

use std::sync::{Arc, Mutex};

use kiana_ai::api::google_generative_ai::{GoogleOptions, stream as stream_google_generative_ai};
use kiana_ai::api::google_vertex::{GoogleVertexOptions, stream as stream_google_vertex};
use kiana_ai::types::{
    AssistantContentBlock, Context, Message, Model, ModelInputModality, StopReason, StreamOptions,
    UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::identity::format_user_agent;
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model(api: &str, provider: &str, id: &str, base_url: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: api.into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

fn sse(chunks: &[Value]) -> String {
    chunks
        .iter()
        .map(|chunk| format!("data: {}\n\n", serde_json::to_string(chunk).unwrap()))
        .collect()
}

async fn mount_gemini(server: &MockServer, model_id: &str, body: String) {
    Mock::given(method("POST"))
        .and(path(format!("/models/{model_id}:streamGenerateContent")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn mount_vertex(server: &MockServer, model_id: &str, body: String) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/publishers/google/models/{model_id}:streamGenerateContent"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

fn gemini_options() -> GoogleOptions {
    GoogleOptions {
        base: StreamOptions {
            api_key: Some("test-api-key".into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn vertex_options() -> GoogleVertexOptions {
    GoogleVertexOptions {
        base: StreamOptions {
            api_key: Some("test-api-key".into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_errors() {
    let server = MockServer::start().await;
    mount_gemini(
        &server,
        "gemini-2.5-flash",
        sse(&[json!({
            "responseId": "google-response-id",
            "candidates": [{"finishReason": "MALFORMED_FUNCTION_CALL"}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 0, "totalTokenCount": 1},
        })]),
    )
    .await;
    let model = model(
        "google-generative-ai",
        "google",
        "gemini-2.5-flash",
        &server.uri(),
    );
    let message = stream_google_generative_ai(&model, &context(), gemini_options())
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.raw_stop_reason.as_deref(),
        Some("MALFORMED_FUNCTION_CALL")
    );
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: MALFORMED_FUNCTION_CALL")
    );
}

#[tokio::test]
async fn accepts_lone_surrogate_escapes_in_streamed_chunks() {
    // serde_json 会拒绝 `\ud800`；processStreamResponse 的 JSON.parse 接受它。
    // Rust 侧按 ARCHITECTURE §8 第 31 条把孤立代理项落成 U+FFFD。
    let server = MockServer::start().await;
    let body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\\ud800\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n";
    mount_gemini(&server, "gemini-2.5-flash", body.to_string()).await;
    let model = model(
        "google-generative-ai",
        "google",
        "gemini-2.5-flash",
        &server.uri(),
    );
    let message = stream_google_generative_ai(&model, &context(), gemini_options())
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    match &message.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "\u{fffd}"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn preserves_raw_vertex_finish_reasons_for_errors() {
    let server = MockServer::start().await;
    mount_vertex(
        &server,
        "gemini-3-flash-preview",
        sse(&[json!({
            "responseId": "vertex-response-id",
            "candidates": [{"finishReason": "SAFETY"}],
        })]),
    )
    .await;
    let model = model(
        "google-vertex",
        "google-vertex",
        "gemini-3-flash-preview",
        &server.uri(),
    );
    let message = stream_google_vertex(&model, &context(), vertex_options())
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("SAFETY"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: SAFETY")
    );
}

#[tokio::test]
async fn preserves_max_tokens_with_a_tool_call_as_length() {
    for use_vertex in [false, true] {
        let server = MockServer::start().await;
        let id = if use_vertex {
            "gemini-3-flash-preview"
        } else {
            "gemini-2.5-flash"
        };
        let body = sse(&[json!({
            "candidates": [{
                "finishReason": "MAX_TOKENS",
                "content": {"parts": [{"functionCall": {"id": "call-1", "name": "echo", "args": {"value": "truncated"}}}]},
            }],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 0, "totalTokenCount": 1},
        })]);
        if use_vertex {
            mount_vertex(&server, id, body).await;
        } else {
            mount_gemini(&server, id, body).await;
        }
        let model = if use_vertex {
            model("google-vertex", "google-vertex", id, &server.uri())
        } else {
            model("google-generative-ai", "google", id, &server.uri())
        };
        let message = if use_vertex {
            stream_google_vertex(&model, &context(), vertex_options())
                .result()
                .await
        } else {
            stream_google_generative_ai(&model, &context(), gemini_options())
                .result()
                .await
        }
        .expect("result");
        assert_eq!(message.stop_reason, StopReason::Length, "{id}");
        assert_eq!(message.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
        assert!(
            message
                .content
                .iter()
                .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
        );
    }
}

#[tokio::test]
async fn maps_stop_with_a_tool_call_to_tool_use() {
    for use_vertex in [false, true] {
        let server = MockServer::start().await;
        let id = if use_vertex {
            "gemini-3-flash-preview"
        } else {
            "gemini-2.5-flash"
        };
        let body = sse(&[json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [{"functionCall": {"id": "call-1", "name": "echo", "args": {}}}]},
            }],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 0, "totalTokenCount": 1},
        })]);
        if use_vertex {
            mount_vertex(&server, id, body).await;
        } else {
            mount_gemini(&server, id, body).await;
        }
        let model = if use_vertex {
            model("google-vertex", "google-vertex", id, &server.uri())
        } else {
            model("google-generative-ai", "google", id, &server.uri())
        };
        let message = if use_vertex {
            stream_google_vertex(&model, &context(), vertex_options())
                .result()
                .await
        } else {
            stream_google_generative_ai(&model, &context(), gemini_options())
                .result()
                .await
        }
        .expect("result");
        assert_eq!(message.stop_reason, StopReason::ToolUse, "{id}");
        assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    }
}

#[tokio::test]
async fn forwards_each_sdk_chunk_in_order_before_normalizing_it() {
    for use_vertex in [false, true] {
        let server = MockServer::start().await;
        let id = if use_vertex {
            "gemini-3-flash-preview"
        } else {
            "gemini-2.5-flash"
        };
        let chunks = vec![
            json!({"responseId": "resp_google", "candidates": [{"content": {"parts": [{"text": "hello"}]}}]}),
            json!({"candidates": [{"finishReason": "STOP"}],
                   "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 1, "totalTokenCount": 3}}),
        ];
        let body = sse(&chunks);
        if use_vertex {
            mount_vertex(&server, id, body).await;
        } else {
            mount_gemini(&server, id, body).await;
        }
        let model = if use_vertex {
            model("google-vertex", "google-vertex", id, &server.uri())
        } else {
            model("google-generative-ai", "google", id, &server.uri())
        };
        let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let received_cb = Arc::clone(&received);
        let event_models: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let models_cb = Arc::clone(&event_models);
        let on_event: kiana_ai::types::OnProviderStreamEventFn =
            Arc::new(move |chunk, event_model| {
                received_cb.lock().unwrap().push(chunk);
                models_cb.lock().unwrap().push(event_model.provider.clone());
            });
        let result = if use_vertex {
            stream_google_vertex(
                &model,
                &context(),
                GoogleVertexOptions {
                    base: StreamOptions {
                        api_key: Some("test-api-key".into()),
                        on_provider_stream_event: Some(on_event),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .result()
            .await
        } else {
            stream_google_generative_ai(
                &model,
                &context(),
                GoogleOptions {
                    base: StreamOptions {
                        api_key: Some("test-api-key".into()),
                        on_provider_stream_event: Some(on_event),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .result()
            .await
        }
        .expect("result");
        assert_eq!(*received.lock().unwrap(), chunks);
        assert_eq!(event_models.lock().unwrap().len(), 2);
        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(result.response_id.as_deref(), Some("resp_google"));
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            AssistantContentBlock::Text(text) => assert_eq!(text.text, "hello"),
            other => panic!("expected text, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn uses_pis_user_agent_and_lets_explicit_headers_override_it() {
    let server = MockServer::start().await;
    mount_gemini(
        &server,
        "gemini-2.5-flash",
        sse(&[json!({
            "candidates": [{"finishReason": "STOP"}],
        })]),
    )
    .await;
    let gemini_model = model(
        "google-generative-ai",
        "google",
        "gemini-2.5-flash",
        &server.uri(),
    );
    let _ = stream_google_generative_ai(&gemini_model, &context(), gemini_options())
        .result()
        .await;
    let requests = server.received_requests().await.expect("requests");
    let headers: std::collections::HashMap<_, _> = requests[0]
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_lowercase(),
                value.to_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        headers.get("user-agent").map(String::as_str),
        Some(format_user_agent(None).as_str())
    );
    assert_eq!(
        headers.get("x-goog-api-key").map(String::as_str),
        Some("test-api-key")
    );

    // 显式头覆盖默认 User-Agent（新服务器，避免 mock 匹配顺序问题）。
    let server = MockServer::start().await;
    mount_gemini(
        &server,
        "gemini-2.5-flash",
        sse(&[json!({
            "candidates": [{"finishReason": "STOP"}],
        })]),
    )
    .await;
    let overridden_model = model(
        "google-generative-ai",
        "google",
        "gemini-2.5-flash",
        &server.uri(),
    );
    let _ = stream_google_generative_ai(
        &overridden_model,
        &context(),
        GoogleOptions {
            base: StreamOptions {
                api_key: Some("test-api-key".into()),
                headers: Some(
                    [("User-Agent".to_string(), Some("custom-agent".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await;
    let requests = server.received_requests().await.expect("requests");
    let headers: std::collections::HashMap<_, _> = requests[0]
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_lowercase(),
                value.to_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        headers.get("user-agent").map(String::as_str),
        Some("custom-agent")
    );
}
