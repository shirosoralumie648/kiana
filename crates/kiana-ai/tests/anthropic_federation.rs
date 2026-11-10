//! `anthropic-federation.test.ts` 的 Rust 移植。
//!
//! TS 用 mock SDK 断言构造参数；Rust 侧没有 SDK，直接断言 [`get_anthropic_federation`]
//! 的配置与请求头（federation 的 token 交换由本模块自行实现，见 `anthropic_federation_sdk.rs`）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, get_anthropic_federation, stream};
use kiana_ai::types::{Model, ModelCost, ModelInputModality, UserMessage, UserRole};

fn env(pairs: &[(&str, &str)]) -> indexmap::IndexMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn federation_env() -> indexmap::IndexMap<String, String> {
    env(&[
        ("ANTHROPIC_FEDERATION_RULE_ID", "fdrl_test"),
        ("ANTHROPIC_ORGANIZATION_ID", "org-test"),
        ("ANTHROPIC_SERVICE_ACCOUNT_ID", "svac_test"),
        ("ANTHROPIC_IDENTITY_TOKEN_FILE", "/tmp/identity.jwt"),
    ])
}

fn model(provider: &str, base_url: &str) -> Model {
    Model {
        id: "claude-test".into(),
        name: "Claude Test".into(),
        api: "anthropic-messages".into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(100_000),
        max_tokens: Some(4096),
        ..Model::default()
    }
}

fn user() -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text("Hello".into()),
        timestamp: 0,
    })
}

#[test]
fn resolves_federation_variables() {
    let config = get_anthropic_federation(
        &model("anthropic", "https://api.anthropic.com"),
        None,
        None,
        Some(&federation_env()),
    )
    .expect("federation");
    assert_eq!(config.organization_id, "org-test");
    assert_eq!(config.workspace_id, None);
    assert_eq!(config.federation_rule_id, "fdrl_test");
    assert_eq!(config.service_account_id.as_deref(), Some("svac_test"));
    assert_eq!(config.identity_token_path, "/tmp/identity.jwt");
    assert_eq!(
        config.to_json(),
        serde_json::json!({
            "organization_id": "org-test",
            "authentication": {
                "type": "oidc_federation",
                "federation_rule_id": "fdrl_test",
                "service_account_id": "svac_test",
                "identity_token": { "source": "file", "path": "/tmp/identity.jwt" },
            },
        })
    );
}

#[test]
fn passes_workspace_id_through_and_treats_service_account_as_optional() {
    let mut with_workspace = federation_env();
    with_workspace.insert("ANTHROPIC_WORKSPACE_ID".into(), "wrkspc_test".into());
    let config = get_anthropic_federation(
        &model("anthropic", "https://api.anthropic.com"),
        None,
        None,
        Some(&with_workspace),
    )
    .expect("federation");
    assert_eq!(config.workspace_id.as_deref(), Some("wrkspc_test"));

    let mut partial = federation_env();
    partial.shift_remove("ANTHROPIC_SERVICE_ACCOUNT_ID");
    let config = get_anthropic_federation(
        &model("anthropic", "https://api.anthropic.com"),
        None,
        None,
        Some(&partial),
    )
    .expect("federation");
    assert_eq!(config.service_account_id, None);
}

#[test]
fn is_not_configured_when_a_required_variable_is_missing() {
    let mut partial = federation_env();
    partial.shift_remove("ANTHROPIC_IDENTITY_TOKEN_FILE");
    assert!(
        get_anthropic_federation(
            &model("anthropic", "https://api.anthropic.com"),
            None,
            None,
            Some(&partial)
        )
        .is_none()
    );
}

#[test]
fn keeps_api_key_and_auth_header_precedence_over_federation() {
    let anthropic = model("anthropic", "https://api.anthropic.com");
    assert!(
        get_anthropic_federation(&anthropic, Some("api-key"), None, Some(&federation_env()))
            .is_none()
    );
    let headers =
        indexmap::IndexMap::from([("Authorization".to_string(), Some("Bearer x".to_string()))]);
    assert!(
        get_anthropic_federation(&anthropic, None, Some(&headers), Some(&federation_env()))
            .is_none()
    );
}

#[tokio::test]
async fn hands_federation_a_bearer_token_without_x_api_key() {
    let fetch = MockHttpClient::with_handler(|request| {
        if request.url.ends_with("/v1/oauth/token") {
            return Ok(MockResponse::json(
                200,
                "{\"access_token\":\"federated-token\",\"expires_in\":3600}",
            ));
        }
        Ok(MockResponse::sse(
            [
                (
                    "message_start",
                    serde_json::json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
                ),
                (
                    "message_delta",
                    serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }),
                ),
                ("message_stop", serde_json::json!({ "type": "message_stop" })),
            ]
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect::<String>(),
        ))
    });
    let identity = std::env::temp_dir().join(format!("kiana-identity-{}.jwt", std::process::id()));
    std::fs::write(&identity, "identity-token").expect("write identity token");
    let mut env = federation_env();
    env.insert(
        "ANTHROPIC_IDENTITY_TOKEN_FILE".into(),
        identity.to_string_lossy().to_string(),
    );
    let anthropic = model("anthropic", "http://127.0.0.1:47001");
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = None;
    options.base.env = Some(env);
    let result =
        harness::run_to_result(stream(&anthropic, &context(None, vec![user()]), options)).await;
    let _ = std::fs::remove_file(&identity);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);

    let requests = fetch.requests();
    let messages_request = requests
        .iter()
        .find(|request| request.url.contains("/v1/messages"))
        .expect("messages request");
    let header = |name: &str| {
        messages_request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("authorization"), Some("Bearer federated-token"));
    assert_eq!(header("x-api-key"), None);
    // SDK `prepareRequest`：token-cache 鉴权（federation）时在 /v1/messages 上追加
    // `oauth-2025-04-20`（request-id 回归：此前 Rust 完全不带 anthropic-beta）。
    assert_eq!(header("anthropic-beta"), Some("oauth-2025-04-20"));
    let body = harness::decode_body(messages_request.body.as_deref().unwrap()).unwrap();
    assert!(body.get("system").is_none());
}

#[tokio::test]
async fn does_not_federate_other_anthropic_messages_providers() {
    let kimi = model("kimi-coding", "https://api.kimi.com/coding");
    let mut options = AnthropicOptions::default();
    options.base.env = Some(federation_env());
    let result = harness::run_to_result(stream(&kimi, &context(None, vec![user()]), options)).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("No API key for provider: kimi-coding")
    );
}

#[tokio::test]
async fn appends_the_oauth_beta_to_explicit_beta_headers_for_federation() {
    let fetch = MockHttpClient::with_handler(|request| {
        if request.url.ends_with("/v1/oauth/token") {
            return Ok(MockResponse::json(
                200,
                "{\"access_token\":\"federated-token\",\"expires_in\":3600}",
            ));
        }
        Ok(MockResponse::sse(
            [
                (
                    "message_start",
                    serde_json::json!({ "type": "message_start", "message": { "id": "msg_test", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
                ),
                (
                    "message_delta",
                    serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1 } }),
                ),
                ("message_stop", serde_json::json!({ "type": "message_stop" })),
            ]
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect::<String>(),
        ))
    });
    let identity =
        std::env::temp_dir().join(format!("kiana-beta-identity-{}.jwt", std::process::id()));
    std::fs::write(&identity, "identity-token").expect("write identity token");
    let mut env = federation_env();
    env.insert(
        "ANTHROPIC_IDENTITY_TOKEN_FILE".into(),
        identity.to_string_lossy().to_string(),
    );
    let anthropic = model("anthropic", "http://127.0.0.1:47020");
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = None;
    options.base.env = Some(env);
    options.base.headers = Some(indexmap::IndexMap::from([(
        "anthropic-beta".to_string(),
        Some("existing-beta".to_string()),
    )]));
    let result =
        harness::run_to_result(stream(&anthropic, &context(None, vec![user()]), options)).await;
    let _ = std::fs::remove_file(&identity);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let requests = fetch.requests();
    let messages_request = requests
        .iter()
        .find(|request| request.url.contains("/v1/messages"))
        .expect("messages request");
    let beta = messages_request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .map(|(_, value)| value.as_str());
    // SDK `prepareRequest`：追加到已有的 beta 列表末尾（不重复、不改顺序）。
    assert_eq!(beta, Some("existing-beta,oauth-2025-04-20"));
}
