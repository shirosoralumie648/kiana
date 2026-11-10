//! 从 Kiana 遥测快照到 OTLP 数据模型的转换。
//!
//! - [`trace_request`]：`StoredSpan`（已结算 span）→ `ExportTraceServiceRequest`；
//! - [`metrics_request`]：内存适配器的原始测量 → `ExportMetricsServiceRequest`；
//! - [`resource`]：`service.name` / SDK 标识等资源属性。
//!
//! # trace/span id
//!
//! 内存适配器的 trace id 是 `"<scope>:<trace>"`、span id 是 `"<scope>:<n>"` 文本，不是
//! W3C hex。导出时必须换算成 16/8 字节 hex：
//!
//! - 本身已经是合法 32/16 位小写 hex 且非零 → 原样使用；
//! - 否则用 FNV-1a（两个不同种子拼成 16 字节；span 用一个种子取 8 字节）做**确定性**
//!   映射：同一 trace 文本得到同一 trace id，父子 span 的关联保持不变；
//! - 结果保证非全零。
//!
//! 这不是密码学哈希；它只要求稳定与低碰撞，用于把进程内标识映射到 OTLP 的字节形状。

use std::collections::BTreeMap;

use crate::index::MetricKind;
use crate::index::{AttributeValue, SpanAttributes, SpanStatus, StoredSpan};
use crate::memory::RecordedTelemetryMeasurement;
use crate::w3c::TraceId;

use super::types::{
    AggregationTemporality, AnyValue, ArrayValue, Event as OtlpEvent, ExportMetricsServiceRequest,
    ExportTraceServiceRequest, Gauge, Histogram as OtlpHistogram, HistogramDataPoint,
    InstrumentationScope, KeyValue, KvlistValue, Metric, NumberDataPoint, Resource,
    ResourceMetrics, ResourceSpans, ScopeMetrics, ScopeSpans, Span, SpanKind, Status, StatusCode,
};

/// SDK scope 名。
pub const INSTRUMENTATION_SCOPE: &str = "kiana-telemetry";

/// 当前 UNIX 纳秒（无法表示时返回 0）。
pub fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ seed;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// 把任意文本映射为 32 位 hex trace id。
pub fn derive_trace_id(text: &str) -> String {
    if let Some(trace_id) = TraceId::from_hex(text) {
        return trace_id.to_hex();
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&fnv1a64(text.as_bytes(), 0).to_be_bytes());
    bytes[8..].copy_from_slice(&fnv1a64(text.as_bytes(), 0x9e37_79b9_7f4a_7c15).to_be_bytes());
    if bytes.iter().all(|byte| *byte == 0) {
        bytes[15] = 1;
    }
    hex_bytes(&bytes)
}

/// 把任意文本映射为 16 位 hex span id。
pub fn derive_span_id(text: &str) -> String {
    if text.len() == 16
        && text
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        && text.chars().any(|character| character != '0')
    {
        return text.to_string();
    }
    let mut bytes = fnv1a64(text.as_bytes(), 2).to_be_bytes();
    if bytes.iter().all(|byte| *byte == 0) {
        bytes[7] = 1;
    }
    hex_bytes(&bytes)
}

fn millis_to_nanos(ms: f64) -> u64 {
    if !ms.is_finite() || ms <= 0.0 {
        return 0;
    }
    // 先把「整毫秒」与「小数毫秒」分开：1.7e12 ms 乘 1e6 会超过 f64 的精确整数范围，
    // 直接乘会在纳秒位丢精度（1700000000250.0 * 1e6 → ...999872）。
    let whole_ms = ms.trunc().min(u64::MAX as f64) as u64;
    let fraction_nanos = (ms.fract() * 1_000_000.0).round().min(999_999.0) as u64;
    whole_ms
        .saturating_mul(1_000_000)
        .saturating_add(fraction_nanos)
}

/// span 起点/终点的纳秒字符串。
fn span_times(span: &StoredSpan) -> (String, String) {
    let start = millis_to_nanos(span.started_ms);
    let end = millis_to_nanos(span.ended_ms).max(start);
    (start.to_string(), end.to_string())
}

/// 属性值转换。
pub fn attribute_value(value: &AttributeValue) -> AnyValue {
    match value {
        AttributeValue::String(text) => AnyValue::StringValue(text.clone()),
        AttributeValue::Number(number) => {
            if !number.is_finite() {
                AnyValue::StringValue(number.to_string())
            } else if number.fract() == 0.0
                && *number >= i64::MIN as f64
                && *number <= i64::MAX as f64
            {
                AnyValue::IntValue(format!("{}", *number as i64))
            } else {
                AnyValue::DoubleValue(*number)
            }
        }
        AttributeValue::Boolean(flag) => AnyValue::BoolValue(*flag),
        AttributeValue::StringArray(values) => AnyValue::ArrayValue(ArrayValue {
            values: values
                .iter()
                .map(|value| AnyValue::StringValue(value.clone()))
                .collect(),
        }),
        AttributeValue::NumberArray(values) => AnyValue::ArrayValue(ArrayValue {
            values: values
                .iter()
                .map(|value| {
                    if value.is_finite() {
                        AnyValue::DoubleValue(*value)
                    } else {
                        AnyValue::StringValue(value.to_string())
                    }
                })
                .collect(),
        }),
        AttributeValue::BooleanArray(values) => AnyValue::ArrayValue(ArrayValue {
            values: values
                .iter()
                .map(|value| AnyValue::BoolValue(*value))
                .collect(),
        }),
    }
}

/// 属性 map 转换（保持插入序）。
pub fn attributes(attributes: &SpanAttributes) -> Vec<KeyValue> {
    attributes
        .iter()
        .map(|(key, value)| KeyValue {
            key: key.clone(),
            value: attribute_value(value),
        })
        .collect()
}

/// 资源属性。
pub fn resource(service_name: &str) -> Resource {
    Resource {
        attributes: vec![
            KeyValue {
                key: "service.name".into(),
                value: AnyValue::StringValue(service_name.to_string()),
            },
            KeyValue {
                key: "telemetry.sdk.name".into(),
                value: AnyValue::StringValue(INSTRUMENTATION_SCOPE.to_string()),
            },
            KeyValue {
                key: "telemetry.sdk.language".into(),
                value: AnyValue::StringValue("rust".into()),
            },
        ],
    }
}

fn scope() -> InstrumentationScope {
    InstrumentationScope {
        name: INSTRUMENTATION_SCOPE.to_string(),
        version: None,
    }
}

fn status(span: &StoredSpan) -> Status {
    match &span.status {
        SpanStatus::Ok => Status {
            code: Some(StatusCode::Ok),
            message: None,
        },
        SpanStatus::Error { error } => Status {
            code: Some(StatusCode::Error),
            message: error.as_ref().map(|error| {
                if error.name.is_empty() {
                    error.message.clone()
                } else {
                    format!("{}: {}", error.name, error.message)
                }
            }),
        },
    }
}

/// 单个 `StoredSpan` → OTLP Span。
pub fn span(stored: &StoredSpan) -> Span {
    let (start, end) = span_times(stored);
    let start_ms: f64 = stored.started_ms;
    Span {
        trace_id: derive_trace_id(&stored.trace_id),
        span_id: derive_span_id(&stored.id),
        trace_state: None,
        parent_span_id: stored.parent_id.as_deref().map(derive_span_id),
        name: stored.name.clone(),
        kind: SpanKind::Internal,
        start_time_unix_nano: start,
        end_time_unix_nano: end,
        attributes: attributes(&stored.attributes),
        events: stored
            .events
            .iter()
            .map(|event| OtlpEvent {
                time_unix_nano: millis_to_nanos(start_ms + event.time_ms).to_string(),
                name: event.name.clone(),
                attributes: attributes(&event.attributes),
            })
            .collect(),
        status: status(stored),
    }
}

/// 一组 `StoredSpan` → `ExportTraceServiceRequest`（一个 resource/scope）。
pub fn trace_request(service_name: &str, spans: &[StoredSpan]) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(resource(service_name)),
            scope_spans: vec![ScopeSpans {
                scope: Some(scope()),
                spans: spans.iter().map(span).collect(),
            }],
        }],
    }
}

fn number_point(measurement: &RecordedTelemetryMeasurement) -> NumberDataPoint {
    let nanos = now_unix_nanos().to_string();
    NumberDataPoint {
        attributes: attributes(&measurement.attributes),
        start_time_unix_nano: nanos.clone(),
        time_unix_nano: nanos,
        as_double: Some(measurement.value),
        as_int: None,
    }
}

fn histogram_point(measurement: &RecordedTelemetryMeasurement) -> HistogramDataPoint {
    let nanos = now_unix_nanos().to_string();
    let boundaries = measurement.boundaries.clone().unwrap_or_default();
    let mut bucket_counts = vec!["0".to_string(); boundaries.len() + 1];
    let bucket = boundaries
        .iter()
        .position(|boundary| measurement.value <= *boundary)
        .unwrap_or(boundaries.len());
    bucket_counts[bucket] = "1".to_string();
    HistogramDataPoint {
        attributes: attributes(&measurement.attributes),
        start_time_unix_nano: nanos.clone(),
        time_unix_nano: nanos,
        count: "1".to_string(),
        sum: Some(measurement.value),
        explicit_bounds: boundaries,
        bucket_counts,
    }
}

/// 原始测量 → `ExportMetricsServiceRequest`。
///
/// 同名的测量合并到一个 [`Metric`]：counter/upDownCounter 用 `Sum`（delta），histogram 用
/// `Histogram`。测量本身没有时间戳，数据点时间取导出时刻的 UNIX 纳秒。
pub fn metrics_request(
    service_name: &str,
    measurements: &[RecordedTelemetryMeasurement],
) -> ExportMetricsServiceRequest {
    #[derive(Default)]
    struct Group {
        kind: Option<MetricKind>,
        description: Option<String>,
        unit: Option<String>,
        boundaries: Option<Vec<f64>>,
        number_points: Vec<NumberDataPoint>,
        histogram_points: Vec<HistogramDataPoint>,
    }

    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for measurement in measurements {
        let group = groups.entry(measurement.name.clone()).or_default();
        group.kind = Some(measurement.kind);
        group.description = measurement.description.clone();
        group.unit = measurement.unit.clone();
        group.boundaries = measurement.boundaries.clone();
        match measurement.kind {
            MetricKind::Histogram => group.histogram_points.push(histogram_point(measurement)),
            MetricKind::Counter | MetricKind::UpDownCounter => {
                group.number_points.push(number_point(measurement))
            }
        }
    }

    let metrics = groups
        .into_iter()
        .map(|(name, group)| {
            let base = Metric {
                name,
                description: group.description,
                unit: group.unit,
                gauge: None,
                sum: None,
                histogram: None,
            };
            match group.kind {
                Some(MetricKind::Histogram) => Metric {
                    histogram: Some(OtlpHistogram {
                        data_points: group.histogram_points,
                        aggregation_temporality: AggregationTemporality::Delta,
                    }),
                    ..base
                },
                Some(MetricKind::Counter) => Metric {
                    sum: Some(super::types::Sum {
                        data_points: group.number_points,
                        aggregation_temporality: AggregationTemporality::Delta,
                        is_monotonic: true,
                    }),
                    ..base
                },
                Some(MetricKind::UpDownCounter) => Metric {
                    sum: Some(super::types::Sum {
                        data_points: group.number_points,
                        aggregation_temporality: AggregationTemporality::Delta,
                        is_monotonic: false,
                    }),
                    ..base
                },
                None => base,
            }
        })
        .collect();

    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(resource(service_name)),
            scope_metrics: vec![ScopeMetrics {
                scope: Some(scope()),
                metrics,
            }],
        }],
    }
}

/// 构造一个字符串属性（供 session/log helper 使用）。
pub fn string_key_value(key: impl Into<String>, value: impl Into<String>) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::StringValue(value.into()),
    }
}

/// 构造一个 int 属性（OTLP JSON 的 int64 用字符串）。
pub fn int_key_value(key: impl Into<String>, value: i64) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::IntValue(value.to_string()),
    }
}

/// 构造一个 double 属性。
pub fn double_key_value(key: impl Into<String>, value: f64) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::DoubleValue(value),
    }
}

/// 构造键值列表属性。
pub fn kvlist_key_value(key: impl Into<String>, values: Vec<KeyValue>) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::KvlistValue(KvlistValue { values }),
    }
}

/// 构造 gauge metric（单点）。
pub fn gauge_metric(
    name: impl Into<String>,
    unit: Option<String>,
    description: Option<String>,
    value: f64,
    attributes: Vec<KeyValue>,
) -> Metric {
    let nanos = now_unix_nanos().to_string();
    Metric {
        name: name.into(),
        description,
        unit,
        gauge: Some(Gauge {
            data_points: vec![NumberDataPoint {
                attributes,
                start_time_unix_nano: nanos.clone(),
                time_unix_nano: nanos,
                as_double: Some(value),
                as_int: None,
            }],
        }),
        sum: None,
        histogram: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::StoredSpanEvent;
    use indexmap::IndexMap;

    fn stored(id: &str, trace: &str, parent: Option<&str>, name: &str) -> StoredSpan {
        let mut attributes = IndexMap::new();
        attributes.insert(
            "gen_ai.operation.name".to_string(),
            AttributeValue::String("chat".into()),
        );
        attributes.insert(
            "gen_ai.usage.input_tokens".to_string(),
            AttributeValue::Number(1234.0),
        );
        StoredSpan {
            id: id.into(),
            trace_id: trace.into(),
            parent_id: parent.map(ToString::to_string),
            name: name.into(),
            started_ms: 1_700_000_000_000.0,
            ended_ms: 1_700_000_000_250.0,
            attributes,
            events: vec![StoredSpanEvent {
                name: "first_chunk".into(),
                time_ms: 50.0,
                attributes: IndexMap::new(),
            }],
            links: Vec::new(),
            status: SpanStatus::Ok,
        }
    }

    #[test]
    fn maps_parent_and_child_spans_into_one_trace() {
        let root = stored("scope:1", "scope:trace", None, "invoke_agent");
        let child = stored("scope:2", "scope:trace", Some("scope:1"), "chat");
        let request = trace_request("kiana", &[root, child]);
        let spans = &request.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].trace_id, spans[1].trace_id);
        assert!(spans[0].parent_span_id.is_none());
        assert_eq!(
            spans[1].parent_span_id.as_deref(),
            Some(spans[0].span_id.as_str())
        );
        assert_eq!(spans[0].start_time_unix_nano, "1700000000000000000");
        assert_eq!(spans[0].end_time_unix_nano, "1700000000250000000");
        assert_eq!(spans[0].events[0].time_unix_nano, "1700000000050000000");
        assert_eq!(
            request.resource_spans[0]
                .resource
                .as_ref()
                .unwrap()
                .attributes[0]
                .value,
            AnyValue::StringValue("kiana".into())
        );
        assert_eq!(spans[0].attributes[0].key, "gen_ai.operation.name");
        assert_eq!(
            spans[0].attributes[1].value,
            AnyValue::IntValue("1234".into())
        );
    }

    #[test]
    fn ids_are_deterministic_and_non_zero() {
        let a = derive_trace_id("scope:trace");
        let b = derive_trace_id("scope:trace");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert_ne!(a, "0".repeat(32));
        assert_eq!(derive_span_id("scope:1").len(), 16);
        assert_ne!(derive_span_id("scope:1"), "0".repeat(16));
        // 已经是合法 hex 的 id 原样保留。
        assert_eq!(
            derive_trace_id("4bf92f3577b34da6a3ce929d0e0e4736"),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert_eq!(derive_span_id("00f067aa0ba902b7"), "00f067aa0ba902b7");
    }

    #[test]
    fn maps_metrics_by_kind() {
        let measurements = vec![
            RecordedTelemetryMeasurement {
                kind: MetricKind::Counter,
                name: "kiana.tokens".into(),
                description: Some("tokens".into()),
                unit: Some("{token}".into()),
                boundaries: None,
                value: 10.0,
                attributes: IndexMap::new(),
                context: None,
            },
            RecordedTelemetryMeasurement {
                kind: MetricKind::Histogram,
                name: "kiana.latency".into(),
                description: None,
                unit: Some("ms".into()),
                boundaries: Some(vec![10.0, 100.0]),
                value: 50.0,
                attributes: IndexMap::new(),
                context: None,
            },
        ];
        let request = metrics_request("kiana", &measurements);
        let metrics = &request.resource_metrics[0].scope_metrics[0].metrics;
        assert_eq!(metrics.len(), 2);
        let latency = metrics
            .iter()
            .find(|metric| metric.name == "kiana.latency")
            .unwrap();
        let point = &latency.histogram.as_ref().unwrap().data_points[0];
        assert_eq!(point.count, "1");
        assert_eq!(point.explicit_bounds, vec![10.0, 100.0]);
        assert_eq!(point.bucket_counts, vec!["0", "1", "0"]);
        let tokens = metrics
            .iter()
            .find(|metric| metric.name == "kiana.tokens")
            .unwrap();
        assert!(tokens.sum.as_ref().unwrap().is_monotonic);
        assert_eq!(
            tokens.sum.as_ref().unwrap().data_points[0].as_double,
            Some(10.0)
        );
    }

    #[test]
    fn attribute_arrays_keep_their_shape() {
        let mut map = IndexMap::new();
        map.insert(
            "flags".into(),
            AttributeValue::BooleanArray(vec![true, false]),
        );
        let values = attributes(&map);
        assert_eq!(
            values[0].value,
            AnyValue::ArrayValue(ArrayValue {
                values: vec![AnyValue::BoolValue(true), AnyValue::BoolValue(false)]
            })
        );
    }
}
