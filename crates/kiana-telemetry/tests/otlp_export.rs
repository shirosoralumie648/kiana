//! OTLP 导出的端到端测试：本地 mock collector（HTTP/1.1）接收 trace/log/metrics。
//!
//! 这里不依赖 wiremock：一个手写的 tokio TCP collector 足够验证请求行、Content-Type 与
//! JSON 载荷。gRPC 的帧/路径由 `otel::proto` 与 `otel::exporter` 的单元测试覆盖。

use indexmap::IndexMap;
use kiana_telemetry::index::MetricKind;
use kiana_telemetry::index::{
    AttributeValue, SpanAttributes, SpanOptions, StoredSpan, TelemetryContextExt, TraceQuery,
};
use kiana_telemetry::memory::RecordedTelemetryMeasurement;
use kiana_telemetry::otel::{ExportTraceServiceRequest, OtlpConfig, OtlpExporter, OtlpProtocol};
use kiana_telemetry::{ContentCapture, InMemoryTelemetryContext};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

struct CapturedRequest {
    #[allow(dead_code)]
    method: String,
    path: String,
    content_type: String,
    body: Vec<u8>,
}

fn attributes(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
    let mut attributes = IndexMap::new();
    for (key, value) in pairs {
        attributes.insert((*key).to_string(), value.clone());
    }
    attributes
}

fn options(name: &str, attributes: SpanAttributes) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        attributes: Some(attributes),
        links: None,
    }
}

/// 启动一个本地 collector，接收 `expected` 个请求后退出。
async fn spawn_collector(expected: usize) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind collector");
    let address = listener.local_addr().expect("local addr");
    let (sender, receiver) = mpsc::channel(expected.max(1));
    tokio::spawn(async move {
        for _ in 0..expected {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let request = read_request(&mut socket).await;
            let _ = sender.send(request).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{address}"), receiver)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await.expect("read request");
        assert!(read > 0, "connection closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_subsequence(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = headers.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or_default().to_string();
    let mut content_type = String::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-type") {
                content_type = value.trim().to_string();
            }
            if key.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await.expect("read body");
        assert!(read > 0, "connection closed before body");
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    CapturedRequest {
        method,
        path,
        content_type,
        body,
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 构造 root(agent) + chat + tool 三个 span，模拟一次完整会话。
async fn build_session_trace(telemetry: &InMemoryTelemetryContext) -> Vec<StoredSpan> {
    let _root = telemetry
        .start_span_typed(
            options(
                "invoke_agent kiana",
                attributes(&[
                    (
                        "gen_ai.operation.name",
                        AttributeValue::String("invoke_agent".into()),
                    ),
                    ("gen_ai.agent.name", AttributeValue::String("kiana".into())),
                ]),
            ),
            move |root| {
                Box::pin(async move {
                    let root_context: &dyn kiana_telemetry::TelemetryContext = root.as_ref();
                    let chat = TelemetryContextExt::start_span_typed(
                        root_context,
                        options(
                            "chat gpt-5",
                            attributes(&[
                                (
                                    "gen_ai.operation.name",
                                    AttributeValue::String("chat".into()),
                                ),
                                (
                                    "gen_ai.provider.name",
                                    AttributeValue::String("openai".into()),
                                ),
                                (
                                    "gen_ai.request.model",
                                    AttributeValue::String("gpt-5".into()),
                                ),
                                ("gen_ai.usage.input_tokens", AttributeValue::Number(120.0)),
                                ("gen_ai.usage.output_tokens", AttributeValue::Number(30.0)),
                            ]),
                        ),
                        |_| Box::pin(async {}),
                    );
                    chat.await.expect("chat span");
                    let tool = TelemetryContextExt::start_span_typed(
                        root_context,
                        options(
                            "execute_tool bash",
                            attributes(&[
                                (
                                    "gen_ai.operation.name",
                                    AttributeValue::String("execute_tool".into()),
                                ),
                                ("gen_ai.tool.name", AttributeValue::String("bash".into())),
                                (
                                    "gen_ai.tool.call.id",
                                    AttributeValue::String("call_1".into()),
                                ),
                                ("kiana.tool.kind", AttributeValue::String("builtin".into())),
                            ]),
                        ),
                        |_| Box::pin(async {}),
                    );
                    tool.await.expect("tool span");
                    // 内容捕获默认关闭：即使这里“有” prompt，也不会写进 span。
                    let capture = ContentCapture::disabled();
                    if let Some(prompt) = capture.maybe("super secret prompt") {
                        root.add_event(
                            "gen_ai.content.prompt",
                            Some(attributes(&[(
                                "prompt",
                                AttributeValue::String(prompt.into()),
                            )])),
                        );
                    }
                })
            },
        )
        .await
        .expect("root span");
    let page = telemetry
        .find_traces(Some(TraceQuery::default()))
        .await
        .expect("find traces");
    assert_eq!(page.traces.len(), 1, "one trace");
    page.traces[0].spans.clone()
}

#[tokio::test]
async fn a_complete_session_exports_one_root_and_chat_tool_children() {
    let (endpoint, mut requests) = spawn_collector(1).await;
    let telemetry = InMemoryTelemetryContext::new();
    let spans = build_session_trace(&telemetry).await;
    assert_eq!(spans.len(), 3);

    let exporter = OtlpExporter::new(
        OtlpConfig::new(endpoint)
            .with_protocol(OtlpProtocol::HttpJson)
            .with_service_name("kiana-test"),
    )
    .expect("exporter");
    let outcome = exporter.export_spans(&spans).await.expect("export");
    assert_eq!(outcome.exported_spans, 3);

    let request = requests.recv().await.expect("collector request");
    assert_eq!(request.path, "/v1/traces");
    assert!(request.content_type.starts_with("application/json"));
    let body: ExportTraceServiceRequest =
        serde_json::from_slice(&request.body).expect("decode OTLP JSON");
    let resource_spans = &body.resource_spans[0];
    let resource_attributes = &resource_spans
        .resource
        .as_ref()
        .expect("resource")
        .attributes;
    assert!(resource_attributes.iter().any(|attribute| {
        attribute.key == "service.name"
            && attribute.value == kiana_telemetry::otel::AnyValue::StringValue("kiana-test".into())
    }));

    let exported = &resource_spans.scope_spans[0].spans;
    assert_eq!(exported.len(), 3);
    let root = exported
        .iter()
        .find(|span| span.name.starts_with("invoke_agent"))
        .expect("root span");
    let chat = exported
        .iter()
        .find(|span| span.name.starts_with("chat"))
        .expect("chat span");
    let tool = exported
        .iter()
        .find(|span| span.name.starts_with("execute_tool"))
        .expect("tool span");

    assert!(root.parent_span_id.is_none());
    assert_eq!(chat.parent_span_id.as_deref(), Some(root.span_id.as_str()));
    assert_eq!(tool.parent_span_id.as_deref(), Some(root.span_id.as_str()));
    assert_eq!(chat.trace_id, root.trace_id);
    assert_eq!(tool.trace_id, root.trace_id);

    fn attrs(span: &kiana_telemetry::otel::Span) -> Vec<(&str, &kiana_telemetry::otel::AnyValue)> {
        span.attributes
            .iter()
            .map(|attribute| (attribute.key.as_str(), &attribute.value))
            .collect()
    }
    let chat_attributes = attrs(chat);
    assert!(
        chat_attributes
            .iter()
            .any(|(key, _)| *key == "gen_ai.operation.name")
    );
    assert!(
        chat_attributes
            .iter()
            .any(|(key, _)| *key == "gen_ai.usage.input_tokens")
    );
    let tool_attributes = attrs(tool);
    assert!(
        tool_attributes
            .iter()
            .any(|(key, _)| *key == "gen_ai.tool.name")
    );

    // 内容捕获关闭：导出的载荷里没有 prompt 文本。
    let serialized = String::from_utf8_lossy(&request.body);
    assert!(
        !serialized.contains("super secret prompt"),
        "prompt content leaked into the OTLP payload"
    );
}

#[tokio::test]
async fn logs_and_metrics_are_exported_to_their_own_paths() {
    let (endpoint, mut requests) = spawn_collector(2).await;
    let exporter =
        OtlpExporter::new(OtlpConfig::new(endpoint).with_protocol(OtlpProtocol::HttpJson))
            .expect("exporter");

    let log = kiana_telemetry::otel::log_record(
        "compaction finished",
        vec![kiana_telemetry::otel::string_key_value("trigger", "auto")],
        None,
        None,
    );
    exporter.export_logs(&[log]).await.expect("logs");

    let measurement = RecordedTelemetryMeasurement {
        kind: MetricKind::Counter,
        name: "kiana.api.call.tokens".into(),
        description: None,
        unit: Some("{token}".into()),
        boundaries: None,
        value: 42.0,
        attributes: IndexMap::new(),
        context: None,
    };
    exporter
        .export_metrics(&[measurement])
        .await
        .expect("metrics");

    let first = requests.recv().await.expect("logs request");
    assert_eq!(first.path, "/v1/logs");
    let logs: Value = serde_json::from_slice(&first.body).expect("logs json");
    assert_eq!(
        logs["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        logs["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"],
        Value::String("compaction finished".into())
    );

    let second = requests.recv().await.expect("metrics request");
    assert_eq!(second.path, "/v1/metrics");
    let metrics: Value = serde_json::from_slice(&second.body).expect("metrics json");
    let exported = &metrics["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0];
    assert_eq!(
        exported["name"],
        Value::String("kiana.api.call.tokens".into())
    );
    assert_eq!(exported["sum"]["isMonotonic"], Value::Bool(true));
    assert_eq!(
        exported["sum"]["dataPoints"][0]["asDouble"],
        Value::from(42.0)
    );
}

#[tokio::test]
async fn shutdown_flushes_buffered_spans() {
    let (endpoint, mut requests) = spawn_collector(1).await;
    let telemetry = InMemoryTelemetryContext::new();
    let spans = build_session_trace(&telemetry).await;
    let exporter = OtlpExporter::new(
        OtlpConfig::new(endpoint)
            .with_protocol(OtlpProtocol::HttpJson)
            .with_timeout_ms(2_000),
    )
    .expect("exporter");
    assert_eq!(exporter.enqueue_spans(spans), 3);
    assert_eq!(exporter.buffered(), (3, 0, 0));
    let outcome = exporter.shutdown().await;
    assert_eq!(outcome.exported_spans, 3);
    assert_eq!(exporter.buffered(), (0, 0, 0));
    assert!(requests.recv().await.is_some());
    // 重复 shutdown 是 no-op。
    let second = exporter.shutdown().await;
    assert_eq!(second, kiana_telemetry::ExportOutcome::default());
}

#[tokio::test]
async fn unreachable_exporter_does_not_block_the_agent_path() {
    // 绑定后立即释放端口，确保连接被拒绝。
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    let exporter = OtlpExporter::new(
        OtlpConfig::new(format!("http://{address}"))
            .with_protocol(OtlpProtocol::HttpJson)
            .with_timeout_ms(300),
    )
    .expect("exporter");
    let telemetry = InMemoryTelemetryContext::new();
    let spans = build_session_trace(&telemetry).await;

    let outcome = exporter.export_spans_best_effort(&spans).await;
    assert!(!outcome.is_success());

    // exporter 的失败不影响后续 agent 工作。
    let produced = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let span = telemetry
            .start_span_typed(options("agent continues", SpanAttributes::new()), |_| {
                Box::pin(async { 42 })
            })
            .await
            .expect("span");
        span
    })
    .await
    .expect("agent path did not block");
    assert_eq!(produced, 42);
}

#[test]
fn otlp_exporter_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OtlpExporter>();
}
