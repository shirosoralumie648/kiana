//! `packages/coding-agent/test/mcp-oauth-refresh.test.ts`：OAuth 刷新、登录、CIMD/DCR。
//!
//! 共享同一个 `InMemoryAuthStorageBackend` 与锁目录的多个 store 实例代表不同的 kiana 进程。

mod mcp_oauth_support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_builtin_extensions::mcp::oauth::{
    McpAuthProvider, McpAuthProviderOptions, McpOAuthCredentialStore, McpOAuthServerStore,
    McpOAuthSettings, McpSignInError, McpSignInOptions, McpSignInPrompt, create_mcp_auth_provider,
    set_client_metadata_base_url_for_tests, sign_in_mcp_server,
};
use kiana_config::auth::storage::{AuthStorageBackend, InMemoryAuthStorageBackend};
use kiana_mcp::auth_provider::{AuthProvider, ReqwestFetch, UnauthorizedContext};
use kiana_mcp::oauth::errors::OAuthFailure;
use kiana_mcp::oauth::{McpOAuthStateStore, OAuthChallenge};
use mcp_oauth_support::{OAuthServerOptions, OAuthTestServer};
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;
use url::Url;

/// 模拟浏览器：打开授权 URL（跟随重定向到回环回调）。
struct BrowserPrompt;

async fn open_in_browser(url: String) {
    let Ok(client) = reqwest::Client::builder().no_proxy().build() else {
        return;
    };
    let _ = client.get(url).send().await;
}

#[async_trait]
impl McpSignInPrompt for BrowserPrompt {
    fn show_authorization_url(&self, url: &Url) {
        let url = url.to_string();
        tokio::spawn(open_in_browser(url));
    }

    async fn prompt_for_redirect_url(&self, signal: CancellationToken) -> Option<String> {
        signal.cancelled().await;
        None
    }
}

/// 记录打开的授权 URL，并像浏览器一样访问它。
struct CollectingPrompt {
    opened: Arc<Mutex<Vec<Url>>>,
}

#[async_trait]
impl McpSignInPrompt for CollectingPrompt {
    fn show_authorization_url(&self, url: &Url) {
        self.opened.lock().unwrap().push(url.clone());
        let url = url.to_string();
        tokio::spawn(open_in_browser(url));
    }

    async fn prompt_for_redirect_url(&self, signal: CancellationToken) -> Option<String> {
        signal.cancelled().await;
        None
    }
}

/// 模拟“从浏览器地址栏粘贴重定向 URL”。
struct PastePrompt {
    shown: Arc<Mutex<Option<Url>>>,
}

#[async_trait]
impl McpSignInPrompt for PastePrompt {
    fn show_authorization_url(&self, url: &Url) {
        *self.shown.lock().unwrap() = Some(url.clone());
    }

    async fn prompt_for_redirect_url(&self, _signal: CancellationToken) -> Option<String> {
        let shown = self.shown.lock().unwrap().clone()?;
        let redirect_uri = shown
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .map(|(_, value)| value.into_owned())?;
        let state = shown
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default();
        let mut redirect = Url::parse(&redirect_uri).ok()?;
        redirect.set_path("/callback");
        redirect.set_query(Some(&format!("code=code-1&state={state}")));
        Some(redirect.to_string())
    }
}

fn credentials(
    backend: &InMemoryAuthStorageBackend,
    lock_dir: Option<&std::path::Path>,
) -> McpOAuthCredentialStore {
    McpOAuthCredentialStore::new(
        Some(AuthStorageBackend::InMemory(backend.clone())),
        lock_dir.map(|path| path.to_path_buf()),
    )
}

fn store_for(
    credentials: &McpOAuthCredentialStore,
    server: &OAuthTestServer,
) -> Arc<dyn McpOAuthStateStore> {
    Arc::new(credentials.for_server("test", &server.url))
}

async fn sign_in(
    server: &OAuthTestServer,
    store: Arc<dyn McpOAuthStateStore>,
    settings: McpOAuthSettings,
    challenge: Option<OAuthChallenge>,
    prompt: Arc<dyn McpSignInPrompt>,
) -> Result<(), McpSignInError> {
    sign_in_mcp_server(McpSignInOptions {
        server_url: server.url.clone(),
        store,
        settings,
        challenge,
        prompt,
        signal: None,
    })
    .await
}

fn unauthorized_context(server_url: &str, token: &str) -> UnauthorizedContext {
    UnauthorizedContext {
        status: 401,
        headers: Default::default(),
        body: Vec::new(),
        server_url: Url::parse(server_url).unwrap(),
        fetch: Arc::new(ReqwestFetch::new()),
        token: Some(token.to_string()),
    }
}

/// 一次已完成登录的服务器 + 共享凭据文件/锁目录。
struct SignedIn {
    server: OAuthTestServer,
    lock_dir: tempfile::TempDir,
    backend: InMemoryAuthStorageBackend,
}

impl SignedIn {
    /// 一个“独立进程”的 store + provider。
    fn process(&self) -> (McpOAuthServerStore, McpAuthProvider) {
        let store = credentials(&self.backend, Some(self.lock_dir.path()))
            .for_server("test", &self.server.url);
        let provider = create_mcp_auth_provider(McpAuthProviderOptions {
            server_url: self.server.url.clone(),
            store: store.clone(),
            settings: Arc::new(|| Ok(McpOAuthSettings::default())),
            on_challenge: Arc::new(|_| {}),
        });
        (store, provider)
    }
}

async fn signed_in() -> SignedIn {
    let server = OAuthTestServer::start(OAuthServerOptions::default()).await;
    let lock_dir = tempfile::tempdir().unwrap();
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, Some(lock_dir.path())), &server);
    sign_in(
        &server,
        store,
        McpOAuthSettings::default(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .expect("sign-in succeeds");
    SignedIn {
        server,
        lock_dir,
        backend,
    }
}

#[tokio::test]
async fn refreshes_once_when_several_processes_find_the_same_token_rejected() {
    let harness = signed_in().await;
    harness.server.expire_access_tokens();
    let processes = [harness.process(), harness.process(), harness.process()];
    let server_url = harness.server.url.clone();
    let refreshes = processes.iter().map(|(_, provider)| {
        let provider = provider.clone();
        let context = unauthorized_context(&server_url, "access-1");
        async move { provider.on_unauthorized(context).await }
    });
    let results = futures::future::join_all(refreshes).await;
    for result in results {
        result.expect("refresh succeeds");
    }
    assert_eq!(
        harness
            .server
            .log()
            .iter()
            .filter(|entry| *entry == "token refresh")
            .count(),
        1
    );
    for (_, provider) in &processes {
        assert_eq!(provider.token().await.as_deref(), Some("access-2"));
    }
    // 锁已释放。
    assert_eq!(
        std::fs::read_dir(harness.lock_dir.path()).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn waits_for_a_running_refresh_to_save_the_new_tokens() {
    let harness = signed_in().await;
    harness.server.expire_access_tokens();
    let (store, provider) = harness.process();
    let server_url = harness.server.url.clone();
    let background = {
        let provider = provider.clone();
        tokio::spawn(async move {
            provider
                .on_unauthorized(unauthorized_context(&server_url, "access-1"))
                .await
        })
    };
    // 让后台任务跑到“已持有刷新锁”的位置（TS 的 promise 创建即执行）。
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let entries = std::fs::read_dir(harness.lock_dir.path()).unwrap().count();
        if entries > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the refresh never took the lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    provider.settled().await;
    let state = store.load().await.unwrap().expect("state is stored");
    assert_eq!(
        state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.string("access_token")),
        Some("access-2")
    );
    background.await.unwrap().unwrap();
}

#[tokio::test]
async fn rejects_an_authorization_response_from_another_issuer() {
    let server = OAuthTestServer::start(OAuthServerOptions {
        iss: Some("https://attacker.example".to_string()),
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &server);
    let error = sign_in(
        &server,
        store,
        McpOAuthSettings::default(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            McpSignInError::Failure(OAuthFailure::IssuerMismatch(_))
        ),
        "{error:?}"
    );
    server.close().await;
}

#[tokio::test]
async fn keeps_the_granted_scope_when_the_server_asks_for_more() {
    let server = OAuthTestServer::start(OAuthServerOptions::default()).await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = credentials(&backend, None).for_server("test", &server.url);
    let opened: Arc<Mutex<Vec<Url>>> = Arc::new(Mutex::new(Vec::new()));
    let sign_in_with = |challenge: OAuthChallenge| {
        let store: Arc<dyn McpOAuthStateStore> = Arc::new(store.clone());
        let prompt = Arc::new(CollectingPrompt {
            opened: Arc::clone(&opened),
        });
        let server = &server;
        async move {
            sign_in(
                server,
                store,
                McpOAuthSettings::default(),
                Some(challenge),
                prompt,
            )
            .await
        }
    };
    sign_in_with(OAuthChallenge {
        scope: Some("issues:read".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    let saved = store.load().await.unwrap().unwrap();
    assert_eq!(
        saved
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.string("scope")),
        Some("issues:read")
    );
    sign_in_with(OAuthChallenge {
        error: Some("insufficient_scope".to_string()),
        scope: Some("issues:write".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();
    let scopes: Vec<Option<String>> = opened
        .lock()
        .unwrap()
        .iter()
        .map(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "scope")
                .map(|(_, value)| value.into_owned())
        })
        .collect();
    assert_eq!(
        scopes,
        vec![
            Some("issues:read".to_string()),
            Some("issues:read issues:write".to_string())
        ]
    );
    let saved = store.load().await.unwrap().unwrap();
    assert_eq!(
        saved
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.string("scope")),
        Some("issues:read issues:write")
    );
    server.close().await;
}

#[tokio::test]
async fn uses_the_configured_authorization_server_metadata_url() {
    let server = OAuthTestServer::start(OAuthServerOptions::default()).await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &server);
    let settings = McpOAuthSettings {
        auth_server_metadata_url: Some(Url::parse(&server.url).unwrap().join("/missing").unwrap()),
        ..McpOAuthSettings::default()
    };
    let error = sign_in(&server, store, settings, None, Arc::new(BrowserPrompt))
        .await
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("HTTP 404 loading authorization server metadata"),
        "{}",
        error.message()
    );
    server.close().await;
}

#[tokio::test]
async fn registers_dynamically_by_default_even_when_the_server_supports_documents() {
    let server = OAuthTestServer::start(OAuthServerOptions {
        cimd: true,
        iss_parameter: true,
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &server);
    sign_in(
        &server,
        store,
        McpOAuthSettings::default(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap();
    assert_eq!(server.registrations().len(), 1);
    assert_eq!(
        server.authorizations()[0]
            .get("client_id")
            .map(String::as_str),
        Some("client-1")
    );
    server.close().await;
}

fn cimd_settings() -> McpOAuthSettings {
    set_client_metadata_base_url_for_tests(Some("https://kiana.dev/oauth".to_string()));
    McpOAuthSettings {
        client_registration: Some(kiana_core::mcp_servers::McpClientRegistration::Cimd),
        ..McpOAuthSettings::default()
    }
}

#[tokio::test]
async fn uses_its_document_when_authorization_responses_name_their_issuer() {
    let server = OAuthTestServer::start(OAuthServerOptions {
        cimd: true,
        iss_parameter: true,
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = credentials(&backend, None).for_server("test", &server.url);
    let store_dyn: Arc<dyn McpOAuthStateStore> = Arc::new(store.clone());
    sign_in(
        &server,
        store_dyn,
        cimd_settings(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap();
    let authorization = server.authorizations()[0].clone();
    assert_eq!(
        authorization.get("client_id").map(String::as_str),
        Some("https://kiana.dev/oauth/client.json")
    );
    let redirect = Url::parse(authorization.get("redirect_uri").unwrap()).unwrap();
    assert_eq!(
        format!("{}{}", redirect.host_str().unwrap(), redirect.path()),
        "127.0.0.1/callback"
    );
    assert!(redirect.port().is_some());
    assert!(server.registrations().is_empty());
    assert_eq!(
        server.token_requests()[0]
            .get("client_id")
            .map(String::as_str),
        Some("https://kiana.dev/oauth/client.json")
    );
    assert_eq!(
        server.token_requests()[0]
            .get("redirect_uri")
            .map(String::as_str),
        Some(redirect.as_str())
    );
    // 文档不落盘，因此再次登录会刷新令牌而不是丢弃它们。
    assert!(
        store
            .load()
            .await
            .unwrap()
            .unwrap()
            .client_information
            .is_none()
    );
    let store_dyn: Arc<dyn McpOAuthStateStore> = Arc::new(store.clone());
    sign_in(
        &server,
        store_dyn,
        cimd_settings(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap();
    assert_eq!(server.authorizations().len(), 1);
    let requests = server.token_requests();
    assert_eq!(
        requests[1].get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        requests[1].get("client_id").map(String::as_str),
        Some("https://kiana.dev/oauth/client.json")
    );
    server.close().await;
}

#[tokio::test]
async fn uses_a_document_and_callback_path_specific_to_the_mcp_server_without_the_iss_parameter() {
    let server = OAuthTestServer::start(OAuthServerOptions {
        cimd: true,
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &server);
    sign_in(
        &server,
        store,
        cimd_settings(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap();
    // 与 Codex 相同：MCP 服务器 URL 的 SHA-256 前 9 字节。
    let digest = sha2::Sha256::digest(server.url.as_bytes());
    let id = kiana_ai::auth::oauth::pkce::base64url_encode(&digest[..9]);
    let authorization = server.authorizations()[0].clone();
    let expected_client = format!("https://kiana.dev/oauth/{id}/client.json");
    assert_eq!(
        authorization.get("client_id").map(String::as_str),
        Some(expected_client.as_str())
    );
    let redirect = Url::parse(authorization.get("redirect_uri").unwrap()).unwrap();
    assert_eq!(redirect.path(), format!("/callback/{id}"));
    assert!(server.registrations().is_empty());
    assert_eq!(
        server.token_requests()[0]
            .get("redirect_uri")
            .map(String::as_str),
        Some(redirect.as_str())
    );
    server.close().await;
}

#[tokio::test]
async fn replaces_a_registered_client_when_switching_to_the_document() {
    let server = OAuthTestServer::start(OAuthServerOptions {
        cimd: true,
        iss_parameter: true,
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = credentials(&backend, None).for_server("test", &server.url);
    let dynamic_store: Arc<dyn McpOAuthStateStore> = Arc::new(store.clone());
    sign_in(
        &server,
        dynamic_store,
        McpOAuthSettings::default(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .load()
            .await
            .unwrap()
            .unwrap()
            .client_information
            .as_ref()
            .and_then(|client| client.string("client_id")),
        Some("client-1")
    );
    let cimd_store: Arc<dyn McpOAuthStateStore> = Arc::new(store.clone());
    sign_in(
        &server,
        cimd_store,
        cimd_settings(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap();
    let clients: Vec<Option<String>> = server
        .authorizations()
        .iter()
        .map(|authorization| authorization.get("client_id").cloned())
        .collect();
    assert_eq!(
        clients,
        vec![
            Some("client-1".to_string()),
            Some("https://kiana.dev/oauth/client.json".to_string())
        ]
    );
    assert!(
        store
            .load()
            .await
            .unwrap()
            .unwrap()
            .client_information
            .is_none()
    );
    server.close().await;
}

#[tokio::test]
async fn accepts_the_authorization_response_only_on_the_server_specific_redirect_uri() {
    // 串台的授权服务器把响应送到共享的回调路径。
    let mixed_up = OAuthTestServer::start(OAuthServerOptions {
        cimd: true,
        redirect_path: Some("/callback".to_string()),
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &mixed_up);
    let error = sign_in(
        &mixed_up,
        store,
        cimd_settings(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap_err();
    assert!(
        error.message().contains("arrived on another redirect URI"),
        "{}",
        error.message()
    );
    mixed_up.close().await;

    // 从浏览器粘贴的 redirect URL 也一样。
    let server = OAuthTestServer::start(OAuthServerOptions {
        cimd: true,
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &server);
    let prompt = Arc::new(PastePrompt {
        shown: Arc::new(Mutex::new(None)),
    });
    let error = sign_in(&server, store, cimd_settings(), None, prompt)
        .await
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("does not match this sign-in's redirect URI"),
        "{}",
        error.message()
    );
    server.close().await;
}

#[tokio::test]
async fn fails_instead_of_registering_when_the_server_does_not_support_documents() {
    let server = OAuthTestServer::start(OAuthServerOptions {
        iss_parameter: true,
        ..OAuthServerOptions::default()
    })
    .await;
    let backend = InMemoryAuthStorageBackend::new();
    let store = store_for(&credentials(&backend, None), &server);
    let error = sign_in(
        &server,
        store,
        cimd_settings(),
        None,
        Arc::new(BrowserPrompt),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .message()
            .contains("does not support Client ID Metadata Documents"),
        "{}",
        error.message()
    );
    assert!(server.registrations().is_empty());
    server.close().await;
}
