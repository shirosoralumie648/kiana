//! 手写 OTLP protobuf 编码（gRPC 与 HTTP/protobuf 共用）。
//!
//! 不引入 `prost`/`tonic`：本 crate 需要编码的消息子集很小（traces/logs/metrics 的
//! Resource/Scope/Span/LogRecord/Metric 与 common 的 AnyValue/KeyValue），手写 varint 与
//! length-delimited 编码既能保持依赖面小，也能逐字节测试。
//!
//! 字段号来自 opentelemetry-proto v1（见各函数注释）。这里只编码 Kiana 实际产生的字段，
//! 未用的字段（links / exemplars / flags / schema_url）不写——protobuf 允许缺省字段。
//!
//! gRPC 传输的请求体是 5 字节前缀（压缩标志 0 + 大端 u32 长度）+ 消息字节，见
//! [`grpc_frame`]。

use super::types::{
    AnyValue, Event, ExportLogsServiceRequest, ExportMetricsServiceRequest,
    ExportTraceServiceRequest, HistogramDataPoint, KeyValue, LogRecord, Metric, NumberDataPoint,
    Resource, ResourceMetrics, ResourceSpans, ScopeSpans, Span, Status,
};

const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_LENGTH_DELIMITED: u8 = 2;

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn write_tag(out: &mut Vec<u8>, field: u32, wire: u8) {
    write_varint(out, (u64::from(field) << 3) | u64::from(wire));
}

fn write_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    write_tag(out, field, WIRE_VARINT);
    write_varint(out, value);
}

fn write_bool_field(out: &mut Vec<u8>, field: u32, value: bool) {
    write_varint_field(out, field, u64::from(value));
}

fn write_bytes_field(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    write_tag(out, field, WIRE_LENGTH_DELIMITED);
    write_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn write_string_field(out: &mut Vec<u8>, field: u32, value: &str) {
    write_bytes_field(out, field, value.as_bytes());
}

fn write_message_field(out: &mut Vec<u8>, field: u32, message: &[u8]) {
    write_bytes_field(out, field, message);
}

fn write_fixed64_field(out: &mut Vec<u8>, field: u32, value: u64) {
    write_tag(out, field, WIRE_FIXED64);
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_double_field(out: &mut Vec<u8>, field: u32, value: f64) {
    write_fixed64_field(out, field, value.to_bits());
}

fn write_packed_doubles(out: &mut Vec<u8>, field: u32, values: &[f64]) {
    let mut payload = Vec::with_capacity(values.len() * 8);
    for value in values {
        payload.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    write_bytes_field(out, field, &payload);
}

fn write_packed_varints(out: &mut Vec<u8>, field: u32, values: &[u64]) {
    let mut payload = Vec::new();
    for value in values {
        write_varint(&mut payload, *value);
    }
    write_bytes_field(out, field, &payload);
}

fn parse_hex(text: &str) -> Vec<u8> {
    if text.len() % 2 != 0 {
        return Vec::new();
    }
    (0..text.len() / 2)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap_or(0))
        .collect()
}

fn parse_u64(text: &str) -> u64 {
    text.parse::<u64>().unwrap_or(0)
}

fn parse_i64(text: &str) -> i64 {
    text.parse::<i64>().unwrap_or(0)
}

/// common.v1.AnyValue（oneof 字段号 1..7）。
fn encode_any_value(value: &AnyValue) -> Vec<u8> {
    let mut out = Vec::new();
    match value {
        AnyValue::StringValue(text) => write_string_field(&mut out, 1, text),
        AnyValue::BoolValue(flag) => write_bool_field(&mut out, 2, *flag),
        AnyValue::IntValue(text) => {
            // int_value 是 int64（varint，负数用二进制补码）。
            write_varint_field(&mut out, 3, parse_i64(text) as u64);
        }
        AnyValue::DoubleValue(number) => write_double_field(&mut out, 4, *number),
        AnyValue::ArrayValue(array) => {
            let mut payload = Vec::new();
            for item in &array.values {
                write_message_field(&mut payload, 1, &encode_any_value(item));
            }
            write_message_field(&mut out, 5, &payload);
        }
        AnyValue::KvlistValue(list) => {
            let mut payload = Vec::new();
            for item in &list.values {
                write_message_field(&mut payload, 1, &encode_key_value(item));
            }
            write_message_field(&mut out, 6, &payload);
        }
    }
    out
}

fn encode_key_value(kv: &KeyValue) -> Vec<u8> {
    let mut out = Vec::new();
    write_string_field(&mut out, 1, &kv.key);
    write_message_field(&mut out, 2, &encode_any_value(&kv.value));
    out
}

fn encode_resource(resource: &Resource) -> Vec<u8> {
    let mut out = Vec::new();
    for attribute in &resource.attributes {
        write_message_field(&mut out, 1, &encode_key_value(attribute));
    }
    out
}

fn encode_scope(scope: &super::types::InstrumentationScope) -> Vec<u8> {
    let mut out = Vec::new();
    write_string_field(&mut out, 1, &scope.name);
    if let Some(version) = &scope.version {
        write_string_field(&mut out, 2, version);
    }
    out
}

fn encode_event(event: &Event) -> Vec<u8> {
    let mut out = Vec::new();
    write_fixed64_field(&mut out, 1, parse_u64(&event.time_unix_nano));
    write_string_field(&mut out, 2, &event.name);
    for attribute in &event.attributes {
        write_message_field(&mut out, 3, &encode_key_value(attribute));
    }
    out
}

fn encode_status(status: &Status) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(message) = &status.message {
        write_string_field(&mut out, 2, message);
    }
    if let Some(code) = status.code {
        write_varint_field(&mut out, 3, code.as_i32() as u64);
    }
    out
}

fn encode_span(span: &Span) -> Vec<u8> {
    let mut out = Vec::new();
    write_bytes_field(&mut out, 1, &parse_hex(&span.trace_id));
    write_bytes_field(&mut out, 2, &parse_hex(&span.span_id));
    if let Some(trace_state) = &span.trace_state {
        write_string_field(&mut out, 3, trace_state);
    }
    if let Some(parent) = &span.parent_span_id {
        write_bytes_field(&mut out, 4, &parse_hex(parent));
    }
    write_string_field(&mut out, 5, &span.name);
    write_varint_field(&mut out, 6, span.kind.as_i32() as u64);
    write_fixed64_field(&mut out, 7, parse_u64(&span.start_time_unix_nano));
    write_fixed64_field(&mut out, 8, parse_u64(&span.end_time_unix_nano));
    for attribute in &span.attributes {
        write_message_field(&mut out, 9, &encode_key_value(attribute));
    }
    for event in &span.events {
        write_message_field(&mut out, 11, &encode_event(event));
    }
    write_message_field(&mut out, 15, &encode_status(&span.status));
    out
}

/// `ExportTraceServiceRequest`（collector/trace/v1）。
pub fn encode_trace_request(request: &ExportTraceServiceRequest) -> Vec<u8> {
    let mut out = Vec::new();
    for resource_spans in &request.resource_spans {
        write_message_field(&mut out, 1, &encode_resource_spans(resource_spans));
    }
    out
}

fn encode_resource_spans(resource_spans: &ResourceSpans) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(resource) = &resource_spans.resource {
        write_message_field(&mut out, 1, &encode_resource(resource));
    }
    for scope_spans in &resource_spans.scope_spans {
        write_message_field(&mut out, 2, &encode_scope_spans(scope_spans));
    }
    out
}

fn encode_scope_spans(scope_spans: &ScopeSpans) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(scope) = &scope_spans.scope {
        write_message_field(&mut out, 1, &encode_scope(scope));
    }
    for span in &scope_spans.spans {
        write_message_field(&mut out, 2, &encode_span(span));
    }
    out
}

fn encode_log_record(record: &LogRecord) -> Vec<u8> {
    let mut out = Vec::new();
    write_fixed64_field(&mut out, 1, parse_u64(&record.time_unix_nano));
    write_varint_field(&mut out, 2, record.severity_number.as_i32() as u64);
    write_string_field(&mut out, 3, &record.severity_text);
    if let Some(body) = &record.body {
        write_message_field(&mut out, 5, &encode_any_value(body));
    }
    for attribute in &record.attributes {
        write_message_field(&mut out, 6, &encode_key_value(attribute));
    }
    if let Some(trace_id) = &record.trace_id {
        write_bytes_field(&mut out, 9, &parse_hex(trace_id));
    }
    if let Some(span_id) = &record.span_id {
        write_bytes_field(&mut out, 10, &parse_hex(span_id));
    }
    write_fixed64_field(&mut out, 11, parse_u64(&record.observed_time_unix_nano));
    out
}

/// `ExportLogsServiceRequest`（collector/logs/v1）。
pub fn encode_logs_request(request: &ExportLogsServiceRequest) -> Vec<u8> {
    let mut out = Vec::new();
    for resource_logs in &request.resource_logs {
        let mut encoded = Vec::new();
        if let Some(resource) = &resource_logs.resource {
            write_message_field(&mut encoded, 1, &encode_resource(resource));
        }
        for scope_logs in &resource_logs.scope_logs {
            let mut scope = Vec::new();
            if let Some(scope_info) = &scope_logs.scope {
                write_message_field(&mut scope, 1, &encode_scope(scope_info));
            }
            for record in &scope_logs.log_records {
                write_message_field(&mut scope, 2, &encode_log_record(record));
            }
            write_message_field(&mut encoded, 2, &scope);
        }
        write_message_field(&mut out, 1, &encoded);
    }
    out
}

fn encode_number_point(point: &NumberDataPoint) -> Vec<u8> {
    let mut out = Vec::new();
    write_fixed64_field(&mut out, 2, parse_u64(&point.start_time_unix_nano));
    write_fixed64_field(&mut out, 3, parse_u64(&point.time_unix_nano));
    if let Some(value) = point.as_double {
        write_double_field(&mut out, 4, value);
    } else if let Some(value) = &point.as_int {
        write_fixed64_field(&mut out, 6, parse_i64(value) as u64);
    }
    for attribute in &point.attributes {
        write_message_field(&mut out, 7, &encode_key_value(attribute));
    }
    out
}

fn encode_histogram_point(point: &HistogramDataPoint) -> Vec<u8> {
    let mut out = Vec::new();
    write_fixed64_field(&mut out, 2, parse_u64(&point.start_time_unix_nano));
    write_fixed64_field(&mut out, 3, parse_u64(&point.time_unix_nano));
    write_varint_field(&mut out, 4, parse_u64(&point.count));
    if let Some(sum) = point.sum {
        write_double_field(&mut out, 5, sum);
    }
    if !point.explicit_bounds.is_empty() {
        write_packed_doubles(&mut out, 6, &point.explicit_bounds);
    }
    let counts: Vec<u64> = point
        .bucket_counts
        .iter()
        .map(|count| parse_u64(count))
        .collect();
    if !counts.is_empty() {
        write_packed_varints(&mut out, 7, &counts);
    }
    for attribute in &point.attributes {
        write_message_field(&mut out, 9, &encode_key_value(attribute));
    }
    out
}

fn encode_metric(metric: &Metric) -> Vec<u8> {
    let mut out = Vec::new();
    write_string_field(&mut out, 1, &metric.name);
    if let Some(description) = &metric.description {
        write_string_field(&mut out, 2, description);
    }
    if let Some(unit) = &metric.unit {
        write_string_field(&mut out, 3, unit);
    }
    if let Some(gauge) = &metric.gauge {
        let mut payload = Vec::new();
        for point in &gauge.data_points {
            write_message_field(&mut payload, 1, &encode_number_point(point));
        }
        write_message_field(&mut out, 5, &payload);
    }
    if let Some(sum) = &metric.sum {
        let mut payload = Vec::new();
        for point in &sum.data_points {
            write_message_field(&mut payload, 1, &encode_number_point(point));
        }
        write_varint_field(&mut payload, 2, sum.aggregation_temporality.as_i32() as u64);
        write_bool_field(&mut payload, 3, sum.is_monotonic);
        write_message_field(&mut out, 7, &payload);
    }
    if let Some(histogram) = &metric.histogram {
        let mut payload = Vec::new();
        for point in &histogram.data_points {
            write_message_field(&mut payload, 1, &encode_histogram_point(point));
        }
        write_varint_field(
            &mut payload,
            2,
            histogram.aggregation_temporality.as_i32() as u64,
        );
        write_message_field(&mut out, 9, &payload);
    }
    out
}

/// `ExportMetricsServiceRequest`（collector/metrics/v1）。
pub fn encode_metrics_request(request: &ExportMetricsServiceRequest) -> Vec<u8> {
    let mut out = Vec::new();
    for resource_metrics in &request.resource_metrics {
        write_message_field(&mut out, 1, &encode_resource_metrics(resource_metrics));
    }
    out
}

fn encode_resource_metrics(resource_metrics: &ResourceMetrics) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(resource) = &resource_metrics.resource {
        write_message_field(&mut out, 1, &encode_resource(resource));
    }
    for scope_metrics in &resource_metrics.scope_metrics {
        let mut scope = Vec::new();
        if let Some(scope_info) = &scope_metrics.scope {
            write_message_field(&mut scope, 1, &encode_scope(scope_info));
        }
        for metric in &scope_metrics.metrics {
            write_message_field(&mut scope, 2, &encode_metric(metric));
        }
        write_message_field(&mut out, 2, &scope);
    }
    out
}

/// gRPC 消息帧：1 字节压缩标志（0）+ 大端 u32 长度 + 消息。
pub fn grpc_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// 供 tests 使用的固定 32 位字段读取（仅验证编码前缀）。
#[cfg(test)]
fn read_varint(bytes: &[u8], offset: &mut usize) -> u64 {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = bytes[*offset];
        *offset += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_follow_the_protobuf_encoding() {
        let mut out = Vec::new();
        write_varint(&mut out, 150);
        assert_eq!(out, vec![0x96, 0x01]);
        let mut out = Vec::new();
        write_varint(&mut out, 300);
        assert_eq!(out, vec![0xac, 0x02]);
        let mut out = Vec::new();
        write_varint(&mut out, 0);
        assert_eq!(out, vec![0x00]);
    }

    #[test]
    fn grpc_frame_has_the_five_byte_prefix() {
        let frame = grpc_frame(b"abc");
        assert_eq!(&frame[..5], &[0, 0, 0, 0, 3]);
        assert_eq!(&frame[5..], b"abc");
    }

    #[test]
    fn trace_request_encodes_nested_spans_and_attributes() {
        let span = Span {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: "00f067aa0ba902b7".into(),
            trace_state: Some("vendor=value".into()),
            parent_span_id: None,
            name: "chat gpt".into(),
            kind: super::super::types::SpanKind::Internal,
            start_time_unix_nano: "1".into(),
            end_time_unix_nano: "2".into(),
            attributes: vec![KeyValue {
                key: "gen_ai.operation.name".into(),
                value: AnyValue::StringValue("chat".into()),
            }],
            events: Vec::new(),
            status: Status::default(),
        };
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".into(),
                        value: AnyValue::StringValue("kiana".into()),
                    }],
                }),
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![span],
                }],
            }],
        };
        let encoded = encode_trace_request(&request);
        assert!(!encoded.is_empty());
        // 顶层 field 1（ResourceSpans），wire 2。
        let mut offset = 0;
        let tag = read_varint(&encoded, &mut offset);
        assert_eq!(tag, (1 << 3) | 2);
        let length = read_varint(&encoded, &mut offset) as usize;
        assert_eq!(length + offset, encoded.len());
        // 已知字符串在字节流里以 UTF-8 出现。
        let haystack = String::from_utf8_lossy(&encoded);
        assert!(haystack.contains("gen_ai.operation.name"));
        assert!(haystack.contains("chat gpt"));
        assert!(haystack.contains("vendor=value"));
    }

    #[test]
    fn metrics_and_logs_encode_non_empty_payloads() {
        use super::super::types::{
            AggregationTemporality, ExportLogsServiceRequest, Gauge, Metric, ResourceLogs,
            ResourceMetrics, ScopeLogs, ScopeMetrics, SeverityNumber,
        };
        let metric = Metric {
            name: "kiana.test".into(),
            description: None,
            unit: Some("ms".into()),
            gauge: Some(Gauge {
                data_points: Vec::new(),
            }),
            sum: None,
            histogram: None,
        };
        let metrics = encode_metrics_request(&ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: None,
                scope_metrics: vec![ScopeMetrics {
                    scope: None,
                    metrics: vec![metric],
                }],
            }],
        });
        assert!(!metrics.is_empty());
        assert!(String::from_utf8_lossy(&metrics).contains("kiana.test"));

        let logs = encode_logs_request(&ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        time_unix_nano: "1".into(),
                        observed_time_unix_nano: "1".into(),
                        severity_number: SeverityNumber::Info,
                        severity_text: "INFO".into(),
                        body: Some(AnyValue::StringValue("hello".into())),
                        attributes: Vec::new(),
                        trace_id: None,
                        span_id: None,
                    }],
                }],
            }],
        });
        assert!(!logs.is_empty());
        assert!(String::from_utf8_lossy(&logs).contains("hello"));
        let _ = AggregationTemporality::Delta;
    }
}
