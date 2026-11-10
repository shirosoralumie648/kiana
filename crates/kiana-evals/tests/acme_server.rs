//! `packages/evals/test/acme-server.test.ts` 的移植。
//!
//! 每个 Rust 测试名对应 TS 的 `it` 标题（保留在 doc comment 里）；`describe.each(fixtures)`
//! 的 2 个 fixture × 5 个用例逐条展开。

#[path = "support/acme_server.rs"]
mod acme_server;

use acme_server::{
    AcmeServer, OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, OPENAI_PROBE_RESPONSE, STREAM_MODEL_ID,
    STREAM_PROBE_PROMPT, STREAM_PROBE_RESPONSE, ServerMode,
};
use serde_json::{Value, json};

fn body(model: &str, prompt: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
    })
}

async fn post(url: &str, headers: &[(&str, &str)], payload: &Value) -> (u16, String, String) {
    let client = reqwest::Client::new();
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .json(payload);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("fixture request");
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = response.text().await.expect("fixture response body");
    (status, content_type, text)
}

async fn start(mode: ServerMode) -> AcmeServer {
    let server = AcmeServer::new(mode);
    server.start().await.expect("fixture server starts");
    server
}

fn streamed_text(mode: ServerMode, raw: &str) -> String {
    match mode {
        ServerMode::Stream => raw
            .split('\n')
            .filter(|line| !line.is_empty())
            // TS 对每一行调用 `JSON.parse`：格式错误会让用例失败，而不是被忽略。
            .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON line is valid JSON"))
            .filter(|event| event.get("type").and_then(Value::as_str) == Some("text_delta"))
            .map(|event| {
                event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
        ServerMode::Openai => raw
            .split('\n')
            .filter(|line| line.starts_with("data: ") && *line != "data: [DONE]")
            .map(|line| {
                serde_json::from_str::<Value>(&line["data: ".len()..])
                    .expect("SSE data line is valid JSON")
            })
            .map(|chunk| {
                chunk
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// streamedText（Rust-only 回归：坏行必须像 JSON.parse 一样失败）
// ---------------------------------------------------------------------------

/// Rust-only: a malformed NDJSON line fails the helper (TS `JSON.parse` throws).
#[test]
#[should_panic(expected = "NDJSON line is valid JSON")]
fn malformed_ndjson_line_fails_the_streamed_text_helper() {
    let _ = streamed_text(ServerMode::Stream, "{\"type\":\"text_delta\"}\nnot json\n");
}

/// Rust-only: a malformed SSE data line fails the helper (TS `JSON.parse` throws).
#[test]
#[should_panic(expected = "SSE data line is valid JSON")]
fn malformed_sse_data_line_fails_the_streamed_text_helper() {
    let _ = streamed_text(ServerMode::Openai, "data: not json\n\n");
}

// ---------------------------------------------------------------------------
// openai fixture
// ---------------------------------------------------------------------------

/// TS: `openai fixture > accepts the probe request and records it`
#[tokio::test]
async fn openai_fixture_accepts_the_probe_request_and_records_it() {
    let server = start(ServerMode::Openai).await;
    let url = format!("{}/chat/completions", server.base_url());
    let (status, content_type, text) = post(
        &url,
        &[("authorization", "Bearer resolved-acme-key")],
        &body(OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, true),
    )
    .await;
    assert_eq!(status, 200);
    assert!(content_type.contains("text/event-stream"));
    assert_eq!(
        streamed_text(ServerMode::Openai, &text),
        OPENAI_PROBE_RESPONSE
    );
    assert!(server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `openai fixture > rejects a bad credential with 401 and does not record a probe`
#[tokio::test]
async fn openai_fixture_rejects_a_bad_credential_with_401_and_does_not_record_a_probe() {
    let server = start(ServerMode::Openai).await;
    let url = format!("{}/chat/completions", server.base_url());
    let (status, _, text) = post(
        &url,
        &[("authorization", "Bearer wrong-key")],
        &body(OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, true),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(text, "Invalid Acme credential");
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `openai fixture > rejects a malformed request with 422 and does not record a probe`
#[tokio::test]
async fn openai_fixture_rejects_a_malformed_request_with_422_and_does_not_record_a_probe() {
    let server = start(ServerMode::Openai).await;
    let url = format!("{}/chat/completions", server.base_url());
    let (status, _, text) = post(
        &url,
        &[("authorization", "Bearer resolved-acme-key")],
        &body(OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, false),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(text, "Invalid OpenAI-compatible request");
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `openai fixture > does not treat a non-probe success as the probe`
#[tokio::test]
async fn openai_fixture_does_not_treat_a_non_probe_success_as_the_probe() {
    let server = start(ServerMode::Openai).await;
    let url = format!("{}/chat/completions", server.base_url());
    let (status, _, _) = post(
        &url,
        &[("authorization", "Bearer resolved-acme-key")],
        &body(OPENAI_MODEL_ID, "hello", true),
    )
    .await;
    assert_eq!(status, 200);
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `openai fixture > clears the probe flag on reset`
#[tokio::test]
async fn openai_fixture_clears_the_probe_flag_on_reset() {
    let server = start(ServerMode::Openai).await;
    let url = format!("{}/chat/completions", server.base_url());
    let _ = post(
        &url,
        &[("authorization", "Bearer resolved-acme-key")],
        &body(OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, true),
    )
    .await;
    assert!(server.valid_request_received());
    server.reset();
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

// ---------------------------------------------------------------------------
// stream fixture
// ---------------------------------------------------------------------------

/// TS: `stream fixture > accepts the probe request and records it`
#[tokio::test]
async fn stream_fixture_accepts_the_probe_request_and_records_it() {
    let server = start(ServerMode::Stream).await;
    let url = format!("{}/generate", server.origin());
    let (status, content_type, text) = post(
        &url,
        &[("x-acme-key", "resolved-stream-key")],
        &body(STREAM_MODEL_ID, STREAM_PROBE_PROMPT, true),
    )
    .await;
    assert_eq!(status, 200);
    assert!(content_type.contains("application/x-ndjson"));
    assert_eq!(
        streamed_text(ServerMode::Stream, &text),
        STREAM_PROBE_RESPONSE
    );
    assert!(server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `stream fixture > rejects a bad credential with 401 and does not record a probe`
#[tokio::test]
async fn stream_fixture_rejects_a_bad_credential_with_401_and_does_not_record_a_probe() {
    let server = start(ServerMode::Stream).await;
    let url = format!("{}/generate", server.origin());
    let (status, _, text) = post(
        &url,
        &[("x-acme-key", "wrong-key")],
        &body(STREAM_MODEL_ID, STREAM_PROBE_PROMPT, true),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(text, "Invalid Acme Stream credential");
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `stream fixture > rejects a malformed request with 422 and does not record a probe`
#[tokio::test]
async fn stream_fixture_rejects_a_malformed_request_with_422_and_does_not_record_a_probe() {
    let server = start(ServerMode::Stream).await;
    let url = format!("{}/generate", server.origin());
    let (status, _, text) = post(
        &url,
        &[("x-acme-key", "resolved-stream-key")],
        &body(STREAM_MODEL_ID, STREAM_PROBE_PROMPT, false),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(text, "Invalid Acme Stream request");
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `stream fixture > does not treat a non-probe success as the probe`
#[tokio::test]
async fn stream_fixture_does_not_treat_a_non_probe_success_as_the_probe() {
    let server = start(ServerMode::Stream).await;
    let url = format!("{}/generate", server.origin());
    let (status, _, _) = post(
        &url,
        &[("x-acme-key", "resolved-stream-key")],
        &body(STREAM_MODEL_ID, "hello", true),
    )
    .await;
    assert_eq!(status, 200);
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

/// TS: `stream fixture > clears the probe flag on reset`
#[tokio::test]
async fn stream_fixture_clears_the_probe_flag_on_reset() {
    let server = start(ServerMode::Stream).await;
    let url = format!("{}/generate", server.origin());
    let _ = post(
        &url,
        &[("x-acme-key", "resolved-stream-key")],
        &body(STREAM_MODEL_ID, STREAM_PROBE_PROMPT, true),
    )
    .await;
    assert!(server.valid_request_received());
    server.reset();
    assert!(!server.valid_request_received());
    server.stop().await.expect("fixture server stops");
}

// ---------------------------------------------------------------------------
// 传输层（Rust-only 回归：与 Node `http.Server` 的响应帧一致）
// ---------------------------------------------------------------------------

/// 一次原始 HTTP/1.1 交换，返回到终止 chunk（或连接关闭）为止的原始字节。
async fn raw_exchange(addr: &str, request: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("raw fixture connection");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("raw fixture request");
    let mut data = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut buffer = [0_u8; 1024];
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    data.extend_from_slice(&buffer[..read]);
                    // keep-alive 响应以 `0\r\n\r\n` 结束；close 响应等到 EOF。
                    if data.windows(5).any(|window| window == b"0\r\n\r\n") {
                        break;
                    }
                }
            }
        }
    })
    .await;
    String::from_utf8_lossy(&data).into_owned()
}

/// Rust-only: Node 的 fixture 用 `writeHead` + `write`/`end`，响应没有 `Content-Length`，
/// HTTP/1.1 用 `Transfer-Encoding: chunked`，keep-alive 请求还带 `Connection`/`Keep-Alive`。
#[tokio::test]
async fn responses_match_node_http_framing() {
    let server = start(ServerMode::Openai).await;
    let addr = server.origin().trim_start_matches("http://").to_string();
    let not_found = |connection: &str| {
        format!(
            "POST /nope HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\ncontent-length: 0\r\nconnection: {connection}\r\n\r\n"
        )
    };

    let keep_alive = raw_exchange(&addr, &not_found("keep-alive")).await;
    let keep_alive_lower = keep_alive.to_ascii_lowercase();
    assert!(
        keep_alive_lower.contains("transfer-encoding: chunked"),
        "{keep_alive}"
    );
    assert!(!keep_alive_lower.contains("content-length"), "{keep_alive}");
    assert!(
        keep_alive_lower.contains("connection: keep-alive"),
        "{keep_alive}"
    );
    assert!(
        keep_alive_lower.contains("keep-alive: timeout=5"),
        "{keep_alive}"
    );
    assert!(keep_alive.ends_with("\r\n0\r\n\r\n"), "{keep_alive}");
    assert!(keep_alive.contains("Unknown endpoint"), "{keep_alive}");

    let closing = raw_exchange(&addr, &not_found("close")).await;
    let closing_lower = closing.to_ascii_lowercase();
    assert!(
        closing_lower.contains("transfer-encoding: chunked"),
        "{closing}"
    );
    assert!(!closing_lower.contains("content-length"), "{closing}");
    assert!(closing_lower.contains("connection: close"), "{closing}");
    assert!(!closing_lower.contains("keep-alive: timeout"), "{closing}");

    server.stop().await.expect("fixture server stops");
}
