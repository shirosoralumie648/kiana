//! `packages/mcp/test/helpers.ts` 的移植：真实的回环 HTTP 服务器（不 mock）。
//!
//! TS 用 `node:http` 的 `createServer` 绑定 `127.0.0.1:0`、用 `readBody` 读请求体、用
//! `closeServers`（`closeAllConnections` + `close`）在 `afterEach` 里清理。Rust 侧用
//! `tokio::net::TcpListener` + 手写 HTTP/1.1（只实现 TS 测试用到的子集）：
//!
//! - [`listen`]：绑定回环端口、返回 origin（`http://127.0.0.1:<port>`）。
//! - [`read_body`]：按 `content-length` 读请求体（`readBody`）。
//! - [`TestServer`]：`listen` + 请求记录 + `Drop`（= `closeServers`）。
//!
//! 记录形状与 TS 的 `RecordedRequest` 一致：`method`、`headers`、`message`（解析后的 JSON body）。
//! 响应有两种写法：带 `content-length` 的一次性响应（`response.end(body)`），以及
//! `transfer-encoding: chunked` 的流式响应（`response.write(...)`，用于 SSE）。

#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue};
use kiana_mcp::auth_provider::header_joined;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// TS 的 `RecordedRequest`。
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub headers: HeaderMap,
    /// 解析后的 JSON body（非 JSON / 无 body 时为 `None`）。
    pub message: Option<Value>,
    pub body: String,
}

impl RecordedRequest {
    /// `request.headers["<name>"]`（同名多值以 `", "` 合并）。
    pub fn header(&self, name: &str) -> Option<String> {
        header_joined(&self.headers, name)
    }
}

/// 一个已解析的请求（交给 handler）。
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: String,
    pub message: Option<Value>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<String> {
        header_joined(&self.headers, name)
    }
}

/// 响应写入器（`ServerResponse` 的子集）。克隆共享同一条连接的写半边。
#[derive(Clone)]
pub struct ResponseWriter {
    inner: Arc<tokio::sync::Mutex<WriteState>>,
}

struct WriteState {
    write: Option<OwnedWriteHalf>,
    streaming: bool,
    finished: bool,
}

impl ResponseWriter {
    fn new(write: OwnedWriteHalf) -> Self {
        ResponseWriter {
            inner: Arc::new(tokio::sync::Mutex::new(WriteState {
                write: Some(write),
                streaming: false,
                finished: false,
            })),
        }
    }

    async fn write_all(&self, bytes: &[u8]) {
        let mut state = self.inner.lock().await;
        if let Some(write) = state.write.as_mut() {
            let _ = write.write_all(bytes).await;
            let _ = write.flush().await;
        }
    }

    /// `response.writeHead(status, headers); response.end(body)`（带 `content-length`）。
    pub async fn send(&self, status: u16, headers: &[(&str, &str)], body: &str) {
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        // 204 没有 body，也没有 content-length（Node 同样如此）。
        if status != 204 {
            head.push_str(&format!("content-length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        if status != 204 {
            bytes.extend_from_slice(body.as_bytes());
        }
        self.write_all(&bytes).await;
        self.inner.lock().await.finished = true;
    }

    /// 与 [`Self::send`] 相同，但头值按**原始字节**写出。
    ///
    /// `node:http` 的 `response.setHeader` 只接受 latin1 字符串，而服务器可以在头上写任意
    /// obs-text 字节；Rust 侧的 `HeaderValue` 也接受这些字节，因此头值在 `send` 里走 `&str`
    /// 也没问题——只有需要发**裸 0x80..=0xFF**（例如 U+0085 经 latin1 编码后的 0x85）的
    /// 测试才用这个变体，避免把非 ASCII 字符按 UTF-8 编成两三个字节。
    pub async fn send_raw_header(&self, status: u16, headers: &[(&str, &[u8])], body: &[u8]) {
        let mut bytes = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status)).into_bytes();
        for (name, value) in headers {
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(b": ");
            bytes.extend_from_slice(value);
            bytes.extend_from_slice(b"\r\n");
        }
        // 204 没有 body，也没有 content-length（Node 同样如此）。
        if status != 204 {
            bytes.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        if status != 204 {
            bytes.extend_from_slice(body);
        }
        self.write_all(&bytes).await;
        self.inner.lock().await.finished = true;
    }

    /// `response.writeHead(status, headers)` + `transfer-encoding: chunked`（可继续 write）。
    pub async fn stream_head(&self, status: u16, headers: &[(&str, &str)]) {
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("transfer-encoding: chunked\r\n\r\n");
        self.inner.lock().await.streaming = true;
        self.write_all(head.as_bytes()).await;
    }

    /// `response.write(chunk)`（chunked 编码）。
    pub async fn stream_chunk(&self, chunk: &str) {
        if !self.inner.lock().await.streaming {
            self.stream_head(200, &[]).await;
        }
        let bytes = chunk.as_bytes();
        let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
        framed.extend_from_slice(bytes);
        framed.extend_from_slice(b"\r\n");
        self.write_all(&framed).await;
    }

    /// `response.end()`（chunked 终止块）。连接保持打开（keep-alive），与 Node 一致。
    pub async fn stream_end(&self) {
        let mut state = self.inner.lock().await;
        if state.streaming {
            state.streaming = false;
            state.finished = true;
        }
        drop(state);
        self.write_all(b"0\r\n\r\n").await;
    }

    /// `response.end()`：流式响应直接结束，否则发一个空的 200。
    pub async fn end(&self) {
        let streaming = self.inner.lock().await.streaming;
        if streaming {
            self.stream_end().await;
        } else {
            self.send(200, &[], "").await;
        }
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

type HandlerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Handler = Arc<dyn Fn(HttpRequest, ResponseWriter) -> HandlerFuture + Send + Sync>;

/// 回环服务器（= `listen` + `closeServers` + 请求记录）。
pub struct TestServer {
    /// `http://127.0.0.1:<port>`（TS 的 `origin`）。
    origin: String,
    /// 记录下来的请求（TS 的 `requests` 数组）。
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl TestServer {
    /// `origin` + `/mcp`（TS 的 `${origin}/mcp`）。
    pub fn url(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// 当前记录到的请求快照。
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// 等待某个请求出现（后台任务里的 GET/DELETE 需要一点时间）。
    pub async fn wait_for_request<F>(&self, predicate: F) -> RecordedRequest
    where
        F: Fn(&RecordedRequest) -> bool,
    {
        self.wait_for_request_timeout(predicate, Duration::from_secs(5))
            .await
    }

    /// 带超时的 [`Self::wait_for_request`]。
    pub async fn wait_for_request_timeout<F>(
        &self,
        predicate: F,
        timeout: Duration,
    ) -> RecordedRequest
    where
        F: Fn(&RecordedRequest) -> bool,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(found) = self.requests().into_iter().find(|entry| predicate(entry)) {
                return found;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for a request; recorded methods: {:?}",
                    self.requests()
                        .iter()
                        .map(|entry| entry.method.clone())
                        .collect::<Vec<_>>()
                );
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// 记录到的请求数。
    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // `closeAllConnections()` + `close()`：中止所有连接任务。
        self.shutdown.cancel();
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// `listen(handler)`：起一个真实回环服务器。
pub async fn listen<F, Fut>(handler: F) -> TestServer
where
    F: Fn(HttpRequest, ResponseWriter) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let handler: Handler = Arc::new(move |request, response| Box::pin(handler(request, response)));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local_addr");
    let origin = format!("http://127.0.0.1:{}", address.port());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let shutdown = CancellationToken::new();
    let tasks = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let token = shutdown.clone();
    let handles = Arc::clone(&tasks);
    let acceptor = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let handler = Arc::clone(&handler);
            let recorded = Arc::clone(&recorded);
            let token = token.clone();
            let task = tokio::spawn(async move {
                serve_connection(socket, handler, recorded, token).await;
            });
            handles.lock().unwrap().push(task);
        }
    });
    tasks.lock().unwrap().push(acceptor);
    TestServer {
        origin,
        requests,
        shutdown,
        tasks,
    }
}

async fn serve_connection(
    socket: TcpStream,
    handler: Handler,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: CancellationToken,
) {
    let (read_half, write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let writer = ResponseWriter::new(write_half);
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let mut request_line = String::new();
        match reader.read_line(&mut request_line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if request_line.trim().is_empty() {
            continue;
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or("/").to_owned();
        let mut headers = HeaderMap::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.trim().as_bytes()),
                    HeaderValue::from_str(value.trim()),
                )
            {
                headers.append(name, value);
            }
        }
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body).await.is_err() {
            return;
        }
        let body = String::from_utf8_lossy(&body).into_owned();
        let message = serde_json::from_str::<Value>(&body).ok();
        let request = HttpRequest {
            method: method.clone(),
            path,
            headers: headers.clone(),
            body: body.clone(),
            message: message.clone(),
        };
        recorded.lock().unwrap().push(RecordedRequest {
            method,
            headers,
            message,
            body,
        });
        // `void handler(request, response)`：handler 自己等它需要的门闸；这里 await 完成，
        // 因为 Node 的 `createServer` 回调也是"处理完一个请求再读下一个"（同一条连接）。
        handler(request, writer.clone()).await;
        if writer.inner.lock().await.finished {
            return;
        }
    }
}
