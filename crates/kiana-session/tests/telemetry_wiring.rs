//! 第 9 周：telemetry 父子关系、flush 的 traces/logs/metrics 导出与 bash TRACEPARENT。

mod common;

use std::sync::Arc;

use common::*;
use indexmap::IndexMap;
use kiana_agent_core::SessionTelemetry;
use kiana_ai::providers::faux::{FauxAssistantMessageOptions, FauxResponseStep, faux_tool_call};
use kiana_ai::types::StopReason;
use kiana_session::agent_session::PromptOptions;
use kiana_telemetry::otel::{OtlpConfig, OtlpExporter, OtlpProtocol};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

struct CapturedRequest {
    path: String,
    #[allow(dead_code)]
    body: Vec<u8>,
}

async fn spawn_collector(expected: usize) -> (String, mpsc::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind collector");
    let address = listener.local_addr().expect("local addr");
    let (sender, receiver) = mpsc::channel(expected.max(1));
    tokio::spawn(async move {
        for _ in 0..expected {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let read = socket.read(&mut chunk).await.expect("read");
                assert!(read > 0, "collector connection closed");
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(index) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let mut lines = headers.lines();
            let path = lines
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_string();
            let content_length = lines
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = buffer[header_end..].to_vec();
            while body.len() < content_length {
                let read = socket.read(&mut chunk).await.expect("read body");
                assert!(read > 0, "collector body closed");
                body.extend_from_slice(&chunk[..read]);
            }
            body.truncate(content_length);
            let _ = sender.send(CapturedRequest { path, body }).await;
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

fn bash_step(command: &str) -> FauxResponseStep {
    let mut args = IndexMap::new();
    args.insert("command".to_string(), json!(command));
    FauxResponseStep::Message(kiana_ai::providers::faux::faux_assistant_message(
        faux_tool_call("bash", args),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

/// 一轮会话应产生 `invoke_agent` 根 span，turn 与 chat 都是它的后代（不是并列根）。
#[tokio::test]
async fn session_spans_have_a_root_and_nested_children() {
    run(async {
        let telemetry = Arc::new(SessionTelemetry::new());
        let harness = create_harness(HarnessOptions {
            telemetry: Some(Arc::clone(&telemetry)),
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "bypass"}}),
            )),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            bash_step("echo telemetry"),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("run".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        let memory = telemetry.memory().expect("in-memory telemetry");
        let spans = memory.get_spans();
        let root = spans
            .iter()
            .find(|span| span.name.starts_with("invoke_agent"))
            .expect("root span");
        assert_eq!(root.parent_id, None, "root must not have a parent");
        let turn = spans
            .iter()
            .find(|span| span.name.starts_with("kiana.session.turn"))
            .expect("turn span");
        assert_eq!(
            turn.parent_id,
            Some(root.id),
            "turn span must be a child of the agent run"
        );
        let chat = spans
            .iter()
            .find(|span| span.name.starts_with("chat "))
            .expect("chat span");
        assert_eq!(
            chat.parent_id,
            Some(turn.id),
            "chat span must be a child of the turn"
        );
        let tool = spans
            .iter()
            .find(|span| span.name.starts_with("execute_tool"))
            .expect("tool span");
        assert_eq!(
            tool.parent_id,
            Some(turn.id),
            "tool span must be a child of the turn"
        );
        harness.cleanup();
    })
    .await;
}

/// `flush()` 必须把 traces / logs / metrics 三类数据都交给 OTLP exporter。
#[tokio::test]
async fn flush_exports_traces_logs_and_metrics() {
    run(async {
        let (endpoint, mut requests) = spawn_collector(3).await;
        let exporter =
            OtlpExporter::new(OtlpConfig::new(endpoint).with_protocol(OtlpProtocol::HttpJson))
                .expect("exporter");
        let telemetry = Arc::new(SessionTelemetry::new().with_exporter(Arc::new(exporter)));
        let harness = create_harness(HarnessOptions {
            telemetry: Some(Arc::clone(&telemetry)),
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "bypass"}}),
            )),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![bash_step("echo flushed"), faux_text_response("done")]);
        harness
            .session
            .prompt("run".to_string(), PromptOptions::default())
            .await
            .expect("prompt");

        assert!(
            !telemetry.recorded_logs().is_empty(),
            "API calls must buffer a structured log"
        );
        let outcome = telemetry.flush().await;
        assert!(outcome.is_success(), "{outcome:?}");
        assert!(outcome.exported_spans > 0);
        assert!(outcome.exported_logs > 0);
        assert!(outcome.exported_metrics > 0);

        let mut paths = Vec::new();
        for _ in 0..3 {
            paths.push(requests.recv().await.expect("collector request").path);
        }
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/v1/logs".to_string(),
                "/v1/metrics".to_string(),
                "/v1/traces".to_string()
            ]
        );
        harness.cleanup();
    })
    .await;
}

/// 会话的 bash 子进程能看到当前 run 的 W3C `TRACEPARENT`（未设置 TRACESTATE）。
#[tokio::test]
async fn bash_children_receive_the_session_traceparent() {
    run(async {
        let telemetry = Arc::new(SessionTelemetry::new());
        let harness = create_harness(HarnessOptions {
            telemetry: Some(Arc::clone(&telemetry)),
            settings: Some(settings_from_json(
                json!({"permissions": {"mode": "bypass"}}),
            )),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            bash_step(r#"printf '%s|%s' "$TRACEPARENT" "${TRACESTATE-unset}""#),
            faux_text_response("done"),
        ]);
        harness
            .session
            .prompt("trace".to_string(), PromptOptions::default())
            .await
            .expect("prompt");
        let result = get_tool_result(&harness, "bash");
        let text = get_message_text(&kiana_agent_core::types::AgentMessage::Standard(
            kiana_ai::types::Message::ToolResult(result),
        ));
        let (traceparent, tracestate) = text.split_once('|').expect("both values present");
        let parts: Vec<&str> = traceparent.trim().split('-').collect();
        assert_eq!(parts.len(), 4, "not a W3C traceparent: {text}");
        assert_eq!(parts[0], "00");
        assert_eq!(parts[1].len(), 32, "{text}");
        assert_eq!(parts[2].len(), 16, "{text}");
        assert!(parts[1].chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(parts[2].chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(tracestate.trim(), "unset", "TRACESTATE must not leak in");
        harness.cleanup();
    })
    .await;
}
