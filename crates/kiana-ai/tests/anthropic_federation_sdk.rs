//! `anthropic-federation-sdk.test.ts` 的 Rust 移植：OIDC token 交换缓存与头自有鉴权。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{Model, ModelCost, ModelInputModality, UserMessage, UserRole};

fn model(port: u16) -> Model {
    Model {
        id: "claude-test".into(),
        name: "Claude Test".into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        base_url: format!("http://127.0.0.1:{port}"),
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

fn sse_body() -> String {
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
    .collect()
}

fn env_with_identity(path: &str) -> indexmap::IndexMap<String, String> {
    indexmap::IndexMap::from([
        (
            "ANTHROPIC_FEDERATION_RULE_ID".to_string(),
            "fdrl_test".to_string(),
        ),
        (
            "ANTHROPIC_ORGANIZATION_ID".to_string(),
            "org-test".to_string(),
        ),
        (
            "ANTHROPIC_IDENTITY_TOKEN_FILE".to_string(),
            path.to_string(),
        ),
    ])
}

#[tokio::test]
async fn exchanges_the_identity_token_once_across_requests() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let exchanges = Arc::new(AtomicUsize::new(0));
    let exchanges_for_handler = exchanges.clone();
    let fetch = MockHttpClient::with_handler(move |request| {
        if request.url.ends_with("/v1/oauth/token") {
            exchanges_for_handler.fetch_add(1, Ordering::SeqCst);
            return Ok(MockResponse::json(
                200,
                "{\"access_token\":\"federated-token\",\"expires_in\":3600}",
            ));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let identity =
        std::env::temp_dir().join(format!("kiana-sdk-identity-{}.jwt", std::process::id()));
    std::fs::write(&identity, "identity-token").expect("write identity token");
    let anthropic = model(47002);
    let env = env_with_identity(&identity.to_string_lossy());

    for _ in 0..2 {
        let mut options = AnthropicOptions {
            base: harness::stream_options(fetch.clone()),
            ..Default::default()
        };
        options.base.api_key = None;
        options.base.env = Some(env.clone());
        let result =
            harness::run_to_result(stream(&anthropic, &context(None, vec![user()]), options)).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    }
    let _ = std::fs::remove_file(&identity);
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    let messages_requests: Vec<_> = fetch
        .requests()
        .into_iter()
        .filter(|request| request.url.contains("/v1/messages"))
        .collect();
    assert_eq!(messages_requests.len(), 2);
    for request in messages_requests {
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
                    && value == "Bearer federated-token")
        );
    }
}

#[tokio::test]
async fn does_not_exchange_for_header_owned_auth() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(sse_body()));
    let identity =
        std::env::temp_dir().join(format!("kiana-sdk-header-{}.jwt", std::process::id()));
    std::fs::write(&identity, "identity-token").expect("write identity token");
    let anthropic = model(47003);
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch.clone()),
        ..Default::default()
    };
    options.base.api_key = None;
    options.base.env = Some(env_with_identity(&identity.to_string_lossy()));
    options.base.headers = Some(indexmap::IndexMap::from([(
        "Authorization".to_string(),
        Some("Bearer header-token".to_string()),
    )]));
    let result =
        harness::run_to_result(stream(&anthropic, &context(None, vec![user()]), options)).await;
    let _ = std::fs::remove_file(&identity);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let requests = fetch.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.as_str()),
        Some("Bearer header-token")
    );
}

// ---------------------------------------------------------------------------
// review 回归：SDK `TokenCache` 的 120s/30s 分档、单槽 client 缓存、
// WorkloadIdentityError 的文本/脱敏/提示。
// ---------------------------------------------------------------------------

/// SDK 的 `federationClient` 是进程级单槽；测试并行运行会互相替换该槽位，因此串行化。
static FEDERATION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn identity_file(tag: &str) -> (std::path::PathBuf, String) {
    let path = std::env::temp_dir().join(format!("kiana-sdk-{tag}-{}.jwt", std::process::id()));
    std::fs::write(&path, "identity-token").expect("write identity token");
    let env_value = path.to_string_lossy().to_string();
    (path, env_value)
}

async fn run_stream(
    model: &Model,
    env: &indexmap::IndexMap<String, String>,
    fetch: Arc<MockHttpClient>,
) -> kiana_ai::types::AssistantMessage {
    let mut options = AnthropicOptions {
        base: harness::stream_options(fetch),
        ..Default::default()
    };
    options.base.api_key = None;
    options.base.env = Some(env.clone());
    harness::run_to_result(stream(model, &context(None, vec![user()]), options)).await
}

#[tokio::test]
async fn refreshes_a_cached_token_inside_the_mandatory_window() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let issued = Arc::new(AtomicUsize::new(0));
    let issued_for_handler = issued.clone();
    let fetch = MockHttpClient::with_handler(move |request| {
        if request.url.ends_with("/v1/oauth/token") {
            let n = issued_for_handler.fetch_add(1, Ordering::SeqCst) + 1;
            return Ok(MockResponse::json(
                200,
                format!("{{\"access_token\":\"token-{n}\",\"expires_in\":20}}"),
            ));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("mandatory");
    let env = env_with_identity(&identity_env);
    let anthropic = model(47010);
    for _ in 0..2 {
        let result = run_stream(&anthropic, &env, fetch.clone()).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    }
    let _ = std::fs::remove_file(&identity);
    assert_eq!(issued.load(Ordering::SeqCst), 2);
    let messages_requests: Vec<_> = fetch
        .requests()
        .into_iter()
        .filter(|request| request.url.contains("/v1/messages"))
        .collect();
    assert_eq!(messages_requests.len(), 2);
    let authorization = |request: &kiana_ai::http::HttpRequest| {
        request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        authorization(&messages_requests[0]).as_deref(),
        Some("Bearer token-1")
    );
    assert_eq!(
        authorization(&messages_requests[1]).as_deref(),
        Some("Bearer token-2")
    );
}

#[tokio::test]
async fn returns_the_stale_token_and_refreshes_in_the_background_inside_the_advisory_window() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let issued = Arc::new(AtomicUsize::new(0));
    let issued_for_handler = issued.clone();
    let fetch = MockHttpClient::with_handler(move |request| {
        if request.url.ends_with("/v1/oauth/token") {
            let n = issued_for_handler.fetch_add(1, Ordering::SeqCst) + 1;
            return Ok(MockResponse::json(
                200,
                format!("{{\"access_token\":\"token-{n}\",\"expires_in\":60}}"),
            ));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("advisory");
    let env = env_with_identity(&identity_env);
    let anthropic = model(47011);
    for _ in 0..2 {
        let result = run_stream(&anthropic, &env, fetch.clone()).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    }
    // advisory 刷新在后台完成：第二次请求仍用旧 token。
    for _ in 0..200 {
        if issued.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let _ = std::fs::remove_file(&identity);
    assert!(issued.load(Ordering::SeqCst) >= 2);
    let messages_requests: Vec<_> = fetch
        .requests()
        .into_iter()
        .filter(|request| request.url.contains("/v1/messages"))
        .collect();
    for request in messages_requests {
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
                    && value == "Bearer token-1"),
            "advisory window must serve the stale token"
        );
    }
}

#[tokio::test]
async fn re_exchanges_the_token_when_the_federation_base_url_changes() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let issued = Arc::new(AtomicUsize::new(0));
    let issued_for_handler = issued.clone();
    let fetch = MockHttpClient::with_handler(move |request| {
        if request.url.ends_with("/v1/oauth/token") {
            let n = issued_for_handler.fetch_add(1, Ordering::SeqCst) + 1;
            return Ok(MockResponse::json(
                200,
                format!("{{\"access_token\":\"token-{n}\",\"expires_in\":3600}}"),
            ));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("single-slot");
    let env = env_with_identity(&identity_env);
    // TS 的 `federationClient` 只有单槽：交替的 baseUrl 会丢弃旧 token cache。
    for port in [47012u16, 47013, 47012] {
        let result = run_stream(&model(port), &env, fetch.clone()).await;
        assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    }
    let _ = std::fs::remove_file(&identity);
    assert_eq!(issued.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn reports_sdk_style_token_exchange_errors_with_redaction_and_401_hint() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let fetch = MockHttpClient::with_handler(|request| {
        if request.url.ends_with("/v1/oauth/token") {
            return Ok(MockResponse::json(
                401,
                "{\"error\":\"invalid_grant\",\"error_description\":\"bad assertion\",\"assertion\":\"secret\"}",
            )
            .with_headers(vec![("request-id", "req_1")]));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("error-401");
    let env = env_with_identity(&identity_env);
    let result = run_stream(&model(47014), &env, fetch.clone()).await;
    let _ = std::fs::remove_file(&identity);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    // RFC 6749 白名单之外的字段（assertion）被丢弃；401 附带排查提示。
    assert_eq!(
        result.error_message.as_deref(),
        Some(
            "Token exchange failed with status 401 (request-id req_1): {\"error\":\"invalid_grant\",\"error_description\":\"bad assertion\"} Ensure your federation rule matches your identity token. If your federation rule is scoped to multiple workspaces, set the ANTHROPIC_WORKSPACE_ID environment variable, the 'workspace_id' config key, or the `workspaceId` option. View your authentication events in the Workload identity page of Claude Console for more details."
        )
    );
    // token 交换失败不会被 `retryProviderRequest` 重试，也不会发出 messages 请求。
    assert_eq!(
        fetch
            .requests()
            .iter()
            .filter(|request| request.url.ends_with("/v1/oauth/token"))
            .count(),
        1
    );
    assert!(
        fetch
            .requests()
            .iter()
            .all(|request| !request.url.contains("/v1/messages"))
    );
}

#[tokio::test]
async fn reports_non_json_and_incomplete_token_responses_with_redaction() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let mode = Arc::new(AtomicUsize::new(0));
    let mode_for_handler = mode.clone();
    let fetch = MockHttpClient::with_handler(move |request| {
        if request.url.ends_with("/v1/oauth/token") {
            return match mode_for_handler.load(Ordering::SeqCst) {
                0 => Ok(MockResponse::json(200, "not json")),
                _ => Ok(MockResponse::json(200, "{\"access_token\":\"x\"}")),
            };
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("bad-responses");
    let env = env_with_identity(&identity_env);
    let anthropic = model(47015);

    mode.store(0, Ordering::SeqCst);
    let result = run_stream(&anthropic, &env, fetch.clone()).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some("Token endpoint returned non-JSON response (status 200)")
    );

    mode.store(1, Ordering::SeqCst);
    let result = run_stream(&anthropic, &env, fetch.clone()).await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    // `redactSensitive` 只保留 RFC 6749 字段 → `access_token` 被剥离。
    assert_eq!(
        result.error_message.as_deref(),
        Some("Token endpoint response missing required fields: {}")
    );
    let _ = std::fs::remove_file(&identity);
    assert!(
        fetch
            .requests()
            .iter()
            .all(|request| !request.url.contains("/v1/messages"))
    );
}

#[tokio::test]
async fn accepts_a_string_expires_in_like_number() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let fetch = MockHttpClient::with_handler(|request| {
        if request.url.ends_with("/v1/oauth/token") {
            return Ok(MockResponse::json(
                200,
                "{\"access_token\":\"token-1\",\"expires_in\":\"3600\"}",
            ));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("string-expires");
    let env = env_with_identity(&identity_env);
    let result = run_stream(&model(47016), &env, fetch.clone()).await;
    let _ = std::fs::remove_file(&identity);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    // token 请求使用 client 的 User-Agent（SDK `config.userAgent || getUserAgent()`）。
    let token_request = fetch
        .requests()
        .into_iter()
        .find(|request| request.url.ends_with("/v1/oauth/token"))
        .expect("token request");
    let header = |name: &str| {
        token_request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("user-agent"), Some("Anthropic/JS 0.129.0"));
    assert_eq!(
        header("anthropic-beta"),
        Some("oauth-2025-04-20,oidc-federation-2026-04-01")
    );
}

#[tokio::test]
async fn reports_the_sdk_reachability_error_with_the_token_url() {
    let _guard = FEDERATION_TEST_LOCK.lock().await;
    let fetch = MockHttpClient::with_handler(|request| {
        if request.url.ends_with("/v1/oauth/token") {
            return Err(kiana_ai::http::HttpError::new("fetch failed"));
        }
        Ok(MockResponse::sse(sse_body()))
    });
    let (identity, identity_env) = identity_file("unreachable");
    let env = env_with_identity(&identity_env);
    let result = run_stream(&model(47017), &env, fetch.clone()).await;
    let _ = std::fs::remove_file(&identity);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    // TS: `Failed to reach token endpoint ${url}: ${err}`（`${err}` 是 "TypeError: ..."）。
    assert_eq!(
        result.error_message.as_deref(),
        Some(
            "Failed to reach token endpoint http://127.0.0.1:47017/v1/oauth/token: TypeError: fetch failed"
        )
    );
}
