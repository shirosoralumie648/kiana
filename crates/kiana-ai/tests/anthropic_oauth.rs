//! `test/anthropic-oauth.test.ts` 的移植（7 个用例）。

use std::sync::{Arc, Mutex};

use kiana_ai::auth::oauth::AnthropicOAuth;
use kiana_ai::auth::oauth::testing::{
    MockFetch, MockInteraction, PromptFuture, VirtualClock, pending_prompt, responding_prompt,
};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthEvent, AuthPrompt, AuthPromptRequest, OAuthAuth};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

/// 53692 是固定端口：同文件内的用例串行执行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 本机回调页面：绕过环境里的 HTTP 代理（loopback 请求不应被代理转发）。
fn loopback_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("loopback client")
    });
    &CLIENT
}

async fn get_loopback(url: impl reqwest::IntoUrl) -> reqwest::Result<reqwest::Response> {
    loopback_client().get(url).send().await
}

fn json_response(body: Value) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(200, &body)
}

fn token_response() -> OAuthFetchResponse {
    json_response(json!({
        "access_token": "access-token",
        "refresh_token": "refresh-token",
        "expires_in": 3600,
    }))
}

fn oauth_with(fetch: &Arc<MockFetch>) -> AnthropicOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    AnthropicOAuth::with_fetch(fetch)
}

fn prompt_type(request: &AuthPromptRequest) -> &'static str {
    request.prompt.type_name()
}

/// 轮询事件，直到出现 `auth_url`。
async fn wait_for_auth_url(interaction: &MockInteraction) -> String {
    for _ in 0..100_000 {
        if let Some(url) = interaction.auth_url() {
            return url;
        }
        tokio::task::yield_now().await;
    }
    panic!("auth_url event was not emitted");
}

#[tokio::test]
async fn keeps_the_localhost_redirect_uri_for_manual_callback_login() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(request.method, "POST");
        let body: Value =
            serde_json::from_str(&request.body_text()).expect("JSON token exchange body");
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["code"], "manual-code");
        assert_eq!(body["redirect_uri"], "http://localhost:53692/callback");
        Box::pin(async { Ok(token_response()) })
    });
    let oauth = oauth_with(&fetch);

    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            "manual_code" => {
                let events = events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                let auth_url = events
                    .into_iter()
                    .find_map(|event| match event {
                        AuthEvent::AuthUrl { url, .. } => Some(url),
                        _ => None,
                    })
                    .expect("auth_url before manual prompt");
                let url = url::Url::parse(&auth_url).expect("auth url");
                let params: std::collections::HashMap<_, _> =
                    url.query_pairs().into_owned().collect();
                let state = params.get("state").expect("state");
                let redirect_uri = params.get("redirect_uri").expect("redirect_uri");
                responding_prompt(format!("{redirect_uri}?code=manual-code&state={state}"))
            }
            other => panic!("Unexpected prompt: {other}"),
        }
    });

    let credentials = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credentials.access, "access-token");
    assert_eq!(credentials.refresh, "refresh-token");
    assert_eq!(fetch.request_count(), 1);
}

#[tokio::test]
async fn offers_browser_login_first_and_uses_the_selected_anthropic_copy_code_flow() {
    let _guard = SERIAL.lock().await;
    let select_prompts: Arc<Mutex<Vec<AuthPrompt>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let select_prompts = Arc::clone(&select_prompts);
        move |request: OAuthFetchRequest| {
            let _ = &select_prompts;
            assert_eq!(request.url, "https://platform.claude.com/v1/oauth/token");
            let body: Value =
                serde_json::from_str(&request.body_text()).expect("JSON token exchange body");
            assert_eq!(body["grant_type"], "authorization_code");
            assert_eq!(body["code"], "copied-code");
            assert_eq!(
                body["redirect_uri"],
                "https://platform.claude.com/oauth/code/callback"
            );
            Box::pin(async { Ok(token_response()) })
        }
    });
    let oauth = oauth_with(&fetch);

    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    let prompts = Arc::clone(&select_prompts);
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => {
                prompts
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(request.prompt.clone());
                responding_prompt("copy_code")
            }
            "manual_code" => {
                let events = events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                let auth_url = events
                    .into_iter()
                    .find_map(|event| match event {
                        AuthEvent::AuthUrl { url, .. } => Some(url),
                        _ => None,
                    })
                    .expect("auth_url");
                let url = url::Url::parse(&auth_url).expect("auth url");
                let state = url
                    .query_pairs()
                    .find(|(key, _)| key == "state")
                    .map(|(_, value)| value.into_owned())
                    .expect("state");
                responding_prompt(format!("copied-code#{state}"))
            }
            other => panic!("Unexpected prompt: {other}"),
        }
    });

    let credentials = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credentials.access, "access-token");
    assert_eq!(fetch.request_count(), 1);

    let auth_url = interaction.auth_url().expect("auth url");
    let url = url::Url::parse(&auth_url).expect("url");
    let redirect_uri = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned());
    assert_eq!(
        redirect_uri.as_deref(),
        Some("https://platform.claude.com/oauth/code/callback")
    );

    let prompts = select_prompts
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(prompts.len(), 1);
    match &prompts[0] {
        AuthPrompt::Select {
            message, options, ..
        } => {
            assert_eq!(message, "Select Anthropic login method:");
            let pairs: Vec<(&str, &str)> = options
                .iter()
                .map(|option| (option.id.as_str(), option.label.as_str()))
                .collect();
            assert_eq!(
                pairs,
                vec![
                    ("browser", "Browser login (default)"),
                    ("copy_code", "Copy code login (headless)"),
                ]
            );
        }
        other => panic!("unexpected select prompt: {other:?}"),
    }
}

#[tokio::test]
async fn cancels_when_anthropic_login_method_selection_is_cancelled() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request| Box::pin(async { Ok(token_response()) }));
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| {
        Box::pin(async { Err(kiana_ai::auth::types::AuthError::new("Login cancelled")) })
    });
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("cancelled");
    assert_eq!(error.message, "Login cancelled");
}

#[tokio::test]
async fn omits_scope_from_refresh_token_requests() {
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, "https://platform.claude.com/v1/oauth/token");
        assert_eq!(request.method, "POST");
        let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
        assert_eq!(body["grant_type"], "refresh_token");
        assert!(
            body["client_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(body["refresh_token"], "refresh-token");
        assert!(body.get("scope").is_none());
        Box::pin(async {
            Ok(json_response(json!({
                "access_token": "new-access-token",
                "refresh_token": "new-refresh-token",
                "expires_in": 3600,
            })))
        })
    });
    let oauth = oauth_with(&fetch);
    let credentials = oauth
        .refresh(
            &kiana_ai::auth::types::OAuthCredential::new("old-access-token", "refresh-token", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(credentials.access, "new-access-token");
    assert_eq!(credentials.refresh, "new-refresh-token");
    assert_eq!(fetch.request_count(), 1);
}

#[tokio::test]
async fn resolves_through_the_manual_code_prompt_and_aborts_it_after_settling() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert!(request.url.contains("/oauth/token"), "{}", request.url);
        Box::pin(async {
            Ok(json_response(json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "expires_in": 3600,
            })))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    let manual_signal: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));
    let signal_slot = Arc::clone(&manual_signal);

    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            "manual_code" => {
                *signal_slot
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = request.signal.clone();
                responding_prompt("the-code")
            }
            other => panic!("Unexpected prompt: {other}"),
        }
    });

    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credential.access, "access");
    assert!(
        interaction
            .events()
            .iter()
            .any(|event| matches!(event, AuthEvent::AuthUrl { .. }))
    );
    let signal = manual_signal
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("manual prompt had a signal");
    assert!(
        signal.is_aborted(),
        "prompt signal is aborted once login settles"
    );
}

/// 浏览器回调登录的公共辅助：返回 (credential, redirectUri, 交换的 code/redirect_uri, 页面状态与文本)。
struct BrowserLogin {
    credential: kiana_ai::auth::types::OAuthCredential,
    redirect_uri: String,
    exchanged_code: Option<String>,
    exchanged_redirect_uri: Option<String>,
    page_status: u16,
    page_text: String,
}

async fn login_through_browser_callback(
    prefer_port: u16,
) -> (BrowserLogin, Vec<OAuthFetchRequest>) {
    let exchanged: Arc<Mutex<(Option<String>, Option<String>)>> =
        Arc::new(Mutex::new((None, None)));
    let exchange_slot = Arc::clone(&exchanged);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        let slot = Arc::clone(&exchange_slot);
        Box::pin(async move {
            assert_eq!(request.url, "https://platform.claude.com/v1/oauth/token");
            let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
            let mut slot = slot.lock().unwrap_or_else(|error| error.into_inner());
            slot.0 = body["code"].as_str().map(str::to_string);
            slot.1 = body["redirect_uri"].as_str().map(str::to_string);
            drop(slot);
            Ok(json_response(json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "expires_in": 3600,
            })))
        })
    });
    let fetch_handle: Arc<dyn OAuthFetch> = fetch.clone();
    let oauth = AnthropicOAuth::with_fetch(fetch_handle);
    let _ = prefer_port;

    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            _ => pending_prompt(),
        }
    });

    let handle = {
        let interaction = Arc::clone(&interaction);
        let oauth = Arc::new(oauth);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    let auth_url = wait_for_auth_url(&interaction).await;
    let url = url::Url::parse(&auth_url).expect("auth url");
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let redirect_uri = params.get("redirect_uri").expect("redirect_uri").clone();
    let state = params.get("state").expect("state").clone();

    let mut callback_url = url::Url::parse(&redirect_uri).expect("callback url");
    // 浏览器从 127.0.0.1 访问（redirect host 是 localhost）。
    callback_url.set_host(Some("127.0.0.1")).expect("host");
    callback_url.set_query(Some(
        &url::form_urlencoded::Serializer::new(String::new())
            .append_pair("code", "browser-code")
            .append_pair("state", &state)
            .finish(),
    ));
    let page = get_loopback(callback_url.as_str())
        .await
        .expect("callback page");
    let page_status = page.status().as_u16();
    let page_text = page.text().await.expect("page text");

    let credential = handle.await.expect("join").expect("login");
    let requests = fetch.requests();
    let (code, exchanged_redirect) = exchanged
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    (
        BrowserLogin {
            credential,
            redirect_uri,
            exchanged_code: code,
            exchanged_redirect_uri: exchanged_redirect,
            page_status,
            page_text,
        },
        requests,
    )
}

#[tokio::test]
async fn completes_login_through_the_browser_callback_and_shows_the_sign_in_page() {
    let _guard = SERIAL.lock().await;
    let (login, requests) = login_through_browser_callback(53692).await;
    assert_eq!(login.credential.access, "access");
    assert_eq!(login.exchanged_code.as_deref(), Some("browser-code"));
    assert_eq!(
        login.exchanged_redirect_uri.as_deref(),
        Some(login.redirect_uri.as_str())
    );
    assert_eq!(login.page_status, 200);
    assert!(login.page_text.contains("Signed in to Anthropic."));
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn falls_back_to_a_free_callback_port_when_the_preferred_port_cannot_be_bound() {
    let _guard = SERIAL.lock().await;
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 53692)).expect("blocker");
    let (login, _requests) = login_through_browser_callback(53692).await;
    drop(blocker);

    let redirect = url::Url::parse(&login.redirect_uri).expect("redirect uri");
    assert_eq!(redirect.host_str(), Some("localhost"));
    assert_eq!(redirect.path(), "/callback");
    assert_ne!(redirect.port(), Some(53692));
    assert_eq!(login.credential.access, "access");
    assert_eq!(
        login.exchanged_redirect_uri.as_deref(),
        Some(login.redirect_uri.as_str())
    );
    assert_eq!(login.page_status, 200);
}

#[tokio::test]
async fn unknown_login_methods_are_rejected() {
    let _guard = SERIAL.lock().await;
    let oauth = AnthropicOAuth::new();
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| responding_prompt("carrier-pigeon"));
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("unknown method");
    assert_eq!(
        error.message,
        "Unknown Anthropic login method: carrier-pigeon"
    );
}

#[tokio::test]
async fn missing_manual_codes_are_rejected() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request| Box::pin(async { Ok(token_response()) }));
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("copy_code"),
            _ => responding_prompt("   "),
        }
    });
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("missing code");
    assert_eq!(error.message, "Missing authorization code");
    assert_eq!(fetch.request_count(), 0);
}

#[tokio::test]
async fn state_mismatches_are_rejected() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request| Box::pin(async { Ok(token_response()) }));
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("copy_code"),
            _ => responding_prompt("the-code#wrong-state"),
        }
    });
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("bad state");
    assert_eq!(error.message, "OAuth state mismatch");
}

/// 回归：TS 的 `postJson` 只有一个 `Content-Type: application/json`。
///
/// Rust 曾显式加一次、`json_body` 再加一次；传输层 `HeaderMap::append` 会把两行都写到线上。
#[tokio::test]
async fn sends_exactly_one_content_type_header() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let content_types: Vec<String> = request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone())
            .collect();
        assert_eq!(
            content_types,
            vec!["application/json".to_string()],
            "duplicate Content-Type on the wire"
        );
        assert_eq!(request.header_value("Accept"), Some("application/json"));
        Box::pin(async { Ok(token_response()) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("copy_code"),
            _ => responding_prompt("the-code"),
        }
    });
    oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(fetch.request_count(), 1);
}

/// 回归：TS 的 `if (parsed.state && …)` 是**真值**判断，空串 state 不触发 mismatch；
/// 且 `state = parsed.state ?? verifier` 保留空串（`""` 不是 nullish）。
#[tokio::test]
async fn accepts_an_empty_state_in_the_pasted_code() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
        assert_eq!(body["code"], "the-code");
        assert_eq!(body["state"], "");
        Box::pin(async { Ok(token_response()) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("copy_code"),
            _ => responding_prompt("the-code#"),
        }
    });
    oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("empty state is falsy in TS");
    assert_eq!(fetch.request_count(), 1);
}

/// 回归：`new URLSearchParams("?code=…&state=…")` 会去掉一个前导 `?`；Rust 曾把它当成
/// 首键 `"?code"`，导致粘贴带 `?` 的查询串时报 "Missing authorization code"。
#[tokio::test]
async fn accepts_a_pasted_query_string_with_a_leading_question_mark() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
        assert_eq!(body["code"], "manual-code");
        Box::pin(async { Ok(token_response()) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            "manual_code" => {
                let events = events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                let auth_url = events
                    .into_iter()
                    .find_map(|event| match event {
                        AuthEvent::AuthUrl { url, .. } => Some(url),
                        _ => None,
                    })
                    .expect("auth_url before manual prompt");
                let url = url::Url::parse(&auth_url).expect("auth url");
                let params: std::collections::HashMap<_, _> =
                    url.query_pairs().into_owned().collect();
                responding_prompt(format!(
                    "?code=manual-code&state={}",
                    params.get("state").expect("state")
                ))
            }
            other => panic!("Unexpected prompt: {other}"),
        }
    });
    oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("leading question mark");
    assert_eq!(fetch.request_count(), 1);
}

/// 回归（评审发现）：TS 的 `expires = Date.now() + tokenData.expires_in * 1000 - 5min`
/// 对 `expires_in` 做 JS `ToNumber`（字符串/布尔/null/数组都参与算术）。期望值来自
/// Node 26 实测的 deltas：`"3600"` → +3,300,000ms、`true` → -299,000ms、`null`/`""` →
/// -300,000ms、`["7"]` → -293,000ms、`"0x10"` → -284,000ms、缺失/对象 → NaN（JSON null）。
#[tokio::test(start_paused = true)]
async fn refresh_applies_js_numeric_coercion_to_expires_in() {
    const START_MS: f64 = 1_784_851_200_000.0;
    const MARGIN_MS: f64 = 300_000.0;
    let cases: Vec<(Value, Option<f64>)> = vec![
        (json!(3600), Some(3600.0)),
        (json!("3600"), Some(3600.0)),
        (json!(true), Some(1.0)),
        (json!(false), Some(0.0)),
        (json!(null), Some(0.0)),
        (json!(""), Some(0.0)),
        (json!(" 12 "), Some(12.0)),
        (json!(["7"]), Some(7.0)),
        (json!("0x10"), Some(16.0)),
        (json!("abc"), None),
        (json!({}), None),
    ];
    for (expires_in, expected_seconds) in cases {
        let body_expires_in = expires_in.clone();
        let fetch = MockFetch::new(move |_request: OAuthFetchRequest| {
            let expires_in = body_expires_in.clone();
            Box::pin(async move {
                Ok(json_response(json!({
                    "access_token": "a",
                    "refresh_token": "r",
                    "expires_in": expires_in,
                })))
            })
        });
        let clock = VirtualClock::new(START_MS);
        let oauth = AnthropicOAuth::with_dependencies(fetch.clone(), clock);
        let credential = oauth
            .refresh(
                &kiana_ai::auth::types::OAuthCredential::new("old", "r", 0.0),
                &AbortScope::new(),
            )
            .await
            .expect("refresh");
        match expected_seconds {
            Some(seconds) => assert_eq!(
                credential.expires,
                START_MS + seconds * 1000.0 - MARGIN_MS,
                "expires_in {expires_in}"
            ),
            None => assert!(
                credential.expires.is_nan(),
                "expires_in {expires_in} must be NaN"
            ),
        }
    }
}

/// 回归（评审发现）：失败消息里的 `details=` 必须携带注入错误的 `code`/`errno`/`cause`/
/// `stack`（Node 26 实测形状）。真实 reqwest 传输层不暴露 cause（`HttpError` 冻结），
/// 但格式化本身与注入错误逐字一致。
#[tokio::test]
async fn refresh_reports_node_style_error_details_for_a_cause_chain() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        let inner = kiana_ai::auth::types::AuthError::new("connect ECONNREFUSED 127.0.0.1:443")
            .with_error_code("ECONNREFUSED")
            .with_errno("-111")
            .with_stack(
                "Error: connect ECONNREFUSED 127.0.0.1:443\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1606:16)",
            );
        let outer = kiana_ai::auth::types::AuthError::with_name("TypeError", "fetch failed")
            .with_cause(inner)
            .with_stack(
                "TypeError: fetch failed\n    at node:internal/deps/undici/undici:14902:13",
            );
        Box::pin(async move { Err(outer) })
    });
    let oauth = oauth_with(&fetch);
    let error = oauth
        .refresh(
            &kiana_ai::auth::types::OAuthCredential::new("old", "r", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("refresh should fail");
    assert_eq!(
        error.message,
        "Anthropic token refresh request failed. url=https://platform.claude.com/v1/oauth/token; details=TypeError: fetch failed; cause=Error: connect ECONNREFUSED 127.0.0.1:443; code=ECONNREFUSED; errno=-111; stack=Error: connect ECONNREFUSED 127.0.0.1:443\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1606:16); stack=TypeError: fetch failed\n    at node:internal/deps/undici/undici:14902:13"
    );
}

/// 回归：`postJson` 的非 2xx 错误是普通 `Error`，`details=` 保留 `Error: …` 前缀。
#[tokio::test]
async fn refresh_wraps_http_status_failures_like_typescript() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(OAuthFetchResponse::text_response(400, "oops")) })
    });
    let oauth = oauth_with(&fetch);
    let error = oauth
        .refresh(
            &kiana_ai::auth::types::OAuthCredential::new("old", "r", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("refresh should fail");
    assert_eq!(
        error.message,
        "Anthropic token refresh request failed. url=https://platform.claude.com/v1/oauth/token; details=Error: HTTP request failed. status=400; url=https://platform.claude.com/v1/oauth/token; body=oops"
    );
}
