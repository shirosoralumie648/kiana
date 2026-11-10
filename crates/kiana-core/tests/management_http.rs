//! `utils/management-http.ts` 的移植测试（`test/management-http.test.ts` 的 5 个用例）。
//!
//! TS 的测试用 `vi.stubGlobal("fetch")` 注入假的 fetch；Rust 侧没有全局 fetch，改为
//! **本地 TCP 服务器**（禁止真实网络）：连接计数、立即断开（传输失败）、延迟、状态码与
//! 响应体全部可观测。冷却时间用宽松上界断言，不比较精确墙钟值。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kiana_core::management_http::{
    ABORTED_MESSAGE, FetchError, FetchRetryOptions, TIMEOUT_MESSAGE, fetch_with_retry,
};
use tokio_util::sync::CancellationToken;

/// 每个连接在独立线程里处理的极简 HTTP 服务器。
struct TestServer {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
}

impl TestServer {
    fn spawn<F>(handler: F) -> Self
    where
        F: Fn(usize, TcpStream) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local addr");
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&connections);
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || handler(index, stream));
            }
        });
        Self {
            address,
            connections,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// 读到请求头结束（客户端写完后我们才能安全地关闭/响应）。
fn read_request(stream: &mut TcpStream) {
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
}

fn write_response(stream: &mut TcpStream, status_line: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status_line}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// 不经过环境代理的测试 client。
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build client")
}

fn builder_request(client: &reqwest::Client, url: String) -> impl Fn() -> reqwest::RequestBuilder {
    let client = client.clone();
    move || client.get(&url)
}

#[tokio::test]
async fn retries_a_transient_transport_failure_until_success() {
    let server = TestServer::spawn(|index, mut stream| {
        if index < 2 {
            // 接受后立即关闭：客户端看到传输错误。
            drop(stream);
            return;
        }
        read_request(&mut stream);
        write_response(&mut stream, "200 OK", "{\"ok\":true}");
    });
    let client = client();
    let options = FetchRetryOptions::default();

    let response = fetch_with_retry(builder_request(&client, server.url("/x")), options)
        .await
        .expect("third attempt succeeds");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.expect("body"), "{\"ok\":true}");
    assert_eq!(server.connections(), 3);
}

#[tokio::test]
async fn shares_the_timeout_budget_across_attempts() {
    // TS 断言两次尝试拿到**同一个** `AbortSignal`（`signals[0] === signals[1]`）。Rust 的等价
    // 可观察量是“同一个 deadline”：第一次尝试 500ms 后返回可重试的 503，总预算 1500ms 只剩
    // ~1000ms，而第二次尝试需要 1500ms 才响应 → 必须在 ~1500ms 处超时。
    // 若预算按每次尝试独立（错误的实现），第二次尝试会在 ~2000ms 成功返回 200，
    // 因此 `Err(Timeout)` 本身就区分了两种语义。
    let server = TestServer::spawn(|index, mut stream| {
        read_request(&mut stream);
        if index == 0 {
            std::thread::sleep(Duration::from_millis(500));
            write_response(&mut stream, "503 Service Unavailable", "busy");
            return;
        }
        std::thread::sleep(Duration::from_millis(1500));
        write_response(&mut stream, "200 OK", "late");
    });
    let client = client();
    let started = Instant::now();
    let error = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            max_retries: Some(5),
            timeout_ms: Some(1500),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect_err("budget expires");
    let elapsed = started.elapsed();

    assert_eq!(error, FetchError::Timeout);
    assert_eq!(error.to_string(), TIMEOUT_MESSAGE);
    assert!(
        elapsed < Duration::from_millis(2500),
        "shared budget should cut the second attempt short, took {elapsed:?}"
    );
    assert_eq!(server.connections(), 2);
}

#[tokio::test]
async fn retries_an_attempt_timeout() {
    // TS 断言 `AbortSignal.timeout` 被调用了两次（每个尝试一个信号）。Rust 的可观察量：
    // 第一次尝试的服务器 2s 后才响应；只有 `attemptTimeoutMs = 200ms` 生效时才会中止当前
    // 尝试并重试。若 attemptTimeoutMs 被忽略，第一次尝试会在 ~2s 后直接拿到 200，
    // 服务器只会看到 1 个连接。
    let server = TestServer::spawn(|index, mut stream| {
        read_request(&mut stream);
        if index == 0 {
            std::thread::sleep(Duration::from_millis(2000));
            write_response(&mut stream, "200 OK", "slow");
            return;
        }
        write_response(&mut stream, "200 OK", "ok");
    });
    let client = client();
    let started = Instant::now();

    let response = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            attempt_timeout_ms: Some(200),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect("second attempt succeeds");
    let elapsed = started.elapsed();

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.expect("body"), "ok");
    assert_eq!(server.connections(), 2);
    assert!(
        elapsed < Duration::from_millis(1500),
        "attempt timeout should abort the first attempt, took {elapsed:?}"
    );
}

#[tokio::test]
async fn throws_the_attempt_timeout_once_retries_are_exhausted() {
    let server = TestServer::spawn(|_index, mut stream| {
        read_request(&mut stream);
        std::thread::sleep(Duration::from_millis(1000));
    });
    let client = client();
    let error = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            max_retries: Some(1),
            attempt_timeout_ms: Some(100),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect_err("timeout");
    assert_eq!(error, FetchError::Timeout);
    assert_eq!(error.to_string(), TIMEOUT_MESSAGE);
    assert_eq!(server.connections(), 2);
}

#[tokio::test]
async fn retries_transient_http_responses_and_returns_the_successful_response() {
    let server = TestServer::spawn(|index, mut stream| {
        read_request(&mut stream);
        if index == 0 {
            write_response(&mut stream, "503 Service Unavailable", "busy");
        } else {
            write_response(&mut stream, "200 OK", "{\"ok\":true}");
        }
    });
    let client = client();

    let response = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions::default(),
    )
    .await
    .expect("retry succeeds");
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(server.connections(), 2);
}

#[tokio::test]
async fn returns_a_retryable_response_when_retries_are_exhausted() {
    let server = TestServer::spawn(|_index, mut stream| {
        read_request(&mut stream);
        write_response(&mut stream, "503 Service Unavailable", "busy");
    });
    let client = client();

    let response = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            max_retries: Some(0),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect("response is returned, not an error");
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn retry_on_status_false_returns_the_response_immediately() {
    let server = TestServer::spawn(|_index, mut stream| {
        read_request(&mut stream);
        write_response(&mut stream, "500 Internal Server Error", "boom");
    });
    let client = client();

    let response = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            retry_on_status: Some(false),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect("response");
    assert_eq!(response.status().as_u16(), 500);
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn does_not_retry_caller_cancellation() {
    let server = TestServer::spawn(|_index, mut stream| {
        read_request(&mut stream);
        write_response(&mut stream, "200 OK", "unexpected");
    });
    let client = client();
    let signal = CancellationToken::new();
    signal.cancel();

    let error = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            signal: Some(signal),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect_err("pre-cancelled");
    assert_eq!(error, FetchError::Aborted);
    assert_eq!(error.to_string(), ABORTED_MESSAGE);
    // 预取消 → 一次请求都不发。
    assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn cancelling_mid_flight_aborts_the_attempt() {
    let server = TestServer::spawn(|_index, mut stream| {
        read_request(&mut stream);
        std::thread::sleep(Duration::from_millis(2000));
    });
    let client = client();
    let signal = CancellationToken::new();
    let cancel_handle = signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_handle.cancel();
    });

    let error = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            signal: Some(signal),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect_err("cancelled");
    assert_eq!(error, FetchError::Aborted);
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn transport_failures_surface_as_fetch_failed_with_a_cause() {
    let server = TestServer::spawn(|_index, stream| drop(stream));
    let client = client();
    let error = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            max_retries: Some(0),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect_err("transport failure");
    match &error {
        FetchError::Transport { message, cause } => {
            assert_eq!(message, "fetch failed");
            let cause = cause.as_ref().expect("reqwest error has a source chain");
            assert!(!cause[0].message.is_empty());
        }
        other => panic!("expected transport error, got {other:?}"),
    }
    assert_eq!(error.to_string(), "fetch failed");
}

#[tokio::test]
async fn response_bodies_keep_the_shared_timeout_budget() {
    // TS 的 `AbortSignal.timeout(timeoutMs)` 覆盖 body 读取：`fetchWithRetry` 返回的 `Response`
    // 在读取 body 时必须仍在同一 deadline 上（`RequestBuilder::timeout` 的 `TotalTimeoutBody`）。
    let server = TestServer::spawn(|_index, mut stream| {
        read_request(&mut stream);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\nconnection: close\r\n\r\nhel");
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(2000));
    });
    let client = client();
    let started = Instant::now();
    let response = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            timeout_ms: Some(300),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect("headers arrive before the deadline");
    assert_eq!(response.status().as_u16(), 200);

    let error = response.text().await.expect_err("stalled body times out");
    assert!(error.is_timeout(), "expected a timeout error, got {error}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "body read should stop at the shared deadline, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn max_retries_counts_additional_attempts() {
    let server = TestServer::spawn(|_index, stream| drop(stream));
    let client = client();
    let error = fetch_with_retry(
        builder_request(&client, server.url("/x")),
        FetchRetryOptions {
            max_retries: Some(1),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .expect_err("transport failure");
    assert!(matches!(error, FetchError::Transport { .. }));
    assert_eq!(server.connections(), 2);
}
