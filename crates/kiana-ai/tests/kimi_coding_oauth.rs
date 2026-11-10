//! `test/kimi-coding-oauth.test.ts` 的移植（6 个用例）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::auth::oauth::KimiCodingOAuth;
use kiana_ai::auth::oauth::testing::{MockFetch, MockInteraction, PromptFuture, VirtualClock};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthEvent, AuthPromptRequest, OAuthAuth, OAuthCredential};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

/// `KIMI_CODE_OAUTH_HOST` 是进程级全局：同文件用例串行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const OAUTH_HOST: &str = "https://auth.kimi.com";
/// 2026-07-20T00:00:00Z。
const START_MS: f64 = 1_784_505_600_000.0;

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

fn device_authorization_response(overrides: Value) -> OAuthFetchResponse {
    let mut body = json!({
        "user_code": "ABCD-1234",
        "device_code": "device-code-123",
        "verification_uri": "https://www.kimi.com/code",
        "verification_uri_complete": "https://www.kimi.com/code?user_code=ABCD-1234",
        "interval": 5,
        "expires_in": 600,
    });
    let Value::Object(overrides) = overrides else {
        panic!("overrides must be an object");
    };
    for (key, value) in overrides {
        body.as_object_mut().expect("object").insert(key, value);
    }
    json_response(body, 200)
}

fn oauth_with(fetch: &Arc<MockFetch>, clock: Arc<VirtualClock>) -> KimiCodingOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    KimiCodingOAuth::with_dependencies(fetch, clock)
}

fn no_prompt() -> impl Fn(AuthPromptRequest) -> PromptFuture + Send + Sync + 'static {
    |_request| {
        Box::pin(async {
            Err(kiana_ai::auth::types::AuthError::new(
                "Kimi Code login should not prompt",
            ))
        })
    }
}

fn device_code_events(
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

#[tokio::test(start_paused = true)]
async fn logs_in_with_the_device_authorization_flow() {
    let _guard = SERIAL.lock().await;
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let responses = Arc::new(Mutex::new(VecDeque::from([
        json_response(json!({"error": "authorization_pending"}), 400),
        json_response(
            json!({"access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600}),
            200,
        ),
    ])));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        let responses = Arc::clone(&responses);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            if url == format!("{OAUTH_HOST}/api/oauth/device_authorization") {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.header_value("Content-Type"),
                    Some("application/x-www-form-urlencoded")
                );
                assert_eq!(request.header_value("Accept"), Some("application/json"));
                let params = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                assert_eq!(params[0], ("client_id".to_string(), CLIENT_ID.to_string()));
                return Box::pin(async { Ok(device_authorization_response(json!({}))) });
            }
            if url == format!("{OAUTH_HOST}/api/oauth/token") {
                poll_times
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(clock.now_ms());
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
            panic!("Unexpected fetch URL: {url}");
        }
    });
    let oauth = oauth_with(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());

    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    for _ in 0..100_000 {
        if !device_code_events(&interaction).is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        device_code_events(&interaction),
        vec![(
            "ABCD-1234".to_string(),
            "https://www.kimi.com/code?user_code=ABCD-1234".to_string(),
            Some(5.0),
            Some(600.0),
        )]
    );

    // waitBeforeFirstPoll：首次轮询在 5s 后。
    tokio::time::advance(Duration::from_millis(4999)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert!(
        poll_times
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0]
    );

    tokio::time::advance(Duration::from_millis(5000)).await;
    let credential = handle.await.expect("join").expect("login");
    assert_eq!(
        credential,
        OAuthCredential::new(
            "access-token",
            "refresh-token",
            START_MS + 10_000.0 + 3600.0 * 1000.0
        )
    );
    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![START_MS + 5000.0, START_MS + 10_000.0]
    );
}

#[tokio::test(start_paused = true)]
async fn fails_when_the_device_code_expires() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url.ends_with("/api/oauth/device_authorization") {
            return Box::pin(async { Ok(device_authorization_response(json!({}))) });
        }
        Box::pin(async { Ok(json_response(json!({"error": "expired_token"}), 400)) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    tokio::time::advance(Duration::from_millis(5000)).await;
    let error = handle.await.expect("join").expect_err("expired");
    assert!(error.message.contains("expired"), "{}", error.message);
    assert_eq!(
        error.message,
        "Kimi Code device authorization expired. Please restart login."
    );
}

#[tokio::test(start_paused = true)]
async fn fails_when_the_user_denies_the_login() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url.ends_with("/api/oauth/device_authorization") {
            return Box::pin(async { Ok(device_authorization_response(json!({}))) });
        }
        Box::pin(async { Ok(json_response(json!({"error": "access_denied"}), 400)) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(no_prompt());
    let handle = {
        let oauth = Arc::new(oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    tokio::time::advance(Duration::from_millis(5000)).await;
    let error = handle.await.expect("join").expect_err("denied");
    assert_eq!(error.message, "Kimi Code login was denied.");
}

#[tokio::test(start_paused = true)]
async fn honors_the_kimi_code_oauth_host_override() {
    let _guard = SERIAL.lock().await;
    // SAFETY: 同文件用例串行；变量在用例结束前移除。
    unsafe { std::env::set_var("KIMI_CODE_OAUTH_HOST", "https://auth.example.com/") };
    let result = async {
        let urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let fetch = MockFetch::new({
            let urls = Arc::clone(&urls);
            move |request: OAuthFetchRequest| {
                urls.lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(request.url.clone());
                let url = request.url.clone();
                if url == "https://auth.example.com/api/oauth/device_authorization" {
                    return Box::pin(async {
                        Ok(device_authorization_response(json!({"interval": 1})))
                    });
                }
                if url == "https://auth.example.com/api/oauth/token" {
                    return Box::pin(async {
                        Ok(json_response(
                            json!({"access_token": "a", "refresh_token": "r", "expires_in": 60}),
                            200,
                        ))
                    });
                }
                panic!("Unexpected fetch URL: {url}");
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
        tokio::time::advance(Duration::from_millis(1000)).await;
        let credential = handle.await.expect("join").expect("login");
        assert_eq!(credential.access, "a");
        assert_eq!(credential.refresh, "r");
        urls.lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
    .await;
    unsafe { std::env::remove_var("KIMI_CODE_OAUTH_HOST") };
    assert_eq!(
        result,
        vec![
            "https://auth.example.com/api/oauth/device_authorization".to_string(),
            "https://auth.example.com/api/oauth/token".to_string(),
        ]
    );
}

#[tokio::test]
async fn refreshes_tokens_and_returns_a_bearer_header_for_requests() {
    // `KIMI_CODE_OAUTH_HOST` 是进程级全局：所有读取它的用例都要串行。
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, format!("{OAUTH_HOST}/api/oauth/token"));
        let params = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
        let get = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("grant_type"), Some("refresh_token"));
        assert_eq!(get("refresh_token"), Some("old-refresh"));
        assert_eq!(get("client_id"), Some(CLIENT_ID));
        Box::pin(async {
            Ok(json_response(
                json!({"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}),
                200,
            ))
        })
    });
    let fetch_handle: Arc<dyn OAuthFetch> = fetch.clone();
    let oauth = KimiCodingOAuth::with_fetch(fetch_handle);
    let before = kiana_ai::auth::oauth::device_code::AuthClock::now_ms(
        &kiana_ai::auth::oauth::device_code::SystemAuthClock,
    );
    let credential = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", before),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(credential.access, "new-access");
    assert_eq!(credential.refresh, "new-refresh");
    assert!(credential.expires >= before + 3600.0 * 1000.0);

    let auth = oauth.to_auth(&credential).await.expect("toAuth");
    assert_eq!(
        auth.headers
            .as_ref()
            .and_then(|headers| headers.get("Authorization"))
            .and_then(|value| value.as_deref()),
        Some("Bearer new-access")
    );
    assert!(auth.api_key.is_none());
}

#[tokio::test(start_paused = true)]
async fn retries_refresh_on_429_and_fails_unauthorized_on_invalid_grant() {
    let _guard = SERIAL.lock().await;
    // 429 once, then success.
    let calls = Arc::new(Mutex::new(0u32));
    let fetch = MockFetch::new({
        let calls = Arc::clone(&calls);
        move |_request: OAuthFetchRequest| {
            let mut calls = calls.lock().unwrap_or_else(|error| error.into_inner());
            *calls += 1;
            let call = *calls;
            drop(calls);
            Box::pin(async move {
                if call == 1 {
                    Ok(json_response(
                        json!({"error": "temporarily_unavailable"}),
                        429,
                    ))
                } else {
                    Ok(json_response(
                        json!({"access_token": "a", "refresh_token": "r", "expires_in": 60}),
                        200,
                    ))
                }
            })
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let stored = OAuthCredential::new("old", "old", 0.0);
    let signal = AbortScope::new();
    let refresh = oauth.refresh(&stored, &signal);
    tokio::pin!(refresh);
    for _ in 0..100_000 {
        if *calls.lock().unwrap_or_else(|error| error.into_inner()) >= 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(1000)).await;
    let credential = refresh.await.expect("refresh");
    assert_eq!(credential.access, "a");
    assert_eq!(*calls.lock().unwrap_or_else(|error| error.into_inner()), 2);

    // invalid_grant is not retried.
    let invalid_calls = Arc::new(Mutex::new(0u32));
    let fetch = MockFetch::new({
        let invalid_calls = Arc::clone(&invalid_calls);
        move |_request: OAuthFetchRequest| {
            *invalid_calls
                .lock()
                .unwrap_or_else(|error| error.into_inner()) += 1;
            Box::pin(async { Ok(json_response(json!({"error": "invalid_grant"}), 400)) })
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(&OAuthCredential::new("old", "old", 0.0), &AbortScope::new())
        .await
        .expect_err("unauthorized");
    assert!(error.message.contains("unauthorized"), "{}", error.message);
    assert_eq!(
        *invalid_calls
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
        1
    );
}

/// 回归：TS 的 `readJson` 把解析失败/非对象归一为 `null`，错误消息里的
/// `JSON.stringify(json)` 因此是字面量 `null`（不是 `undefined`）。
#[tokio::test(start_paused = true)]
async fn reports_null_for_unparseable_device_responses() {
    let _guard = SERIAL.lock().await;
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
        "Invalid Kimi Code device authorization response: null"
    );
}

/// 回归：刷新时 `readJson` 为 `null` 也走 `parseTokenResponse`，
/// 消息为 `Kimi Code token refresh response missing fields: null`。
#[tokio::test(start_paused = true)]
async fn reports_null_for_unparseable_refresh_responses() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "not json")) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(&OAuthCredential::new("old", "old", 0.0), &AbortScope::new())
        .await
        .expect_err("missing fields");
    assert_eq!(
        error.message,
        "Kimi Code token refresh response missing fields: null"
    );
}
