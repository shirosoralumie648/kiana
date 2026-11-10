//! render.test.ts 的 Rust 移植（8 个 `it` 标题保留既有文案，作为稳定契约）。
//!
//! 前 6 条覆盖既有测试场景，另加 2 条黄金向量（`tests/fixtures/render_traces.json`
//! 由 TS 的 `renderTrace` 生成）。

use indexmap::IndexMap;
use kiana_telemetry::render::render_trace;
use kiana_telemetry::{
    AttributeValue, SpanAttributes, SpanStatus, StatusError, StoredSpan, StoredSpanEvent,
    StoredSpanLink,
};
use serde_json::Value;

fn span(id: &str, overrides: StoredSpan) -> StoredSpan {
    StoredSpan {
        id: id.to_string(),
        trace_id: "trace".to_string(),
        name: id.to_string(),
        started_ms: 0.0,
        ended_ms: 10.0,
        attributes: SpanAttributes::new(),
        events: Vec::new(),
        links: Vec::new(),
        status: SpanStatus::Ok,
        ..overrides
    }
}

fn base(id: &str) -> StoredSpan {
    span(id, StoredSpan::default())
}

fn attrs(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
    let mut map = IndexMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

#[test]
fn renders_an_unordered_trace_with_attributes_durations_errors_events_and_links() {
    let root = {
        let mut value = base("root");
        value.started_ms = 1000.0;
        value.ended_ms = 1020.0;
        value.attributes = attrs(&[
            ("request", AttributeValue::String("a b".into())),
            ("attempt", AttributeValue::Number(2.0)),
            ("cached", AttributeValue::Boolean(false)),
            (
                "tags",
                AttributeValue::StringArray(vec!["a".into(), "b".into()]),
            ),
            ("sizes", AttributeValue::NumberArray(vec![1.0, 2.0])),
            ("flags", AttributeValue::BooleanArray(vec![true, false])),
        ]);
        value.events = vec![
            StoredSpanEvent {
                name: "checkpoint".to_string(),
                time_ms: 2.0,
                attributes: attrs(&[("phase", AttributeValue::String("tools".into()))]),
            },
            StoredSpanEvent {
                name: "tick".to_string(),
                time_ms: 3.5,
                attributes: SpanAttributes::new(),
            },
        ];
        value.links = vec![
            StoredSpanLink {
                trace_id: "remote-trace".to_string(),
                span_id: "remote".to_string(),
                attributes: Some(attrs(&[("reason", AttributeValue::String("steer".into()))])),
            },
            StoredSpanLink {
                trace_id: "other-trace".to_string(),
                span_id: "other".to_string(),
                attributes: None,
            },
        ];
        value
    };
    let first = {
        let mut value = base("first");
        value.parent_id = Some("root".to_string());
        value.started_ms = 1005.0;
        value.ended_ms = 1009.5;
        value.status = SpanStatus::Error {
            error: Some(StatusError {
                name: "Error".to_string(),
                message: "failed call".to_string(),
            }),
        };
        value
    };
    let nested = {
        let mut value = base("nested");
        value.parent_id = Some("first".to_string());
        value.started_ms = 1006.0;
        value.ended_ms = 1007.0;
        value
    };
    let second = {
        let mut value = base("second");
        value.parent_id = Some("root".to_string());
        value.started_ms = 1010.0;
        value.ended_ms = 1012.0;
        value.status = SpanStatus::Error { error: None };
        value
    };
    let spans = vec![second, nested, first, root];
    let before = spans.clone();
    assert_eq!(
        render_trace(&spans),
        [
            r#"root request="a b" attempt=2 cached=false tags=["a","b"] sizes=[1,2] flags=[true,false] (20ms)"#,
            r#"  · checkpoint phase="tools" (+2ms)"#,
            "  · tick (+3.5ms)",
            r#"  → link trace remote-trace span remote reason="steer""#,
            "  → link trace other-trace span other",
            "  first (4.5ms) error Error: failed call",
            "    nested (1ms)",
            "  second (2ms) error",
        ]
        .join("\n")
    );
    assert_eq!(spans, before);
}

#[test]
fn treats_missing_parents_as_roots_and_orders_roots_and_children_by_start_time() {
    let later = {
        let mut value = base("later");
        value.parent_id = Some("external".to_string());
        value.started_ms = 20.0;
        value.ended_ms = 30.0;
        value
    };
    let child = {
        let mut value = base("child");
        value.parent_id = Some("later".to_string());
        value.started_ms = 25.0;
        value.ended_ms = 30.0;
        value
    };
    let earlier = {
        let mut value = base("earlier");
        value.started_ms = 10.0;
        value.ended_ms = 15.0;
        value.links = vec![StoredSpanLink {
            trace_id: "trace".to_string(),
            span_id: "later".to_string(),
            attributes: None,
        }];
        value
    };
    assert_eq!(
        render_trace(&[later, child, earlier]),
        [
            "earlier (5ms)",
            "  → link trace trace span later",
            "later (10ms)",
            "  child (5ms)"
        ]
        .join("\n")
    );
}

#[test]
fn prints_cross_trace_links_without_following_them_including_cycles() {
    let server = {
        let mut value = base("server:1");
        value.name = "server".to_string();
        value.trace_id = "server-trace".to_string();
        value.links = vec![StoredSpanLink {
            trace_id: "worker-trace".to_string(),
            span_id: "worker:1".to_string(),
            attributes: None,
        }];
        value
    };
    let worker = {
        let mut value = base("worker:1");
        value.name = "worker".to_string();
        value.trace_id = "worker-trace".to_string();
        value.links = vec![StoredSpanLink {
            trace_id: "server-trace".to_string(),
            span_id: "server:1".to_string(),
            attributes: None,
        }];
        value
    };
    assert_eq!(
        render_trace(&[server, worker]),
        [
            "server (10ms)",
            "  → link trace worker-trace span worker:1",
            "worker (10ms)",
            "  → link trace server-trace span server:1",
        ]
        .join("\n")
    );
}

#[test]
fn deduplicates_spans_merged_from_stores_by_global_id() {
    let root = {
        let mut value = base("server:1");
        value.name = "root".to_string();
        value
    };
    let child = {
        let mut value = base("worker:1");
        value.name = "child".to_string();
        value.parent_id = Some("server:1".to_string());
        value.events = vec![StoredSpanEvent {
            name: "done".to_string(),
            time_ms: 3.0,
            attributes: SpanAttributes::new(),
        }];
        value
    };
    let grandchild = {
        let mut value = base("worker:2");
        value.name = "grandchild".to_string();
        value.parent_id = Some("worker:1".to_string());
        value
    };
    let spans = vec![
        child.clone(),
        root.clone(),
        grandchild,
        root.clone(),
        child.clone(),
    ];
    assert_eq!(
        render_trace(&spans),
        "root (10ms)\n  child (10ms)\n    · done (+3ms)\n    grandchild (10ms)"
    );
}

#[test]
fn preserves_input_order_for_equal_start_times_without_mutating_the_input() {
    let root = base("root");
    let z = {
        let mut value = base("z");
        value.parent_id = Some("root".to_string());
        value
    };
    let a = {
        let mut value = base("a");
        value.parent_id = Some("root".to_string());
        value
    };
    let other = base("other");
    let spans = vec![root, z, a, other];
    let before = spans.clone();
    assert_eq!(
        render_trace(&spans),
        "root (10ms)\n  z (10ms)\n  a (10ms)\nother (10ms)"
    );
    assert_eq!(spans, before);
}

#[test]
fn escapes_string_attribute_values_without_hiding_their_contents() {
    let value = {
        let mut value = base("root");
        value.attributes = attrs(&[(
            "text",
            AttributeValue::String("line one\n\"line two\"".into()),
        )]);
        value
    };
    assert_eq!(
        render_trace(&[value]),
        'r'.to_string() + r#"oot text="line one\n\"line two\"" (10ms)"#
    );
}

#[test]
fn renders_deep_trees_without_recursive_calls() {
    let spans: Vec<StoredSpan> = (0..2000)
        .map(|index| {
            let mut value = base(&index.to_string());
            if index > 0 {
                value.parent_id = Some((index - 1).to_string());
            }
            value
        })
        .collect();
    let rendered = render_trace(&spans);
    let lines: Vec<&str> = rendered.split('\n').collect();
    assert_eq!(lines.len(), spans.len());
    assert!(rendered.ends_with(&format!("{}1999 (10ms)", "  ".repeat(1999))));
}

#[test]
fn renders_an_empty_input_as_an_empty_string() {
    assert_eq!(render_trace(&[]), "");
}

// ---------------------------------------------------------------------------
// 黄金向量（TS renderTrace 生成）
// ---------------------------------------------------------------------------

fn fixture() -> serde_json::Value {
    let text = include_str!("fixtures/render_traces.json");
    serde_json::from_str(text).expect("render_traces.json")
}

fn attribute_from_value(value: &Value) -> AttributeValue {
    match value {
        // `JSON.stringify` 把非有限数字写成 null；fixture 用哨兵字符串保住它们。
        Value::String(text) if text == "__NaN" => AttributeValue::Number(f64::NAN),
        Value::String(text) if text == "__Infinity" => AttributeValue::Number(f64::INFINITY),
        Value::String(text) => AttributeValue::String(text.clone()),
        Value::Number(number) => AttributeValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(flag) => AttributeValue::Boolean(*flag),
        Value::Array(items) => {
            if items.iter().all(|item| item.is_string()) {
                AttributeValue::StringArray(
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect(),
                )
            } else if items.iter().all(|item| item.is_boolean()) {
                AttributeValue::BooleanArray(
                    items.iter().filter_map(|item| item.as_bool()).collect(),
                )
            } else {
                AttributeValue::NumberArray(
                    items
                        .iter()
                        .map(|item| item.as_f64().unwrap_or(f64::NAN))
                        .collect(),
                )
            }
        }
        other => panic!("unsupported fixture attribute value {other:?}"),
    }
}

fn attributes_from_value(value: &Value) -> SpanAttributes {
    let mut map = SpanAttributes::new();
    if let Value::Object(entries) = value {
        for (key, value) in entries {
            map.insert(key.clone(), attribute_from_value(value));
        }
    }
    // JS 对象键序（整数样键前置）。
    map
}

fn span_from_fixture(value: &Value) -> StoredSpan {
    let get_str = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let status = match value
        .get("status")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
    {
        Some("error") => SpanStatus::Error {
            error: value
                .get("status")
                .and_then(|s| s.get("error"))
                .map(|error| StatusError {
                    name: error
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
        },
        _ => SpanStatus::Ok,
    };
    StoredSpan {
        id: get_str("id").unwrap_or_default(),
        trace_id: get_str("traceId").unwrap_or_default(),
        parent_id: get_str("parentId"),
        name: get_str("name").unwrap_or_default(),
        started_ms: value
            .get("startedMs")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        ended_ms: value.get("endedMs").and_then(Value::as_f64).unwrap_or(0.0),
        attributes: attributes_from_value(value.get("attributes").unwrap_or(&Value::Null)),
        events: value
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .map(|event| StoredSpanEvent {
                        name: event
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        time_ms: event.get("timeMs").and_then(Value::as_f64).unwrap_or(0.0),
                        attributes: attributes_from_value(
                            event.get("attributes").unwrap_or(&Value::Null),
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        links: value
            .get("links")
            .and_then(Value::as_array)
            .map(|links| {
                links
                    .iter()
                    .map(|link| StoredSpanLink {
                        trace_id: link
                            .get("traceId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        span_id: link
                            .get("spanId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        attributes: link.get("attributes").map(attributes_from_value),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        status,
    }
}

#[test]
fn formats_fractional_durations_like_javascript() {
    // `0.19999999999999998` 是 f64 的最短往返表示；serde_json 的默认（非 float_roundtrip）
    // 解析会把它读成 0.2，因此这里直接在 Rust 源码里用字面量构造。
    let mut value = base("numbers");
    value.started_ms = 0.0;
    value.ended_ms = 0.19999999999999998_f64;
    assert_eq!(render_trace(&[value]), "numbers (0.19999999999999998ms)");
}

#[test]
fn matches_ts_render_goldens() {
    let fixture = fixture();
    let vectors = fixture.as_array().expect("array of vectors");
    assert_eq!(vectors.len(), 8);
    for vector in vectors {
        let name = vector.get("name").and_then(Value::as_str).unwrap_or("?");
        let spans: Vec<StoredSpan> = vector
            .get("spans")
            .and_then(Value::as_array)
            .map(|spans| spans.iter().map(span_from_fixture).collect())
            .unwrap_or_default();
        let expected = vector.get("expected").and_then(Value::as_str).unwrap_or("");
        assert_eq!(render_trace(&spans), expected, "vector {name}");
    }
}
