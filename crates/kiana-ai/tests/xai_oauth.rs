//! `test/xai-oauth.test.ts` 的移植（9 个用例 + 2 个 `it.each` 展开后共 14 个断言组）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::auth::oauth::XaiOAuth;
use kiana_ai::auth::oauth::testing::{MockFetch, MockInteraction, PromptFuture, VirtualClock};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthEvent, AuthPromptRequest, OAuthAuth, OAuthCredential};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// 2026-07-09T20:00:00Z。
const START_MS: f64 = 1_783_627_200_000.0;

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

fn device_code_response(overrides: Value) -> OAuthFetchResponse {
    let mut body = json!({
        "device_code": "device-code",
        "user_code": "ABCD-1234",
        "verification_uri": "https://accounts.x.ai/oauth2/device",
        "expires_in": 900,
        "interval": 5,
    });
    let Value::Object(overrides) = overrides else {
        panic!("overrides must be an object");
    };
    for (key, value) in overrides {
        body.as_object_mut().expect("object").insert(key, value);
    }
    json_response(body, 200)
}

fn token_response(overrides: Value) -> OAuthFetchResponse {
    let mut body = json!({
        "access_token": "access-token",
        "refresh_token": "refresh-token",
        "expires_in": 21_600,
        "token_type": "Bearer",
    });
    let Value::Object(overrides) = overrides else {
        panic!("overrides must be an object");
    };
    for (key, value) in overrides {
        if value.is_null() {
            body.as_object_mut().expect("object").remove(&key);
        } else {
            body.as_object_mut().expect("object").insert(key, value);
        }
    }
    json_response(body, 200)
}

fn oauth_with(fetch: &Arc<MockFetch>, clock: Arc<VirtualClock>) -> XaiOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    XaiOAuth::with_dependencies(fetch, clock)
}

fn no_prompt() -> impl Fn(AuthPromptRequest) -> PromptFuture + Send + Sync + 'static {
    |_request| Box::pin(async { Err(kiana_ai::auth::types::AuthError::new("Unexpected prompt")) })
}

fn device_code_info(
    interaction: &MockInteraction,
) -> Vec<(String, String, Option<f64>, Option<f64>)> {
    interaction
        .events()
        .into_iter()
        .filter_map(|event| match event {
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
                ..
            } => Some((
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            )),
            _ => None,
        })
        .collect()
}

struct LoginHarness {
    interaction: Arc<MockInteraction>,
    handle: tokio::task::JoinHandle<Result<OAuthCredential, kiana_ai::auth::types::AuthError>>,
}

fn spawn_login(oauth: XaiOAuth, interaction: Arc<MockInteraction>) -> LoginHarness {
    let oauth = Arc::new(oauth);
    let handle_interaction = Arc::clone(&interaction);
    let handle = tokio::spawn(async move {
        oauth
            .login(handle_interaction.as_ref(), &Default::default())
            .await
    });
    LoginHarness {
        interaction,
        handle,
    }
}

async fn wait_for_device_code(interaction: &MockInteraction) {
    for _ in 0..100_000 {
        if !device_code_info(interaction).is_empty() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("device_code event was not emitted");
}

async fn advance_step(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn uses_the_device_grant_delays_polling_and_handles_pending_and_slow_down() {
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let replies = Arc::new(Mutex::new(VecDeque::from([
        json_response(json!({"error": "authorization_pending"}), 400),
        json_response(json!({"error": "slow_down", "interval": 10}), 400),
        token_response(json!({})),
    ])));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        let replies = Arc::clone(&replies);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == DEVICE_CODE_URL {
                let form = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                let get = |name: &str| {
                    form.iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.as_str())
                };
                assert_eq!(get("client_id"), Some(CLIENT_ID));
                assert_eq!(get("scope"), Some(SCOPE));
                assert_eq!(get("referrer"), Some("kiana"));
                return Box::pin(async { Ok(device_code_response(json!({}))) });
            }
            if url == TOKEN_URL {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
                let form = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                let get = |name: &str| {
                    form.iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.as_str())
                };
                assert_eq!(
                    get("grant_type"),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(get("client_id"), Some(CLIENT_ID));
                assert_eq!(get("device_code"), Some("device-code"));
                let reply = replies
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .expect("Unexpected token poll");
                return Box::pin(async move { Ok(reply) });
            }
            panic!("Unexpected request: {url}");
        }
    });
    let oauth = oauth_with(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let harness = spawn_login(oauth, interaction);

    for _ in 0..100_000 {
        if !device_code_info(&harness.interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        device_code_info(&harness.interaction),
        vec![(
            "ABCD-1234".to_string(),
            "https://accounts.x.ai/oauth2/device".to_string(),
            Some(5.0),
            Some(900.0),
        )]
    );
    assert!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );

    advance_step(5000).await;
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0]
    );

    // slow_down 把间隔提高到 10 秒。
    advance_step(5000).await;
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0, START_MS + 10_000.0]
    );

    advance_step(10_000).await;
    let credential = harness.handle.await.expect("join").expect("login");
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0, START_MS + 10_000.0, START_MS + 20_000.0]
    );
    assert_eq!(
        credential,
        OAuthCredential::new(
            "access-token",
            "refresh-token",
            START_MS + 20_000.0 + 21_600_000.0 - 300_000.0
        )
    );
}

#[tokio::test(start_paused = true)]
async fn falls_back_to_the_default_poll_interval_when_the_response_reports_interval_0() {
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        move |request: OAuthFetchRequest| {
            if request.url == DEVICE_CODE_URL {
                return Box::pin(async { Ok(device_code_response(json!({"interval": 0}))) });
            }
            poll_times
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(clock.now_ms());
            Box::pin(async { Ok(token_response(json!({}))) })
        }
    });
    let oauth = oauth_with(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let harness = spawn_login(oauth, interaction);
    wait_for_device_code(&harness.interaction).await;
    // RFC 8628 默认间隔 5 秒。
    advance_step(5000).await;
    harness.handle.await.expect("join").expect("login");
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0]
    );
}

#[tokio::test(start_paused = true)]
async fn prefers_verification_uri_complete_when_the_server_provides_it() {
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        if request.url == DEVICE_CODE_URL {
            return Box::pin(async {
                Ok(device_code_response(json!({
                    "verification_uri_complete": "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234",
                })))
            });
        }
        Box::pin(async { Ok(token_response(json!({}))) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let harness = spawn_login(oauth, interaction);
    wait_for_device_code(&harness.interaction).await;
    advance_step(5000).await;
    harness.handle.await.expect("join").expect("login");
    assert_eq!(
        device_code_info(&harness.interaction),
        vec![(
            "ABCD-1234".to_string(),
            "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234".to_string(),
            Some(5.0),
            Some(900.0),
        )]
    );
}

#[tokio::test]
async fn rejects_a_non_https_verification_uri_complete() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(device_code_response(json!({
                "verification_uri_complete": "http://accounts.x.ai/oauth2/device?user_code=ABCD-1234",
            })))
        })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("untrusted complete URI");
    assert_eq!(
        error.message,
        "Untrusted verification URI in xAI OAuth response"
    );
}

#[tokio::test]
async fn rejects_non_https_verification_uris() {
    for verification_uri in [
        "http://accounts.x.ai/oauth2/device",
        "file:///etc/passwd",
        "not a url",
    ] {
        let fetch = MockFetch::new(move |_request: OAuthFetchRequest| {
            let verification_uri = verification_uri.to_string();
            Box::pin(async move {
                Ok(device_code_response(
                    json!({"verification_uri": verification_uri}),
                ))
            })
        });
        let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
        let interaction = MockInteraction::new(AbortScope::new());
        interaction.set_handler(no_prompt());
        let error = oauth
            .login(interaction.as_ref(), &Default::default())
            .await
            .expect_err("untrusted URI");
        assert_eq!(
            error.message, "Untrusted verification URI in xAI OAuth response",
            "case {verification_uri}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn fails_when_device_authorization_is_denied() {
    for error_code in ["access_denied", "authorization_denied"] {
        let request_count = Arc::new(Mutex::new(0u32));
        let fetch = MockFetch::new({
            let request_count = Arc::clone(&request_count);
            move |_request: OAuthFetchRequest| {
                let count = {
                    let mut guard = request_count
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    *guard += 1;
                    *guard
                };
                let error_code = error_code.to_string();
                Box::pin(async move {
                    if count == 1 {
                        Ok(device_code_response(json!({"interval": 1})))
                    } else {
                        Ok(json_response(json!({"error": error_code}), 400))
                    }
                })
            }
        });
        let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
        let interaction = MockInteraction::new(AbortScope::new());
        interaction.set_handler(no_prompt());
        let handle = {
            let oauth = Arc::new(oauth);
            let interaction = Arc::clone(&interaction);
            tokio::spawn(
                async move { oauth.login(interaction.as_ref(), &Default::default()).await },
            )
        };
        advance_step(1000).await;
        let error = handle.await.expect("join").expect_err("denied");
        assert_eq!(error.message, "xAI device authorization was denied");
    }
}

#[tokio::test(start_paused = true)]
async fn cancels_while_waiting_for_the_first_token_poll() {
    let controller = AbortScope::new();
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(device_code_response(json!({}))) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(controller.clone());
    interaction.set_handler(no_prompt());
    let harness = spawn_login(oauth, interaction);
    for _ in 0..100_000 {
        if !device_code_info(&harness.interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    controller.abort();
    let error = harness.handle.await.expect("join").expect_err("cancelled");
    assert_eq!(error.message, "Login cancelled");
    assert_eq!(fetch.request_count(), 1);
}

#[tokio::test]
async fn refreshes_tokens_and_preserves_an_unrotated_refresh_token() {
    let request_count = Arc::new(Mutex::new(0u32));
    let fetch = MockFetch::new({
        let request_count = Arc::clone(&request_count);
        move |request: OAuthFetchRequest| {
            assert_eq!(request.url, TOKEN_URL);
            let form = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
            let get = |name: &str| {
                form.iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            };
            assert_eq!(get("grant_type").as_deref(), Some("refresh_token"));
            assert_eq!(get("client_id").as_deref(), Some(CLIENT_ID));
            let mut count = request_count
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *count += 1;
            let count = *count;
            if count == 1 {
                assert_eq!(get("refresh_token").as_deref(), Some("old-refresh"));
                Box::pin(async {
                    Ok(token_response(json!({
                        "access_token": "new-access",
                        "refresh_token": "new-refresh",
                    })))
                })
            } else {
                assert_eq!(get("refresh_token").as_deref(), Some("keep-refresh"));
                Box::pin(async {
                    Ok(token_response(json!({
                        "access_token": "newer-access",
                        "refresh_token": null,
                    })))
                })
            }
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let rotated = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(rotated.refresh, "new-refresh");
    assert_eq!(rotated.access, "new-access");
    let preserved = oauth
        .refresh(
            &OAuthCredential::new("old-access", "keep-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(preserved.refresh, "keep-refresh");
    assert_eq!(preserved.access, "newer-access");
    assert_eq!(oauth.name(), "xAI (Grok/X subscription)");
    let auth = oauth.to_auth(&preserved).await.expect("toAuth");
    assert_eq!(auth.api_key.as_deref(), Some("newer-access"));
}

#[tokio::test(start_paused = true)]
async fn assumes_a_one_hour_lifetime_when_expires_in_is_missing() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(token_response(json!({"expires_in": null}))) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let credential = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(credential.expires, START_MS + 3_600_000.0 - 300_000.0);
}

#[tokio::test]
async fn rejects_token_responses_with_missing_fields() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(token_response(json!({"access_token": null}))) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("missing field");
    assert_eq!(
        error.message,
        "Invalid xAI OAuth response field: access_token"
    );
}

#[tokio::test]
async fn surfaces_the_upstream_error_code_and_description_on_refresh_failure() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(json_response(
                json!({"error": "invalid_grant", "error_description": "refresh token revoked"}),
                400,
            ))
        })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("refresh failed");
    assert_eq!(
        error.message,
        "xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token revoked"
    );
}
