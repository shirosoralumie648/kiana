//! `test/google-vertex-api-key-resolution.test.ts` 的移植。
//!
//! TS 通过 mock `GoogleGenAI` 构造函数捕获配置；Rust 侧直接断言
//! [`create_client`]/[`create_client_with_api_key`] 的返回值（同一组字段）。

use kiana_ai::api::google_vertex::{
    GoogleVertexOptions, create_client, create_client_with_api_key, resolve_api_key,
};
use kiana_ai::types::{Model, ModelInputModality, StreamOptions};
use kiana_ai::utils::identity::format_user_agent;

fn model() -> Model {
    Model {
        id: "gemini-3-flash-preview".into(),
        name: "Gemini 3 Flash Preview".into(),
        api: "google-vertex".into(),
        provider: "google-vertex".into(),
        base_url: "https://{location}-aiplatform.googleapis.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(1_000_000),
        max_tokens: Some(65_536),
        ..Model::default()
    }
}

fn options(api_key: Option<&str>) -> GoogleVertexOptions {
    GoogleVertexOptions {
        base: StreamOptions {
            api_key: api_key.map(str::to_string),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn with_options(api_key: Option<&str>) -> GoogleVertexOptions {
    options(api_key)
}

#[test]
fn falls_back_to_adc_when_options_api_key_is_a_placeholder_marker() {
    for api_key in ["<authenticated>", "gcp-vertex-credentials"] {
        let options = options(Some(api_key));
        assert!(resolve_api_key(&options).is_none());
        let config = create_client(&model(), "test-project", "us-central1", None, None, None);
        assert!(config.vertexai);
        assert_eq!(config.project.as_deref(), Some("test-project"));
        assert_eq!(config.location.as_deref(), Some("us-central1"));
        assert_eq!(config.api_version.as_deref(), Some("v1"));
        assert!(config.api_key.is_none());
    }
}

#[test]
fn still_uses_the_api_key_client_for_real_api_keys() {
    let options = with_options(Some("AIzaSyExampleRealisticLookingApiKey123456"));
    let api_key = resolve_api_key(&options).expect("api key");
    let config = create_client_with_api_key(&model(), &api_key, None, None);
    assert!(config.vertexai);
    assert_eq!(
        config.api_key.as_deref(),
        Some("AIzaSyExampleRealisticLookingApiKey123456")
    );
    assert_eq!(config.api_version.as_deref(), Some("v1"));
    assert!(config.project.is_none());
    assert!(config.location.is_none());
}

#[test]
fn does_not_forward_generated_vertex_base_url_placeholders() {
    let config = create_client(&model(), "test-project", "us-central1", None, None, None);
    let http = config.http_options.expect("http options");
    assert!(http.base_url.is_none());
    assert_eq!(
        http.headers,
        Some(
            [("User-Agent".to_string(), format_user_agent(None))]
                .into_iter()
                .collect()
        )
    );
}

#[test]
fn lets_explicit_headers_override_the_default_user_agent() {
    let headers = [("User-Agent".to_string(), Some("custom-agent".to_string()))]
        .into_iter()
        .collect();
    let config = create_client(
        &model(),
        "test-project",
        "us-central1",
        Some(&headers),
        None,
        None,
    );
    let http = config.http_options.expect("http options");
    assert_eq!(
        http.headers,
        Some(
            [("User-Agent".to_string(), "custom-agent".to_string())]
                .into_iter()
                .collect()
        )
    );
}

#[test]
fn forwards_custom_base_url_to_the_adc_client() {
    let mut model = model();
    model.base_url = "https://proxy.example.com".into();
    let config = create_client(&model, "test-project", "us-central1", None, None, None);
    let http = config.http_options.expect("http options");
    assert_eq!(http.base_url.as_deref(), Some("https://proxy.example.com"));
    assert_eq!(http.base_url_resource_scope.as_deref(), Some("COLLECTION"));
    assert!(http.api_version.is_none());
}

#[test]
fn forwards_custom_base_url_to_the_api_key_client() {
    let mut model = model();
    model.base_url = "https://proxy.example.com".into();
    let config = create_client_with_api_key(
        &model,
        "AIzaSyExampleRealisticLookingApiKey123456",
        None,
        None,
    );
    let http = config.http_options.expect("http options");
    assert_eq!(http.base_url.as_deref(), Some("https://proxy.example.com"));
    assert_eq!(http.base_url_resource_scope.as_deref(), Some("COLLECTION"));
}

#[test]
fn does_not_append_api_version_when_custom_base_url_already_includes_one() {
    let mut model = model();
    model.base_url = "https://proxy.example.com/v1/projects/test-project/locations/global".into();
    let config = create_client(&model, "test-project", "us-central1", None, None, None);
    let http = config.http_options.expect("http options");
    assert_eq!(
        http.base_url.as_deref(),
        Some("https://proxy.example.com/v1/projects/test-project/locations/global")
    );
    assert_eq!(http.base_url_resource_scope.as_deref(), Some("COLLECTION"));
    assert_eq!(http.api_version.as_deref(), Some(""));
}
