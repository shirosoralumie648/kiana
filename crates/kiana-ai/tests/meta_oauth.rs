//! `test/meta-oauth.test.ts` 的移植（4 个用例）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::auth::oauth::MetaOAuth;
use kiana_ai::auth::oauth::testing::{MockFetch, MockInteraction, PromptFuture, VirtualClock};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthEvent, AuthPromptRequest, OAuthAuth, OAuthCredential};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

const CLIENT_ID: &str = "1031625952748946";
const DEVICE_AUTHORIZATION_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const DEVICE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const MINT_URL: &str = "https://api.meta.ai/muse-code/key";
const DAY_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
/// 2026-09-03T00:00:00Z。
const START_MS: f64 = 1_788_393_600_000.0;

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

fn oauth_with(fetch: &Arc<MockFetch>, clock: Arc<VirtualClock>) -> MetaOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    MetaOAuth::with_dependencies(fetch, clock)
}

fn no_prompt() -> impl Fn(AuthPromptRequest) -> PromptFuture + Send + Sync + 'static {
    |_request| {
        Box::pin(async {
            Err(kiana_ai::auth::types::AuthError::new(
                "Meta login should not prompt",
            ))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn logs_in_with_the_device_flow_and_mints_a_model_api_key() {
    let clock = VirtualClock::new(START_MS);
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(json!({"error": "authorization_pending"}), 400),
        json_response(
            json!({"access_token": "identity-token", "token_type": "Bearer"}),
            200,
        ),
    ])));
    let fetch = MockFetch::new({
        let responses = Arc::clone(&responses);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == DEVICE_AUTHORIZATION_URL {
                assert_eq!(request.method, "POST");
                let params = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                assert_eq!(
                    params
                        .iter()
                        .find(|(key, _)| key == "client_id")
                        .map(|(_, value)| value.as_str()),
                    Some(CLIENT_ID)
                );
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code-123",
                            "user_code": "ABCD-1234",
                            "verification_uri": "https://auth.meta.com/oauth/device/",
                            "verification_uri_complete": "https://auth.meta.com/oauth/device/?code=ABCD-1234",
                            "interval": 5,
                            "expires_in": 600,
                        }),
                        200,
                    ))
                });
            }
            if url == DEVICE_TOKEN_URL {
                let params = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                let get = |name: &str| {
                    params
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.as_str())
                };
                assert_eq!(
                    get("grant_type"),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(get("client_id"), Some(CLIENT_ID));
                assert_eq!(get("device_code"), Some("device-code-123"));
                let response = responses
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .pop_front()
                    .expect("unexpected extra token poll");
                return Box::pin(async move { Ok(response) });
            }
            if url == MINT_URL {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.header_value("Authorization"),
                    Some("Bearer identity-token")
                );
                return Box::pin(async {
                    Ok(json_response(json!({"api_key": "LLM|minted-key"}), 200))
                });
            }
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    let interaction_events = interaction.events_handle();
    interaction.set_handler(no_prompt());

    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !interaction_events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let first_event = interaction_events
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .first()
        .cloned()
        .expect("device_code event");
    match first_event {
        AuthEvent::DeviceCode {
            user_code,
            verification_uri,
            interval_seconds,
            expires_in_seconds,
            ..
        } => {
            assert_eq!(user_code, "ABCD-1234");
            assert_eq!(
                verification_uri,
                "https://auth.meta.com/oauth/device/?code=ABCD-1234"
            );
            assert_eq!(interval_seconds, Some(5.0));
            assert_eq!(expires_in_seconds, Some(600.0));
        }
        other => panic!("unexpected first event: {other:?}"),
    }

    // `tokio::time::advance` 直接跳到目标时刻（不像 vitest 那样逐 timer 推进），
    // 因此按轮询间隔分两步推进。
    tokio::time::advance(Duration::from_millis(5000)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(5000)).await;
    let credential = handle.await.expect("join").expect("login");
    assert_eq!(
        credential,
        OAuthCredential::new(
            "LLM|minted-key",
            "identity-token",
            START_MS + 10_000.0 + DAY_MS
        )
    );
}

#[tokio::test(start_paused = true)]
async fn re_mints_the_api_key_from_the_stored_identity_token_on_refresh() {
    let now = START_MS + 12.0 * 60.0 * 60.0 * 1000.0;
    let clock = VirtualClock::new(now);
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, MINT_URL);
        assert_eq!(
            request.header_value("Authorization"),
            Some("Bearer identity-token")
        );
        Box::pin(async { Ok(json_response(json!({"api_key": "LLM|fresh-key"}), 200)) })
    });
    let oauth = oauth_with(&fetch, clock);
    let credential = oauth
        .refresh(
            &OAuthCredential::new("LLM|old-key", "identity-token", 1.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(
        credential,
        OAuthCredential::new("LLM|fresh-key", "identity-token", now + DAY_MS)
    );
}

#[tokio::test]
async fn reports_the_setup_url_when_meta_issues_no_key() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(json_response(
                json!({"require_payment": true, "action_url": "https://dev.meta.ai/billing"}),
                200,
            ))
        })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &OAuthCredential::new("", "identity-token", 1.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("no key");
    assert_eq!(
        error.message,
        "Meta did not issue an API key. Complete setup at https://dev.meta.ai/billing"
    );
}

#[tokio::test]
async fn uses_the_minted_key_as_the_request_api_key() {
    let auth = MetaOAuth::new()
        .to_auth(&OAuthCredential::new("LLM|key", "identity-token", 1.0))
        .await
        .expect("toAuth");
    assert_eq!(auth.api_key.as_deref(), Some("LLM|key"));
}

/// 回归：`readJson` 失败在 TS 里是 `null`，错误消息里的 `JSON.stringify(json)` 是 `null`。
#[tokio::test(start_paused = true)]
async fn reports_null_for_unparseable_device_responses() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "not json")) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("invalid device authorization response");
    assert_eq!(
        error.message,
        "Invalid Meta device authorization response: null"
    );
}
