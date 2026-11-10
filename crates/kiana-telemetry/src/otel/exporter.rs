//! OTLP 导出器：HTTP/JSON、HTTP/protobuf 与 gRPC（http2）三种传输。
//!
//! # 传输
//!
//! - `http/json`：`POST {endpoint}/v1/{signal}`，`Content-Type: application/json`；
//! - `http/protobuf`：同一路径，`Content-Type: application/x-protobuf`，正文是
//!   [`super::proto`] 的手写编码；
//! - `grpc`：`POST {endpoint}/opentelemetry.proto.collector.<signal>.v1.<Service>/Export`，
//!   `Content-Type: application/grpc+proto`，正文为 5 字节 gRPC 帧。明文 `http://` 使用
//!   HTTP/2 prior knowledge（h2c）；`https://` 走 ALPN。
//!
//! # 失败语义
//!
//! 导出失败只返回 [`OtlpError`]，**不会 panic、也不会影响 agent 主流程**。调用方可以：
//!
//! - 用 [`OtlpExporter::export_*`] 拿到 `Result` 并记录；
//! - 用 `*_best_effort` 版本把失败折叠进 [`ExportOutcome::failures`]。
//!
//! # 缓冲与 flush
//!
//! `enqueue_*` 把数据放进进程内缓冲；`flush` 在 shutdown 前发送。SDK 初始化前的早期事件
//! 可以先 enqueue，等 exporter 就绪后统一 flush。缓冲只在内存中，不落盘。
//!
//! # 环境变量
//!
//! [`OtlpConfig::from_env`] 读取标准变量：`OTEL_EXPORTER_OTLP_ENDPOINT`、
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`、`OTEL_EXPORTER_OTLP_PROTOCOL`、
//! `OTEL_EXPORTER_OTLP_HEADERS`、`OTEL_SERVICE_NAME`。没有任何 endpoint 时返回 `None`
//! （遥测整体关闭）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::index::StoredSpan;
use crate::memory::RecordedTelemetryMeasurement;

use super::convert::{metrics_request, trace_request};
use super::proto::{encode_logs_request, encode_metrics_request, encode_trace_request, grpc_frame};
use super::types::{
    AnyValue, ExportLogsServiceRequest, ExportMetricsServiceRequest, ExportTraceServiceRequest,
    LogRecord, SeverityNumber,
};

/// OTLP 传输协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OtlpProtocol {
    /// gRPC（HTTP/2 + protobuf）。
    #[serde(rename = "grpc")]
    Grpc,
    /// OTLP/HTTP + JSON。
    #[serde(rename = "http/json")]
    HttpJson,
    /// OTLP/HTTP + protobuf。
    #[serde(rename = "http/protobuf")]
    HttpProtobuf,
}

impl OtlpProtocol {
    /// 稳定字面量（与 `OTEL_EXPORTER_OTLP_PROTOCOL` 一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            OtlpProtocol::Grpc => "grpc",
            OtlpProtocol::HttpJson => "http/json",
            OtlpProtocol::HttpProtobuf => "http/protobuf",
        }
    }

    /// 从环境变量字面量解析；未知值回落到 `http/protobuf`（OTel 默认）。
    pub fn from_str_or_default(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "grpc" => OtlpProtocol::Grpc,
            "http/json" | "http_json" | "json" => OtlpProtocol::HttpJson,
            "http/protobuf" | "http_protobuf" | "protobuf" => OtlpProtocol::HttpProtobuf,
            _ => OtlpProtocol::HttpProtobuf,
        }
    }
}

impl std::fmt::Display for OtlpProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 导出器配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtlpConfig {
    /// collector 地址（HTTP 是 base URL；gRPC 是 host:port）。
    pub endpoint: String,
    /// 传输协议。
    pub protocol: OtlpProtocol,
    /// 请求超时（毫秒）。
    pub timeout_ms: u64,
    /// 额外请求头（鉴权等）。
    #[serde(default)]
    pub headers: IndexMap<String, String>,
    /// `service.name` 资源属性。
    pub service_name: String,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        OtlpConfig {
            endpoint: "http://localhost:4318".to_string(),
            protocol: OtlpProtocol::HttpProtobuf,
            timeout_ms: 5_000,
            headers: IndexMap::new(),
            service_name: "kiana".to_string(),
        }
    }
}

impl OtlpConfig {
    /// 用 endpoint 构造默认配置。
    pub fn new(endpoint: impl Into<String>) -> Self {
        OtlpConfig {
            endpoint: endpoint.into(),
            ..Self::default()
        }
    }

    /// 设置协议。
    pub fn with_protocol(mut self, protocol: OtlpProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// 设置超时。
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// 设置 service.name。
    pub fn with_service_name(mut self, service_name: impl Into<String>) -> Self {
        self.service_name = service_name.into();
        self
    }

    /// 追加一个请求头。
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// 从标准 OTel 环境变量构造；没有 endpoint 时返回 `None`。
    ///
    /// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` 优先于通用 endpoint。需要分别为 logs/metrics
    /// 配置不同地址时，请直接构造配置（本类型只保留一个 endpoint）。
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })?;
        let protocol = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL")
            .map(|value| OtlpProtocol::from_str_or_default(&value))
            .unwrap_or(OtlpProtocol::HttpProtobuf);
        let service_name = std::env::var("OTEL_SERVICE_NAME")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "kiana".to_string());
        let mut headers = IndexMap::new();
        if let Ok(raw) = std::env::var("OTEL_EXPORTER_OTLP_HEADERS") {
            for entry in raw.split(',') {
                if let Some((key, value)) = entry.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();
                    if !key.is_empty() && !value.is_empty() {
                        headers.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }
        Some(OtlpConfig {
            endpoint,
            protocol,
            timeout_ms: 5_000,
            headers,
            service_name,
        })
    }

    /// 某个 signal 的 HTTP URL（补 `/v1/{signal}`，已带则不重复）。
    pub fn signal_url(&self, signal: &str) -> String {
        let base = self.endpoint.trim_end_matches('/');
        if base.ends_with(&format!("/v1/{signal}")) {
            return base.to_string();
        }
        format!("{base}/v1/{signal}")
    }

    /// 某个 signal 的 gRPC URL。
    pub fn grpc_url(&self, signal: &str) -> String {
        let base = self.endpoint.trim_end_matches('/');
        if base.contains("opentelemetry.proto.collector") {
            return base.to_string();
        }
        let service = match signal {
            "traces" => "trace.v1.TraceService",
            "logs" => "logs.v1.LogsService",
            "metrics" => "metrics.v1.MetricsService",
            _ => "trace.v1.TraceService",
        };
        format!("{base}/opentelemetry.proto.collector.{service}/Export")
    }
}

/// 导出错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpError {
    /// 错误文本。
    pub message: String,
    /// HTTP 状态（传输层失败时为 `None`）。
    pub status: Option<u16>,
}

impl OtlpError {
    /// 传输层错误。
    pub fn transport(message: impl Into<String>) -> Self {
        OtlpError {
            message: message.into(),
            status: None,
        }
    }

    /// HTTP 状态错误。
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        OtlpError {
            message: message.into(),
            status: Some(status),
        }
    }
}

impl std::fmt::Display for OtlpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(
                formatter,
                "OTLP export failed (HTTP {status}): {}",
                self.message
            ),
            None => write!(formatter, "OTLP export failed: {}", self.message),
        }
    }
}

impl std::error::Error for OtlpError {}

/// 一次 flush / 导出的结果汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportOutcome {
    /// 尝试发送的请求数。
    pub attempted_requests: usize,
    /// 成功导出的 span 数。
    pub exported_spans: usize,
    /// 成功导出的日志数。
    pub exported_logs: usize,
    /// 成功导出的指标测量数。
    pub exported_metrics: usize,
    /// 失败描述；空表示全部成功。
    pub failures: Vec<String>,
}

impl ExportOutcome {
    /// 是否没有失败。
    pub fn is_success(&self) -> bool {
        self.failures.is_empty()
    }

    /// 合并另一次导出的计数与失败。
    pub fn merge(&mut self, other: ExportOutcome) {
        self.attempted_requests += other.attempted_requests;
        self.exported_spans += other.exported_spans;
        self.exported_logs += other.exported_logs;
        self.exported_metrics += other.exported_metrics;
        self.failures.extend(other.failures);
    }
}

#[derive(Debug, Default)]
struct Buffer {
    spans: Vec<StoredSpan>,
    logs: Vec<LogRecord>,
    metrics: Vec<RecordedTelemetryMeasurement>,
    shutdown: bool,
}

/// OTLP 导出器。可 clone（内部共享同一个缓冲）。
#[derive(Debug, Clone)]
pub struct OtlpExporter {
    config: OtlpConfig,
    client: reqwest::Client,
    buffer: Arc<Mutex<Buffer>>,
}

impl OtlpExporter {
    /// 构造导出器。超时/HTTP 客户端构建失败返回错误（不会 panic）。
    pub fn new(config: OtlpConfig) -> Result<Self, OtlpError> {
        let mut builder =
            reqwest::Client::builder().timeout(Duration::from_millis(config.timeout_ms.max(1)));
        if config.protocol == OtlpProtocol::Grpc && config.endpoint.starts_with("http://") {
            // 明文 collector 用 h2c（HTTP/2 prior knowledge）。
            builder = builder.http2_prior_knowledge();
        }
        let client = builder
            .build()
            .map_err(|error| OtlpError::transport(format!("build HTTP client: {error}")))?;
        Ok(OtlpExporter {
            config,
            client,
            buffer: Arc::new(Mutex::new(Buffer::default())),
        })
    }

    /// 从环境变量构造；未配置 endpoint 时返回 `Ok(None)`。
    pub fn from_env() -> Result<Option<Self>, OtlpError> {
        match OtlpConfig::from_env() {
            Some(config) => Self::new(config).map(Some),
            None => Ok(None),
        }
    }

    /// 配置引用。
    pub fn config(&self) -> &OtlpConfig {
        &self.config
    }

    /// 当前缓冲数量 `(spans, logs, metrics)`。
    pub fn buffered(&self) -> (usize, usize, usize) {
        let buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (buffer.spans.len(), buffer.logs.len(), buffer.metrics.len())
    }

    /// 缓冲一批 span（SDK 初始化前可调用）。
    pub fn enqueue_spans(&self, spans: impl IntoIterator<Item = StoredSpan>) -> usize {
        let mut buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = buffer.spans.len();
        buffer.spans.extend(spans);
        buffer.spans.len() - before
    }

    /// 缓冲一批日志。
    pub fn enqueue_logs(&self, logs: impl IntoIterator<Item = LogRecord>) -> usize {
        let mut buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = buffer.logs.len();
        buffer.logs.extend(logs);
        buffer.logs.len() - before
    }

    /// 缓冲一批测量。
    pub fn enqueue_metrics(
        &self,
        measurements: impl IntoIterator<Item = RecordedTelemetryMeasurement>,
    ) -> usize {
        let mut buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = buffer.metrics.len();
        buffer.metrics.extend(measurements);
        buffer.metrics.len() - before
    }

    /// 发送一批 span（不经过缓冲）。
    pub async fn export_spans(&self, spans: &[StoredSpan]) -> Result<ExportOutcome, OtlpError> {
        let request = trace_request(&self.config.service_name, spans);
        self.send_trace(&request).await?;
        Ok(ExportOutcome {
            attempted_requests: 1,
            exported_spans: spans.len(),
            ..ExportOutcome::default()
        })
    }

    /// 发送一批日志。
    pub async fn export_logs(&self, logs: &[LogRecord]) -> Result<ExportOutcome, OtlpError> {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![super::types::ResourceLogs {
                resource: Some(super::convert::resource(&self.config.service_name)),
                scope_logs: vec![super::types::ScopeLogs {
                    scope: Some(super::types::InstrumentationScope {
                        name: super::convert::INSTRUMENTATION_SCOPE.to_string(),
                        version: None,
                    }),
                    log_records: logs.to_vec(),
                }],
            }],
        };
        self.send_logs(&request).await?;
        Ok(ExportOutcome {
            attempted_requests: 1,
            exported_logs: logs.len(),
            ..ExportOutcome::default()
        })
    }

    /// 发送一批原始测量。
    pub async fn export_metrics(
        &self,
        measurements: &[RecordedTelemetryMeasurement],
    ) -> Result<ExportOutcome, OtlpError> {
        let request = metrics_request(&self.config.service_name, measurements);
        self.send_metrics(&request).await?;
        Ok(ExportOutcome {
            attempted_requests: 1,
            exported_metrics: measurements.len(),
            ..ExportOutcome::default()
        })
    }

    /// span 导出的 best-effort 版本：失败折叠进返回值，绝不 panic。
    pub async fn export_spans_best_effort(&self, spans: &[StoredSpan]) -> ExportOutcome {
        match self.export_spans(spans).await {
            Ok(outcome) => outcome,
            Err(error) => ExportOutcome {
                attempted_requests: 1,
                failures: vec![error.message],
                ..ExportOutcome::default()
            },
        }
    }

    /// 发送缓冲中的全部数据；返回汇总。单个 signal 失败不影响其它 signal。
    pub async fn flush(&self) -> ExportOutcome {
        let (spans, logs, metrics) = {
            let mut buffer = self
                .buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                std::mem::take(&mut buffer.spans),
                std::mem::take(&mut buffer.logs),
                std::mem::take(&mut buffer.metrics),
            )
        };
        let mut outcome = ExportOutcome::default();
        if !spans.is_empty() {
            match self.export_spans(&spans).await {
                Ok(result) => outcome.merge(result),
                Err(error) => {
                    outcome.attempted_requests += 1;
                    outcome.failures.push(error.message);
                }
            }
        }
        if !logs.is_empty() {
            match self.export_logs(&logs).await {
                Ok(result) => outcome.merge(result),
                Err(error) => {
                    outcome.attempted_requests += 1;
                    outcome.failures.push(error.message);
                }
            }
        }
        if !metrics.is_empty() {
            match self.export_metrics(&metrics).await {
                Ok(result) => outcome.merge(result),
                Err(error) => {
                    outcome.attempted_requests += 1;
                    outcome.failures.push(error.message);
                }
            }
        }
        outcome
    }

    /// shutdown：先 flush（失败不传播），再标记关闭。重复调用安全。
    pub async fn shutdown(&self) -> ExportOutcome {
        let already_closed = {
            let mut buffer = self
                .buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let already = buffer.shutdown;
            buffer.shutdown = true;
            already
        };
        if already_closed {
            return ExportOutcome::default();
        }
        self.flush().await
    }

    async fn send_trace(&self, request: &ExportTraceServiceRequest) -> Result<(), OtlpError> {
        match self.config.protocol {
            OtlpProtocol::HttpJson => {
                let body = serde_json::to_vec(request)
                    .map_err(|error| OtlpError::transport(format!("encode OTLP JSON: {error}")))?;
                self.post(
                    self.config.signal_url("traces"),
                    "application/json",
                    body,
                    false,
                )
                .await
            }
            OtlpProtocol::HttpProtobuf => {
                self.post(
                    self.config.signal_url("traces"),
                    "application/x-protobuf",
                    encode_trace_request(request),
                    false,
                )
                .await
            }
            OtlpProtocol::Grpc => {
                self.post(
                    self.config.grpc_url("traces"),
                    "application/grpc+proto",
                    grpc_frame(&encode_trace_request(request)),
                    true,
                )
                .await
            }
        }
    }

    async fn send_logs(&self, request: &ExportLogsServiceRequest) -> Result<(), OtlpError> {
        match self.config.protocol {
            OtlpProtocol::HttpJson => {
                let body = serde_json::to_vec(request)
                    .map_err(|error| OtlpError::transport(format!("encode OTLP JSON: {error}")))?;
                self.post(
                    self.config.signal_url("logs"),
                    "application/json",
                    body,
                    false,
                )
                .await
            }
            OtlpProtocol::HttpProtobuf => {
                self.post(
                    self.config.signal_url("logs"),
                    "application/x-protobuf",
                    encode_logs_request(request),
                    false,
                )
                .await
            }
            OtlpProtocol::Grpc => {
                self.post(
                    self.config.grpc_url("logs"),
                    "application/grpc+proto",
                    grpc_frame(&encode_logs_request(request)),
                    true,
                )
                .await
            }
        }
    }

    async fn send_metrics(&self, request: &ExportMetricsServiceRequest) -> Result<(), OtlpError> {
        match self.config.protocol {
            OtlpProtocol::HttpJson => {
                let body = serde_json::to_vec(request)
                    .map_err(|error| OtlpError::transport(format!("encode OTLP JSON: {error}")))?;
                self.post(
                    self.config.signal_url("metrics"),
                    "application/json",
                    body,
                    false,
                )
                .await
            }
            OtlpProtocol::HttpProtobuf => {
                self.post(
                    self.config.signal_url("metrics"),
                    "application/x-protobuf",
                    encode_metrics_request(request),
                    false,
                )
                .await
            }
            OtlpProtocol::Grpc => {
                self.post(
                    self.config.grpc_url("metrics"),
                    "application/grpc+proto",
                    grpc_frame(&encode_metrics_request(request)),
                    true,
                )
                .await
            }
        }
    }

    async fn post(
        &self,
        url: String,
        content_type: &str,
        body: Vec<u8>,
        grpc: bool,
    ) -> Result<(), OtlpError> {
        let mut request = self
            .client
            .post(&url)
            .header("content-type", content_type)
            .header("user-agent", "kiana-telemetry")
            .body(body);
        if grpc {
            request = request.header("te", "trailers");
        }
        for (key, value) in &self.config.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|error| OtlpError::transport(format!("{url}: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(512).collect();
            return Err(OtlpError::http(status.as_u16(), snippet));
        }
        if grpc
            && let Some(grpc_status) = response.headers().get("grpc-status")
            && grpc_status.to_str().ok() != Some("0")
        {
            return Err(OtlpError::transport(format!(
                "gRPC status {}",
                grpc_status.to_str().unwrap_or("<invalid>")
            )));
        }
        Ok(())
    }
}

/// 构造一条 INFO 日志记录。
pub fn log_record(
    body: impl Into<String>,
    attributes: Vec<super::types::KeyValue>,
    trace_id: Option<String>,
    span_id: Option<String>,
) -> LogRecord {
    let now = super::convert::now_unix_nanos().to_string();
    LogRecord {
        time_unix_nano: now.clone(),
        observed_time_unix_nano: now,
        severity_number: SeverityNumber::Info,
        severity_text: "INFO".to_string(),
        body: Some(AnyValue::StringValue(body.into())),
        attributes,
        trace_id,
        span_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_urls_follow_the_otel_http_convention() {
        let config = OtlpConfig::new("http://collector:4318/");
        assert_eq!(
            config.signal_url("traces"),
            "http://collector:4318/v1/traces"
        );
        let explicit = OtlpConfig::new("http://collector:4318/v1/traces");
        assert_eq!(
            explicit.signal_url("traces"),
            "http://collector:4318/v1/traces"
        );
    }

    #[test]
    fn grpc_urls_use_the_collector_service_paths() {
        let config = OtlpConfig::new("http://collector:4317").with_protocol(OtlpProtocol::Grpc);
        assert_eq!(
            config.grpc_url("traces"),
            "http://collector:4317/opentelemetry.proto.collector.trace.v1.TraceService/Export"
        );
        assert_eq!(
            config.grpc_url("logs"),
            "http://collector:4317/opentelemetry.proto.collector.logs.v1.LogsService/Export"
        );
        assert_eq!(
            config.grpc_url("metrics"),
            "http://collector:4317/opentelemetry.proto.collector.metrics.v1.MetricsService/Export"
        );
    }

    #[test]
    fn protocol_parsing_is_forgiving_and_defaults_to_protobuf() {
        assert_eq!(
            OtlpProtocol::from_str_or_default("grpc"),
            OtlpProtocol::Grpc
        );
        assert_eq!(
            OtlpProtocol::from_str_or_default("HTTP/JSON"),
            OtlpProtocol::HttpJson
        );
        assert_eq!(
            OtlpProtocol::from_str_or_default("unknown"),
            OtlpProtocol::HttpProtobuf
        );
    }

    #[test]
    fn enqueue_and_flush_keep_counts() {
        let exporter = OtlpExporter::new(OtlpConfig::new("http://127.0.0.1:1")).expect("exporter");
        assert_eq!(exporter.buffered(), (0, 0, 0));
        let measurement = RecordedTelemetryMeasurement {
            kind: crate::index::MetricKind::Counter,
            name: "kiana.test".into(),
            description: None,
            unit: None,
            boundaries: None,
            value: 1.0,
            attributes: IndexMap::new(),
            context: None,
        };
        assert_eq!(exporter.enqueue_metrics([measurement]), 1);
        assert_eq!(exporter.buffered(), (0, 0, 1));
    }

    #[test]
    fn unreachable_exporter_returns_an_error_without_panicking() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let exporter =
            OtlpExporter::new(OtlpConfig::new("http://127.0.0.1:9").with_timeout_ms(200))
                .expect("exporter");
        let span = crate::index::StoredSpan {
            id: "scope:1".into(),
            trace_id: "scope:trace".into(),
            name: "root".into(),
            started_ms: 1.0,
            ended_ms: 2.0,
            ..crate::index::StoredSpan::default()
        };
        let outcome = runtime.block_on(exporter.export_spans_best_effort(&[span]));
        assert!(!outcome.is_success());
        assert_eq!(outcome.attempted_requests, 1);
        assert!(!outcome.failures.is_empty());
    }
}
