//! `packages/evals/evals/acme-server.ts` 的移植（测试支持，不属于 kiana-evals 公开 API）。
//!
//! 规范 §8.1：`kiana-evals-fixtures` **未采纳**为 crate，acme-server / configured-runtime
//! 保留为 kiana-evals 内的 test fixture。这里保持 fixture 常量、请求校验顺序、状态码、
//! 响应头与响应体（错误体是 `text/plain`、无换行）。
//!
//! 两种模式：
//! - `OpenAI`：`POST /v1/chat/completions`，OpenAI-compatible SSE（两个 data 块 + `[DONE]`）。
//! - `Stream`：`POST /generate`，四行 NDJSON（文本拆成 `ACME_` + `STREAM_OK`）。
//!
//! TS 源：`packages/evals/evals/acme-server.ts`（154 行）。

// 本模块被 `tests/acme_server.rs` 与 `tests/configured_runtime.rs` 两个测试二进制分别以
// `#[path]` 引入；每个二进制只使用 TS fixture 导出面的一个子集（例如 `STREAM_API_DOCUMENTATION`
// 只被未移植的 custom-provider eval 使用），因此按二进制编译时会出现“未使用”项。
#![allow(dead_code)]

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use http_body_util::BodyExt as _;
use hyper::body::{Bytes, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// TS: `OPENAI_PROVIDER_ID`。
pub const OPENAI_PROVIDER_ID: &str = "acme";
/// TS: `OPENAI_MODEL_ID`。
pub const OPENAI_MODEL_ID: &str = "acme-chat";
/// TS: `OPENAI_PROBE_PROMPT`。
pub const OPENAI_PROBE_PROMPT: &str = "Reply with ACME_OK.";
/// TS: `OPENAI_PROBE_RESPONSE`。
pub const OPENAI_PROBE_RESPONSE: &str = "ACME_OK";
/// TS: `STREAM_PROVIDER_ID`。
pub const STREAM_PROVIDER_ID: &str = "acme-stream";
/// TS: `STREAM_MODEL_ID`。
pub const STREAM_MODEL_ID: &str = "acme-stream-chat";
/// TS: `STREAM_PROBE_PROMPT`。
pub const STREAM_PROBE_PROMPT: &str = "Reply with ACME_STREAM_OK.";
/// TS: `STREAM_PROBE_RESPONSE`。
pub const STREAM_PROBE_RESPONSE: &str = "ACME_STREAM_OK";

/// TS: `STREAM_API_DOCUMENTATION`（`JSON.stringify(..., null, 2)` + 末尾换行）。
///
/// 注意文档里的 usage 是 3/2，实际响应是 4/3（故意不同）。
pub const STREAM_API_DOCUMENTATION: &str = r#"{
  "name": "Acme Streaming API",
  "request": {
    "method": "POST",
    "path": "/generate",
    "headers": {
      "content-type": "application/json",
      "x-acme-key": "resolved credential"
    },
    "body": {
      "model": "acme-stream-chat",
      "messages": [
        {
          "role": "user",
          "content": "Hello"
        }
      ],
      "stream": true
    }
  },
  "response": {
    "contentType": "application/x-ndjson",
    "events": [
      {
        "type": "text_delta",
        "text": "Hello"
      },
      {
        "type": "usage",
        "input_tokens": 3,
        "output_tokens": 2
      },
      {
        "type": "done",
        "reason": "stop"
      }
    ]
  }
}
"#;

/// fixture 服务器模式（TS 的内部类型 `ServerMode`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerMode {
    /// OpenAI-compatible `POST /v1/chat/completions`。
    Openai,
    /// Acme Stream `POST /generate`。
    Stream,
}

/// fixture 服务器错误（`Display` 为错误文本）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcmeServerError(pub String);

impl std::fmt::Display for AcmeServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AcmeServerError {}

#[derive(Default)]
struct ServerState {
    received_valid_request: AtomicBool,
}

struct ServerInner {
    origin: String,
    task: Option<JoinHandle<()>>,
    shutdown: Option<CancellationToken>,
}

/// TS: `AcmeServer`。
pub struct AcmeServer {
    mode: ServerMode,
    state: Arc<ServerState>,
    inner: Mutex<ServerInner>,
}

impl std::fmt::Debug for AcmeServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcmeServer")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// TS: `createAcmeServer(mode)`。
pub fn create_acme_server(mode: ServerMode) -> AcmeServer {
    AcmeServer::new(mode)
}

impl AcmeServer {
    /// 新建一个未启动的 fixture 服务器。
    pub fn new(mode: ServerMode) -> Self {
        AcmeServer {
            mode,
            state: Arc::new(ServerState::default()),
            inner: Mutex::new(ServerInner {
                origin: String::new(),
                task: None,
                shutdown: None,
            }),
        }
    }

    /// TS: `start()`：监听 `127.0.0.1:0`。
    pub async fn start(&self) -> Result<(), AcmeServerError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| AcmeServerError(node_listen_error(&error)))?;
        let address = listener
            .local_addr()
            .map_err(|_| AcmeServerError("Acme fixture server did not bind a TCP port.".into()))?;
        let origin = format!("http://127.0.0.1:{}", address.port());
        let shutdown = CancellationToken::new();
        let state = Arc::clone(&self.state);
        let mode = self.mode;
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            accept_loop(listener, state, mode, task_shutdown).await;
        });
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.origin = origin;
        inner.task = Some(task);
        inner.shutdown = Some(shutdown);
        Ok(())
    }

    /// TS: `stop()`：`await server.close()`。
    ///
    /// Node 的 `close()` 停止接受新连接，关闭空闲 keep-alive 连接，并等待在途请求结束后
    /// 才回调；回调错误（这里只可能是 accept 任务失败）向上传播。重复调用是 no-op。
    pub async fn stop(&self) -> Result<(), AcmeServerError> {
        let (task, shutdown) = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            (inner.task.take(), inner.shutdown.take())
        };
        if let Some(shutdown) = shutdown {
            shutdown.cancel();
        }
        match task {
            Some(task) => task
                .await
                .map_err(|error| AcmeServerError(error.to_string())),
            None => Ok(()),
        }
    }

    /// TS: `reset()`：只清 probe 标志。
    pub fn reset(&self) {
        self.state
            .received_valid_request
            .store(false, Ordering::SeqCst);
    }

    /// TS: `origin()`。
    pub fn origin(&self) -> String {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .origin
            .clone()
    }

    /// TS: `baseUrl()`：两种模式下都恒为 `${origin}/v1`。
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.origin())
    }

    /// TS: `validRequestReceived()`。
    pub fn valid_request_received(&self) -> bool {
        self.state.received_valid_request.load(Ordering::SeqCst)
    }
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<ServerState>,
    mode: ServerMode,
    shutdown: CancellationToken,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let state = Arc::clone(&state);
                let connection_shutdown = shutdown.clone();
                connections.spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |request| {
                        let state = Arc::clone(&state);
                        async move { Ok::<_, Infallible>(handle_request(request, state, mode).await) }
                    });
                    let connection = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = connection.as_mut() => {}
                        _ = connection_shutdown.cancelled() => {
                            // Node 的 `server.close()`：空闲连接立即关闭，在途请求完成后关闭。
                            connection.as_mut().graceful_shutdown();
                            let _ = connection.await;
                        }
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    // 等待所有连接优雅结束（`JoinSet::shutdown` 会直接 abort，与 Node 的 close 不同）。
    while connections.join_next().await.is_some() {}
}

/// Node `listen` 失败时的错误文本（`Error: listen EADDRINUSE: address already in use ...`）。
fn node_listen_error(error: &std::io::Error) -> String {
    let detail = match error.kind() {
        std::io::ErrorKind::AddrInUse => Some("EADDRINUSE: address already in use 127.0.0.1:0"),
        std::io::ErrorKind::PermissionDenied => Some("EACCES: permission denied 127.0.0.1:0"),
        _ => None,
    };
    match detail {
        Some(detail) => format!("listen {detail}"),
        None => error.to_string(),
    }
}

/// fixture 响应的 body。
///
/// `size_hint` 保持未知（[`hyper::body::Body`] 的默认实现），因此 hyper 对 HTTP/1.1 响应
/// 使用 `Transfer-Encoding: chunked` 且不写 `Content-Length`——与 Node 的
/// `response.writeHead()` + `write()`/`end()` 一致（Node 也不会自动补 `Content-Length`）。
struct FixtureBody {
    data: Option<Bytes>,
}

impl hyper::body::Body for FixtureBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.data.take().map(|data| Ok(Frame::data(data))))
    }
}

/// Node `http.Server` 的 keep-alive 判定：HTTP/1.1 默认保持连接，`Connection: close` 关闭。
///
/// 命中时 Node 追加 `Connection: keep-alive` 与 `Keep-Alive: timeout=5`
/// （`server.keepAliveTimeout` 默认 5 秒）；`Connection: close` 由 hyper 自动补。
fn request_keep_alive(request: &Request<Incoming>) -> bool {
    if request.version() != hyper::Version::HTTP_11 {
        return false;
    }
    !request
        .headers()
        .get(hyper::header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case("close"))
}

/// 请求处理（顺序严格照 TS：路径 → 方法 → content-type → JSON → 对象 → prompt → 认证 → 载荷）。
async fn handle_request(
    request: Request<Incoming>,
    state: Arc<ServerState>,
    mode: ServerMode,
) -> Response<FixtureBody> {
    let keep_alive = request_keep_alive(&request);
    let expected_path = match mode {
        ServerMode::Openai => "/v1/chat/completions",
        ServerMode::Stream => "/generate",
    };
    let request_url = request.uri().path_and_query().map_or_else(
        || request.uri().path().to_string(),
        |value| value.as_str().to_string(),
    );
    if request_url != expected_path {
        return reject(keep_alive, StatusCode::NOT_FOUND, "Unknown endpoint");
    }
    if request.method() != Method::POST {
        return reject(keep_alive, StatusCode::METHOD_NOT_ALLOWED, "Expected POST");
    }
    let content_type = header_value(&request, "content-type").unwrap_or_default();
    if !content_type.starts_with("application/json") {
        return reject(
            keep_alive,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected application/json",
        );
    }
    let x_acme_key = header_value(&request, "x-acme-key");
    let authorization = header_value(&request, "authorization");
    // TS 的 `readJson` 按 chunk 调用 `chunk.toString()` 再拼接；这里逐 frame 解码，
    // 保留同样的「跨 chunk 的多字节字符会变成 U+FFFD」行为。
    let mut incoming = request.into_body();
    let mut body = String::new();
    while let Some(frame) = incoming.frame().await {
        match frame {
            Ok(frame) => {
                if let Some(data) = frame.data_ref() {
                    body.push_str(&String::from_utf8_lossy(data));
                }
            }
            Err(_) => return reject(keep_alive, StatusCode::BAD_REQUEST, "Invalid JSON"),
        }
    }
    let payload: Value = match serde_json::from_str(&body) {
        Ok(payload) => payload,
        Err(_) => return reject(keep_alive, StatusCode::BAD_REQUEST, "Invalid JSON"),
    };
    let Value::Object(payload) = payload else {
        return reject(
            keep_alive,
            StatusCode::UNPROCESSABLE_ENTITY,
            "Expected a JSON object",
        );
    };
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let user_message = messages.iter().find(|message| {
        matches!(
            message,
            Value::Object(record)
                if record.get("role") == Some(&Value::String("user".to_string()))
        )
    });
    let prompt = user_message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str);

    match mode {
        ServerMode::Stream => {
            if x_acme_key.as_deref() != Some("resolved-stream-key") {
                return reject(
                    keep_alive,
                    StatusCode::UNAUTHORIZED,
                    "Invalid Acme Stream credential",
                );
            }
            let model_matches =
                payload.get("model").and_then(Value::as_str) == Some(STREAM_MODEL_ID);
            let stream_matches = payload.get("stream") == Some(&Value::Bool(true));
            if !model_matches || prompt.is_none() || !stream_matches {
                return reject(
                    keep_alive,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Invalid Acme Stream request",
                );
            }
            state
                .received_valid_request
                .store(prompt == Some(STREAM_PROBE_PROMPT), Ordering::SeqCst);
            let body = concat!(
                "{\"type\":\"text_delta\",\"text\":\"ACME_\"}\n",
                "{\"type\":\"text_delta\",\"text\":\"STREAM_OK\"}\n",
                "{\"type\":\"usage\",\"input_tokens\":4,\"output_tokens\":3}\n",
                "{\"type\":\"done\",\"reason\":\"stop\"}\n",
            );
            respond(
                keep_alive,
                StatusCode::OK,
                &[("content-type", "application/x-ndjson")],
                body.as_bytes().to_vec(),
            )
        }
        ServerMode::Openai => {
            if authorization.as_deref() != Some("Bearer resolved-acme-key") {
                return reject(
                    keep_alive,
                    StatusCode::UNAUTHORIZED,
                    "Invalid Acme credential",
                );
            }
            let model_matches =
                payload.get("model").and_then(Value::as_str) == Some(OPENAI_MODEL_ID);
            let stream_matches = payload.get("stream") == Some(&Value::Bool(true));
            if !model_matches || prompt.is_none() || !stream_matches {
                return reject(
                    keep_alive,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Invalid OpenAI-compatible request",
                );
            }
            state
                .received_valid_request
                .store(prompt == Some(OPENAI_PROBE_PROMPT), Ordering::SeqCst);
            let body = concat!(
                "data: {\"id\":\"chatcmpl-acme\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"acme-chat\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ACME_OK\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-acme\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"acme-chat\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
                "data: [DONE]\n\n",
            );
            respond(
                keep_alive,
                StatusCode::OK,
                &[
                    ("content-type", "text/event-stream"),
                    ("cache-control", "no-cache"),
                ],
                body.as_bytes().to_vec(),
            )
        }
    }
}

fn header_value(request: &Request<Incoming>, name: &str) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn reject(keep_alive: bool, status: StatusCode, message: &str) -> Response<FixtureBody> {
    respond(
        keep_alive,
        status,
        &[("content-type", "text/plain")],
        message.as_bytes().to_vec(),
    )
}

fn respond(
    keep_alive: bool,
    status: StatusCode,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Response<FixtureBody> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if keep_alive {
        // Node 对 keep-alive 响应追加这两个头（`server.keepAliveTimeout` 默认 5 秒）。
        builder = builder
            .header("connection", "keep-alive")
            .header("keep-alive", "timeout=5");
    }
    builder
        .body(FixtureBody {
            data: Some(Bytes::from(body)),
        })
        .expect("fixture responses use static, valid headers")
}
