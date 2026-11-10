//! `model-runtime-cloudflare-compat.test.ts` 的移植（2 个用例）。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::Arc;

use kiana_ai::types::StreamOptions;
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::json;
use support::*;

const CLOUDFLARE_COMPAT_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/test-account/test-gateway/compat/chat/completions";

fn cloudflare_chunk() -> String {
    let chunk = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn create_cloudflare_runtime() -> (Arc<ModelRuntime>, ModelRegistry) {
    let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
        "cloudflare-ai-gateway": {
            "type": "api_key",
            "key": "test-token",
            "env": {
                "CLOUDFLARE_ACCOUNT_ID": "test-account",
                "CLOUDFLARE_GATEWAY_ID": "test-gateway"
            }
        }
    })));
    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(
                Arc::new(auth_storage) as Arc<dyn kiana_ai::auth::types::CredentialStore>
            ),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: false,
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    let registry = ModelRegistry::new(Arc::clone(&runtime));
    (runtime, registry)
}

#[tokio::test]
async fn materializes_the_cloudflare_endpoint_through_model_runtime_streaming() {
    let (runtime, _registry) = create_cloudflare_runtime().await;
    let model = runtime
        .get_model(
            "cloudflare-ai-gateway",
            "workers-ai/@cf/moonshotai/kimi-k2.6",
        )
        .expect("model");

    let client = MockHttpClient::new(|_request| Ok(MockResponse::sse(&cloudflare_chunk())));
    let result = runtime
        .complete_simple(
            &model,
            &kiana_ai::types::Context::default(),
            kiana_ai::types::SimpleStreamOptions {
                base: StreamOptions {
                    fetch: client.fetch(),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        )
        .await;

    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, CLOUDFLARE_COMPAT_URL);
    assert_eq!(
        requests[0].header("cf-aig-authorization"),
        Some("Bearer test-token")
    );
}

#[tokio::test]
async fn materializes_the_cloudflare_endpoint_after_extension_style_auth_resolution() {
    let (_runtime, registry) = create_cloudflare_runtime().await;
    let model = registry
        .find(
            "cloudflare-ai-gateway",
            "workers-ai/@cf/moonshotai/kimi-k2.6",
        )
        .expect("model");

    let auth = registry.get_api_key_and_headers(&model).await;
    let kiana_config::models::registry::ResolvedRequestAuth::Ok(auth) = auth else {
        panic!("expected ok");
    };
    let headers = auth.headers.clone().expect("headers");
    assert_eq!(
        header_value(&headers, "cf-aig-authorization"),
        Some(Some("Bearer test-token"))
    );
    assert_eq!(header_value(&headers, "Authorization"), Some(None));
    assert_eq!(header_value(&headers, "x-api-key"), Some(None));

    let client = MockHttpClient::new(|_request| Ok(MockResponse::sse(&cloudflare_chunk())));
    let result = kiana_ai::compat::complete(
        &model,
        &kiana_ai::types::Context::default(),
        StreamOptions {
            api_key: auth.api_key.clone(),
            headers: auth.headers.clone(),
            env: auth.env.clone(),
            fetch: client.fetch(),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, CLOUDFLARE_COMPAT_URL);
    assert_eq!(
        requests[0].header("cf-aig-authorization"),
        Some("Bearer test-token")
    );
    assert!(requests[0].header("authorization").is_none());
    assert!(requests[0].header("x-api-key").is_none());
}
