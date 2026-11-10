//! `packages/mcp/test/oauth.test.ts` 的移植（13 个 `it`，两个 `describe`：
//! `MCP OAuth` 10 个 + `OAuthCallbackServer pages` 3 个）。
//!
//! TS 用 `node:http` 起真实回环服务器（`test/helpers.ts`）并用 `McpClient` 驱动
//! `StreamableHttpTransport`；Rust 侧同样走真实 TCP（`tests/support/mod.rs`，`helpers.ts` 的移植），
//! 只有两处工具替换：
//!
//! - `fetch` → [`kiana_mcp::auth_provider::ReqwestFetch`]（测试里统一用 `no_proxy` 的 client）；
//! - `AbortController` → `tokio_util::sync::CancellationToken`（§4.2）。
//!
//! `createHash("sha256").digest("base64url")` 的 PKCE 断言用 `sha2` + `base64` 复刻。

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use http::{HeaderMap, HeaderValue};
use kiana_mcp::auth_provider::{AuthProvider, McpFetch, ReqwestFetch, UnauthorizedContext};
use kiana_mcp::client::{McpClient, McpClientOptions, McpRequestOptions};
use kiana_mcp::oauth::callback::{
    OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions,
};
use kiana_mcp::oauth::discovery::{
    DiscoverAuthorizationServerOptions, discover_authorization_server_metadata,
};
use kiana_mcp::oauth::errors::OAuthFailure;
use kiana_mcp::oauth::flow::{
    CredentialKind, OAuthClientProvider, OAuthFlowOptions, OAuthFlowResult, RegisterClientOptions,
    StartAuthorizationOptions, adapt_oauth_provider, authorize_mcp, register_client,
    start_authorization,
};
use kiana_mcp::oauth::provider::{
    McpOAuthProvider, McpOAuthProviderOptions, McpOAuthStateStore, MemoryOAuthStateStore,
};
use kiana_mcp::oauth::types::{
    AuthorizationServerMetadata, OAuthClientInformation, OAuthClientInformationMixed,
    OAuthClientMetadata, OAuthDiscoveryState, OAuthTokens, parse_authorization_server_metadata,
};
use kiana_mcp::transports::streamable_http::{
    StreamableHttpTransport, StreamableHttpTransportOptions,
};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use support::{HttpRequest, ResponseWriter, listen};
use tokio_util::sync::CancellationToken;
use url::Url;

const BASE64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("JSON object").clone()
}

fn client_information(value: Value) -> OAuthClientInformationMixed {
    OAuthClientInformationMixed::from_fields(object(value))
}

fn client_metadata(value: Value) -> OAuthClientMetadata {
    OAuthClientMetadata::from_fields(object(value))
}

fn oauth_tokens(value: Value) -> OAuthTokens {
    OAuthTokens::from_fields(object(value))
}

fn authorization_server_metadata(value: Value) -> AuthorizationServerMetadata {
    AuthorizationServerMetadata::from_fields(object(value))
}

/// `url.searchParams.get(name)`。
fn query_param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// `new URLSearchParams(body)`。
fn form_params(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn form_param(params: &[(String, String)], name: &str) -> Option<String> {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// `serverOrigin`（TS 的 `listen` 回调第三个参数）：Rust 侧从 `Host` 头还原。
fn request_origin(request: &HttpRequest) -> String {
    format!(
        "http://{}",
        request
            .header("host")
            .unwrap_or_else(|| "127.0.0.1".to_owned())
    )
}

fn request_url(request: &HttpRequest, origin: &str) -> Url {
    Url::parse(&format!("{origin}{}", request.path)).expect("request URL")
}

async fn send_json(response: &ResponseWriter, status: u16, body: &Value) {
    response
        .send(
            status,
            &[("content-type", "application/json")],
            &body.to_string(),
        )
        .await;
}

/// 测试用的 `fetch`：`no_proxy`，否则环境里的代理变量会把回环请求也代理走。
fn test_fetch() -> Arc<dyn McpFetch> {
    ReqwestFetch::from_client(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
    )
    .shared()
}

/// TS 里 `fetch(url, { redirect: "manual" })` 的等价物。
fn manual_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP client")
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `new Response(null, { status, headers: { "www-authenticate": … } })` 的传输层形态。
fn unauthorized(
    status: u16,
    www_authenticate: &str,
    server_url: &str,
    fetch: Arc<dyn McpFetch>,
    token: Option<&str>,
) -> UnauthorizedContext {
    let mut headers = HeaderMap::new();
    headers.insert(
        "www-authenticate",
        HeaderValue::from_str(www_authenticate).expect("header value"),
    );
    UnauthorizedContext {
        status,
        headers,
        body: Vec::new(),
        server_url: Url::parse(server_url).expect("server URL"),
        fetch,
        token: token.map(str::to_owned),
    }
}

fn streamable_http_options(
    url: &str,
    auth_provider: Arc<dyn AuthProvider>,
    fetch: Arc<dyn McpFetch>,
) -> StreamableHttpTransportOptions {
    StreamableHttpTransportOptions {
        url: url.to_owned(),
        headers: Some(vec![(
            "Authorization".to_owned(),
            "Bearer caller-supplied-stale-token".to_owned(),
        )]),
        fetch: Some(fetch),
        open_get_stream: Some(false),
        auth_provider: Some(auth_provider),
        ..StreamableHttpTransportOptions::default()
    }
}

// ---------------------------------------------------------------------------
// TS 的 `TestOAuthProvider`
// ---------------------------------------------------------------------------

struct TestOAuthProvider {
    redirect_url: String,
    client_metadata: OAuthClientMetadata,
    client: Mutex<Option<OAuthClientInformationMixed>>,
    token_set: Mutex<Option<OAuthTokens>>,
    verifier: Mutex<Option<String>>,
    discovery: Mutex<Option<OAuthDiscoveryState>>,
    authorization_url: Mutex<Option<Url>>,
}

impl TestOAuthProvider {
    fn new(redirect_url: impl Into<String>) -> Self {
        let redirect_url = redirect_url.into();
        TestOAuthProvider {
            client_metadata: client_metadata(json!({
                "redirect_uris": [redirect_url.clone()],
                "client_name": "kiana-mcp-test",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            })),
            redirect_url,
            client: Mutex::new(None),
            token_set: Mutex::new(None),
            verifier: Mutex::new(None),
            discovery: Mutex::new(None),
            authorization_url: Mutex::new(None),
        }
    }

    fn client(&self) -> Option<OAuthClientInformationMixed> {
        lock(&self.client).clone()
    }

    fn set_client(&self, client: Value) {
        *lock(&self.client) = Some(client_information(client));
    }

    fn token_set(&self) -> Option<OAuthTokens> {
        lock(&self.token_set).clone()
    }

    fn set_token_set(&self, tokens: OAuthTokens) {
        *lock(&self.token_set) = Some(tokens);
    }

    fn set_verifier(&self, verifier: &str) {
        *lock(&self.verifier) = Some(verifier.to_owned());
    }

    fn set_discovery(&self, discovery: OAuthDiscoveryState) {
        *lock(&self.discovery) = Some(discovery);
    }

    fn authorization_url(&self) -> Option<Url> {
        lock(&self.authorization_url).clone()
    }
}

#[async_trait]
impl OAuthClientProvider for TestOAuthProvider {
    fn redirect_url(&self) -> &str {
        &self.redirect_url
    }

    fn client_metadata(&self) -> &OAuthClientMetadata {
        &self.client_metadata
    }

    async fn state(&self) -> Result<Option<String>, OAuthFailure> {
        Ok(Some("expected-state".to_owned()))
    }

    async fn client_information(
        &self,
    ) -> Result<Option<OAuthClientInformationMixed>, OAuthFailure> {
        Ok(self.client())
    }

    async fn save_client_information(
        &self,
        information: &OAuthClientInformationMixed,
    ) -> Result<(), OAuthFailure> {
        *lock(&self.client) = Some(information.clone());
        Ok(())
    }

    fn has_save_client_information(&self) -> bool {
        true
    }

    async fn tokens(&self) -> Result<Option<OAuthTokens>, OAuthFailure> {
        Ok(self.token_set())
    }

    async fn save_tokens(&self, tokens: &OAuthTokens) -> Result<(), OAuthFailure> {
        *lock(&self.token_set) = Some(tokens.clone());
        Ok(())
    }

    async fn redirect_to_authorization(&self, url: &Url) -> Result<(), OAuthFailure> {
        *lock(&self.authorization_url) = Some(url.clone());
        Ok(())
    }

    async fn save_code_verifier(&self, verifier: &str) -> Result<(), OAuthFailure> {
        *lock(&self.verifier) = Some(verifier.to_owned());
        Ok(())
    }

    async fn code_verifier(&self) -> Result<String, OAuthFailure> {
        lock(&self.verifier)
            .clone()
            .ok_or_else(|| OAuthFailure::error("Missing code verifier"))
    }

    async fn invalidate_credentials(&self, kind: CredentialKind) -> Result<(), OAuthFailure> {
        if matches!(kind, CredentialKind::All | CredentialKind::Client) {
            *lock(&self.client) = None;
        }
        if matches!(kind, CredentialKind::All | CredentialKind::Tokens) {
            *lock(&self.token_set) = None;
        }
        if matches!(kind, CredentialKind::All | CredentialKind::Verifier) {
            *lock(&self.verifier) = None;
        }
        if matches!(kind, CredentialKind::All | CredentialKind::Discovery) {
            *lock(&self.discovery) = None;
        }
        Ok(())
    }

    async fn save_discovery_state(&self, state: &OAuthDiscoveryState) -> Result<(), OAuthFailure> {
        *lock(&self.discovery) = Some(state.clone());
        Ok(())
    }

    async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>, OAuthFailure> {
        Ok(lock(&self.discovery).clone())
    }
}

fn as_client_provider(provider: &Arc<TestOAuthProvider>) -> Arc<dyn OAuthClientProvider> {
    Arc::clone(provider) as Arc<dyn OAuthClientProvider>
}

// ---------------------------------------------------------------------------
// describe("MCP OAuth")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovers_registers_authorizes_with_pkce_and_refreshes_on_401() {
    // `it("discovers, registers, authorizes with PKCE, and refreshes on 401")`
    let expected_challenge: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let challenge_slot = Arc::clone(&expected_challenge);
    let refresh_count = Arc::clone(&refreshes);
    let server = listen(move |request: HttpRequest, response: ResponseWriter| {
        let challenge_slot = Arc::clone(&challenge_slot);
        let refresh_count = Arc::clone(&refresh_count);
        async move {
            let origin = request_origin(&request);
            let url = request_url(&request, &origin);
            match url.path() {
                "/.well-known/oauth-protected-resource/mcp" => {
                    send_json(
                        &response,
                        200,
                        &json!({
                            "resource": format!("{origin}/mcp"),
                            "authorization_servers": [origin.clone()],
                            "scopes_supported": ["org:read"],
                        }),
                    )
                    .await;
                }
                "/.well-known/oauth-authorization-server" => {
                    send_json(
                        &response,
                        200,
                        &json!({
                            "issuer": origin.clone(),
                            "authorization_endpoint": format!("{origin}/authorize"),
                            "token_endpoint": format!("{origin}/token"),
                            "registration_endpoint": format!("{origin}/register"),
                            "response_types_supported": ["code"],
                            "grant_types_supported": ["authorization_code", "refresh_token"],
                            "token_endpoint_auth_methods_supported": ["none"],
                            "code_challenge_methods_supported": ["S256"],
                        }),
                    )
                    .await;
                }
                "/register" => {
                    // 空与 null 的可选字段视为缺省（#10266）：回显里的 `client_secret: ""`。
                    let mut body = object(request.message.clone().unwrap_or_else(|| json!({})));
                    body.insert("client_id".to_owned(), json!("test-client"));
                    body.insert("client_secret".to_owned(), json!(""));
                    send_json(&response, 201, &Value::Object(body)).await;
                }
                "/authorize" => {
                    *lock(&challenge_slot) = query_param(&url, "code_challenge");
                    let redirect_uri = query_param(&url, "redirect_uri").unwrap_or_default();
                    let mut redirect = Url::parse(&redirect_uri).expect("redirect URI");
                    redirect.query_pairs_mut().append_pair("code", "test-code");
                    redirect.query_pairs_mut().append_pair(
                        "state",
                        &query_param(&url, "state").unwrap_or_default(),
                    );
                    response
                        .send(302, &[("location", redirect.as_str())], "")
                        .await;
                }
                "/token" => {
                    let params = form_params(&request.body);
                    if form_param(&params, "grant_type").as_deref() == Some("refresh_token") {
                        refresh_count.fetch_add(1, Ordering::SeqCst);
                        send_json(
                            &response,
                            200,
                            &json!({
                                "access_token": "refreshed-token",
                                "token_type": "Bearer",
                                "refresh_token": "",
                                "expires_in": null,
                            }),
                        )
                        .await;
                    } else {
                        let verifier = form_param(&params, "code_verifier").unwrap_or_default();
                        let challenge = BASE64URL.encode(Sha256::digest(verifier.as_bytes()));
                        let expected = lock(&challenge_slot).clone();
                        if form_param(&params, "code").as_deref() != Some("test-code")
                            || Some(challenge) != expected
                        {
                            send_json(&response, 400, &json!({ "error": "invalid_grant" })).await;
                        } else {
                            send_json(
                                &response,
                                200,
                                &json!({
                                    "access_token": "first-token",
                                    "refresh_token": "refresh-token",
                                    "token_type": "Bearer",
                                    "scope": "",
                                }),
                            )
                            .await;
                        }
                    }
                }
                "/mcp" => {
                    if request.method == "GET" {
                        response.send(405, &[], "").await;
                        return;
                    }
                    if request.method == "DELETE" {
                        response.send(200, &[], "").await;
                        return;
                    }
                    let token = request.header("authorization");
                    if token.as_deref() != Some("Bearer first-token")
                        && token.as_deref() != Some("Bearer refreshed-token")
                    {
                        // 空 scope 会下落到资源元数据的 scopes_supported。
                        let challenge = format!(
                            "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\", scope=\"\""
                        );
                        response
                            .send(
                                401,
                                &[("www-authenticate", challenge.as_str())],
                                "Unauthorized",
                            )
                            .await;
                        return;
                    }
                    let message = request.message.clone().unwrap_or_else(|| json!({}));
                    if message.get("id").is_none() {
                        response.send(202, &[], "").await;
                        return;
                    }
                    let result = if message.get("method").and_then(Value::as_str)
                        == Some("initialize")
                    {
                        json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "oauth-test", "version": "1.0.0" },
                        })
                    } else {
                        json!({ "tools": [{ "name": "issues", "inputSchema": { "type": "object" } }] })
                    };
                    send_json(
                        &response,
                        200,
                        &json!({ "jsonrpc": "2.0", "id": message["id"], "result": result }),
                    )
                    .await;
                }
                _ => {
                    response.send(404, &[], "").await;
                }
            }
        }
    })
    .await;

    let fetch = test_fetch();
    let callback = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
        .await
        .expect("callback server");
    let provider = Arc::new(TestOAuthProvider::new(callback.redirect_url()));
    let server_url = format!("{}/mcp", server.origin());

    let first_client = McpClient::new(McpClientOptions::new("oauth-test", "1.0.0"));
    let transport = StreamableHttpTransport::new(streamable_http_options(
        &server_url,
        adapt_oauth_provider(as_client_provider(&provider)),
        fetch.clone(),
    ))
    .expect("transport");
    let error = first_client
        .connect(Arc::new(transport))
        .await
        .expect_err("a 401 without tokens must ask for authorization");
    assert_eq!(error.name(), "McpOAuthAuthorizationRequiredError");

    let authorization_url = provider.authorization_url().expect("authorization URL");
    assert_eq!(
        query_param(&authorization_url, "scope").as_deref(),
        Some("org:read")
    );
    assert_eq!(
        query_param(&authorization_url, "resource").as_deref(),
        Some(server_url.as_str())
    );

    let callback_result = callback
        .wait_for_callback("expected-state", None)
        .expect("wait for callback");
    let manual = manual_client();
    let authorization_response = manual
        .get(authorization_url.clone())
        .send()
        .await
        .expect("authorize request");
    assert_eq!(authorization_response.status().as_u16(), 302);
    let location = authorization_response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .expect("location header")
        .to_owned();
    manual
        .get(&location)
        .send()
        .await
        .expect("callback request");
    let callback_data = callback_result.await.expect("callback");

    let result = authorize_mcp(
        provider.as_ref(),
        &OAuthFlowOptions {
            server_url: server_url.clone(),
            authorization_code: Some(callback_data.code.clone()),
            fetch: Some(fetch.clone()),
            ..OAuthFlowOptions::default()
        },
    )
    .await
    .expect("authorize");
    assert_eq!(result, OAuthFlowResult::Authorized);

    let client = McpClient::new(McpClientOptions::new("oauth-test", "1.0.0"));
    let transport = StreamableHttpTransport::new(streamable_http_options(
        &server_url,
        adapt_oauth_provider(as_client_provider(&provider)),
        fetch.clone(),
    ))
    .expect("transport");
    client.connect(Arc::new(transport)).await.expect("connect");
    let tools = client
        .list_tools(&McpRequestOptions::new())
        .await
        .expect("list tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "issues");
    assert_eq!(tools[0].input_schema, object(json!({ "type": "object" })));
    client.close().await.expect("close");

    let mut tokens = provider.token_set().expect("tokens");
    tokens.set("access_token", json!("stale-token"));
    provider.set_token_set(tokens);
    let refreshed_client = McpClient::new(McpClientOptions::new("oauth-test", "1.0.0"));
    let transport = StreamableHttpTransport::new(streamable_http_options(
        &server_url,
        adapt_oauth_provider(as_client_provider(&provider)),
        fetch.clone(),
    ))
    .expect("transport");
    refreshed_client
        .connect(Arc::new(transport))
        .await
        .expect("connect with a refresh");
    // 两个令牌响应都不带 scope，授权保留请求的 scope。
    assert_eq!(
        provider.token_set().expect("tokens").to_value(),
        json!({
            "access_token": "refreshed-token",
            "refresh_token": "refresh-token",
            "token_type": "Bearer",
            "scope": "org:read",
        })
    );
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    refreshed_client.close().await.expect("close");
    callback.close().await.expect("close");
}

#[tokio::test]
async fn shares_one_refresh_between_concurrent_401s_when_refresh_tokens_rotate() {
    // `it("shares one refresh between concurrent 401s when refresh tokens rotate")`
    let grants: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let grants_slot = Arc::clone(&grants);
    let server = listen(move |request: HttpRequest, response: ResponseWriter| {
        let grants_slot = Arc::clone(&grants_slot);
        async move {
            let origin = request_origin(&request);
            let url = request_url(&request, &origin);
            match url.path() {
                "/.well-known/oauth-protected-resource/mcp" => {
                    // 无效的资源元数据回落到服务器 origin，而不是让发现失败。
                    send_json(
                        &response,
                        200,
                        &json!({
                            "resource": format!("{origin}/mcp"),
                            "authorization_servers": ["not a url"],
                        }),
                    )
                    .await;
                }
                "/.well-known/oauth-authorization-server" => {
                    send_json(
                        &response,
                        200,
                        &json!({
                            // issuer 不带 URL 解析给回落服务器 URL 加上的尾斜杠。
                            "issuer": origin.clone(),
                            "authorization_endpoint": format!("{origin}/authorize"),
                            "token_endpoint": format!("{origin}/token"),
                            "response_types_supported": ["code"],
                        }),
                    )
                    .await;
                }
                "/token" => {
                    let params = form_params(&request.body);
                    let refresh_token = form_param(&params, "refresh_token").unwrap_or_default();
                    lock(&grants_slot).push(refresh_token.clone());
                    if refresh_token != "r1" {
                        send_json(&response, 400, &json!({ "error": "invalid_grant" })).await;
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    send_json(
                        &response,
                        200,
                        &json!({
                            "access_token": "a2",
                            "refresh_token": "r2",
                            "token_type": "Bearer",
                            "expires_in": 3600,
                        }),
                    )
                    .await;
                }
                _ => {
                    response.send(404, &[], "").await;
                }
            }
        }
    })
    .await;

    let store = Arc::new(MemoryOAuthStateStore::new());
    let provider = Arc::new(
        McpOAuthProvider::new(McpOAuthProviderOptions {
            server_url: format!("{}/mcp", server.origin()),
            redirect_url: "http://127.0.0.1/callback".to_owned(),
            client_metadata: client_metadata(json!({ "client_name": "test" })),
            client_metadata_document: None,
            client_id: Some("client".to_owned()),
            client_secret: None,
            store: Some(Arc::clone(&store) as Arc<dyn McpOAuthStateStore>),
            on_redirect: Arc::new(|_url| Box::pin(async { Ok(()) })),
        })
        .expect("provider"),
    );
    provider
        .save_tokens(&oauth_tokens(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "token_type": "Bearer",
        })))
        .await
        .expect("save tokens");
    let auth = adapt_oauth_provider(Arc::clone(&provider) as Arc<dyn OAuthClientProvider>);
    let fetch = test_fetch();
    let server_url = format!("{}/mcp", server.origin());
    let unauthorized_now = || unauthorized(401, "Bearer", &server_url, fetch.clone(), Some("a1"));
    let (first, second) = tokio::join!(
        auth.on_unauthorized(unauthorized_now()),
        auth.on_unauthorized(unauthorized_now()),
    );
    first.expect("first 401");
    second.expect("second 401");
    // 仍在带旧令牌的请求迟到的 401 不得再刷新一次。
    auth.on_unauthorized(unauthorized_now())
        .await
        .expect("late 401");
    assert_eq!(*lock(&grants), vec!["r1".to_owned()]);
    assert_eq!(auth.token().await.as_deref(), Some("a2"));
    let state = store.load().await.expect("load").expect("state");
    assert_eq!(
        state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.refresh_token())
            .map(str::to_owned),
        Some("r2".to_owned())
    );
    assert!(
        state.tokens_expire_at.expect("tokensExpireAt") > now_ms() + 3_500_000.0,
        "a refresh must record when its token expires"
    );
}

#[tokio::test]
async fn asks_for_authorization_instead_of_refreshing_when_the_server_needs_more_scope() {
    // `it("asks for authorization instead of refreshing when the server needs more scope")`
    let server = listen(
        |request: HttpRequest, response: ResponseWriter| async move {
            let origin = request_origin(&request);
            let url = request_url(&request, &origin);
            if url.path() == "/.well-known/oauth-authorization-server" {
                send_json(
                    &response,
                    200,
                    &json!({
                        "issuer": origin.clone(),
                        "authorization_endpoint": format!("{origin}/authorize"),
                        "token_endpoint": format!("{origin}/token"),
                        "response_types_supported": ["code"],
                    }),
                )
                .await;
            } else {
                let status = if url.path() == "/token" { 500 } else { 404 };
                response.send(status, &[], "").await;
            }
        },
    )
    .await;

    let provider = Arc::new(TestOAuthProvider::new("http://127.0.0.1/callback"));
    provider.set_client(json!({ "client_id": "client" }));
    provider.set_token_set(oauth_tokens(json!({
        "access_token": "a1",
        "refresh_token": "r1",
        "token_type": "Bearer",
        "scope": "repo read:org",
    })));
    let auth = adapt_oauth_provider(as_client_provider(&provider));
    let error = auth
        .on_unauthorized(unauthorized(
            403,
            "Bearer error=\"insufficient_scope\", scope=\"repo admin\"",
            &format!("{}/mcp", server.origin()),
            test_fetch(),
            Some("a1"),
        ))
        .await
        .expect_err("more scope must ask for authorization");
    assert_eq!(error.name(), "McpOAuthAuthorizationRequiredError");
    // challenge 可能只列出缺少的 scope；新的授权保留旧 scope。
    assert_eq!(
        query_param(
            &provider.authorization_url().expect("authorization URL"),
            "scope"
        )
        .as_deref(),
        Some("repo read:org admin")
    );
    // 在用户授权新的 scope 之前，能用的授权保留。
    assert_eq!(
        provider
            .token_set()
            .and_then(|tokens| tokens.access_token().map(str::to_owned)),
        Some("a1".to_owned())
    );
}

#[tokio::test]
async fn binds_persisted_credentials_to_the_exact_mcp_server_url() {
    // `it("binds persisted credentials to the exact MCP server URL")`
    let store = Arc::new(MemoryOAuthStateStore::new());
    let first = McpOAuthProvider::new(McpOAuthProviderOptions {
        server_url: "https://one.example/mcp".to_owned(),
        redirect_url: "http://127.0.0.1/callback".to_owned(),
        client_metadata: client_metadata(json!({ "client_name": "test" })),
        client_metadata_document: None,
        client_id: None,
        client_secret: None,
        store: Some(Arc::clone(&store) as Arc<dyn McpOAuthStateStore>),
        on_redirect: Arc::new(|_url| Box::pin(async { Ok(()) })),
    })
    .expect("provider");
    first
        .save_tokens(&oauth_tokens(json!({
            "access_token": "secret",
            "token_type": "Bearer",
        })))
        .await
        .expect("save tokens");
    assert_eq!(
        first
            .tokens()
            .await
            .expect("tokens")
            .and_then(|tokens| tokens.access_token().map(str::to_owned)),
        Some("secret".to_owned())
    );

    let second = McpOAuthProvider::new(McpOAuthProviderOptions {
        server_url: "https://two.example/mcp".to_owned(),
        redirect_url: "http://127.0.0.1/callback".to_owned(),
        client_metadata: client_metadata(json!({ "client_name": "test" })),
        client_metadata_document: None,
        client_id: None,
        client_secret: None,
        store: Some(Arc::clone(&store) as Arc<dyn McpOAuthStateStore>),
        on_redirect: Arc::new(|_url| Box::pin(async { Ok(()) })),
    })
    .expect("provider");
    assert!(second.tokens().await.expect("tokens").is_none());
}

// #10493
#[tokio::test]
async fn registers_with_an_application_type_derived_from_the_redirect_uris_unless_one_is_set() {
    // `it("registers with an application_type derived from the redirect URIs unless one is set")`
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies_slot = Arc::clone(&bodies);
    let server = listen(move |request: HttpRequest, response: ResponseWriter| {
        let bodies_slot = Arc::clone(&bodies_slot);
        async move {
            let metadata = request.message.clone().unwrap_or_else(|| json!({}));
            lock(&bodies_slot).push(metadata.clone());
            let mut body = object(metadata);
            body.insert("client_id".to_owned(), json!("client"));
            send_json(&response, 201, &Value::Object(body)).await;
        }
    })
    .await;

    async fn register(
        origin: &str,
        redirect_uris: &[&str],
        application_type: Option<&str>,
    ) -> Result<OAuthClientInformation, OAuthFailure> {
        let mut metadata = json!({ "redirect_uris": redirect_uris });
        if let Some(application_type) = application_type {
            metadata
                .as_object_mut()
                .expect("object")
                .insert("application_type".to_owned(), json!(application_type));
        }
        register_client(
            origin,
            RegisterClientOptions {
                client_metadata: client_metadata(metadata),
                ..RegisterClientOptions::default()
            },
        )
        .await
    }

    register(server.origin(), &["http://127.0.0.1:1234/callback"], None)
        .await
        .expect("register");
    register(server.origin(), &["http://[::1]/callback"], None)
        .await
        .expect("register");
    register(server.origin(), &["com.example.app:/callback"], None)
        .await
        .expect("register");
    register(server.origin(), &["https://app.example/callback"], None)
        .await
        .expect("register");
    register(server.origin(), &["http://localhost/callback"], Some("web"))
        .await
        .expect("register");
    let seen: Vec<Value> = lock(&bodies)
        .iter()
        .map(|body| body.get("application_type").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        seen,
        vec![
            json!("native"),
            json!("native"),
            json!("native"),
            json!("web"),
            json!("web")
        ]
    );
}

#[tokio::test]
async fn rejects_authorization_metadata_whose_issuer_does_not_match_discovery() {
    // `it("rejects authorization metadata whose issuer does not match discovery")`
    let server = listen(
        |request: HttpRequest, response: ResponseWriter| async move {
            let origin = request_origin(&request);
            let url = request_url(&request, &origin);
            if url.path() == "/.well-known/oauth-authorization-server" {
                send_json(
                    &response,
                    200,
                    &json!({
                        "issuer": "https://attacker.example",
                        "authorization_endpoint": format!("{origin}/authorize"),
                        "token_endpoint": format!("{origin}/token"),
                        "response_types_supported": ["code"],
                    }),
                )
                .await;
            } else {
                response.send(404, &[], "").await;
            }
        },
    )
    .await;
    let error = discover_authorization_server_metadata(
        server.origin(),
        &DiscoverAuthorizationServerOptions {
            fetch: Some(test_fetch()),
            ..DiscoverAuthorizationServerOptions::default()
        },
    )
    .await
    .expect_err("issuer mismatch");
    assert!(
        error.as_issuer_mismatch().is_some(),
        "expected an issuer mismatch, got {error:?}"
    );
}

#[tokio::test]
async fn rejects_non_http_authorization_endpoints_which_the_platform_url_handler_would_launch() {
    // `it("rejects non-HTTP authorization endpoints, which the platform URL handler would launch")`
    let endpoint: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let endpoint_slot = Arc::clone(&endpoint);
    let server = listen(move |request: HttpRequest, response: ResponseWriter| {
        let endpoint_slot = Arc::clone(&endpoint_slot);
        async move {
            let origin = request_origin(&request);
            let endpoint = lock(&endpoint_slot).clone();
            send_json(
                &response,
                200,
                &json!({
                    "issuer": origin.clone(),
                    "authorization_endpoint": endpoint,
                    "token_endpoint": format!("{origin}/token"),
                    "response_types_supported": ["code"],
                }),
            )
            .await;
        }
    })
    .await;
    for value in [
        "file://attacker.example/share/x.exe",
        "search-ms:query=x",
        "ms-msdt:/id",
    ] {
        *lock(&endpoint) = value.to_owned();
        let error = discover_authorization_server_metadata(
            server.origin(),
            &DiscoverAuthorizationServerOptions {
                fetch: Some(test_fetch()),
                ..DiscoverAuthorizationServerOptions::default()
            },
        )
        .await
        .expect_err("non-HTTP endpoint");
        assert!(
            error.message().contains("Invalid authorization endpoint"),
            "unexpected message: {}",
            error.message()
        );
        let metadata = authorization_server_metadata(json!({
            "issuer": server.origin(),
            "authorization_endpoint": value,
            "token_endpoint": format!("{}/token", server.origin()),
            "response_types_supported": ["code"],
        }));
        let error = start_authorization(
            server.origin(),
            StartAuthorizationOptions {
                metadata: Some(metadata),
                client_information: client_information(json!({ "client_id": "client" })),
                redirect_url: "http://127.0.0.1/callback".to_owned(),
                ..StartAuthorizationOptions::default()
            },
        )
        .await
        .expect_err("non-HTTP endpoint");
        assert!(
            error.message().contains("Invalid authorization endpoint"),
            "unexpected message: {}",
            error.message()
        );
    }
}

// #10172
#[tokio::test]
async fn uses_a_configured_authorization_server_metadata_document_as_is() {
    // `it("uses a configured authorization server metadata document as is")`
    let server = listen(
        |request: HttpRequest, response: ResponseWriter| async move {
            let origin = request_origin(&request);
            let url = request_url(&request, &origin);
            if url.path() == "/.well-known/oauth-protected-resource/mcp" {
                // 指向 MCP 服务器自己，它没有授权服务器元数据。
                send_json(
                    &response,
                    200,
                    &json!({
                        "resource": format!("{origin}/mcp"),
                        "authorization_servers": [origin.clone()],
                    }),
                )
                .await;
            } else if url.path() == "/idp/metadata.json" {
                send_json(
                    &response,
                    200,
                    &json!({
                        // 无法从文档 URL 推导；配置的文档不做校验。
                        "issuer": "https://idp.example",
                        "authorization_endpoint": format!("{origin}/idp/authorize"),
                        "token_endpoint": format!("{origin}/idp/token"),
                        "response_types_supported": ["code"],
                    }),
                )
                .await;
            } else {
                response.send(404, &[], "").await;
            }
        },
    )
    .await;
    let provider = Arc::new(TestOAuthProvider::new("http://127.0.0.1/callback"));
    provider.set_client(json!({ "client_id": "client" }));
    let options = OAuthFlowOptions {
        server_url: format!("{}/mcp", server.origin()),
        authorization_server_metadata_url: Some(
            Url::parse(&format!("{}/idp/metadata.json", server.origin())).expect("metadata URL"),
        ),
        fetch: Some(test_fetch()),
        ..OAuthFlowOptions::default()
    };
    assert_eq!(
        authorize_mcp(provider.as_ref(), &options)
            .await
            .expect("authorize"),
        OAuthFlowResult::Redirect
    );
    let authorization_url = provider.authorization_url().expect("authorization URL");
    assert_eq!(
        format!(
            "{}://{}{}",
            authorization_url.scheme(),
            authorization_url.authority(),
            authorization_url.path()
        ),
        format!("{}/idp/authorize", server.origin())
    );
    assert_eq!(
        query_param(&authorization_url, "resource").as_deref(),
        Some(format!("{}/mcp", server.origin()).as_str())
    );

    let insecure = OAuthFlowOptions {
        authorization_server_metadata_url: Some(
            Url::parse("http://idp.example/metadata.json").expect("metadata URL"),
        ),
        ..options
    };
    let error = authorize_mcp(provider.as_ref(), &insecure)
        .await
        .expect_err("insecure metadata URL");
    assert!(
        error.as_insecure_endpoint().is_some(),
        "expected an insecure endpoint error, got {error:?}"
    );
}

#[tokio::test]
async fn exchanges_a_code_only_when_its_iss_parameter_names_the_authorization_server() {
    // `it("exchanges a code only when its iss parameter names the authorization server")`
    let codes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let codes_slot = Arc::clone(&codes);
    let server = listen(move |request: HttpRequest, response: ResponseWriter| {
        let codes_slot = Arc::clone(&codes_slot);
        async move {
            if request.method != "POST" {
                response.send(404, &[], "").await;
                return;
            }
            let params = form_params(&request.body);
            lock(&codes_slot).push(form_param(&params, "code").unwrap_or_default());
            send_json(
                &response,
                200,
                &json!({ "access_token": "token", "token_type": "Bearer" }),
            )
            .await;
        }
    })
    .await;
    let origin = server.origin().to_owned();

    async fn exchange(
        origin: &str,
        code: &str,
        iss: Option<&str>,
        iss_parameter_supported: bool,
        metadata: bool,
    ) -> Result<OAuthFlowResult, OAuthFailure> {
        let provider = TestOAuthProvider::new("http://127.0.0.1/callback");
        provider.set_client(json!({ "client_id": "client" }));
        provider.set_verifier("verifier");
        provider.set_discovery(OAuthDiscoveryState {
            authorization_server_url: origin.to_owned(),
            authorization_server_metadata: metadata.then(|| {
                parse_authorization_server_metadata(&json!({
                    "issuer": origin,
                    "authorization_endpoint": format!("{origin}/authorize"),
                    "token_endpoint": format!("{origin}/token"),
                    "response_types_supported": ["code"],
                    "authorization_response_iss_parameter_supported": iss_parameter_supported,
                }))
                .expect("metadata")
            }),
            ..OAuthDiscoveryState::default()
        });
        authorize_mcp(
            &provider,
            &OAuthFlowOptions {
                server_url: format!("{origin}/mcp"),
                authorization_code: Some(code.to_owned()),
                iss: iss.map(str::to_owned),
                fetch: Some(test_fetch()),
                ..OAuthFlowOptions::default()
            },
        )
        .await
    }

    assert!(
        exchange(
            &origin,
            "other",
            Some("https://attacker.example"),
            false,
            true
        )
        .await
        .expect_err("foreign iss")
        .as_issuer_mismatch()
        .is_some()
    );
    assert!(
        exchange(&origin, "missing", None, true, true)
            .await
            .expect_err("missing iss")
            .as_issuer_mismatch()
            .is_some()
    );
    assert_eq!(
        exchange(&origin, "matching", Some(&origin), true, true)
            .await
            .expect("matching iss"),
        OAuthFlowResult::Authorized
    );
    // 不承诺该参数的服务器可以省略它。
    assert_eq!(
        exchange(&origin, "omitted", None, false, true)
            .await
            .expect("omitted iss"),
        OAuthFlowResult::Authorized
    );
    // 没有元数据就没有可比的 issuer，服务器不能靠隐藏它绕过检查。
    assert!(
        exchange(
            &origin,
            "unverifiable",
            Some("https://attacker.example"),
            false,
            false
        )
        .await
        .expect_err("unverifiable iss")
        .as_issuer_mismatch()
        .is_some()
    );
    assert_eq!(
        exchange(&origin, "no-metadata", None, false, false)
            .await
            .expect("no metadata"),
        OAuthFlowResult::Authorized
    );
    assert_eq!(
        *lock(&codes),
        vec![
            "matching".to_owned(),
            "omitted".to_owned(),
            "no-metadata".to_owned()
        ]
    );
}

// #10565
#[tokio::test]
async fn stops_when_its_signal_aborts_without_falling_back_to_a_redirect() {
    // `it("stops when its signal aborts, without falling back to a redirect")`
    let stalled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stalled_slot = Arc::clone(&stalled);
    // 接受每个请求但从不回答。
    let server = listen(move |request: HttpRequest, response: ResponseWriter| {
        let stalled_slot = Arc::clone(&stalled_slot);
        async move {
            let _response = response;
            let origin = request_origin(&request);
            let url = request_url(&request, &origin);
            lock(&stalled_slot).push(url.path().to_owned());
            // 永远挂起（连接保持打开）。
            std::future::pending::<()>().await;
        }
    })
    .await;
    let server_url = format!("{}/mcp", server.origin());

    async fn run(
        server_url: &str,
        stalled: &Arc<Mutex<Vec<String>>>,
        provider: &TestOAuthProvider,
    ) -> String {
        let controller = CancellationToken::new();
        let count = lock(stalled).len();
        let options = OAuthFlowOptions {
            server_url: server_url.to_owned(),
            signal: Some(controller.clone()),
            fetch: Some(test_fetch()),
            ..OAuthFlowOptions::default()
        };
        let mut flow = Box::pin(authorize_mcp(provider, &options));
        loop {
            tokio::select! {
                result = &mut flow => panic!("the flow finished before the abort: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(2)) => {
                    if lock(stalled).len() > count {
                        break;
                    }
                }
            }
        }
        controller.cancel();
        let error = flow.await.expect_err("the flow must abort");
        assert_eq!(error.name(), "AbortError");
        assert!(provider.authorization_url().is_none());
        lock(stalled)
            .last()
            .cloned()
            .expect("a stalled request was recorded")
    }

    // 发现阶段。
    let discovery_provider = TestOAuthProvider::new("http://127.0.0.1/callback");
    assert_eq!(
        run(&server_url, &stalled, &discovery_provider).await,
        "/.well-known/oauth-protected-resource/mcp"
    );

    // 失败的刷新本会回落到新的授权。
    let origin = server.origin().to_owned();
    let refreshing = TestOAuthProvider::new("http://127.0.0.1/callback");
    refreshing.set_client(json!({ "client_id": "client" }));
    refreshing.set_token_set(oauth_tokens(json!({
        "access_token": "a1",
        "refresh_token": "r1",
        "token_type": "Bearer",
    })));
    refreshing.set_discovery(OAuthDiscoveryState {
        authorization_server_url: origin.clone(),
        authorization_server_metadata: Some(
            parse_authorization_server_metadata(&json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "response_types_supported": ["code"],
            }))
            .expect("metadata"),
        ),
        ..OAuthDiscoveryState::default()
    });
    assert_eq!(run(&server_url, &stalled, &refreshing).await, "/token");
}

// ---------------------------------------------------------------------------
// describe("OAuthCallbackServer pages")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn renders_plain_text_by_default() {
    // `it("renders plain text by default")`
    let callback = OAuthCallbackServer::listen(OAuthCallbackServerOptions::default())
        .await
        .expect("callback server");
    let pending = callback
        .wait_for_callback("s1", None)
        .expect("wait for callback");
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client")
        .get(format!("{}?code=abc&state=s1", callback.redirect_url()))
        .send()
        .await
        .expect("callback request");
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        response.text().await.expect("body"),
        "Authorization complete. You may close this window."
    );
    assert_eq!(pending.await.expect("callback").code, "abc");
    callback.close().await.expect("close");
}

// #10302
#[tokio::test]
async fn rejects_a_response_on_another_path_than_the_expected_one() {
    // `it("rejects a response on another path than the expected one")`
    let callback = OAuthCallbackServer::listen(OAuthCallbackServerOptions {
        extra_paths: vec!["/callback/server-id".to_owned()],
        ..OAuthCallbackServerOptions::default()
    })
    .await
    .expect("callback server");
    let origin = Url::parse(callback.redirect_url())
        .expect("redirect URL")
        .origin()
        .ascii_serialization();
    let mixed_up = callback
        .wait_for_callback("s1", Some("/callback/server-id".to_owned()))
        .expect("wait for callback");
    let wrong = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client")
        .get(format!("{origin}/callback?code=abc&state=s1"))
        .send()
        .await
        .expect("callback request");
    assert_eq!(wrong.status().as_u16(), 400);
    let error = mixed_up.await.expect_err("another path");
    assert!(
        error.message().contains("arrived on another redirect URI"),
        "unexpected message: {}",
        error.message()
    );

    let pending = callback
        .wait_for_callback("s2", Some("/callback/server-id".to_owned()))
        .expect("wait for callback");
    let right = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client")
        .get(format!("{origin}/callback/server-id?code=abc&state=s2"))
        .send()
        .await
        .expect("callback request");
    assert_eq!(right.status().as_u16(), 200);
    assert_eq!(pending.await.expect("callback").code, "abc");
    callback.close().await.expect("close");
}

#[tokio::test]
async fn renders_pages_through_render_page() {
    // `it("renders pages through renderPage")`
    let pages: Arc<Mutex<Vec<OAuthCallbackPage>>> = Arc::new(Mutex::new(Vec::new()));
    let pages_slot = Arc::clone(&pages);
    let callback = OAuthCallbackServer::listen(OAuthCallbackServerOptions {
        render_page: Some(Arc::new(move |page: &OAuthCallbackPage| {
            lock(&pages_slot).push(page.clone());
            match page {
                OAuthCallbackPage::Success => "<p>ok</p>".to_owned(),
                OAuthCallbackPage::Failure { message, .. } => format!("<p>{message}</p>"),
            }
        })),
        ..OAuthCallbackServerOptions::default()
    })
    .await
    .expect("callback server");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client");

    let denied = callback
        .wait_for_callback("s1", None)
        .expect("wait for callback");
    let failure = client
        .get(format!(
            "{}?error=access_denied&error_description=Denied&state=s1",
            callback.redirect_url()
        ))
        .send()
        .await
        .expect("callback request");
    assert_eq!(
        failure
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let error = denied.await.expect_err("denied");
    assert_eq!(error.message(), "Denied");
    assert_eq!(
        lock(&pages).last().cloned(),
        Some(OAuthCallbackPage::Failure {
            message: "Authorization failed. You may close this window.".to_owned(),
            details: Some("Denied".to_owned()),
        })
    );

    let pending = callback
        .wait_for_callback("s2", None)
        .expect("wait for callback");
    let success = client
        .get(format!("{}?code=abc&state=s2", callback.redirect_url()))
        .send()
        .await
        .expect("callback request");
    assert_eq!(success.text().await.expect("body"), "<p>ok</p>");
    assert_eq!(pending.await.expect("callback").code, "abc");
    callback.close().await.expect("close");
}
