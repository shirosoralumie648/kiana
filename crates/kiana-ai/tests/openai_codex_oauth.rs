//! `test/openai-codex-oauth.test.ts` 的移植（10 个用例）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use kiana_ai::auth::oauth::testing::{
    MockFetch, MockInteraction, PromptFuture, VirtualClock, responding_prompt,
};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse, OpenAICodexOAuth};
use kiana_ai::auth::types::{AuthEvent, AuthPromptRequest, AuthSelectOption, OAuthAuth};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

/// 1455 是固定端口：同文件内的用例串行执行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 2026-05-20T00:00:00Z。
const START_MS: f64 = 1_779_235_200_000.0;

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

fn create_access_token(account_id: &str) -> String {
    let header = base64::engine::general_purpose::STANDARD.encode(br#"{"alg":"none"}"#);
    let payload = base64::engine::general_purpose::STANDARD.encode(
        format!(r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{account_id}"}}}}"#)
            .as_bytes(),
    );
    format!("{header}.{payload}.signature")
}

fn device_auth_pending_response() -> OAuthFetchResponse {
    json_response(
        json!({
            "error": {
                "message": "Device authorization is pending. Please try again.",
                "type": "invalid_request_error",
                "param": null,
                "code": "deviceauth_authorization_pending"
            }
        }),
        403,
    )
}

fn prompt_type(request: &AuthPromptRequest) -> &'static str {
    request.prompt.type_name()
}

fn select_device_code() -> PromptFuture {
    responding_prompt("device_code")
}

fn oauth(fetch: &Arc<MockFetch>, clock: Arc<VirtualClock>) -> OpenAICodexOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    OpenAICodexOAuth::with_dependencies(fetch, clock)
}

#[tokio::test(start_paused = true)]
async fn logs_in_with_the_openai_codex_device_code_flow() {
    let _guard = SERIAL.lock().await;
    let access_token = create_access_token("account-123");
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from([
        device_auth_pending_response(),
        json_response(
            json!({
                "authorization_code": "oauth-code",
                "code_challenge": "device-code-challenge",
                "code_verifier": "device-code-verifier",
            }),
            200,
        ),
    ])));

    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let access_token = access_token.clone();
        let poll_times = Arc::clone(&poll_times);
        let responses = Arc::clone(&responses);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                assert_eq!(request.method, "POST");
                let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
                assert_eq!(body, json!({"client_id": "app_EMoamEEZ73f0CkXaXp7hrann"}));
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_auth_id": "device-auth-id",
                            "user_code": "ABCD-1234",
                            "interval": "5",
                        }),
                        200,
                    ))
                });
            }
            if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                assert_eq!(request.method, "POST");
                let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
                assert_eq!(
                    body,
                    json!({"device_auth_id": "device-auth-id", "user_code": "ABCD-1234"})
                );
                let response = responses
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .expect("unexpected extra device auth poll");
                return Box::pin(async move { Ok(response) });
            }
            if url == "https://auth.openai.com/oauth/token" {
                assert_eq!(request.method, "POST");
                let params = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                let get = |name: &str| {
                    params
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.clone())
                };
                assert_eq!(get("grant_type").as_deref(), Some("authorization_code"));
                assert_eq!(
                    get("client_id").as_deref(),
                    Some("app_EMoamEEZ73f0CkXaXp7hrann")
                );
                assert_eq!(get("code").as_deref(), Some("oauth-code"));
                assert_eq!(
                    get("redirect_uri").as_deref(),
                    Some("https://auth.openai.com/deviceauth/callback")
                );
                assert_eq!(
                    get("code_verifier").as_deref(),
                    Some("device-code-verifier")
                );
                let access_token = access_token.clone();
                return Box::pin(async move {
                    Ok(json_response(
                        json!({
                            "access_token": access_token,
                            "refresh_token": "refresh-token",
                            "expires_in": 3600,
                        }),
                        200,
                    ))
                });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth(&fetch, Arc::clone(&clock));

    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        assert_eq!(prompt_type(&request), "select");
        select_device_code()
    });
    let event_slot = Arc::clone(&events);

    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let infos: Vec<(String, String, Option<f64>, Option<f64>)> = event_slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter_map(|event| match event {
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
                ..
            } => Some((
                user_code.clone(),
                verification_uri.clone(),
                *interval_seconds,
                *expires_in_seconds,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        infos,
        vec![(
            "ABCD-1234".to_string(),
            "https://auth.openai.com/codex/device".to_string(),
            Some(5.0),
            Some(900.0),
        )]
    );
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS]
    );

    tokio::time::advance(Duration::from_millis(4999)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS]
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let credential = handle.await.expect("join").expect("login");
    assert_eq!(credential.access, access_token);
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.expires, START_MS + 5000.0 + 3600.0 * 1000.0);
    assert_eq!(credential.get_str("accountId"), Some("account-123"));
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS, START_MS + 5000.0]
    );
}

#[tokio::test]
async fn offers_browser_login_first_and_uses_the_selected_openai_codex_device_code_flow() {
    let _guard = SERIAL.lock().await;
    let access_token = create_access_token("account-456");
    let select_prompts: Arc<Mutex<Vec<kiana_ai::auth::types::AuthPrompt>>> =
        Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let access_token = access_token.clone();
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_auth_id": "device-auth-id",
                            "user_code": "WXYZ-7890",
                            "interval": "5",
                        }),
                        200,
                    ))
                });
            }
            if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "authorization_code": "oauth-code",
                            "code_challenge": "device-code-challenge",
                            "code_verifier": "device-code-verifier",
                        }),
                        200,
                    ))
                });
            }
            if url == "https://auth.openai.com/oauth/token" {
                let access_token = access_token.clone();
                return Box::pin(async move {
                    Ok(json_response(
                        json!({
                            "access_token": access_token,
                            "refresh_token": "refresh-token",
                            "expires_in": 3600,
                        }),
                        200,
                    ))
                });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    let prompts = Arc::clone(&select_prompts);
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        assert_eq!(prompt_type(&request), "select");
        prompts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.prompt.clone());
        responding_prompt("device_code")
    });
    let event_slot = Arc::clone(&events);

    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credential.access, access_token);
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.get_str("accountId"), Some("account-456"));

    let prompts = select_prompts
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(prompts.len(), 1);
    match &prompts[0] {
        kiana_ai::auth::types::AuthPrompt::Select { message, options } => {
            assert_eq!(message, "Select OpenAI Codex login method:");
            let expected = vec![
                AuthSelectOption {
                    id: "browser".into(),
                    label: "Browser login (default)".into(),
                    description: None,
                },
                AuthSelectOption {
                    id: "device_code".into(),
                    label: "Device code login (headless)".into(),
                    description: None,
                },
            ];
            assert_eq!(options, &expected);
        }
        other => panic!("unexpected prompt: {other:?}"),
    }

    let infos = event_slot
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let device_info: Vec<_> = infos
        .iter()
        .filter_map(|event| match event {
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
                ..
            } => Some((
                user_code.clone(),
                verification_uri.clone(),
                *interval_seconds,
                *expires_in_seconds,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        device_info,
        vec![(
            "WXYZ-7890".to_string(),
            "https://auth.openai.com/codex/device".to_string(),
            Some(5.0),
            Some(900.0),
        )]
    );
    assert!(
        !infos
            .iter()
            .any(|event| matches!(event, AuthEvent::AuthUrl { .. }))
    );
}

#[tokio::test]
async fn cancels_when_openai_codex_login_method_selection_is_cancelled() {
    let _guard = SERIAL.lock().await;
    let oauth = OpenAICodexOAuth::new();
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

#[tokio::test(start_paused = true)]
async fn cancels_the_openai_codex_device_code_flow_while_waiting() {
    let _guard = SERIAL.lock().await;
    let controller = AbortScope::new();
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_auth_id": "device-auth-id",
                            "user_code": "ABCD-1234",
                            "interval": "5",
                        }),
                        200,
                    ))
                });
            }
            if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                return Box::pin(async { Ok(device_auth_pending_response()) });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth(&fetch, clock);
    let interaction = MockInteraction::new(controller.clone());
    interaction.set_handler(|_request| select_device_code());

    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );

    controller.abort();
    let error = handle.await.expect("join").expect_err("cancelled");
    assert_eq!(error.message, "Login cancelled");
}

#[tokio::test(start_paused = true)]
async fn times_out_the_openai_codex_device_code_flow_after_15_minutes() {
    let _guard = SERIAL.lock().await;
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_auth_id": "device-auth-id",
                            "user_code": "ABCD-1234",
                            "interval": "60",
                        }),
                        200,
                    ))
                });
            }
            if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                return Box::pin(async { Ok(device_auth_pending_response()) });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| select_device_code());

    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );

    tokio::time::advance(Duration::from_millis(15 * 60 * 1000)).await;
    let error = handle.await.expect("join").expect_err("timed out");
    assert_eq!(error.message, "Device flow timed out");
}

#[tokio::test(start_paused = true)]
async fn treats_openai_codex_device_auth_403_and_404_responses_as_pending() {
    let _guard = SERIAL.lock().await;
    let access_token = create_access_token("account-403-404");
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(
            json!({"error": "access_denied", "error_description": "denied"}),
            403,
        ),
        OAuthFetchResponse::text_response(404, "not ready"),
        json_response(
            json!({
                "authorization_code": "oauth-code",
                "code_challenge": "device-code-challenge",
                "code_verifier": "device-code-verifier",
            }),
            200,
        ),
    ])));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        let responses = Arc::clone(&responses);
        let access_token = access_token.clone();
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_auth_id": "device-auth-id",
                            "user_code": "ABCD-1234",
                            "interval": "1",
                        }),
                        200,
                    ))
                });
            }
            if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                let response = responses
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .expect("unexpected extra device auth poll");
                return Box::pin(async move { Ok(response) });
            }
            if url == "https://auth.openai.com/oauth/token" {
                let access_token = access_token.clone();
                return Box::pin(async move {
                    Ok(json_response(
                        json!({
                            "access_token": access_token,
                            "refresh_token": "refresh-token",
                            "expires_in": 3600,
                        }),
                        200,
                    ))
                });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| select_device_code());

    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(1000)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(1000)).await;

    let credential = handle.await.expect("join").expect("login");
    assert_eq!(credential.access, access_token);
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.get_str("accountId"), Some("account-403-404"));
    assert_eq!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        3
    );
}

#[tokio::test]
async fn includes_the_response_body_in_openai_codex_device_auth_poll_failures() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
            return Box::pin(async {
                Ok(json_response(
                    json!({
                        "device_auth_id": "device-auth-id",
                        "user_code": "ABCD-1234",
                        "interval": "5",
                    }),
                    200,
                ))
            });
        }
        if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
            return Box::pin(async {
                Ok(json_response(
                    json!({"error": "server_error", "error_description": "try again later"}),
                    500,
                ))
            });
        }
        panic!("Unexpected fetch URL: {url}");
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| select_device_code());
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("failed poll");
    assert_eq!(
        error.message,
        "OpenAI Codex device auth failed with status 500: {\"error\":\"server_error\",\"error_description\":\"try again later\"}"
    );
}

#[tokio::test]
async fn does_not_write_token_refresh_failures_to_stderr() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                401,
                &json!({
                    "error": {
                        "message": "Could not validate your token. Please try signing in again.",
                        "type": "invalid_request_error"
                    }
                }),
            )
            .with_status_text("Unauthorized"))
        })
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &kiana_ai::auth::types::OAuthCredential::new(
                "invalid-access-token",
                "invalid-refresh-token",
                0.0,
            ),
            &AbortScope::new(),
        )
        .await
        .expect_err("refresh failed");
    assert!(
        error
            .message
            .contains("OpenAI Codex token refresh failed (401)"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("Could not validate your token"),
        "{}",
        error.message
    );
}

/// 浏览器流程（端口 1455）的公共辅助；返回凭证与授权 URL。
async fn browser_login(
    options: &kiana_ai::auth::types::LoginOptions,
) -> Result<kiana_ai::auth::types::OAuthCredential, kiana_ai::auth::types::AuthError> {
    let access_token = create_access_token("acct");
    let fetch = MockFetch::new({
        let access_token = access_token.clone();
        move |request: OAuthFetchRequest| {
            assert_eq!(request.url, "https://auth.openai.com/oauth/token");
            let access_token = access_token.clone();
            Box::pin(async move {
                Ok(json_response(
                    json!({
                        "access_token": access_token,
                        "refresh_token": "refresh",
                        "expires_in": 3600,
                    }),
                    200,
                ))
            })
        }
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
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
                    .expect("auth_url");
                let url = url::Url::parse(&auth_url).expect("url");
                let state = url
                    .query_pairs()
                    .find(|(key, _)| key == "state")
                    .map(|(_, value)| value.into_owned())
                    .expect("state");
                responding_prompt(format!(
                    "http://localhost:1455/auth/callback?code=pasted-code&state={state}"
                ))
            }
            other => panic!("Unexpected prompt: {other}"),
        }
    });
    oauth.login(interaction.as_ref(), options).await
}

#[tokio::test]
async fn uses_the_apps_agent_name_as_the_browser_login_originator() {
    let _guard = SERIAL.lock().await;
    let options = kiana_ai::auth::types::LoginOptions {
        agent_name: Some("my-app".into()),
        ..Default::default()
    };
    let credential = browser_login(&options).await.expect("login");
    assert_eq!(credential.get_str("accountId"), Some("acct"));

    // 单独再跑一次以捕获授权 URL：originator 必须来自 agentName。
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(json_response(
                json!({
                    "access_token": create_access_token("acct"),
                    "refresh_token": "refresh",
                    "expires_in": 3600,
                }),
                200,
            ))
        })
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            _ => {
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
                let url = url::Url::parse(&auth_url).expect("url");
                let state = url
                    .query_pairs()
                    .find(|(key, _)| key == "state")
                    .map(|(_, value)| value.into_owned())
                    .expect("state");
                responding_prompt(format!(
                    "http://localhost:1455/auth/callback?code=pasted-code&state={state}"
                ))
            }
        }
    });
    oauth
        .login(interaction.as_ref(), &options)
        .await
        .expect("login");
    let auth_url = interaction.auth_url().expect("auth url");
    let url = url::Url::parse(&auth_url).expect("url");
    let originator = url
        .query_pairs()
        .find(|(key, _)| key == "originator")
        .map(|(_, value)| value.into_owned());
    assert_eq!(originator.as_deref(), Some("my-app"));
}

#[tokio::test]
async fn falls_back_to_the_pasted_redirect_url_when_the_fixed_callback_port_is_taken() {
    let _guard = SERIAL.lock().await;
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 1455)).expect("blocker");
    let options = kiana_ai::auth::types::LoginOptions::default();
    let credential = browser_login(&options).await.expect("login");
    drop(blocker);
    assert_eq!(credential.get_str("accountId"), Some("acct"));
}

/// 回归：TS 的 `if (parsed.state && parsed.state !== state)` 是真值判断，空串 state
/// （粘贴 `code#`）不触发 "State mismatch"。
#[tokio::test]
async fn accepts_an_empty_state_in_the_pasted_authorization_input() {
    let _guard = SERIAL.lock().await;
    let access_token = create_access_token("acct");
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, "https://auth.openai.com/oauth/token");
        let access_token = access_token.clone();
        Box::pin(async move {
            Ok(json_response(
                json!({
                    "access_token": access_token,
                    "refresh_token": "refresh",
                    "expires_in": 3600,
                }),
                200,
            ))
        })
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            _ => responding_prompt("pasted-code#"),
        }
    });
    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("empty state is falsy in TS");
    assert_eq!(credential.get_str("accountId"), Some("acct"));
}

/// 回归：TS 的 `DeviceAuthInfo` 只做真值检查后**原样**把 `device_auth_id`/`user_code`
/// 放进轮询请求体（数字保持数字），事件里的 `userCode` 走 JS `String()` 转换。
#[tokio::test(start_paused = true)]
async fn forwards_truthy_non_string_device_auth_fields() {
    let _guard = SERIAL.lock().await;
    let access_token = create_access_token("account-123");
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url == "https://auth.openai.com/api/accounts/deviceauth/usercode" {
            return Box::pin(async {
                Ok(json_response(
                    json!({"device_auth_id": 123, "user_code": 456, "interval": 1}),
                    200,
                ))
            });
        }
        if url == "https://auth.openai.com/api/accounts/deviceauth/token" {
            let body: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
            assert_eq!(body, json!({"device_auth_id": 123, "user_code": 456}));
            return Box::pin(async {
                Ok(json_response(
                    json!({"authorization_code": "oauth-code", "code_verifier": "verifier"}),
                    200,
                ))
            });
        }
        if url == "https://auth.openai.com/oauth/token" {
            let access_token = access_token.clone();
            return Box::pin(async move {
                Ok(json_response(
                    json!({
                        "access_token": access_token,
                        "refresh_token": "refresh-token",
                        "expires_in": 3600,
                    }),
                    200,
                ))
            });
        }
        panic!("Unexpected fetch URL: {url}");
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        assert_eq!(prompt_type(&request), "select");
        select_device_code()
    });
    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert!(!credential.access.is_empty());
    let events = interaction.events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AuthEvent::DeviceCode { user_code, .. } if user_code == "456"
        )),
        "{events:?}"
    );
}

/// 回归：`new URLSearchParams("?code=…")` 会去掉一个前导 `?`；Rust 曾把首键解析成
/// `"?code"`，导致粘贴带 `?` 的查询串时报 "Missing authorization code"。
#[tokio::test]
async fn accepts_a_pasted_query_string_with_a_leading_question_mark() {
    let _guard = SERIAL.lock().await;
    let access_token = create_access_token("acct");
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, "https://auth.openai.com/oauth/token");
        let params = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
        assert_eq!(
            params
                .iter()
                .find(|(key, _)| key == "code")
                .map(|(_, value)| value.as_str()),
            Some("pasted-code")
        );
        let access_token = access_token.clone();
        Box::pin(async move {
            Ok(json_response(
                json!({
                    "access_token": access_token,
                    "refresh_token": "refresh",
                    "expires_in": 3600,
                }),
                200,
            ))
        })
    });
    let oauth = oauth(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|request: AuthPromptRequest| -> PromptFuture {
        match prompt_type(&request) {
            "select" => responding_prompt("browser"),
            _ => responding_prompt("?code=pasted-code"),
        }
    });
    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("leading question mark");
    assert_eq!(credential.get_str("accountId"), Some("acct"));
}
