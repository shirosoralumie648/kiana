//! `test/radius-oauth.test.ts` 的移植（4 个用例）。

use std::sync::{Arc, Mutex};

use kiana_ai::auth::oauth::radius::{RadiusOAuthOptions, create_radius_oauth_with};
use kiana_ai::auth::oauth::testing::{
    MockFetch, MockInteraction, PromptFuture, VirtualClock, responding_prompt,
};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthEvent, AuthPromptRequest, OAuthAuth, OAuthCredential};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};

/// 回调端口固定 1456：同文件用例串行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const GATEWAY: &str = "https://radius.example";
/// 2026-07-24T00:00:00Z。
const START_MS: f64 = 1_784_851_200_000.0;

/// 记录一次表单请求的字段。
type RecordedForm = Arc<Mutex<Option<Vec<(String, String)>>>>;

fn loopback_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("loopback client")
    });
    &CLIENT
}

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

fn oauth_with(fetch: &Arc<MockFetch>, clock: Arc<VirtualClock>) -> Arc<dyn OAuthAuth> {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    create_radius_oauth_with(
        RadiusOAuthOptions {
            name: "Radius".into(),
            gateway: GATEWAY.into(),
        },
        fetch,
        clock,
    )
}

fn prompt_with(method: &'static str) -> impl Fn(AuthPromptRequest) -> PromptFuture + Send + Sync {
    move |request: AuthPromptRequest| {
        assert_eq!(request.prompt.type_name(), "select");
        responding_prompt(method)
    }
}

#[tokio::test(start_paused = true)]
async fn uses_gateway_endpoints_directly_for_device_login() {
    let _guard = SERIAL.lock().await;
    let clock = VirtualClock::new(START_MS);
    let urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let fetch = MockFetch::new({
        let urls = Arc::clone(&urls);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            urls.lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(url.clone());
            let form = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
            let get = |name: &str| {
                form.iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            };
            if url == format!("{GATEWAY}/v1/oauth/device") {
                assert_eq!(get("client_id").as_deref(), Some("kiana-gateway"));
                assert_eq!(get("scope").as_deref(), Some("gateway offline_access"));
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-1234",
                            "verification_uri": "https://radius-ui.example/pair",
                            "expires_in": 600,
                            "interval": 5,
                        }),
                        200,
                    ))
                });
            }
            if url == format!("{GATEWAY}/v1/oauth/token") {
                assert_eq!(
                    get("grant_type").as_deref(),
                    Some("urn:ietf:params:oauth:grant-type:device_code")
                );
                assert_eq!(get("client_id").as_deref(), Some("kiana-gateway"));
                assert_eq!(get("device_code").as_deref(), Some("device-code"));
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "access_token": "access-token",
                            "refresh_token": "refresh-token",
                            "expires_in": 3600,
                            "scope": "gateway offline_access",
                        }),
                        200,
                    ))
                });
            }
            panic!("Unexpected request: {url}");
        }
    });
    let oauth = oauth_with(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(prompt_with("device-code"));

    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    let mut expected = OAuthCredential::new(
        "access-token",
        "refresh-token",
        START_MS + 3600.0 * 1000.0 - 60_000.0,
    );
    expected.extra.insert(
        "scope".into(),
        Value::String("gateway offline_access".into()),
    );
    assert_eq!(credential, expected);

    let events = interaction.events();
    assert_eq!(
        events,
        vec![AuthEvent::DeviceCode {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://radius-ui.example/pair".into(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(600.0),
        }]
    );
    assert_eq!(
        *urls.lock().unwrap_or_else(|error| error.into_inner()),
        vec![
            format!("{GATEWAY}/v1/oauth/device"),
            format!("{GATEWAY}/v1/oauth/token"),
        ]
    );
}

#[tokio::test]
async fn refreshes_directly_through_the_gateway_without_discovery() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, format!("{GATEWAY}/v1/oauth/token"));
        let form = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
        let get = |name: &str| {
            form.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(get("grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(get("client_id").as_deref(), Some("kiana-gateway"));
        assert_eq!(get("refresh_token").as_deref(), Some("old-refresh"));
        Box::pin(async {
            Ok(json_response(
                json!({
                    "access_token": "new-access",
                    "refresh_token": "new-refresh",
                    "expires_in": 3600,
                }),
                200,
            ))
        })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let refreshed = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect("refresh");
    assert_eq!(refreshed.access, "new-access");
    assert_eq!(refreshed.refresh, "new-refresh");
    assert_eq!(fetch.request_count(), 1);
}

#[tokio::test]
async fn discovers_only_the_interactive_browser_authorization_endpoint() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, format!("{GATEWAY}/v1/oauth"));
        Box::pin(async {
            Ok(json_response(
                json!({"issuer": "https://radius-ui.example"}),
                200,
            ))
        })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(prompt_with("browser"));
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("invalid discovery");
    assert_eq!(
        error.message,
        format!("Invalid Radius OAuth config from {GATEWAY}")
    );
    assert_eq!(fetch.request_count(), 1);
}

struct BrowserLoginOutcome {
    credential: Option<OAuthCredential>,
    error: Option<kiana_ai::auth::types::AuthError>,
    status: u16,
    page: String,
}

async fn run_browser_login(token_status: Arc<Mutex<u16>>) -> BrowserLoginOutcome {
    let fetch = MockFetch::new({
        let token_status = Arc::clone(&token_status);
        move |request: OAuthFetchRequest| {
            let url = request.url.clone();
            let status = *token_status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if url == format!("{GATEWAY}/v1/oauth") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"authorizationEndpoint": format!("{GATEWAY}/authorize")}),
                        200,
                    ))
                });
            }
            if url == format!("{GATEWAY}/v1/oauth/token") {
                let form = kiana_ai::auth::oauth::form_urldecode(&request.body_text());
                assert_eq!(
                    form.iter()
                        .find(|(key, _)| key == "code")
                        .map(|(_, value)| value.as_str()),
                    Some("browser-code")
                );
                return Box::pin(async move {
                    if status == 200 {
                        Ok(json_response(
                            json!({
                                "access_token": "access",
                                "refresh_token": "refresh",
                                "expires_in": 3600,
                            }),
                            200,
                        ))
                    } else {
                        Ok(json_response(
                            json!({"error": "invalid_grant", "error_description": "code expired"}),
                            status,
                        ))
                    }
                });
            }
            panic!("Unexpected request: {url}");
        }
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(prompt_with("browser"));

    let handle = {
        let oauth = Arc::clone(&oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    // 等授权 URL → 用浏览器（loopback client）访问回调。
    let callback_url = {
        let mut found = None;
        for _ in 0..100_000 {
            if let Some(auth_url) = interaction.auth_url() {
                let url = url::Url::parse(&auth_url).expect("auth url");
                let params: std::collections::HashMap<_, _> =
                    url.query_pairs().into_owned().collect();
                let mut callback =
                    url::Url::parse(params.get("redirect_uri").expect("redirect_uri"))
                        .expect("callback url");
                let mut query = url::form_urlencoded::Serializer::new(String::new());
                query.append_pair("code", "browser-code");
                query.append_pair("state", params.get("state").expect("state"));
                callback.set_query(Some(&query.finish()));
                found = Some(callback.to_string());
                break;
            }
            tokio::task::yield_now().await;
        }
        found.expect("auth url")
    };
    let page_future = {
        let client = loopback_client();
        let url = callback_url;
        async move {
            let response = client.get(url).send().await.expect("callback page");
            let status = response.status().as_u16();
            let text = response.text().await.expect("page text");
            (status, text)
        }
    };
    // 回调页面与登录必须并发等待：页面请求驱动服务器的 complete。
    let ((status, page), outcome) = tokio::join!(page_future, handle);
    let outcome = outcome.expect("join");
    BrowserLoginOutcome {
        credential: outcome.as_ref().ok().cloned(),
        error: outcome.err(),
        status,
        page,
    }
}

#[tokio::test]
async fn exchanges_the_browser_callback_code_before_showing_the_sign_in_page() {
    let _guard = SERIAL.lock().await;
    let token_status = Arc::new(Mutex::new(400u16));
    let failed = run_browser_login(Arc::clone(&token_status)).await;
    let error = failed.error.expect("login failed");
    assert!(
        error.message.contains("invalid_grant: code expired"),
        "{}",
        error.message
    );
    assert_eq!(failed.status, 502);
    assert!(failed.page.contains("code expired"), "{}", failed.page);

    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    *token_status
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = 200;
    let succeeded = run_browser_login(Arc::clone(&token_status)).await;
    let credential = succeeded.credential.expect("credential");
    assert_eq!(credential.access, "access");
    assert_eq!(succeeded.status, 200);
    assert!(
        succeeded.page.contains("Signed in to Radius."),
        "{}",
        succeeded.page
    );
}

/// 回归：错误体 `JSON.parse` 得到 `null` 时，TS 的 `data.error` 抛 TypeError 并落到
/// `catch` → `description = text`，因此消息是 `…: null`（不是 HTTP status）。
#[tokio::test]
async fn reports_the_raw_null_body_as_the_error_detail() {
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, format!("{GATEWAY}/v1/oauth/token"));
        Box::pin(async { Ok(OAuthFetchResponse::text_response(400, "null")) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("token request failed");
    assert_eq!(error.message, "Radius OAuth token request failed: null");
}

/// 回归：2xx + JSON `null` 会让 TS 在 `data.access_token` 上抛 TypeError（无字段校验）。
#[tokio::test]
async fn rejects_a_json_null_token_response_with_a_type_error() {
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, format!("{GATEWAY}/v1/oauth/token"));
        Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "null")) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let error = oauth
        .refresh(
            &OAuthCredential::new("old-access", "old-refresh", 0.0),
            &AbortScope::new(),
        )
        .await
        .expect_err("null token response");
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "Cannot read properties of null (reading 'access_token')"
    );
}

/// 回归：设备授权字段只做真值检查（非字符串的真值原样转发），`expires_in` 只认 number。
#[tokio::test(start_paused = true)]
async fn forwards_truthy_non_string_device_authorization_fields() {
    let device_code_form: RecordedForm = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&device_code_form);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        let url = request.url.clone();
        if url == format!("{GATEWAY}/v1/oauth/device") {
            return Box::pin(async {
                Ok(json_response(
                    json!({
                        "device_code": 12345,
                        "user_code": 678,
                        "verification_uri": "https://radius-ui.example/pair",
                        "expires_in": "600",
                    }),
                    200,
                ))
            });
        }
        if url == format!("{GATEWAY}/v1/oauth/token") {
            *slot.lock().unwrap_or_else(|error| error.into_inner()) =
                Some(kiana_ai::auth::oauth::form_urldecode(&request.body_text()));
            return Box::pin(async {
                Ok(json_response(
                    json!({
                        "access_token": "access-token",
                        "refresh_token": "refresh-token",
                        "expires_in": 3600,
                    }),
                    200,
                ))
            });
        }
        panic!("Unexpected request: {url}");
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(prompt_with("device-code"));

    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credential.access, "access-token");

    let events = interaction.events();
    assert_eq!(
        events[0],
        AuthEvent::DeviceCode {
            user_code: "678".into(),
            verification_uri: "https://radius-ui.example/pair".into(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: None,
            // `expires_in: "600"` 不是 number → TS 的 deadline 为 +∞。
            expires_in_seconds: None,
        }
    );
    let form = device_code_form
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("token poll body");
    assert!(
        form.iter()
            .any(|(key, value)| key == "device_code" && value == "12345"),
        "{form:?}"
    );
}

/// 回归：设备授权体为 JSON `null` 时 TS 在 `data.device_code` 上抛 TypeError。
#[tokio::test(start_paused = true)]
async fn rejects_a_json_null_device_authorization_with_a_type_error() {
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(OAuthFetchResponse::text_response(200, "null")) })
    });
    let oauth = oauth_with(&fetch, VirtualClock::new(START_MS));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(prompt_with("device-code"));
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("null device authorization");
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "Cannot read properties of null (reading 'device_code')"
    );
}

/// 回归（评审发现）：Radius 设备码的 `interval` 原样进入轮询器，并按 `(interval ?? 5) * 1000`
/// 做 JS 数值运算。Node 26 实测：`interval:"2"` 的第二次轮询在 +2006ms（Rust 曾回落到默认
/// 5000ms）；这里用暂停的 tokio 时间断言精确的 +2000ms。
#[tokio::test(start_paused = true)]
async fn coerces_string_device_intervals_like_javascript() {
    let _guard = SERIAL.lock().await;
    let clock = VirtualClock::new(START_MS);
    let poll_times: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetch = MockFetch::new({
        let clock = Arc::clone(&clock);
        let poll_times = Arc::clone(&poll_times);
        let polls = Arc::clone(&polls);
        move |request: OAuthFetchRequest| {
            if request.url == format!("{GATEWAY}/v1/oauth/device") {
                return Box::pin(async {
                    Ok(json_response(
                        json!({
                            "device_code": "device-code",
                            "user_code": "ABCD-1234",
                            "verification_uri": "https://radius-ui.example/pair",
                            "expires_in": 600,
                            "interval": "2",
                        }),
                        200,
                    ))
                });
            }
            assert_eq!(request.url, format!("{GATEWAY}/v1/oauth/token"));
            poll_times
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(clock.now_ms() - START_MS);
            let poll = polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if poll == 0 {
                return Box::pin(async {
                    Ok(json_response(
                        json!({"error": "authorization_pending"}),
                        400,
                    ))
                });
            }
            Box::pin(async {
                Ok(json_response(
                    json!({
                        "access_token": "access-token",
                        "refresh_token": "refresh-token",
                        "expires_in": 3600,
                    }),
                    200,
                ))
            })
        }
    });
    let oauth = oauth_with(&fetch, Arc::clone(&clock));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(prompt_with("device-code"));

    oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");

    assert_eq!(
        *poll_times.lock().unwrap_or_else(|error| error.into_inner()),
        vec![0.0, 2000.0]
    );
    let events = interaction.events();
    match events.first() {
        Some(AuthEvent::DeviceCode {
            interval_seconds,
            expires_in_seconds,
            ..
        }) => {
            assert_eq!(*interval_seconds, Some(2.0));
            assert_eq!(*expires_in_seconds, Some(600.0));
        }
        other => panic!("unexpected events: {other:?}"),
    }
}
