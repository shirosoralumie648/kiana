//! `azure-openai-base-url.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::azure_openai_config::{AzureEndpointOptions, resolve_azure_base_url};
use kiana_ai::api::azure_openai_responses::AzureOpenAIResponsesOptions;
use kiana_ai::types::{Context, Model, ProviderHeaders, StreamOptions};
use serde_json::{Value, json};

fn model() -> Model {
    mock_http::catalog_model("azure", "gpt-4o-mini")
}

fn completed_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "sequence_number": 1,
        "response": { "id": "resp_azure", "status": "completed" },
    })])
}

fn default_options(mock: &mock_http::MockHttp, api_key: &str) -> AzureOpenAIResponsesOptions {
    AzureOpenAIResponsesOptions {
        base: StreamOptions {
            api_key: Some(api_key.into()),
            fetch: Some(Arc::new(mock.clone())),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn capture_url(base_url: Option<&str>) -> String {
    let mock = mock_http::MockHttp::single(completed_sse());
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = base_url.map(str::to_string);
    let _ = mock_http::run_azure(&model(), &mock_http::context(vec![]), options).await;
    mock.last_request().url
}

#[tokio::test]
async fn normalizes_azure_endpoints_and_builds_the_request_url() {
    for (base_url, expected) in [
        (
            "https://marc-quicktests-resource.cognitiveservices.azure.com",
            "https://marc-quicktests-resource.cognitiveservices.azure.com/openai/v1",
        ),
        (
            "https://marc-quicktests-resource.ai.azure.com",
            "https://marc-quicktests-resource.ai.azure.com/openai/v1",
        ),
        (
            "https://my-resource.openai.azure.com",
            "https://my-resource.openai.azure.com/openai/v1",
        ),
        (
            "https://my-resource.cognitiveservices.azure.com/openai",
            "https://my-resource.cognitiveservices.azure.com/openai/v1",
        ),
        (
            "https://my-resource.cognitiveservices.azure.com/openai/v1",
            "https://my-resource.cognitiveservices.azure.com/openai/v1",
        ),
        (
            "https://my-resource.services.ai.azure.com/openai/v1/responses",
            "https://my-resource.services.ai.azure.com/openai/v1",
        ),
    ] {
        let options = AzureEndpointOptions {
            azure_base_url: Some(base_url.into()),
            ..Default::default()
        };
        let resolved = resolve_azure_base_url(&model(), Some(&options)).expect("resolved");
        assert_eq!(resolved, expected, "{base_url}");
        let url = capture_url(Some(base_url)).await;
        assert_eq!(
            url,
            format!("{expected}/responses?api-version=v1"),
            "{base_url}"
        );
    }

    // 非 Azure 代理路径原样保留（含 query）。
    let options = AzureEndpointOptions {
        azure_base_url: Some("https://my-proxy.example.com/v1".into()),
        ..Default::default()
    };
    assert_eq!(
        resolve_azure_base_url(&model(), Some(&options)).unwrap(),
        "https://my-proxy.example.com/v1"
    );
    let options = AzureEndpointOptions {
        azure_base_url: Some("https://my-proxy.example.com/v1?custom=true".into()),
        ..Default::default()
    };
    assert_eq!(
        resolve_azure_base_url(&model(), Some(&options)).unwrap(),
        "https://my-proxy.example.com/v1?custom=true"
    );

    // Azure 主机上的 query 会被清掉。
    let options = AzureEndpointOptions {
        azure_base_url: Some(
            "https://my-resource.openai.azure.com/openai?api-version=2024-12-01".into(),
        ),
        ..Default::default()
    };
    assert_eq!(
        resolve_azure_base_url(&model(), Some(&options)).unwrap(),
        "https://my-resource.openai.azure.com/openai/v1"
    );
}

#[tokio::test]
async fn surfaces_invalid_urls_as_error_events() {
    let mock = mock_http::MockHttp::single(completed_sse());
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = Some("not-a-url".into());
    let result = mock_http::run_azure(&model(), &mock_http::context(vec![]), options).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("Invalid Azure OpenAI base URL"),
        "{:?}",
        result.error_message
    );
}

#[tokio::test]
async fn builds_the_default_url_from_resource_name_and_respects_env() {
    // 进程级 env 用例集中在这个测试里顺序执行。
    let resource = "my-resource";
    unsafe { std::env::set_var("AZURE_OPENAI_RESOURCE_NAME", resource) };
    let url = capture_url(None).await;
    unsafe { std::env::remove_var("AZURE_OPENAI_RESOURCE_NAME") };
    assert_eq!(
        url,
        "https://my-resource.openai.azure.com/openai/v1/responses?api-version=v1"
    );

    unsafe {
        std::env::set_var(
            "AZURE_OPENAI_BASE_URL",
            "https://env-resource.cognitiveservices.azure.com",
        )
    };
    let url = capture_url(None).await;
    unsafe { std::env::remove_var("AZURE_OPENAI_BASE_URL") };
    assert_eq!(
        url,
        "https://env-resource.cognitiveservices.azure.com/openai/v1/responses?api-version=v1"
    );

    unsafe { std::env::set_var("AZURE_OPENAI_BASE_URL", "not-a-url") };
    let mock = mock_http::MockHttp::single(completed_sse());
    let result = mock_http::run_azure(
        &model(),
        &mock_http::context(vec![]),
        default_options(&mock, "test-api-key"),
    )
    .await;
    unsafe { std::env::remove_var("AZURE_OPENAI_BASE_URL") };
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("Invalid Azure OpenAI base URL")
    );
}

#[tokio::test]
async fn clamps_prompt_cache_key_and_disables_server_side_storage() {
    let mock = mock_http::MockHttp::single(completed_sse());
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = Some("https://my-resource.openai.azure.com".into());
    options.base.session_id = Some("x".repeat(67));
    let _ = mock_http::run_azure(&model(), &mock_http::context(vec![]), options).await;
    let body = mock.last_body();
    assert_eq!(body["prompt_cache_key"], json!("x".repeat(64)));
    assert_eq!(body["store"], json!(false));
}

#[tokio::test]
async fn honors_supports_strict_mode_false() {
    let mut model = model();
    model.compat = Some(json!({ "supportsStrictMode": false }));
    let mock = mock_http::MockHttp::single(completed_sse());
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = Some("https://my-resource.openai.azure.com".into());
    let context = mock_http::context_with(&Context {
        messages: vec![mock_http::user("hi")],
        tools: Some(vec![kiana_ai::types::Tool {
            name: "preferred".into(),
            description: "Preferred constrained tool".into(),
            parameters: json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
            constrained_sampling: Some(
                serde_json::from_value(json!({ "type": "json_schema", "strict": "prefer" }))
                    .unwrap(),
            ),
        }]),
        ..Context::default()
    });
    let _ = mock_http::run_azure(&model, &context, options).await;
    assert!(mock.last_body()["tools"][0].get("strict").is_none());
}

#[tokio::test]
async fn forwards_parsed_events_in_order_before_normalizing_the_response() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_azure" } }),
        json!({ "type": "response.completed", "sequence_number": 1, "response": { "id": "resp_azure", "status": "completed" } }),
    ]));
    let received = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let captured = received.clone();
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = Some("https://my-resource.openai.azure.com".into());
    options.base.on_provider_stream_event = Some(Arc::new(move |event, _model| {
        captured.lock().unwrap().push(event);
    }));
    let result = mock_http::run_azure(&model(), &mock_http::context(vec![]), options).await;
    let received = received.lock().unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0]["type"], json!("response.created"));
    assert_eq!(received[1]["type"], json!("response.completed"));
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(result.response_id.as_deref(), Some("resp_azure"));
}

#[tokio::test]
async fn uses_kiana_user_agent_by_default_and_allows_overrides() {
    let expected = kiana_ai::utils::identity::format_user_agent(None);
    let mock = mock_http::MockHttp::single(completed_sse());
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = Some("https://my-resource.openai.azure.com".into());
    let _ = mock_http::run_azure(&model(), &mock_http::context(vec![]), options).await;
    assert_eq!(
        mock.last_header("user-agent").as_deref(),
        Some(expected.as_str())
    );

    let mut headers = ProviderHeaders::new();
    headers.insert("User-Agent".into(), Some("custom-agent".into()));
    let mock = mock_http::MockHttp::single(completed_sse());
    let mut options = default_options(&mock, "test-api-key");
    options.azure_base_url = Some("https://my-resource.openai.azure.com".into());
    options.base.headers = Some(headers);
    let _ = mock_http::run_azure(&model(), &mock_http::context(vec![]), options).await;
    assert_eq!(
        mock.last_header("user-agent").as_deref(),
        Some("custom-agent")
    );
}
