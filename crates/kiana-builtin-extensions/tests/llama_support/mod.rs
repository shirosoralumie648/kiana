//! `test/llama-extension.test.ts` 的共享测试设施：真实回环 HTTP 服务器（不 mock fetch）。
//!
//! TS 用 `node:http` 的 `createServer` + `listen(0)`；Rust 侧用 `tokio::net::TcpListener`
//! 与手写 HTTP/1.1（只实现测试用到的子集）。镜像 `crates/kiana-mcp/tests/support/mod.rs` 的
//! 结构，但**不依赖 `http` crate**：header 用 `Vec<(String, String)>`。
//!
//! - [`listen`]：绑定 `127.0.0.1:0`，返回 [`TestServer`]（含 `origin` 与请求记录）。
//! - [`ResponseWriter::send`]：带 `content-length` 的一次性响应。
//! - [`ResponseWriter::stream_head`] / [`ResponseWriter::stream_chunk`]：chunked 流式响应
//!   （SSE；跨 chunk 的半帧由客户端缓冲）。
//! - `Drop` 中止所有连接任务（= `closeAllConnections` + `close`）。

#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use kiana_ai::error::AiError;
use kiana_ai::models::{ModelsPublication, ModelsPublicationPersist, ModelsStoreEntry, PublishFn};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// 一条记录下来的请求（TS 的 `request` 回调参数）。
#[derive(Debug, Clone)]
pub struct TestRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl TestRequest {
    /// `request.headers["<name>"]`（大小写不敏感；同名多值以 `", "` 合并）。
    pub fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        let values: Vec<&str> = self
            .headers
            .iter()
            .filter(|(header, _)| *header == name)
            .map(|(_, value)| value.as_str())
            .collect();
        if values.is_empty() {
            None
        } else {
            Some(values.join(", "))
        }
    }

    /// 解析后的 JSON body（非 JSON / 无 body 时为 `None`）。
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }

    /// 路径（不含 query）。
    pub fn pathname(&self) -> &str {
        self.path.split('?').next().unwrap_or("")
    }

    /// query 参数解码（`URLSearchParams.get` 的近似）。
    pub fn query_param(&self, name: &str) -> Option<String> {
        let query = self.path.split_once('?')?.1;
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }
}

struct WriteState {
    write: Option<OwnedWriteHalf>,
}

/// 响应写入器（`ServerResponse` 的子集）。克隆共享同一条连接。
#[derive(Clone)]
pub struct ResponseWriter {
    inner: Arc<tokio::sync::Mutex<WriteState>>,
}

impl ResponseWriter {
    fn new(write: OwnedWriteHalf) -> Self {
        ResponseWriter {
            inner: Arc::new(tokio::sync::Mutex::new(WriteState { write: Some(write) })),
        }
    }

    async fn write_all(&self, bytes: &[u8]) {
        let mut state = self.inner.lock().await;
        if let Some(write) = state.write.as_mut() {
            let _ = write.write_all(bytes).await;
            let _ = write.flush().await;
        }
    }

    /// `response.writeHead(status, headers); response.end(body)`。
    pub async fn send(&self, status: u16, headers: &[(&str, &str)], body: &str) {
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason_phrase(status));
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("content-length: {}\r\n", body.len()));
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body.as_bytes());
        self.write_all(&bytes).await;
        // 处理完一个请求后关闭连接（Node 的 keep-alive 语义不在测试范围内）。
        self.inner.lock().await.write = None;
    }

    /// 以 JSON 响应 200。
    pub async fn send_json(&self, value: &Value) {
        self.send(
            200,
            &[("content-type", "application/json")],
            &kiana_json::js_object::stringify(value),
        )
        .await;
    }

    /// 404（TS `response.writeHead(404).end()`）。
    pub async fn send_not_found(&self) {
        self.send(404, &[], "").await;
    }

    /// `response.writeHead(status, headers)` + `transfer-encoding: chunked`。
    pub async fn stream_head(&self, status: u16, content_type: &str) {
        let head = format!(
            "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n\r\n",
            reason_phrase(status)
        );
        self.write_all(head.as_bytes()).await;
    }

    /// `response.write(chunk)`（chunked 编码）。
    pub async fn stream_chunk(&self, chunk: &str) {
        self.stream_bytes(chunk.as_bytes()).await;
    }

    /// 与 [`Self::stream_chunk`] 相同，但按原始字节写出（用于跨 chunk 的 UTF-8 半字符）。
    pub async fn stream_bytes(&self, bytes: &[u8]) {
        let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
        framed.extend_from_slice(bytes);
        framed.extend_from_slice(b"\r\n");
        self.write_all(&framed).await;
    }

    /// `response.end()`（chunked 终止块）。
    pub async fn stream_end(&self) {
        self.write_all(b"0\r\n\r\n").await;
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

type HandlerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Handler = Arc<dyn Fn(TestRequest, ResponseWriter) -> HandlerFuture + Send + Sync>;

/// 回环测试服务器。
pub struct TestServer {
    origin: String,
    requests: Arc<Mutex<Vec<TestRequest>>>,
    shutdown: CancellationToken,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl TestServer {
    /// `http://127.0.0.1:<port>`。
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// 当前记录到的请求快照。
    pub fn requests(&self) -> Vec<TestRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// 记录到的请求数。
    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// `listen(handler)`：起一个真实回环服务器。
pub async fn listen<F, Fut>(handler: F) -> TestServer
where
    F: Fn(TestRequest, ResponseWriter) -> Fut + Send + Sync + 'static,
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
    recorded: Arc<Mutex<Vec<TestRequest>>>,
    shutdown: CancellationToken,
) {
    let (read_half, write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    if shutdown.is_cancelled() {
        return;
    }
    let mut request_line = String::new();
    match reader.read_line(&mut request_line).await {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    let request_line = request_line.trim_end_matches(['\r', '\n']).to_string();
    if request_line.is_empty() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
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
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let request = TestRequest {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    recorded.lock().unwrap().push(request.clone());
    let response = ResponseWriter::new(write_half);
    handler(request, response).await;
}

/// 记录持久化条目的 publish 回调（TS 测试里的 `cachedEntry`）。
pub type StoredEntry = Arc<Mutex<Option<ModelsStoreEntry>>>;

/// 构造一个把 `persist` 记进 [`StoredEntry`] 并执行 `update` 的 publish 回调。
pub fn publish_into(entry: StoredEntry) -> PublishFn {
    Arc::new(move |publication: ModelsPublication| {
        let entry = Arc::clone(&entry);
        Box::pin(async move {
            match publication.persist {
                ModelsPublicationPersist::Unchanged => {}
                ModelsPublicationPersist::Delete => {
                    *entry.lock().unwrap() = None;
                }
                ModelsPublicationPersist::Entry(value) => {
                    *entry.lock().unwrap() = Some(value);
                }
            }
            if let Some(update) = publication.update {
                update();
            }
            Ok::<bool, AiError>(true)
        })
    })
}

/// 不记录持久化、只执行 `update` 的 publish 回调。
pub fn publish_ok() -> PublishFn {
    Arc::new(|publication: ModelsPublication| {
        Box::pin(async move {
            if let Some(update) = publication.update {
                update();
            }
            Ok::<bool, AiError>(true)
        })
    })
}

/// SSE 广播器：多个 `/models/sse` 连接共享。
#[derive(Clone, Default)]
pub struct SseStreams {
    writers: Arc<Mutex<Vec<ResponseWriter>>>,
}

impl SseStreams {
    /// 注册一个新连接（handler 在 `stream_head` 之后调用它，然后永不返回）。
    pub fn add(&self, writer: ResponseWriter) {
        self.writers.lock().unwrap().push(writer);
    }

    /// `for (const response of streams) response.write("data: …\n\n")`。
    pub async fn send(&self, event: &Value) {
        let writers = self.writers.lock().unwrap().clone();
        let frame = format!("data: {}\n\n", kiana_json::js_object::stringify(event));
        for writer in writers {
            writer.stream_chunk(&frame).await;
        }
    }
}

/// 把多个答案排队的登录交互（TS 测试的 `answers.shift()`）。
pub struct FakeInteraction {
    signal: kiana_ai::utils::abort::AbortScope,
    answers: Mutex<VecDeque<String>>,
}

impl FakeInteraction {
    pub fn new(signal: kiana_ai::utils::abort::AbortScope, answers: Vec<String>) -> Self {
        FakeInteraction {
            signal,
            answers: Mutex::new(answers.into()),
        }
    }
}

#[async_trait::async_trait]
impl kiana_ai::auth::types::AuthInteraction for FakeInteraction {
    fn signal(&self) -> &kiana_ai::utils::abort::AbortScope {
        &self.signal
    }

    async fn prompt(
        &self,
        request: kiana_ai::auth::types::AuthPromptRequest,
    ) -> Result<String, kiana_ai::auth::types::AuthError> {
        match request.prompt {
            kiana_ai::auth::types::AuthPrompt::Text { .. } => {}
            kiana_ai::auth::types::AuthPrompt::Secret { .. } => {}
            other => panic!("unexpected prompt: {other:?}"),
        }
        let answer = self.answers.lock().unwrap().pop_front();
        answer.ok_or_else(|| kiana_ai::auth::types::AuthError::new("no answer"))
    }

    fn notify(&self, _event: &kiana_ai::auth::types::AuthEvent) {}
}

/// 空 AuthContext（`env` 恒 `None`，`fileExists` 恒 false）。
#[derive(Default)]
pub struct EmptyAuthContext;

#[async_trait::async_trait]
impl kiana_ai::auth::types::AuthContext for EmptyAuthContext {
    async fn env(&self, _name: &str) -> Option<String> {
        None
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

/// 等待一小段时间（让后台事件先到达）。
pub async fn wait_ms(milliseconds: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(milliseconds)).await;
}
