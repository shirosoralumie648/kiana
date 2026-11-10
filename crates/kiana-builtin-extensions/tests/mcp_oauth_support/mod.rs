//! `packages/coding-agent/test/suite/mcp-oauth-server.ts` 的移植：受 OAuth 保护的 MCP 服务器，
//! 自带授权服务器（发现、DCR、PKCE、刷新）。
//!
//! 与 TS 版一样是一个真实的回环 HTTP/1.1 服务器（`tokio::net::TcpListener` + 手写报文解析）。
//! `iss` 作为授权响应的 `iss` 参数（RFC 9207）；`iss_parameter` 声明该参数并发送服务器自己的
//! issuer；`cimd` 声明 Client ID Metadata Documents；`redirect_path` 替换 redirect URI 的路径
//! （模拟串台的授权服务器）。`stall` 里的路径接收请求但永不回答（无响应的服务器）。

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::{Value, json};
use sha2::Digest as _;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

/// `startOAuthMcpServer(options)` 的参数。
#[derive(Debug, Clone, Default)]
pub struct OAuthServerOptions {
    /// 授权响应的 `iss`（覆盖默认）。
    pub iss: Option<String>,
    /// 声明并发送 `iss` 参数。
    pub iss_parameter: bool,
    /// 声明 Client ID Metadata Documents。
    pub cimd: bool,
    /// 替换 redirect URI 的路径。
    pub redirect_path: Option<String>,
}

/// 一个响应。
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn json(status: u16, body: Value) -> Self {
        Response {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: body.to_string(),
        }
    }

    fn empty(status: u16) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    fn redirect(location: &str) -> Self {
        Response {
            status: 302,
            headers: vec![("location".to_string(), location.to_string())],
            body: String::new(),
        }
    }
}

#[derive(Default)]
struct ServerState {
    issued: u64,
    valid_tokens: HashSet<String>,
    refresh_tokens: HashSet<String>,
    /// code → code_challenge。
    challenges: HashMap<String, String>,
}

/// 共享的服务器数据。
struct Shared {
    options: OAuthServerOptions,
    origin: Mutex<String>,
    log: Mutex<Vec<String>>,
    registrations: Mutex<Vec<Value>>,
    authorizations: Mutex<Vec<IndexMap<String, String>>>,
    token_requests: Mutex<Vec<IndexMap<String, String>>>,
    deletes: Mutex<Vec<Option<String>>>,
    stall: Mutex<HashSet<String>>,
    stalled: Mutex<Vec<String>>,
    state: Mutex<ServerState>,
}

/// 一个运行中的 OAuth MCP 测试服务器。
pub struct OAuthTestServer {
    pub url: String,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept_task: Option<JoinHandle<()>>,
}

impl OAuthTestServer {
    /// `startOAuthMcpServer()`。
    pub async fn start(options: OAuthServerOptions) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let shared = Arc::new(Shared {
            options,
            origin: Mutex::new(origin.clone()),
            log: Mutex::new(Vec::new()),
            registrations: Mutex::new(Vec::new()),
            authorizations: Mutex::new(Vec::new()),
            token_requests: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            stall: Mutex::new(HashSet::new()),
            stalled: Mutex::new(Vec::new()),
            state: Mutex::new(ServerState::default()),
        });
        let cancel = CancellationToken::new();
        let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let accept_task = {
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            let connections = Arc::clone(&connections);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break };
                            let shared = Arc::clone(&shared);
                            let cancel = cancel.clone();
                            let handle = tokio::spawn(async move {
                                handle_connection(stream, shared, cancel).await;
                            });
                            connections.lock().unwrap().push(handle);
                        }
                    }
                }
            })
        };
        OAuthTestServer {
            url: format!("{origin}/mcp"),
            shared,
            cancel,
            connections,
            accept_task: Some(accept_task),
        }
    }

    /// `server.log`。
    pub fn log(&self) -> Vec<String> {
        self.shared.log.lock().unwrap().clone()
    }

    /// `server.registrations`。
    pub fn registrations(&self) -> Vec<Value> {
        self.shared.registrations.lock().unwrap().clone()
    }

    /// `server.authorizations`。
    pub fn authorizations(&self) -> Vec<IndexMap<String, String>> {
        self.shared.authorizations.lock().unwrap().clone()
    }

    /// `server.tokenRequests`。
    pub fn token_requests(&self) -> Vec<IndexMap<String, String>> {
        self.shared.token_requests.lock().unwrap().clone()
    }

    /// `server.deletes`。
    pub fn deletes(&self) -> Vec<Option<String>> {
        self.shared.deletes.lock().unwrap().clone()
    }

    /// `server.stall.add(path)`。
    pub fn stall(&self, path: &str) {
        self.shared.stall.lock().unwrap().insert(path.to_string());
    }

    /// `server.stalled`：停在无响应路径上的请求。
    pub fn stalled(&self) -> Vec<String> {
        self.shared.stalled.lock().unwrap().clone()
    }

    /// `server.expireAccessTokens()`。
    pub fn expire_access_tokens(&self) {
        self.shared.state.lock().unwrap().valid_tokens.clear();
    }

    /// `server.close()`：断开所有连接并停止监听。
    pub async fn close(&self) {
        self.cancel.cancel();
        for handle in self.connections.lock().unwrap().drain(..) {
            handle.abort();
        }
    }
}

impl Drop for OAuthTestServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.accept_task.take() {
            handle.abort();
        }
    }
}

async fn handle_connection(stream: TcpStream, shared: Arc<Shared>, cancel: CancellationToken) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body).await;
    let body = String::from_utf8_lossy(&body).into_owned();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.clone(), String::new()),
    };
    if shared.stall.lock().unwrap().contains(&path) {
        shared.stalled.lock().unwrap().push(path);
        // 接收请求但永不回答；连接保持到服务器关闭。
        cancel.cancelled().await;
        return;
    }
    let response = route(&shared, &method, &path, &query, &headers, &body).await;
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    // 每个响应后关闭连接（reqwest/undici 都能处理；测试服务器因此无需 keep-alive）。
    head.push_str(&format!("content-length: {}\r\n", response.body.len()));
    head.push_str("connection: close\r\n\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(response.body.as_bytes());
    let _ = write.write_all(&bytes).await;
    let _ = write.flush().await;
    let _ = write.shutdown().await;
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    }
}

fn form_values(text: &str) -> IndexMap<String, String> {
    let mut values = IndexMap::new();
    for (key, value) in url::form_urlencoded::parse(text.as_bytes()) {
        values
            .entry(key.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    values
}

async fn route(
    shared: &Arc<Shared>,
    method: &str,
    path: &str,
    query: &str,
    headers: &HashMap<String, String>,
    body: &str,
) -> Response {
    let origin = shared.origin.lock().unwrap().clone();
    match path {
        "/mcp" => handle_mcp(shared, method, headers, body, &origin),
        "/.well-known/oauth-protected-resource/mcp" => Response::json(
            200,
            json!({"resource": format!("{origin}/mcp"), "authorization_servers": [origin]}),
        ),
        "/.well-known/oauth-authorization-server" => {
            let mut metadata = json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "registration_endpoint": format!("{origin}/register"),
                "response_types_supported": ["code"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            });
            if shared.options.cimd {
                metadata["client_id_metadata_document_supported"] = json!(true);
            }
            if shared.options.iss_parameter {
                metadata["authorization_response_iss_parameter_supported"] = json!(true);
            }
            Response::json(200, metadata)
        }
        "/register" => {
            let metadata: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            shared.log.lock().unwrap().push("register".to_string());
            shared.registrations.lock().unwrap().push(metadata.clone());
            let mut response = metadata.as_object().cloned().unwrap_or_default();
            response.insert("client_id".to_string(), json!("client-1"));
            Response::json(201, Value::Object(response))
        }
        "/authorize" => {
            let parameters = form_values(query);
            shared
                .authorizations
                .lock()
                .unwrap()
                .push(parameters.clone());
            let mut state = shared.state.lock().unwrap();
            let code = format!("code-{}", state.challenges.len() + 1);
            state.challenges.insert(
                code.clone(),
                parameters
                    .get("code_challenge")
                    .cloned()
                    .unwrap_or_default(),
            );
            drop(state);
            let redirect_uri = parameters.get("redirect_uri").cloned().unwrap_or_default();
            let Ok(mut redirect) = Url::parse(&redirect_uri) else {
                return Response::empty(400);
            };
            if let Some(redirect_path) = &shared.options.redirect_path {
                redirect.set_path(redirect_path);
            }
            let mut pairs = redirect.query_pairs_mut();
            pairs.append_pair("code", &code);
            pairs.append_pair(
                "state",
                parameters.get("state").map(String::as_str).unwrap_or(""),
            );
            let iss = shared
                .options
                .iss
                .clone()
                .or_else(|| shared.options.iss_parameter.then(|| origin.clone()));
            if let Some(iss) = iss {
                pairs.append_pair("iss", &iss);
            }
            drop(pairs);
            Response::redirect(redirect.as_str())
        }
        "/token" => {
            let parameters = form_values(body);
            shared
                .token_requests
                .lock()
                .unwrap()
                .push(parameters.clone());
            let mut state = shared.state.lock().unwrap();
            if parameters.get("grant_type").map(String::as_str) == Some("authorization_code") {
                let code = parameters.get("code").cloned().unwrap_or_default();
                let verifier = parameters.get("code_verifier").cloned().unwrap_or_default();
                let computed = kiana_ai::auth::oauth::pkce::base64url_encode(
                    &sha2::Sha256::digest(verifier.as_bytes()),
                );
                let expected = state.challenges.remove(&code);
                if expected.as_deref() != Some(computed.as_str()) {
                    return Response::json(400, json!({"error": "invalid_grant"}));
                }
                shared.log.lock().unwrap().push("token code".to_string());
                return Response::json(200, issue_tokens(&mut state));
            }
            let refresh = parameters.get("refresh_token").cloned().unwrap_or_default();
            if !state.refresh_tokens.remove(&refresh) {
                return Response::json(400, json!({"error": "invalid_grant"}));
            }
            shared.log.lock().unwrap().push("token refresh".to_string());
            Response::json(200, issue_tokens(&mut state))
        }
        _ => Response::empty(404),
    }
}

fn issue_tokens(state: &mut ServerState) -> Value {
    state.issued += 1;
    let access = format!("access-{}", state.issued);
    let refresh = format!("refresh-{}", state.issued);
    state.valid_tokens.insert(access.clone());
    state.refresh_tokens.insert(refresh.clone());
    json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 3600,
    })
}

fn handle_mcp(
    shared: &Arc<Shared>,
    method: &str,
    headers: &HashMap<String, String>,
    body: &str,
    origin: &str,
) -> Response {
    let token = headers
        .get("authorization")
        .map(|value| value.strip_prefix("Bearer ").unwrap_or(value).to_string());
    if method == "DELETE" {
        shared.deletes.lock().unwrap().push(token.clone());
    }
    if method != "POST" {
        return Response::empty(if method == "GET" { 405 } else { 200 });
    }
    let valid = token
        .as_ref()
        .is_some_and(|token| shared.state.lock().unwrap().valid_tokens.contains(token));
    if !valid {
        shared
            .log
            .lock()
            .unwrap()
            .push(format!("401 {}", token.as_deref().unwrap_or("none")));
        return Response {
            status: 401,
            headers: vec![(
                "www-authenticate".to_string(),
                format!(
                    "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource/mcp\""
                ),
            )],
            body: String::new(),
        };
    }
    let Ok(message) = serde_json::from_str::<Value>(body) else {
        return Response::empty(400);
    };
    if message.get("id").is_none() {
        return Response::empty(202);
    }
    let method_name = message.get("method").and_then(Value::as_str).unwrap_or("");
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let result = match method_name {
        "initialize" => json!({
            "protocolVersion": kiana_mcp::LATEST_PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "issues", "version": "1.0.0"},
        }),
        "tools/list" => json!({
            "tools": [{"name": "whoami", "inputSchema": {"type": "object", "properties": {}}}],
        }),
        "tools/call" => {
            shared
                .log
                .lock()
                .unwrap()
                .push(format!("call {}", token.as_deref().unwrap_or("none")));
            json!({"content": [{"type": "text", "text": format!("token {}", token.as_deref().unwrap_or("none"))}]})
        }
        _ => json!({}),
    };
    Response {
        status: 200,
        headers: vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("mcp-session-id".to_string(), "session-1".to_string()),
        ],
        body: json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
    }
}
