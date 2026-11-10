//! OTLP（OpenTelemetry Protocol）导出：traces / logs / metrics 的 JSON 与 protobuf 编码、
//! HTTP 与 gRPC 传输、缓冲与 flush。
//!
//! 模块划分：
//!
//! - [`types`]：OTLP JSON 数据模型（proto3 JSON 映射）；
//! - [`convert`]：Kiana `StoredSpan` / 内存测量 → OTLP；
//! - [`proto`]：手写 protobuf 编码与 gRPC 帧；
//! - [`exporter`]：`OtlpExporter` 与 `OtlpConfig`，三种传输、缓冲、best-effort flush。
//!
//! 这个模块只负责「把已经记录的数据导出去」；记录本身仍由
//! `TelemetryContext`/`TelemetrySpan`/`TelemetryMetrics` 契约驱动。导出失败只返回错误，
//! 不改变 agent 的执行结果。

pub mod convert;
pub mod exporter;
pub mod proto;
pub mod types;

pub use convert::{
    INSTRUMENTATION_SCOPE, attribute_value, attributes, derive_span_id, derive_trace_id,
    double_key_value, gauge_metric, int_key_value, kvlist_key_value, metrics_request,
    now_unix_nanos, resource, span, string_key_value, trace_request,
};
pub use exporter::{ExportOutcome, OtlpConfig, OtlpError, OtlpExporter, OtlpProtocol, log_record};
pub use types::{
    AggregationTemporality, AnyValue, ArrayValue, Event, ExportLogsServiceRequest,
    ExportMetricsServiceRequest, ExportTraceServiceRequest, Gauge, Histogram, HistogramDataPoint,
    InstrumentationScope, KeyValue, KvlistValue, LogRecord, Metric, NumberDataPoint, Resource,
    ResourceLogs, ResourceMetrics, ResourceSpans, ScopeLogs, ScopeMetrics, ScopeSpans,
    SeverityNumber, Span, SpanKind, Status, StatusCode, Sum,
};
