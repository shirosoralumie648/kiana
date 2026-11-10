//! `model-catalog-protocol.test.ts` 的客户端一半（1 个用例）。
//!
//! 服务端选择逻辑（`scripts/model-catalog-protocol.ts`）不在本单元范围内；这里用
//! `wiremock` 复刻 kiana.dev 的「无版本 URL → 307 → 带 `kiana-version` URL」协商，并断言客户端
//! 最终命中的 provider 请求路径。

#[path = "fixtures/model_runtime/mod.rs"]
mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::oauth::{OAuthFetchFn, OAuthFetchResponse, set_default_oauth_fetch_for_tests};
use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::types::{
    Context, Message, SimpleStreamOptions, StopReason, StreamOptions, UserContent, UserMessage,
    UserRole,
};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde_json::json;
use support::*;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL_ID: &str = "anthropic/claude-sonnet-5";

fn catalog_body() -> serde_json::Value {
    json!({
        MODEL_ID: {
            "id": MODEL_ID,
            "name": "Claude Sonnet 5",
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 },
            "contextWindow": 200000,
            "maxTokens": 64000,
            "api": "anthropic-messages",
            "baseUrl": "https://openrouter.ai/api",
            "futureCatalogField": {"preserved": true}
        }
    })
}

#[tokio::test]
async fn negotiates_the_catalog_for_its_version_and_reaches_the_openrouter_api() {
    let server = MockServer::start().await;
    let versioned_url = format!(
        "{}/api/models/providers/openrouter?types=chat%2Cimage%2Cclassifier&kiana-version=0.0.0",
        server.uri()
    );

    // 带版本 URL：返回目录分片。Last-Modified 远未来，客户端会应用远端 overlay。
    Mock::given(method("GET"))
        .and(path("/api/models/providers/openrouter"))
        .and(query_param("types", "chat,image,classifier"))
        .and(query_param("kiana-version", "0.0.0"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("last-modified", "Fri, 01 Jan 2099 00:00:00 GMT")
                .insert_header("x-kiana-model-catalog-revision", "sha256-test")
                .set_body_json(catalog_body()),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    // 无版本 URL：kiana.dev 按 User-Agent 选择 revision 后 307 到带版本 URL。
    Mock::given(method("GET"))
        .and(path("/api/models/providers/openrouter"))
        .and(query_param("types", "chat,image,classifier"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", versioned_url.as_str()))
        .with_priority(2)
        .mount(&server)
        .await;

    // TS 用 `vi.stubGlobal("fetch")`：个性化请求也必须被拦截，测试不能依赖外网。
    set_default_oauth_fetch_for_tests(Some(Arc::new(OAuthFetchFn(
        |request: kiana_ai::auth::oauth::OAuthFetchRequest| {
            let future: kiana_ai::auth::oauth::OAuthFetchFuture = Box::pin(async move {
                assert_eq!(request.url, "https://openrouter.ai/api/v1/models/user");
                Ok(OAuthFetchResponse::json_response(404, &json!({})))
            });
            future
        },
    ))));

    let runtime = Arc::new(
        ModelRuntime::create(CreateModelRuntimeOptions {
            credentials: Some(Arc::new(InMemoryCredentialStore::new())),
            models_path: ModelsPathOption::Disabled,
            allow_model_network: true,
            catalog_base_url: Some(server.uri()),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .await
        .expect("runtime"),
    );
    runtime
        .set_runtime_api_key("openrouter", "test-key", Default::default())
        .await
        .expect("runtime key");

    let refresh = runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(true),
            force: Some(true),
            providers: Some(vec!["openrouter".to_string()]),
            ..Default::default()
        })
        .await
        .expect("refresh");
    set_default_oauth_fetch_for_tests(None);
    assert!(refresh.errors.is_empty(), "errors: {:?}", refresh.errors);

    let requests = server.received_requests().await.unwrap_or_default();
    let queries: Vec<String> = requests
        .iter()
        .map(|request| request.url.query().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        queries,
        vec![
            "types=chat%2Cimage%2Cclassifier".to_string(),
            "types=chat%2Cimage%2Cclassifier&kiana-version=0.0.0".to_string()
        ]
    );

    let model = runtime
        .get_model("openrouter", MODEL_ID)
        .expect("remote model");
    assert_eq!(model.api, "anthropic-messages");
    assert_eq!(model.base_url, "https://openrouter.ai/api");
    // Remote overlays also pass through OpenRouter's personalization. Keep the
    // TS shallow-spread shape rather than dropping fields or panicking.
    let all = runtime.get_all_models(Some("openrouter"));
    let remote = all
        .iter()
        .find(|candidate| candidate.model().id == MODEL_ID)
        .expect("remote model in public list");
    assert_eq!(
        serde_json::to_value(remote).unwrap()["futureCatalogField"],
        json!({"preserved": true})
    );

    // 请求最终到达 OpenRouter 的 Anthropic 兼容路径。
    let client = MockHttpClient::new(|_request| {
        Ok(MockResponse {
            status: 500,
            headers: IndexMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: serde_json::to_vec(&json!({ "error": { "message": "captured" } })).expect("json"),
        })
    });
    let message = runtime
        .complete_simple(
            &model,
            &Context {
                messages: vec![Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text("Hello".to_string()),
                    timestamp: 0,
                })],
                ..Default::default()
            },
            SimpleStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_string()),
                    max_retries: Some(0),
                    fetch: client.fetch(),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        )
        .await;
    assert_eq!(message.stop_reason, StopReason::Error);

    let provider_requests = client.requests();
    // 只断言 pathname（Anthropic 兼容端点可能附加查询参数，如 `?beta=true`）。
    assert_eq!(
        provider_requests[0].url.split('?').next(),
        Some("https://openrouter.ai/api/v1/messages")
    );
}
