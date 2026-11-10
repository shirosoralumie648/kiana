//! 验收清单 §26 最后一项：跨单元 serde 形状。
//!
//! 其他 crate（`kiana-durable` 持久化 `TelemetryCarrier`、`kiana-chord` 的 store、`kiana-ai` 的
//! `RequestOptions.telemetry`）按名读取这些类型，因此字段名必须是 TS 的 camelCase 拼写、
//! 可选的 `undefined` 字段必须**省略**（不是 `null`），`SpanStatus` 必须是 `status` 判别标签，
//! 且 `TelemetryCarrier` 必须是普通的「字符串 → 字符串」map（可持久化）。
//!
//! 这些断言只覆盖 `serde` 派生的形状；JS 数字/键序的文本渲染由 [`kiana_telemetry::json`] 负责，
//! 另有 `memory_edges` / `memory_snapshots` 覆盖。

use indexmap::IndexMap;
use kiana_telemetry::{
    AttributeValue, MetricKind, MetricQuery, SpanAttributes, SpanStatus, StatusError, StoredMetric,
    StoredSpan, StoredSpanEvent, StoredSpanLink, StoredTrace, TelemetryCarrier, TracePage,
    TraceQuery,
};
use serde_json::{Value, json};

fn attributes(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
    let mut map = SpanAttributes::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

fn full_span() -> StoredSpan {
    StoredSpan {
        id: "scope:1".to_string(),
        trace_id: "scope:1".to_string(),
        parent_id: Some("scope:0".to_string()),
        name: "request".to_string(),
        started_ms: 1_000.0,
        ended_ms: 1_050.5,
        attributes: attributes(&[
            ("attempt", AttributeValue::Number(2.0)),
            ("cached", AttributeValue::Boolean(false)),
            ("model", AttributeValue::String("gpt".to_string())),
            ("tags", AttributeValue::StringArray(vec!["a".to_string()])),
            ("sizes", AttributeValue::NumberArray(vec![1.0, 2.0])),
            ("flags", AttributeValue::BooleanArray(vec![true, false])),
        ]),
        events: vec![StoredSpanEvent {
            name: "checkpoint".to_string(),
            time_ms: 2.5,
            attributes: attributes(&[("phase", AttributeValue::String("tools".to_string()))]),
        }],
        links: vec![
            StoredSpanLink {
                trace_id: "remote".to_string(),
                span_id: "remote:9".to_string(),
                attributes: Some(attributes(&[(
                    "reason",
                    AttributeValue::String("steer".to_string()),
                )])),
            },
            StoredSpanLink {
                trace_id: "other".to_string(),
                span_id: "other:1".to_string(),
                attributes: None,
            },
        ],
        status: SpanStatus::Error {
            error: Some(StatusError {
                name: "Error".to_string(),
                message: "failed".to_string(),
            }),
        },
    }
}

/// `TelemetryCarrier` 是普通字符串 map（`Record<string, string>`），插入序可持久化。
#[test]
fn telemetry_carrier_is_a_plain_string_map() {
    let mut carrier = TelemetryCarrier::new();
    carrier.insert("kiana-memory-trace".to_string(), "t".to_string());
    carrier.insert("kiana-memory-scope".to_string(), "s".to_string());
    carrier.insert("kiana-memory-span".to_string(), "1".to_string());

    let value: Value = serde_json::to_value(&carrier).expect("carrier serializes");
    assert_eq!(
        value,
        json!({"kiana-memory-trace": "t", "kiana-memory-scope": "s", "kiana-memory-span": "1"})
    );
    // `JSON.stringify(carrier)` 的键序 = 插入序。
    assert_eq!(
        kiana_json::stringify(&value),
        r#"{"kiana-memory-trace":"t","kiana-memory-scope":"s","kiana-memory-span":"1"}"#
    );

    let round_trip: TelemetryCarrier =
        serde_json::from_str(&kiana_json::stringify(&value)).expect("carrier deserializes");
    assert_eq!(round_trip, carrier);
    // 非字符串值被拒绝（TS 的类型是 `Record<string, string>`，durable 会原样持久化）。
    assert!(serde_json::from_str::<TelemetryCarrier>(r#"{"a":1}"#).is_err());
}

/// `StoredSpan`：camelCase 字段名、可选的 `parentId`/`link.attributes` 省略、
/// `status` 是判别标签。
#[test]
fn stored_span_uses_typescript_field_names() {
    let span = full_span();
    let value = serde_json::to_value(&span).expect("span serializes");
    assert_eq!(
        value,
        json!({
            "id": "scope:1",
            "traceId": "scope:1",
            "parentId": "scope:0",
            "name": "request",
            "startedMs": 1000.0,
            "endedMs": 1050.5,
            "attributes": {
                "attempt": 2.0,
                "cached": false,
                "model": "gpt",
                "tags": ["a"],
                "sizes": [1.0, 2.0],
                "flags": [true, false]
            },
            "events": [{ "name": "checkpoint", "timeMs": 2.5, "attributes": { "phase": "tools" } }],
            "links": [
                { "traceId": "remote", "spanId": "remote:9", "attributes": { "reason": "steer" } },
                { "traceId": "other", "spanId": "other:1" }
            ],
            "status": { "status": "error", "error": { "name": "Error", "message": "failed" } }
        })
    );

    let round_trip: StoredSpan = serde_json::from_value(value).expect("span deserializes");
    assert_eq!(round_trip, span);

    // 根 span 不应出现 `parentId: null`（TS 是 `parentId?: string`）。
    let root = StoredSpan {
        parent_id: None,
        ..span
    };
    let value = serde_json::to_value(&root).expect("root span serializes");
    assert!(value.get("parentId").is_none());

    // `{ status: "error" }`（无 error 详情）与 `{ status: "ok" }`。
    let bare_error = StoredSpan {
        status: SpanStatus::Error { error: None },
        ..root.clone()
    };
    let value = serde_json::to_value(&bare_error).expect("bare error serializes");
    assert_eq!(value["status"], json!({"status": "error"}));
    let ok = StoredSpan {
        status: SpanStatus::Ok,
        ..root
    };
    let value = serde_json::to_value(&ok).expect("ok serializes");
    assert_eq!(value["status"], json!({"status": "ok"}));
}

/// `StoredTrace` / `TracePage`：`traceId` + `spans`，`cursor` 仅在存在时出现。
#[test]
fn stored_trace_and_trace_page_match_typescript() {
    let trace = StoredTrace {
        trace_id: "t".to_string(),
        spans: vec![full_span()],
    };
    let value = serde_json::to_value(&trace).expect("trace serializes");
    assert_eq!(
        value,
        json!({"traceId": "t", "spans": [serde_json::to_value(full_span()).expect("span")]})
    );
    assert_eq!(
        serde_json::from_value::<StoredTrace>(value).expect("trace deserializes"),
        trace
    );

    let page = TracePage {
        traces: vec![trace],
        cursor: None,
    };
    let value = serde_json::to_value(&page).expect("page serializes");
    assert_eq!(
        value.as_object().expect("object").len(),
        1,
        "cursor is omitted"
    );
    assert!(value.get("cursor").is_none());
    let page = TracePage {
        cursor: Some("[\"1000\",\"t\"]".to_string()),
        ..page
    };
    let value = serde_json::to_value(&page).expect("page serializes");
    assert_eq!(value["cursor"], json!("[\"1000\",\"t\"]"));
}

/// `StoredMetric`：TS 的字段名与 `kind` 字面量。
#[test]
fn stored_metric_matches_typescript() {
    let metric = StoredMetric {
        interval_ms: 20_000.0,
        name: "calls".to_string(),
        kind: Some(MetricKind::UpDownCounter),
        unit: Some("By".to_string()),
        attributes: attributes(&[("route", AttributeValue::String("/a".to_string()))]),
        count: 1.0,
        sum: 3.0,
        min: 3.0,
        max: 3.0,
    };
    let value = serde_json::to_value(&metric).expect("metric serializes");
    assert_eq!(
        value,
        json!({
            "intervalMs": 20000.0,
            "name": "calls",
            "kind": "upDownCounter",
            "unit": "By",
            "attributes": { "route": "/a" },
            "count": 1.0,
            "sum": 3.0,
            "min": 3.0,
            "max": 3.0
        })
    );
    assert_eq!(
        serde_json::from_value::<StoredMetric>(value).expect("metric deserializes"),
        metric
    );

    // `unit` 缺省时省略。
    let without_unit = StoredMetric {
        unit: None,
        kind: Some(MetricKind::Histogram),
        ..metric
    };
    let value = serde_json::to_value(&without_unit).expect("metric serializes");
    assert!(value.get("unit").is_none());
    assert_eq!(value["kind"], json!("histogram"));
}

/// `TraceQuery` / `MetricQuery`：可选的驼峰字段，缺省省略；`MetricQuery.name` 必填。
#[test]
fn query_types_use_typescript_field_names() {
    let query = TraceQuery {
        name: Some("request".to_string()),
        attributes: Some(attributes(&[("attempt", AttributeValue::Number(2.0))])),
        errors: Some(true),
        start_min: Some(0.0),
        start_max: Some(1000.0),
        limit: Some(20),
        cursor: Some("c".to_string()),
    };
    assert_eq!(
        serde_json::to_value(&query).expect("query serializes"),
        json!({
            "name": "request",
            "attributes": { "attempt": 2.0 },
            "errors": true,
            "startMin": 0.0,
            "startMax": 1000.0,
            "limit": 20,
            "cursor": "c"
        })
    );
    assert_eq!(
        serde_json::to_value(TraceQuery::default()).expect("empty query serializes"),
        json!({})
    );

    let metric_query = MetricQuery {
        name: "calls".to_string(),
        start_min: Some(20_000.0),
        start_max: None,
        attributes: None,
    };
    assert_eq!(
        serde_json::to_value(&metric_query).expect("metric query serializes"),
        json!({"name": "calls", "startMin": 20000.0})
    );
    // `name` 必填：缺失的输入被拒绝。
    assert!(serde_json::from_value::<MetricQuery>(json!({})).is_err());
}

/// `AttributeValue` 的 serde 往返保持类型（untagged 枚举的顺序不能吞掉数组/布尔）。
#[test]
fn attribute_values_round_trip_without_changing_type() {
    let cases = [
        AttributeValue::String("a".to_string()),
        AttributeValue::Number(1.0),
        AttributeValue::Number(-0.5),
        AttributeValue::Boolean(true),
        AttributeValue::StringArray(vec!["a".to_string(), "b".to_string()]),
        AttributeValue::NumberArray(vec![1.0, 2.0]),
        AttributeValue::BooleanArray(vec![true, false]),
        AttributeValue::StringArray(Vec::new()),
    ];
    for value in cases {
        let json = serde_json::to_value(&value).expect("attribute serializes");
        let round_trip: AttributeValue =
            serde_json::from_value(json.clone()).expect("attribute deserializes");
        assert_eq!(round_trip, value, "{json}");
    }
    // 空数组在 TS 里没有类型标记，反序列化为第一个匹配的数组变体（`string[]`）。
    assert_eq!(
        serde_json::from_value::<AttributeValue>(json!([])).expect("empty array"),
        AttributeValue::StringArray(Vec::new())
    );
    // 混合数组不属于任何变体。
    assert!(serde_json::from_value::<AttributeValue>(json!([1, "a"])).is_err());
}

/// `SpanAttributes` 反序列化保留 JSON 文本里的键序（`serde_json` 的 `preserve_order`）。
#[test]
fn span_attributes_preserve_input_key_order() {
    let parsed: SpanAttributes =
        serde_json::from_str(r#"{"b":1,"a":2,"1":3}"#).expect("attributes");
    assert_eq!(
        parsed.keys().cloned().collect::<Vec<_>>(),
        vec!["b".to_string(), "a".to_string(), "1".to_string()],
        "the JSON key order is preserved verbatim"
    );
    // 输出可交给 `kiana_json` 做 JS 键序（整数样键前置）。
    assert_eq!(
        kiana_json::stringify(&serde_json::to_value(&parsed).expect("attributes serialize")),
        r#"{"1":3,"b":1,"a":2}"#
    );
    let _: IndexMap<String, AttributeValue> = parsed;
}

/// 冻结的 schema 类型的 [`Default`] 只能产出 TS 类型系统里**可表达**的形状。
///
/// TS 的 `TelemetrySpanDefinition.parents` 是必填非 null（`TelemetryParentDefinition`），
/// `status.default` 是字面量 `"ok"`。Rust 用 `derive(Default)` 时会得到 `parents: None`
/// （序列化成 `"parents":null`）与 `status.default: ""`，两者都是 TS 无法表达的值；
/// 手工 `Default` 已修正，这里把契约锁住。
#[test]
fn schema_defaults_stay_within_the_typescript_type_system() {
    let span = kiana_telemetry::TelemetrySpanDefinition::default();
    assert_eq!(
        span.status.default, "ok",
        "status.default is the literal \"ok\" in TS"
    );
    assert_eq!(
        span.parents,
        Some(kiana_telemetry::TelemetryParentDefinition::Any),
        "parents is a required non-null parent definition in TS"
    );
    let value = serde_json::to_value(&span).expect("span definition serializes");
    assert_eq!(value["status"]["default"], json!("ok"));
    assert_eq!(
        value["parents"],
        json!({ "kind": "any" }),
        "the default serializes to a shape TS can produce"
    );
    assert_ne!(
        value["parents"],
        Value::Null,
        "TS requires a non-null parents definition"
    );

    let schema = kiana_telemetry::TelemetrySchemaDefinition::default();
    let value = serde_json::to_value(&schema).expect("schema serializes");
    assert_eq!(value["version"], json!(0.0), "version is a JS number");
    assert!(value["spans"].is_object());
    assert!(
        value.get("metrics").is_none(),
        "the optional metrics map is omitted, not null"
    );
}
