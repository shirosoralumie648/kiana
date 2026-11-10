//! `test/openai-chatgpt-oauth.test.ts` 的移植（7 个用例 + closeAllConnections / 端口占用回归）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ai::auth::oauth::openai_chatgpt::OpenAIChatGPTOAuth;
use kiana_ai::auth::oauth::testing::{MockFetch, MockInteraction, PromptFuture, responding_prompt};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{
    AuthEvent, AuthPromptRequest, LoginOptions, OAuthAuth, OAuthCredential,
};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 端口 1455 固定：同文件用例串行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const REQUIRED_SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DEVICE_ID: &str = "e61bbe28-07ef-466d-8e5d-a344f94ab305";

/// 记录一次表单请求的字段。
type RecordedForm = Arc<Mutex<Option<Vec<(String, String)>>>>;

fn token_response(scope: &str) -> Value {
    json!({
        "access_token": "access-token",
        "refresh_token": "refresh-token",
        "expires_in": 3600,
        "id_token": "id-token",
        "scope": scope,
    })
}

fn oauth_with(fetch: &Arc<MockFetch>) -> OpenAIChatGPTOAuth {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    OpenAIChatGPTOAuth::with_fetch(fetch)
}

fn device_options() -> LoginOptions {
    LoginOptions {
        get_device_id: Some(Arc::new(|| DEVICE_ID.to_string())),
        ..Default::default()
    }
}

/// 构造 `{ signal, prompt, notify }`：`prompt` 用授权 URL 拼回调 URL。
fn login_interaction(callback_client_id: Option<&str>) -> Arc<MockInteraction> {
    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    let client_id = callback_client_id.map(str::to_string);
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        assert_eq!(request.prompt.type_name(), "manual_code");
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
            .expect("authorization URL was not emitted before the callback prompt");
        let authorize = url::Url::parse(&auth_url).expect("auth url");
        let params: std::collections::HashMap<_, _> =
            authorize.query_pairs().into_owned().collect();
        let mut callback = url::Url::parse(params.get("redirect_uri").expect("redirect_uri"))
            .expect("callback url");
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("code", "authorization-code");
        query.append_pair("state", params.get("state").expect("state"));
        if let Some(client_id) = &client_id {
            query.append_pair("client_id", client_id);
        }
        callback.set_query(Some(&query.finish()));
        responding_prompt(callback.to_string())
    });
    interaction
}

#[tokio::test]
async fn registers_a_user_owned_client_and_stores_its_issued_id_and_granted_scopes() {
    let _guard = SERIAL.lock().await;
    let exchange_body: RecordedForm = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exchange_body);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        *slot.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(kiana_ai::auth::oauth::form_urldecode(&request.body_text()));
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response(REQUIRED_SCOPE),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = login_interaction(Some("oaiapp_issued"));

    let credential = oauth
        .login(interaction.as_ref(), &device_options())
        .await
        .expect("login");

    let auth_url = interaction.auth_url().expect("auth url");
    let url = url::Url::parse(&auth_url).expect("url");
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("dynamic_agent_client")
    );
    assert_eq!(
        params.get("agent_name_hint").map(String::as_str),
        Some("Kiana")
    );
    assert_eq!(
        params.get("ext_agent_host_id").map(String::as_str),
        Some(format!("urn:uuid:{DEVICE_ID}").as_str())
    );
    assert_eq!(
        params.get("scope").map(String::as_str),
        Some(REQUIRED_SCOPE)
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://127.0.0.1:1455/auth/callback")
    );
    assert_eq!(
        params.get("resource").map(String::as_str),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );

    let body = exchange_body
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("exchange body");
    let get = |name: &str| {
        body.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("client_id"), Some("oaiapp_issued"));
    assert_eq!(get("code"), Some("authorization-code"));
    assert_eq!(get("resource"), Some("https://api.openai.com/v1"));
    assert!(get("code_verifier").is_some_and(|value| !value.is_empty()));

    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.get_str("clientId"), Some("oaiapp_issued"));
    assert_eq!(
        credential.get("scopes"),
        Some(&json!(REQUIRED_SCOPE.split(' ').collect::<Vec<_>>()))
    );
}

#[tokio::test]
async fn uses_the_apps_agent_name_as_the_name_hint() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response(REQUIRED_SCOPE),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = login_interaction(Some("oaiapp_issued"));
    let options = LoginOptions {
        agent_name: Some("my-app".into()),
        ..device_options()
    };
    oauth
        .login(interaction.as_ref(), &options)
        .await
        .expect("login");
    let auth_url = interaction.auth_url().expect("auth url");
    let url = url::Url::parse(&auth_url).expect("url");
    let hint = url
        .query_pairs()
        .find(|(key, _)| key == "agent_name_hint")
        .map(|(_, value)| value.into_owned());
    assert_eq!(hint.as_deref(), Some("my-app"));
}

#[tokio::test]
async fn rejects_registration_without_an_issued_client_id() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response(REQUIRED_SCOPE),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = login_interaction(None);
    let error = oauth
        .login(interaction.as_ref(), &device_options())
        .await
        .expect_err("missing client id");
    assert!(
        error
            .message
            .contains("registration callback did not contain an issued client ID"),
        "{}",
        error.message
    );
    assert_eq!(fetch.request_count(), 0);
}

#[tokio::test]
async fn rejects_a_token_response_that_did_not_grant_direct_token_use() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response("openid profile email offline_access resource.invoke"),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = login_interaction(Some("oaiapp_issued"));
    let error = oauth
        .login(interaction.as_ref(), &device_options())
        .await
        .expect_err("missing direct scope");
    assert!(
        error
            .message
            .contains("grant did not include chatgpt.tokens.use.direct"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn requires_a_device_id_before_starting_authorization() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response(REQUIRED_SCOPE),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = login_interaction(Some("oaiapp_issued"));

    let error = oauth
        .login(interaction.as_ref(), &LoginOptions::default())
        .await
        .expect_err("no device id");
    assert!(
        error.message.contains("requires a device ID"),
        "{}",
        error.message
    );
    let bad_options = LoginOptions {
        get_device_id: Some(Arc::new(|| "not-a-uuid".to_string())),
        ..Default::default()
    };
    let error = oauth
        .login(interaction.as_ref(), &bad_options)
        .await
        .expect_err("bad device id");
    assert!(
        error.message.contains("requires a device ID"),
        "{}",
        error.message
    );
    assert!(interaction.auth_url().is_none());
}

#[tokio::test]
async fn requires_refresh_responses_to_rotate_the_refresh_token() {
    let mut response = token_response(REQUIRED_SCOPE);
    response
        .as_object_mut()
        .expect("object")
        .remove("refresh_token");
    let fetch = MockFetch::new(move |_request: OAuthFetchRequest| {
        let response = response.clone();
        Box::pin(async move { Ok(OAuthFetchResponse::json_response(200, &response)) })
    });
    let oauth = oauth_with(&fetch);
    let mut credential = OAuthCredential::new("old-access", "old-refresh", 0.0);
    credential
        .extra
        .insert("clientId".into(), Value::String("oaiapp_existing".into()));
    let error = oauth
        .refresh(&credential, &AbortScope::new())
        .await
        .expect_err("missing refresh token");
    assert!(
        error
            .message
            .contains("token response has invalid refresh_token"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn refreshes_with_the_credentials_issued_client_id_and_stores_replacement_scopes() {
    let refresh_body: RecordedForm = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&refresh_body);
    let mut response = token_response(REQUIRED_SCOPE);
    response["access_token"] = json!("new-access");
    response["refresh_token"] = json!("new-refresh");
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        *slot.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(kiana_ai::auth::oauth::form_urldecode(&request.body_text()));
        let response = response.clone();
        Box::pin(async move { Ok(OAuthFetchResponse::json_response(200, &response)) })
    });
    let oauth = oauth_with(&fetch);
    let mut credential = OAuthCredential::new("old-access", "old-refresh", 0.0);
    credential
        .extra
        .insert("clientId".into(), Value::String("oaiapp_existing".into()));
    credential.extra.insert(
        "scopes".into(),
        json!(REQUIRED_SCOPE.split(' ').collect::<Vec<_>>()),
    );

    let before = kiana_ai::auth::oauth::device_code::AuthClock::now_ms(
        &kiana_ai::auth::oauth::device_code::SystemAuthClock,
    );
    let refreshed = oauth
        .refresh(&credential, &AbortScope::new())
        .await
        .expect("refresh");
    assert!(refreshed.expires >= before + (3600.0 - 180.0) * 1000.0);
    assert!(
        refreshed.expires
            <= kiana_ai::auth::oauth::device_code::AuthClock::now_ms(
                &kiana_ai::auth::oauth::device_code::SystemAuthClock
            ) + (3600.0 - 180.0) * 1000.0
    );

    let body = refresh_body
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("refresh body");
    let get = |name: &str| {
        body.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(get("grant_type"), Some("refresh_token"));
    assert_eq!(get("client_id"), Some("oaiapp_existing"));
    assert_eq!(get("refresh_token"), Some("old-refresh"));
    assert_eq!(get("resource"), Some("https://api.openai.com/v1"));
    assert!(get("scope").is_none());

    assert_eq!(refreshed.access, "new-access");
    assert_eq!(refreshed.refresh, "new-refresh");
    assert_eq!(refreshed.get_str("clientId"), Some("oaiapp_existing"));
    assert_eq!(
        refreshed.get("scopes"),
        Some(&json!(REQUIRED_SCOPE.split(' ').collect::<Vec<_>>()))
    );
}

/// 回归：`login` 结束时必须调用 `closeAllConnections()`（TS 同位置调用），断开浏览器
/// 预开的空闲连接。
///
/// 只 `close()`（停止 accept）不够：旧 server 的 handler 仍阻塞在读取请求上，这条连接会
/// 活到进程结束；下一次登录的回调可能沿它送达并被旧 state 拒绝，而新登录永远等不到
/// result（spec/ai-auth.md §2.12）。
#[tokio::test]
async fn closes_preconnected_sockets_when_login_settles() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response(REQUIRED_SCOPE),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
    let idle: Arc<Mutex<Option<tokio::net::TcpStream>>> = Arc::new(Mutex::new(None));
    let idle_slot = Arc::clone(&idle);
    interaction.set_handler(move |request: AuthPromptRequest| -> PromptFuture {
        assert_eq!(request.prompt.type_name(), "manual_code");
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
            .expect("authorization URL was not emitted before the callback prompt");
        let authorize = url::Url::parse(&auth_url).expect("auth url");
        let params: std::collections::HashMap<_, _> =
            authorize.query_pairs().into_owned().collect();
        let state = params.get("state").expect("state").clone();
        let idle_slot = Arc::clone(&idle_slot);
        Box::pin(async move {
            // 浏览器预开的空闲连接：建连后不发送任何请求。
            let spare = tokio::net::TcpStream::connect(("127.0.0.1", 1455))
                .await
                .expect("preconnect");
            tokio::time::sleep(Duration::from_millis(100)).await;
            *idle_slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(spare);
            // 用第二条连接完成一次请求来确认 accept 循环仍在运行；accept 队列是 FIFO，
            // 因此第一条连接在此时必然已被 accept 并交给了它的 handler。
            let mut probe = tokio::net::TcpStream::connect(("127.0.0.1", 1455))
                .await
                .expect("probe connect");
            probe
                .write_all(
                    b"GET /not-the-callback HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("probe write");
            let mut buffer = [0u8; 64];
            let read = tokio::time::timeout(Duration::from_secs(2), probe.read(&mut buffer))
                .await
                .expect("probe response")
                .expect("probe read");
            assert!(read > 0, "probe connection was not served");
            Ok(format!(
                "http://127.0.0.1:1455/auth/callback?code=manual-code&state={state}&client_id=oaiapp_issued"
            ))
        })
    });

    let credential = oauth
        .login(interaction.as_ref(), &device_options())
        .await
        .expect("login");
    assert_eq!(credential.access, "access-token");

    let mut spare = idle
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        .expect("preconnected socket");
    let mut buffer = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), spare.read(&mut buffer)).await;
    match read {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        other => panic!("preconnected socket survived login: {other:?}"),
    }
}

/// 回归：端口 1455 被占用时 `login` 必须抛 TS 的固定文案（不换端口、不吞掉错误）。
///
/// 对应 spec/ai-auth.md §2.12：让回调用另一个端口会让浏览器的回调打到占用端口的
/// 另一个登录 / Codex CLI 上，因此这里明确报错。
#[tokio::test]
async fn reports_the_fixed_port_when_it_is_in_use() {
    let _guard = SERIAL.lock().await;
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 1455)).expect("port blocker");
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async {
            Ok(OAuthFetchResponse::json_response(
                200,
                &token_response(REQUIRED_SCOPE),
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    let error = oauth
        .login(interaction.as_ref(), &device_options())
        .await
        .expect_err("port in use");
    drop(blocker);
    assert_eq!(
        error.message,
        "Port 1455 is in use, probably by an unfinished login in another kiana session or by the Codex CLI. Cancel that login and try again."
    );
    assert_eq!(fetch.request_count(), 0);
}
