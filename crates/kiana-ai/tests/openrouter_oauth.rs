//! `test/openrouter-oauth.test.ts` 的移植（13 个用例）。
//!
//! 第 1、2 个用例的 provider/models 接线（`openrouterProvider()`、`createModels`）已随对应
//! 单元合入，因此按 TS 原样断言。

use std::sync::{Arc, Mutex};

use kiana_ai::auth::credential_store::InMemoryCredentialStore;
use kiana_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use kiana_ai::models::{CreateModelsOptions, GetAuthTarget, create_models};
use kiana_ai::providers::openrouter::openrouter_provider;
use kiana_ai::types::ModelType;

use kiana_ai::auth::oauth::openrouter::OpenRouterOAuth;
use kiana_ai::auth::oauth::pkce::base64url_encode;
use kiana_ai::auth::oauth::testing::{
    MockFetch, MockInteraction, PromptFuture, pending_prompt, responding_prompt,
};
use kiana_ai::auth::oauth::{OAuthFetch, OAuthFetchRequest, OAuthFetchResponse};
use kiana_ai::auth::types::{AuthError, AuthEvent, AuthPromptRequest, OAuthAuth};
use kiana_ai::utils::abort::AbortScope;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// `KIANA_OAUTH_CALLBACK_HOST` 是进程级全局：同文件用例串行。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

/// 本机回调页面：绕过环境里的 HTTP 代理（否则 loopback 请求会被代理转发/伪造状态码）。
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

fn json_response(body: Value, status: u16) -> OAuthFetchResponse {
    OAuthFetchResponse::json_response(status, &body)
}

fn oauth_with(fetch: &Arc<MockFetch>) -> Arc<OpenRouterOAuth> {
    let fetch: Arc<dyn OAuthFetch> = fetch.clone();
    Arc::new(OpenRouterOAuth::with_fetch(fetch))
}

async fn wait_for_auth_url(interaction: &MockInteraction) -> url::Url {
    for _ in 0..100_000 {
        if let Some(auth_url) = interaction.auth_url() {
            return url::Url::parse(&auth_url).expect("auth url");
        }
        tokio::task::yield_now().await;
    }
    panic!("auth_url event was not emitted");
}

fn callback_url(auth_url: &url::Url) -> url::Url {
    let value = auth_url
        .query_pairs()
        .find(|(key, _)| key == "callback_url")
        .map(|(_, value)| value.into_owned())
        .expect("callback_url");
    url::Url::parse(&value).expect("callback url")
}

fn with_code(mut url: url::Url, code: &str) -> String {
    url.query_pairs_mut().append_pair("code", code);
    url.into()
}

/// 记录 prompt 的 signal，方便断言「回调胜出后 prompt 被 abort」。
fn pending_prompt_with_signal_slot(
    slot: Arc<Mutex<Option<AbortScope>>>,
) -> impl Fn(AuthPromptRequest) -> PromptFuture + Send + Sync + 'static {
    move |request: AuthPromptRequest| {
        *slot.lock().unwrap_or_else(|error| error.into_inner()) = request.signal.clone();
        pending_prompt()
    }
}

#[tokio::test]
async fn is_exposed_alongside_api_key_auth() {
    let provider = openrouter_provider();
    assert!(provider.auth().api_key.is_some());
    let oauth = provider.auth().oauth.as_ref().expect("oauth auth");
    assert_eq!(oauth.login_label(), Some("Sign in with OpenRouter"));

    // flow 自身的元数据。
    let flow = OpenRouterOAuth::new();
    assert_eq!(flow.name(), "OpenRouter OAuth");
    assert_eq!(flow.login_label(), Some("Sign in with OpenRouter"));
    assert!(!flow.is_subscription());
}

#[tokio::test]
async fn resolves_the_same_stored_oauth_key_for_chat_and_image_models() {
    let credentials = InMemoryCredentialStore::new();
    credentials
        .modify(
            "openrouter",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential::new(
                        "sk-or-stored",
                        "",
                        // Number.MAX_SAFE_INTEGER：永不刷新。
                        9_007_199_254_740_991.0,
                    ))))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();

    let models = create_models(CreateModelsOptions {
        credentials: Some(Arc::new(credentials)),
        ..Default::default()
    });
    models.set_provider(openrouter_provider());
    let chat_model = models
        .get_models(Some("openrouter"))
        .into_iter()
        .next()
        .expect("chat model");
    let image_model = models
        .get_models_of_type(ModelType::Image, Some("openrouter"))
        .into_iter()
        .next()
        .expect("image model");

    for model in [&chat_model, &image_model] {
        assert_eq!(
            models
                .get_auth(GetAuthTarget::Model(model), Default::default())
                .await
                .expect("auth")
                .expect("configured")
                .auth
                .api_key
                .as_deref(),
            Some("sk-or-stored")
        );
    }
}

#[tokio::test]
async fn runs_pkce_on_a_one_shot_loopback_callback_and_exchanges_the_code_for_a_permanent_api_key()
{
    let _guard = SERIAL.lock().await;
    let exchange_body: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exchange_body);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        let parsed: Value = serde_json::from_str(&request.body_text()).expect("JSON body");
        *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(parsed);
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-test"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let manual_signal: Arc<Mutex<Option<AbortScope>>> = Arc::new(Mutex::new(None));
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(pending_prompt_with_signal_slot(Arc::clone(&manual_signal)));

    let handle = {
        let oauth = Arc::clone(&oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    let auth_url = wait_for_auth_url(&interaction).await;
    assert_eq!(
        auth_url.origin().ascii_serialization(),
        "https://openrouter.ai"
    );
    assert_eq!(auth_url.path(), "/auth");
    let challenge = auth_url
        .query_pairs()
        .find(|(key, _)| key == "code_challenge")
        .map(|(_, value)| value.into_owned())
        .expect("code_challenge");
    let method = auth_url
        .query_pairs()
        .find(|(key, _)| key == "code_challenge_method")
        .map(|(_, value)| value.into_owned());
    assert_eq!(method.as_deref(), Some("S256"));

    let callback = callback_url(&auth_url);
    assert_eq!(callback.host_str(), Some("127.0.0.1"));
    let path = callback.path().to_string();
    assert!(path.starts_with("/oauth/callback/"), "{path}");
    let suffix = &path["/oauth/callback/".len()..];
    assert!(
        suffix.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "{suffix}"
    );

    let page = get_loopback(with_code(callback, "authorization-code"))
        .await
        .expect("callback page");
    assert_eq!(page.status().as_u16(), 200);
    let credential = handle.await.expect("join").expect("login");
    assert_eq!(credential.access, "sk-or-test");
    assert_eq!(credential.refresh, "");
    assert_eq!(credential.expires, 9_007_199_254_740_991.0);

    let signal = manual_signal
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("manual prompt signal");
    assert!(signal.is_aborted());

    let body = exchange_body
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("exchange body");
    assert_eq!(body["code"], "authorization-code");
    assert_eq!(body["code_challenge_method"], "S256");
    let verifier = body["code_verifier"].as_str().expect("code_verifier");
    let digest = Sha256::digest(verifier.as_bytes());
    assert_eq!(challenge, base64url_encode(&digest));
    assert_eq!(fetch.request_count(), 1);
}

#[tokio::test]
async fn reports_token_exchange_failures_through_both_the_callback_page_and_login() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        Box::pin(async {
            Ok(json_response(
                json!({"error": {"message": "invalid code"}}),
                403,
            ))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| pending_prompt());

    let handle = {
        let oauth = Arc::clone(&oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    let auth_url = wait_for_auth_url(&interaction).await;
    let page = get_loopback(with_code(callback_url(&auth_url), "bad-code"))
        .await
        .expect("callback page");
    assert_eq!(page.status().as_u16(), 502);
    let error = handle.await.expect("join").expect_err("exchange failed");
    assert_eq!(
        error.message,
        "OpenRouter OAuth key exchange failed (HTTP 403): invalid code"
    );
}

#[tokio::test]
async fn allows_only_one_token_exchange_for_a_callback() {
    let _guard = SERIAL.lock().await;
    let complete: Arc<
        tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<OAuthFetchResponse>>>,
    > = Arc::new(tokio::sync::Mutex::new(None));
    let (sender, receiver) = tokio::sync::oneshot::channel::<OAuthFetchResponse>();
    *complete.lock().await = Some(sender);
    let receiver = Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        let receiver = Arc::clone(&receiver);
        Box::pin(async move {
            let receiver = receiver.lock().await.take().expect("single exchange");
            receiver.await.map_err(|_| AuthError::new("fetch failed"))
        })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| pending_prompt());

    let handle = {
        let oauth = Arc::clone(&oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    let auth_url = wait_for_auth_url(&interaction).await;
    let callback = with_code(callback_url(&auth_url), "authorization-code");

    let first = tokio::spawn(async move { get_loopback(callback).await });
    let callback = with_code(callback_url(&auth_url), "authorization-code");
    // 等第一次交换开始（MockFetch 收到请求）。
    for _ in 0..100_000 {
        if fetch.request_count() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let second = get_loopback(callback).await.expect("second callback");
    assert_eq!(second.status().as_u16(), 409);
    assert_eq!(fetch.request_count(), 1);

    complete
        .lock()
        .await
        .take()
        .expect("complete exchange")
        .send(json_response(json!({"key": "sk-or-test"}), 200))
        .expect("send response");
    let credential = handle.await.expect("join").expect("login");
    assert_eq!(credential.access, "sk-or-test");
    let first_page = first
        .await
        .expect("first join")
        .expect("first callback page");
    assert_eq!(first_page.status().as_u16(), 200);
}

#[tokio::test]
async fn rejects_a_successful_response_that_does_not_contain_a_key() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(json_response(json!({"user_id": "user-1"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| pending_prompt());
    let handle = {
        let oauth = Arc::clone(&oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    let auth_url = wait_for_auth_url(&interaction).await;
    let page = get_loopback(with_code(callback_url(&auth_url), "code-without-key"))
        .await
        .expect("callback page");
    assert_eq!(page.status().as_u16(), 502);
    let error = handle.await.expect("join").expect_err("no key");
    assert_eq!(
        error.message,
        "OpenRouter OAuth response carries no \"key\""
    );
}

#[tokio::test]
async fn mints_a_key_from_a_pasted_redirect_url_when_the_loopback_callback_never_arrives() {
    let _guard = SERIAL.lock().await;
    let exchange_body: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exchange_body);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        *slot.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(serde_json::from_str(&request.body_text()).expect("JSON body"));
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-manual"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    let events = interaction.events_handle();
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
            .expect("auth url");
        let url = url::Url::parse(&auth_url).expect("url");
        let callback_url = url
            .query_pairs()
            .find(|(key, _)| key == "callback_url")
            .map(|(_, value)| value.into_owned())
            .expect("callback_url");
        responding_prompt(format!("{callback_url}?code=manual-code"))
    });
    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credential.access, "sk-or-manual");
    assert_eq!(credential.refresh, "");
    assert_eq!(credential.expires, 9_007_199_254_740_991.0);
    let body = exchange_body
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("exchange body");
    assert_eq!(body["code"], "manual-code");
    assert_eq!(body["code_challenge_method"], "S256");
    assert_eq!(fetch.request_count(), 1);
}

#[tokio::test]
async fn accepts_a_bare_authorization_code_from_the_manual_prompt() {
    let _guard = SERIAL.lock().await;
    let exchange_body: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exchange_body);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        *slot.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(serde_json::from_str(&request.body_text()).expect("JSON body"));
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-manual"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| responding_prompt("  manual-code  "));
    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("login");
    assert_eq!(credential.access, "sk-or-manual");
    let body = exchange_body
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("exchange body");
    assert_eq!(body["code"], "manual-code");
}

/// 回归：`new URLSearchParams("?code=…")` 去掉一个前导 `?`；Rust 曾把首键解析成 `"?code"`，
/// 使粘贴以 `?` 开头的查询串报 "Missing authorization code"。
#[tokio::test]
async fn accepts_a_pasted_query_string_with_a_leading_question_mark() {
    let _guard = SERIAL.lock().await;
    let exchange_body: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exchange_body);
    let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
        assert_eq!(request.url, TOKEN_URL);
        *slot.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(serde_json::from_str(&request.body_text()).expect("JSON body"));
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-manual"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| responding_prompt("?code=manual-code&state=1"));
    let credential = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect("leading question mark");
    assert_eq!(credential.access, "sk-or-manual");
    let body = exchange_body
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("exchange body");
    assert_eq!(body["code"], "manual-code");
}

#[tokio::test]
async fn fails_login_when_the_manual_prompt_is_cancelled() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-unexpected"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| Box::pin(async { Err(AuthError::new("Login cancelled")) }));
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("cancelled");
    assert_eq!(error.message, "Login cancelled");
    assert_eq!(fetch.request_count(), 0);
}

#[tokio::test]
async fn rejects_empty_manual_input_without_exchanging_a_code() {
    let _guard = SERIAL.lock().await;
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-unexpected"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(AbortScope::new());
    interaction.set_handler(|_request| responding_prompt("   "));
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("empty input");
    assert_eq!(error.message, "Missing authorization code");
    assert_eq!(fetch.request_count(), 0);
}

#[tokio::test]
async fn closes_the_pending_callback_when_login_is_cancelled() {
    let _guard = SERIAL.lock().await;
    let controller = AbortScope::new();
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-unexpected"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(controller.clone());
    interaction.set_handler(|_request| pending_prompt());

    let handle = {
        let oauth = Arc::clone(&oauth);
        let interaction = Arc::clone(&interaction);
        tokio::spawn(async move { oauth.login(interaction.as_ref(), &Default::default()).await })
    };
    let auth_url = wait_for_auth_url(&interaction).await;
    let callback = callback_url(&auth_url);
    controller.abort();
    let error = handle.await.expect("join").expect_err("cancelled");
    assert_eq!(error.message, "Login cancelled");
    // `close()` 通过取消 token 停止 accept 循环（任务调度后 listener 才被 drop）；
    // 让出若干次后连接应被拒绝（与 TS 的 `fetch` reject 一致）。
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    let result = get_loopback(with_code(callback, "late")).await;
    assert!(result.is_err(), "callback server should be closed");
}

#[tokio::test]
async fn rejects_before_opening_a_callback_server_when_login_is_already_cancelled() {
    let _guard = SERIAL.lock().await;
    let controller = AbortScope::new();
    controller.abort();
    let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
        Box::pin(async { Ok(json_response(json!({"key": "sk-or-unexpected"}), 200)) })
    });
    let oauth = oauth_with(&fetch);
    let interaction = MockInteraction::new(controller);
    interaction.set_handler(|_request| responding_prompt(""));
    let error = oauth
        .login(interaction.as_ref(), &Default::default())
        .await
        .expect_err("pre-cancelled");
    assert_eq!(error.message, "Login cancelled");
    assert!(
        interaction.events().is_empty(),
        "cancelled login must not emit events"
    );
}

#[tokio::test]
async fn uses_the_configured_oauth_callback_host() {
    let _guard = SERIAL.lock().await;
    // SAFETY: 同文件用例串行，且变量在用例结束前移除。
    unsafe { std::env::set_var("KIANA_OAUTH_CALLBACK_HOST", "localhost") };
    let result = async {
        let controller = AbortScope::new();
        let fetch = MockFetch::new(|_request: OAuthFetchRequest| {
            Box::pin(async { Ok(json_response(json!({"key": "unused"}), 200)) })
        });
        let oauth = oauth_with(&fetch);
        let interaction = MockInteraction::new(controller.clone());
        interaction.set_handler(|_request| pending_prompt());
        let handle = {
            let oauth = Arc::clone(&oauth);
            let interaction = Arc::clone(&interaction);
            tokio::spawn(
                async move { oauth.login(interaction.as_ref(), &Default::default()).await },
            )
        };
        let auth_url = wait_for_auth_url(&interaction).await;
        let callback = callback_url(&auth_url);
        controller.abort();
        let _ = handle.await.expect("join");
        callback.host_str().map(str::to_string)
    }
    .await;
    unsafe { std::env::remove_var("KIANA_OAUTH_CALLBACK_HOST") };
    assert_eq!(result.as_deref(), Some("localhost"));
}

/// 回归（评审发现）：TS 用 `${detail ? `: ${detail}` : ""}`，空的 `error_description` /
/// `error.message` 不追加后缀（Rust 曾输出尾随 ": "）。
#[tokio::test]
async fn empty_error_details_do_not_add_a_suffix() {
    let _guard = SERIAL.lock().await;
    let cases = [
        json!({"error": "x", "error_description": ""}),
        json!({"error_description": ""}),
        json!({"error": {"message": ""}}),
    ];
    for body in cases {
        let body_for_fetch = body.clone();
        let fetch = MockFetch::new(move |request: OAuthFetchRequest| {
            assert_eq!(request.url, TOKEN_URL);
            let body = body_for_fetch.clone();
            Box::pin(async move { Ok(json_response(body, 400)) })
        });
        let oauth = oauth_with(&fetch);
        let interaction = MockInteraction::new(AbortScope::new());
        interaction.set_handler(|_request| pending_prompt());
        let handle = {
            let oauth = Arc::clone(&oauth);
            let interaction = Arc::clone(&interaction);
            tokio::spawn(
                async move { oauth.login(interaction.as_ref(), &Default::default()).await },
            )
        };
        let auth_url = wait_for_auth_url(&interaction).await;
        let _page = get_loopback(with_code(callback_url(&auth_url), "bad-code"))
            .await
            .expect("callback page");
        let error = handle.await.expect("join").expect_err("exchange failed");
        assert_eq!(
            error.message, "OpenRouter OAuth key exchange failed (HTTP 400)",
            "body {body}"
        );
    }
}
