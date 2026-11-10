//! OTLP 数据模型（JSON 形态）。
//!
//! 这些结构体对应 OpenTelemetry Protocol 的 JSON 编码（`application/json`）；字段名、
//! 枚举字面量与 int64 的字符串写法都按 OTLP 规范：
//!
//! - `traceId`/`spanId`/`parentSpanId` 是小写 hex 字符串；
//! - `startTimeUnixNano`/`endTimeUnixNano`/`asInt`/`count`/`bucketCounts` 等 64 位整数在
//!   JSON 中是**字符串**（proto3 JSON 映射）；
//! - 枚举写完整常量名（`SPAN_KIND_INTERNAL`、`STATUS_CODE_OK`、
//!   `AGGREGATION_TEMPORALITY_DELTA`）。
//!
//! 结构体同时用于 HTTP/JSON 传输与单元测试的黄金断言；gRPC 传输走 [`super::proto`] 的
//! 手写 protobuf 编码，二者由同一份数据模型驱动。

use serde::{Deserialize, Serialize};

/// `AnyValue` 的一个元素。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnyValue {
    /// 字符串。
    StringValue(String),
    /// 布尔。
    BoolValue(bool),
    /// int64（JSON 中写成字符串）。
    IntValue(String),
    /// double。
    DoubleValue(f64),
    /// 数组。
    ArrayValue(ArrayValue),
    /// 键值列表。
    KvlistValue(KvlistValue),
}

/// `ArrayValue`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrayValue {
    /// 元素。
    #[serde(default)]
    pub values: Vec<AnyValue>,
}

/// `KeyValueList`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KvlistValue {
    /// 条目。
    #[serde(default)]
    pub values: Vec<KeyValue>,
}

/// `KeyValue`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    /// 键。
    pub key: String,
    /// 值。
    pub value: AnyValue,
}

/// `Resource`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    /// 资源属性。
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
}

/// `InstrumentationScope`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationScope {
    /// scope 名。
    #[serde(default)]
    pub name: String,
    /// scope 版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Span 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanKind {
    /// 未指定。
    #[serde(rename = "SPAN_KIND_UNSPECIFIED")]
    Unspecified,
    /// 内部操作。
    #[serde(rename = "SPAN_KIND_INTERNAL")]
    Internal,
    /// 服务端。
    #[serde(rename = "SPAN_KIND_SERVER")]
    Server,
    /// 客户端。
    #[serde(rename = "SPAN_KIND_CLIENT")]
    Client,
    /// 生产者。
    #[serde(rename = "SPAN_KIND_PRODUCER")]
    Producer,
    /// 消费者。
    #[serde(rename = "SPAN_KIND_CONSUMER")]
    Consumer,
}

impl SpanKind {
    /// protobuf 枚举值。
    pub fn as_i32(self) -> i32 {
        match self {
            SpanKind::Unspecified => 0,
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
            SpanKind::Producer => 4,
            SpanKind::Consumer => 5,
        }
    }
}

/// Span 状态码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusCode {
    /// 未设置。
    #[serde(rename = "STATUS_CODE_UNSET")]
    Unset,
    /// 正常。
    #[serde(rename = "STATUS_CODE_OK")]
    Ok,
    /// 错误。
    #[serde(rename = "STATUS_CODE_ERROR")]
    Error,
}

impl StatusCode {
    /// protobuf 枚举值。
    pub fn as_i32(self) -> i32 {
        match self {
            StatusCode::Unset => 0,
            StatusCode::Ok => 1,
            StatusCode::Error => 2,
        }
    }
}

/// `Status`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// 状态码。
    #[serde(default)]
    pub code: Option<StatusCode>,
    /// 错误消息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            code: Some(StatusCode::Unset),
            message: None,
        }
    }
}

/// `Span.Event`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// 事件时间（纳秒，字符串）。
    pub time_unix_nano: String,
    /// 事件名。
    pub name: String,
    /// 属性。
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
}

/// `Span`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    /// trace id（32 hex）。
    pub trace_id: String,
    /// span id（16 hex）。
    pub span_id: String,
    /// tracestate。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
    /// 父 span id（16 hex）；根 span 缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// span 名。
    pub name: String,
    /// 种类。
    pub kind: SpanKind,
    /// 起点（纳秒，字符串）。
    pub start_time_unix_nano: String,
    /// 终点（纳秒，字符串）。
    pub end_time_unix_nano: String,
    /// 属性。
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    /// 事件。
    #[serde(default)]
    pub events: Vec<Event>,
    /// 状态。
    #[serde(default)]
    pub status: Status,
}

/// `ScopeSpans`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpans {
    /// scope。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstrumentationScope>,
    /// spans。
    #[serde(default)]
    pub spans: Vec<Span>,
}

/// `ResourceSpans`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpans {
    /// resource。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// scope spans。
    #[serde(default)]
    pub scope_spans: Vec<ScopeSpans>,
}

/// `ExportTraceServiceRequest`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportTraceServiceRequest {
    /// resource spans。
    #[serde(default)]
    pub resource_spans: Vec<ResourceSpans>,
}

/// severity number。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeverityNumber {
    /// 未指定。
    #[serde(rename = "SEVERITY_NUMBER_UNSPECIFIED")]
    Unspecified,
    /// trace。
    #[serde(rename = "SEVERITY_NUMBER_TRACE")]
    Trace,
    /// debug。
    #[serde(rename = "SEVERITY_NUMBER_DEBUG")]
    Debug,
    /// info。
    #[serde(rename = "SEVERITY_NUMBER_INFO")]
    Info,
    /// warn。
    #[serde(rename = "SEVERITY_NUMBER_WARN")]
    Warn,
    /// error。
    #[serde(rename = "SEVERITY_NUMBER_ERROR")]
    Error,
    /// fatal。
    #[serde(rename = "SEVERITY_NUMBER_FATAL")]
    Fatal,
}

impl SeverityNumber {
    /// protobuf 枚举值。
    pub fn as_i32(self) -> i32 {
        match self {
            SeverityNumber::Unspecified => 0,
            SeverityNumber::Trace => 1,
            SeverityNumber::Debug => 5,
            SeverityNumber::Info => 9,
            SeverityNumber::Warn => 13,
            SeverityNumber::Error => 17,
            SeverityNumber::Fatal => 21,
        }
    }
}

/// `LogRecord`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    /// 事件时间（纳秒，字符串）。
    pub time_unix_nano: String,
    /// 观察时间（纳秒，字符串）。
    pub observed_time_unix_nano: String,
    /// severity。
    pub severity_number: SeverityNumber,
    /// severity 文本。
    pub severity_text: String,
    /// 正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<AnyValue>,
    /// 属性。
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    /// 关联 trace id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// 关联 span id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

/// `ScopeLogs`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeLogs {
    /// scope。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstrumentationScope>,
    /// 日志记录。
    #[serde(default)]
    pub log_records: Vec<LogRecord>,
}

/// `ResourceLogs`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLogs {
    /// resource。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// scope logs。
    #[serde(default)]
    pub scope_logs: Vec<ScopeLogs>,
}

/// `ExportLogsServiceRequest`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportLogsServiceRequest {
    /// resource logs。
    #[serde(default)]
    pub resource_logs: Vec<ResourceLogs>,
}

/// `NumberDataPoint`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberDataPoint {
    /// 属性。
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    /// 起点（纳秒，字符串）。
    pub start_time_unix_nano: String,
    /// 观测时间（纳秒，字符串）。
    pub time_unix_nano: String,
    /// double 值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_double: Option<f64>,
    /// int64 值（字符串；与 `as_double` 互斥）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_int: Option<String>,
}

/// `HistogramDataPoint`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistogramDataPoint {
    /// 属性。
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    /// 起点（纳秒，字符串）。
    pub start_time_unix_nano: String,
    /// 观测时间（纳秒，字符串）。
    pub time_unix_nano: String,
    /// 计数（uint64 字符串）。
    pub count: String,
    /// 总和。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sum: Option<f64>,
    /// 显式桶边界。
    #[serde(default)]
    pub explicit_bounds: Vec<f64>,
    /// 每桶计数（uint64 字符串）。
    #[serde(default)]
    pub bucket_counts: Vec<String>,
}

/// `Gauge`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gauge {
    /// 数据点。
    #[serde(default)]
    pub data_points: Vec<NumberDataPoint>,
}

/// 聚合时间性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggregationTemporality {
    /// 未指定。
    #[serde(rename = "AGGREGATION_TEMPORALITY_UNSPECIFIED")]
    Unspecified,
    /// delta。
    #[serde(rename = "AGGREGATION_TEMPORALITY_DELTA")]
    Delta,
    /// cumulative。
    #[serde(rename = "AGGREGATION_TEMPORALITY_CUMULATIVE")]
    Cumulative,
}

impl AggregationTemporality {
    /// protobuf 枚举值。
    pub fn as_i32(self) -> i32 {
        match self {
            AggregationTemporality::Unspecified => 0,
            AggregationTemporality::Delta => 1,
            AggregationTemporality::Cumulative => 2,
        }
    }
}

/// `Sum`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sum {
    /// 数据点。
    #[serde(default)]
    pub data_points: Vec<NumberDataPoint>,
    /// 时间性。
    pub aggregation_temporality: AggregationTemporality,
    /// 是否单调。
    pub is_monotonic: bool,
}

/// `Histogram`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Histogram {
    /// 数据点。
    #[serde(default)]
    pub data_points: Vec<HistogramDataPoint>,
    /// 时间性。
    pub aggregation_temporality: AggregationTemporality,
}

/// `Metric`（oneof data 用可选字段表达）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    /// 名称。
    pub name: String,
    /// 描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 单位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// gauge。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gauge: Option<Gauge>,
    /// sum。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sum: Option<Sum>,
    /// histogram。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub histogram: Option<Histogram>,
}

/// `ScopeMetrics`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeMetrics {
    /// scope。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstrumentationScope>,
    /// metrics。
    #[serde(default)]
    pub metrics: Vec<Metric>,
}

/// `ResourceMetrics`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceMetrics {
    /// resource。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// scope metrics。
    #[serde(default)]
    pub scope_metrics: Vec<ScopeMetrics>,
}

/// `ExportMetricsServiceRequest`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportMetricsServiceRequest {
    /// resource metrics。
    #[serde(default)]
    pub resource_metrics: Vec<ResourceMetrics>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn any_value_uses_the_proto_json_camel_case_shape() {
        assert_eq!(
            serde_json::to_value(AnyValue::StringValue("x".into())).unwrap(),
            json!({"stringValue": "x"})
        );
        assert_eq!(
            serde_json::to_value(AnyValue::IntValue("42".into())).unwrap(),
            json!({"intValue": "42"})
        );
        assert_eq!(
            serde_json::to_value(AnyValue::DoubleValue(1.5)).unwrap(),
            json!({"doubleValue": 1.5})
        );
        assert_eq!(
            serde_json::to_value(AnyValue::BoolValue(true)).unwrap(),
            json!({"boolValue": true})
        );
        assert_eq!(
            serde_json::to_value(AnyValue::ArrayValue(ArrayValue {
                values: vec![AnyValue::StringValue("a".into())]
            }))
            .unwrap(),
            json!({"arrayValue": {"values": [{"stringValue": "a"}]}})
        );
    }

    #[test]
    fn enums_serialize_to_otlp_constants() {
        assert_eq!(
            serde_json::to_value(SpanKind::Internal).unwrap(),
            json!("SPAN_KIND_INTERNAL")
        );
        assert_eq!(
            serde_json::to_value(StatusCode::Error).unwrap(),
            json!("STATUS_CODE_ERROR")
        );
        assert_eq!(
            serde_json::to_value(AggregationTemporality::Delta).unwrap(),
            json!("AGGREGATION_TEMPORALITY_DELTA")
        );
        assert_eq!(
            serde_json::to_value(SeverityNumber::Warn).unwrap(),
            json!("SEVERITY_NUMBER_WARN")
        );
    }

    #[test]
    fn optional_fields_are_omitted_not_null() {
        let span = Span {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            trace_state: None,
            parent_span_id: None,
            name: "root".into(),
            kind: SpanKind::Internal,
            start_time_unix_nano: "1".into(),
            end_time_unix_nano: "2".into(),
            attributes: Vec::new(),
            events: Vec::new(),
            status: Status::default(),
        };
        let value = serde_json::to_value(&span).unwrap();
        assert!(!value.as_object().unwrap().contains_key("traceState"));
        assert!(!value.as_object().unwrap().contains_key("parentSpanId"));
        assert_eq!(value["status"]["code"], json!("STATUS_CODE_UNSET"));
    }
}
