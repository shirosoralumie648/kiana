//! `ReqwestFetch` 的环回冒烟测试：HTTP 传输与 OAuth 单元依赖它做**全部**网络访问，
//! 因此这里用真实的 127.0.0.1 TCP 服务器（不 mock）验证请求方法、头、体与响应读取。

use std::sync::Arc;

use http::Method;
use kiana_mcp::auth_provider::{
    AuthProvider, FetchBody, FetchInit, FetchResponse, JsHeaders, McpFetch, ReqwestFetch,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 读一个 HTTP/1.1 请求（头 + Content-Length 体），回一个固定响应。
async fn serve_once(
    listener: TcpListener,
    status_line: &'static str,
    response_headers: &'static str,
    response_body: &'static str,
) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end;
        loop {
            let read = socket.read(&mut chunk).await.expect("read");
            if read == 0 {
                panic!("connection closed before headers");
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = position + 4;
                break;
            }
        }
        let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        while buffer.len() < header_end + content_length {
            let read = socket.read(&mut chunk).await.expect("read body");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        let body = String::from_utf8_lossy(&buffer[header_end..]).into_owned();
        socket
            .write_all(
                format!("{status_line}\r\n{response_headers}\r\n\r\n{response_body}").as_bytes(),
            )
            .await
            .expect("write");
        socket.shutdown().await.ok();
        format!("{head}\n\n{body}")
    })
}

#[tokio::test]
async fn posts_json_and_reads_the_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_once(
        listener,
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\ncontent-length: 11",
        "{\"ok\":true}",
    )
    .await;

    let fetch = ReqwestFetch::new();
    let url = url::Url::parse(&format!("http://{address}/mcp")).unwrap();
    let mut headers = JsHeaders::new();
    headers
        .set("Accept", "application/json, text/event-stream")
        .unwrap();
    let mut response = fetch
        .fetch(
            &url,
            FetchInit::with_method(Method::POST)
                .with_headers(headers)
                .with_body(FetchBody::Text("{\"jsonrpc\":\"2.0\"}".to_owned())),
        )
        .await
        .expect("fetch");

    assert_eq!(response.status(), 200);
    assert!(response.ok());
    assert_eq!(response.content_type().as_deref(), Some("application/json"));
    assert_eq!(
        response.json().await.unwrap()["ok"],
        serde_json::json!(true)
    );

    let request = server.await.unwrap();
    assert!(request.starts_with("POST /mcp HTTP/1.1"), "{request}");
    // reqwest 按 `HeaderName` 的规范形状写名字（首字母大写）；值逐字。
    assert!(
        request
            .to_ascii_lowercase()
            .contains("accept: application/json, text/event-stream"),
        "{request}"
    );
    assert!(request.ends_with("{\"jsonrpc\":\"2.0\"}"), "{request}");
}

#[tokio::test]
async fn non_ok_statuses_are_returned_not_raised() {
    // `fetch` 只在网络失败时 reject；HTTP 状态由调用方（`checkResponse`）判错。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = serve_once(
        listener,
        "HTTP/1.1 401 Unauthorized",
        "www-authenticate: Bearer realm=\"x\"\r\ncontent-length: 4",
        "nope",
    )
    .await;

    let fetch = ReqwestFetch::new();
    let url = url::Url::parse(&format!("http://{address}/")).unwrap();
    let mut response = fetch.fetch(&url, FetchInit::get()).await.expect("fetch");
    assert_eq!(response.status(), 401);
    assert!(!response.ok());
    assert_eq!(
        response.header("www-authenticate").as_deref(),
        Some("Bearer realm=\"x\"")
    );
    assert_eq!(response.text().await.unwrap(), "nope");
    server.await.unwrap();
}

#[tokio::test]
async fn timeout_and_cancellation_surface_as_errors() {
    // 永不响应的服务器：`timeout` 触发。
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let keep = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 64];
        let _ = socket.read(&mut buffer).await;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });

    let fetch = ReqwestFetch::new();
    let url = url::Url::parse(&format!("http://{address}/")).unwrap();
    let error = match fetch
        .fetch(
            &url,
            FetchInit::get().with_timeout(Some(std::time::Duration::from_millis(50))),
        )
        .await
    {
        Ok(_) => panic!("expected a timeout"),
        Err(error) => error,
    };
    // `AbortSignal.timeout` 在 Node 里产出 `DOMException(name="TimeoutError")`（已实测），
    // 不是普通的网络 `TypeError`。
    assert_eq!(error.name(), "TimeoutError");
    assert!(error.is_timeout_error());
    assert!(error.is_dom_exception());

    // 取消令牌：立刻取消。
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let error = match fetch
        .fetch(&url, FetchInit::get().with_signal(Some(token)))
        .await
    {
        Ok(_) => panic!("expected an abort"),
        Err(error) => error,
    };
    // 取消在 Node 里是 `DOMException(name="AbortError", message="This operation was aborted")`。
    assert_eq!(error.name(), "AbortError");
    assert!(error.is_abort());
    assert_eq!(error.to_string(), "This operation was aborted");
    keep.abort();
}

/// `AuthProvider` 的对象安全性（`Arc<dyn AuthProvider>` 是 HTTP 传输的选项类型）。
struct StaticToken(Option<String>);

#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
impl AuthProvider for StaticToken {
    async fn token(&self) -> Option<String> {
        self.0.clone()
    }
}

#[tokio::test]
async fn auth_provider_trait_is_object_safe_with_default_on_unauthorized() {
    let provider: Arc<dyn AuthProvider> = Arc::new(StaticToken(Some("t".to_owned())));
    assert_eq!(provider.token().await, Some("t".to_owned()));
    // 缺省 `on_unauthorized` 是 no-op。
    let context = kiana_mcp::auth_provider::UnauthorizedContext {
        status: 401,
        headers: http::HeaderMap::new(),
        body: b"denied".to_vec(),
        server_url: url::Url::parse("http://127.0.0.1/").unwrap(),
        fetch: Arc::new(ReqwestFetch::new()) as Arc<dyn McpFetch>,
        token: None,
    };
    assert_eq!(context.body_text(), "denied");
    provider.on_unauthorized(context).await.unwrap();
}

/// 回归测试（评审 finding #5，minor）：`timeout` 必须覆盖响应体读取。服务器先回响应头，
/// 然后永不给体数据；TS 的 `AbortSignal.timeout` 会在体读取中途 abort。
#[tokio::test]
async fn timeout_covers_response_body_reads() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let keep = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 1024];
        let _ = socket.read(&mut buffer).await;
        // 响应头声明 100 字节的体，但只发 3 个字节，然后保持连接。
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\nabc")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });

    let fetch = ReqwestFetch::new();
    let url = url::Url::parse(&format!("http://{address}/")).unwrap();
    let mut response = fetch
        .fetch(
            &url,
            FetchInit::get().with_timeout(Some(std::time::Duration::from_millis(150))),
        )
        .await
        .expect("headers arrive before the timeout");
    // 头已到，体还没读完：到期后必须报 `TimeoutError`（而不是永远挂着）。
    let error = response.text().await.expect_err("body read must time out");
    assert_eq!(error.name(), "TimeoutError");
    keep.abort();
}

/// 回归测试（评审 finding #11）：Node `Response` 的体只能读一次，`json()` 失败是 `SyntaxError`。
#[tokio::test]
async fn response_body_semantics_match_node() {
    let mut response = FetchResponse::from_bytes(200, http::HeaderMap::new(), b"not json".to_vec());
    let error = response.json().await.expect_err("invalid JSON");
    assert_eq!(error.name(), "SyntaxError");

    let mut response = FetchResponse::from_bytes(200, http::HeaderMap::new(), b"hello".to_vec());
    assert_eq!(response.text().await.unwrap(), "hello");
    let error = response.text().await.expect_err("body already read");
    assert_eq!(error.name(), "TypeError");
    assert_eq!(
        error.to_string(),
        "Body is unusable: Body has already been read"
    );

    let mut response = FetchResponse::from_bytes(200, http::HeaderMap::new(), b"{}".to_vec());
    response.discard().await;
    assert_eq!(
        response.text().await.unwrap_err().to_string(),
        "Body is unusable: Body has already been read"
    );
}

/// 回归测试（评审 finding #11）：`statusText` 用服务器给的短语，不是规范短语。
#[tokio::test]
async fn status_text_comes_from_the_server() {
    let response = FetchResponse::from_bytes(200, http::HeaderMap::new(), Vec::new());
    assert_eq!(response.status_text(), "");
    let response = response.with_status_text("Custom Reason Phrase");
    assert_eq!(response.status_text(), "Custom Reason Phrase");
}

/// 回归测试（评审 finding #11）：`fetch` 的规范禁止 GET/HEAD 带体。
#[tokio::test]
async fn get_and_head_requests_reject_bodies() {
    let fetch = ReqwestFetch::new();
    let url = url::Url::parse("http://127.0.0.1:1/").unwrap();
    for method in [Method::GET, Method::HEAD] {
        let label = method.as_str().to_owned();
        let error = match fetch
            .fetch(
                &url,
                FetchInit::with_method(method).with_body(FetchBody::Text("x".to_owned())),
            )
            .await
        {
            Ok(_) => panic!("expected a TypeError for {label}"),
            Err(error) => error,
        };
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.to_string(),
            "Request with GET/HEAD method cannot have body."
        );
    }
}

/// 回归测试（评审 finding #3，major）：取消与超时必须是 `DOMException` 的身份，
/// 因为调用方按 `error.name` 分支（coding-agent 的 abort 处理）。
#[tokio::test]
async fn abort_and_timeout_errors_have_dom_exception_identity() {
    let aborted = kiana_mcp::auth_provider::McpTransportError::aborted();
    assert_eq!(aborted.name(), "AbortError");
    assert_eq!(aborted.to_string(), "This operation was aborted");
    assert!(aborted.is_dom_exception());
    assert!(aborted.is_abort());
    assert!(!aborted.is_type_error());

    let timed_out = kiana_mcp::auth_provider::McpTransportError::timed_out();
    assert_eq!(timed_out.name(), "TimeoutError");
    assert_eq!(
        timed_out.to_string(),
        "The operation was aborted due to timeout"
    );
    assert!(timed_out.is_timeout_error());
    assert!(!timed_out.is_abort());
}

/// 回归测试（评审 finding #11）：`statusText` 是服务器状态行里的短语，不是注册短语。
///
/// Node 实测：`HTTP/1.1 200 Weird Reason` → `"Weird Reason"`、`HTTP/1.1 200` → `""`、
/// `HTTP/1.1 404 Not Found` → `"Not Found"`。
#[tokio::test]
async fn status_text_is_the_reason_phrase_from_the_status_line() {
    let cases = [
        ("HTTP/1.1 200 Weird Reason", "Weird Reason"),
        ("HTTP/1.1 200 OK", "OK"),
        ("HTTP/1.1 200", ""),
        ("HTTP/1.1 418 Teapot", "Teapot"),
    ];
    for (status_line, expected) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = serve_once(listener, status_line, "content-length: 2", "ok").await;
        // 环回地址不能走环境里的 HTTP 代理：代理会重写状态行，短语就不是服务器给的了。
        let fetch =
            ReqwestFetch::from_client(reqwest::Client::builder().no_proxy().build().unwrap());
        let url = url::Url::parse(&format!("http://{address}/")).unwrap();
        let mut response = fetch
            .fetch(&url, FetchInit::with_method(Method::GET))
            .await
            .expect("fetch");
        assert_eq!(
            response.status_text(),
            expected,
            "status line: {status_line}"
        );
        assert_eq!(response.text().await.unwrap(), "ok");
        server.await.unwrap();
    }
}

/// 回归测试（评审 finding #8，minor）：`ReqwestFetch` 的线上头与 Node 的 `fetch` 对齐。
///
/// 本机 Node 26 抓包实测（`/home/ctyun/kiana-work/scratch/kiana-mcp__core-protocol/srv*.mjs`）：
/// - 无显式头时 Node 发 `accept: */*`、`accept-language: *`、`sec-fetch-mode: cors`、
///   `accept-encoding: gzip, deflate`、`user-agent: node`；
/// - 字符串体在没有 `content-type` 时补 `text/plain;charset=UTF-8`；
/// - `URLSearchParams` 补 `application/x-www-form-urlencoded;charset=UTF-8`；
/// - 显式设置的头（含 `user-agent` / `accept-encoding`）保留调用方的值。
///
/// 第二轮评审（finding 7，minor）补上原先漏掉的后两项：`accept-language` 与
/// `sec-fetch-mode`（Node 26 实测，`scratch/gate__kiana-mcp/probe_headers.mjs`）。
///
/// 大小写差异（hyper 只写小写名）无法用 reqwest 复刻，见模块文档的偏差说明；这里比较头名
/// 大小写不敏感。
#[tokio::test]
async fn request_headers_match_node_fetch_defaults() {
    async fn capture(build: impl FnOnce() -> FetchInit) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await
                .unwrap();
            socket.shutdown().await.ok();
            String::from_utf8_lossy(&buffer).into_owned()
        });
        // `no_proxy`：环回请求不能被环境里的 HTTP 代理接管。
        let fetch =
            ReqwestFetch::from_client(reqwest::Client::builder().no_proxy().build().unwrap());
        let url = url::Url::parse(&format!("http://{address}/mcp")).unwrap();
        fetch.fetch(&url, build()).await.expect("fetch");
        server.await.unwrap()
    }

    fn header_of<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim_start())
        })
    }

    // 默认头 + 字符串体的自动 content-type。
    let request = capture(|| {
        FetchInit::with_method(Method::POST).with_body(FetchBody::Text("hi".to_owned()))
    })
    .await;
    assert_eq!(header_of(&request, "user-agent"), Some("node"), "{request}");
    assert_eq!(
        header_of(&request, "accept-encoding"),
        Some("gzip, deflate"),
        "{request}"
    );
    assert_eq!(header_of(&request, "accept"), Some("*/*"), "{request}");
    // Node 26 的另外两个默认头（finding 7）。
    assert_eq!(
        header_of(&request, "accept-language"),
        Some("*"),
        "{request}"
    );
    assert_eq!(
        header_of(&request, "sec-fetch-mode"),
        Some("cors"),
        "{request}"
    );
    assert_eq!(
        header_of(&request, "content-type"),
        Some("text/plain;charset=UTF-8"),
        "{request}"
    );

    // 显式头保留调用方的值（大小写不敏感覆盖）。
    let request = capture(|| {
        let mut headers = JsHeaders::new();
        headers
            .set("Accept", "application/json, text/event-stream")
            .unwrap();
        headers.set("accept-language", "de-DE").unwrap();
        headers.set("sec-fetch-mode", "no-cors").unwrap();
        headers.set("user-agent", "custom-agent").unwrap();
        headers.set("Accept-Encoding", "identity").unwrap();
        FetchInit::with_method(Method::POST)
            .with_headers(headers)
            .with_body(FetchBody::Text("{}".to_owned()))
    })
    .await;
    assert_eq!(header_of(&request, "user-agent"), Some("custom-agent"));
    assert_eq!(header_of(&request, "accept-encoding"), Some("identity"));
    assert_eq!(header_of(&request, "accept-language"), Some("de-DE"));
    assert_eq!(header_of(&request, "sec-fetch-mode"), Some("no-cors"));
    assert_eq!(
        header_of(&request, "accept"),
        Some("application/json, text/event-stream")
    );

    // `URLSearchParams` → 表单 content-type（调用方没设时）。
    let request = capture(|| {
        FetchInit::with_method(Method::POST)
            .with_body(FetchBody::Form(vec![("a".to_owned(), "1".to_owned())]))
    })
    .await;
    assert_eq!(
        header_of(&request, "content-type"),
        Some("application/x-www-form-urlencoded;charset=UTF-8")
    );

    // `Bytes` 体不补 content-type（与 `ArrayBuffer`/TypedArray 一致）。
    let request =
        capture(|| FetchInit::with_method(Method::POST).with_body(FetchBody::Bytes(vec![1, 2, 3])))
            .await;
    assert_eq!(header_of(&request, "content-type"), None, "{request}");
}

/// 回归测试（评审 finding #8）：`gzip` 响应在显式 `accept-encoding` 下仍被解压
/// （reqwest 按响应头解码，与 `fetch` 一致）。
#[tokio::test]
async fn explicit_accept_encoding_still_decodes_gzip_responses() {
    // `gzip.compress(b"hello gzip")`（Python 3 的 gzip 模块，node 26 的 zlib 同格式）。
    const GZIP_HELLO: [u8; 30] = [
        31, 139, 8, 0, 4, 62, 224, 106, 2, 255, 203, 72, 205, 201, 201, 87, 72, 175, 202, 44, 0, 0,
        25, 106, 210, 223, 10, 0, 0, 0,
    ];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 4096];
        let _ = socket.read(&mut buffer).await.unwrap();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            GZIP_HELLO.len()
        )
        .into_bytes();
        response.extend_from_slice(&GZIP_HELLO);
        socket.write_all(&response).await.unwrap();
        socket.shutdown().await.ok();
    });
    let fetch = ReqwestFetch::from_client(reqwest::Client::builder().no_proxy().build().unwrap());
    let url = url::Url::parse(&format!("http://{address}/")).unwrap();
    let mut headers = JsHeaders::new();
    headers.set("accept-encoding", "gzip, deflate").unwrap();
    let mut response = fetch
        .fetch(&url, FetchInit::get().with_headers(headers))
        .await
        .expect("fetch");
    assert_eq!(response.text().await.unwrap(), "hello gzip");
    server.await.unwrap();
}
