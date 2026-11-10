//! JS 语义边界的回归向量（由 TS 参考实现生成）。
//!
//! 这些 case 不在 TS 的测试文件里，但直接来自 `packages/telemetry/src/render.ts` 与
//! `packages/telemetry/src/memory.ts` 的实际行为（用 node 26 跑 upstream-reference 逐条录制）：
//!
//! - `render_traces`：模板字面量里的 `NaN`/`Infinity`（`Number::toString`，不是 JSON `null`），
//!   以及 `Object.entries` 的整数样键顺序；
//! - `findTraces` 的 cursor：`after[0]`/`after[1]` 的括号访问、抽象关系比较与严格相等；
//! - traceId 的 UTF-16 code unit 排序（含代理对）；
//! - 非有限 `startedMs` 的 `[null, id]` cursor。
//!
//! fixture：`fixtures/render_edges.json`、`fixtures/cursor_edge.json`。

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_telemetry::{
    AttributeValue, Clock, InMemoryTelemetryContext, SpanAttributes, SpanOptions, SpanStatus,
    StatusError, StoredSpan, StoredSpanEvent, StoredSpanLink, TelemetryContextExt, TraceQuery,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// render_edges.json
// ---------------------------------------------------------------------------

fn number_or_marker(text: &str) -> Option<f64> {
    match text {
        "__NaN" => Some(f64::NAN),
        "__Infinity" => Some(f64::INFINITY),
        "__-Infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}

fn time_from_value(value: &Value) -> f64 {
    match value {
        Value::String(text) => {
            number_or_marker(text).unwrap_or_else(|| panic!("time marker {text}"))
        }
        other => other.as_f64().unwrap_or(f64::NAN),
    }
}

fn attribute_from_value(value: &Value) -> AttributeValue {
    match value {
        Value::String(text) => match number_or_marker(text) {
            Some(number) => AttributeValue::Number(number),
            None => AttributeValue::String(text.clone()),
        },
        Value::Number(number) => AttributeValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        Value::Bool(flag) => AttributeValue::Boolean(*flag),
        Value::Array(items) => {
            if items.iter().all(Value::is_string) {
                AttributeValue::StringArray(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                )
            } else if items.iter().all(Value::is_boolean) {
                AttributeValue::BooleanArray(items.iter().filter_map(Value::as_bool).collect())
            } else {
                AttributeValue::NumberArray(items.iter().map(time_from_value).collect())
            }
        }
        other => panic!("unsupported attribute value {other:?}"),
    }
}

fn attributes_from_value(value: Option<&Value>) -> SpanAttributes {
    let mut map = IndexMap::new();
    if let Some(Value::Object(entries)) = value {
        for (key, value) in entries {
            map.insert(key.clone(), attribute_from_value(value));
        }
    }
    map
}

fn span_from_value(value: &Value) -> StoredSpan {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let status = match value
        .get("status")
        .and_then(|status| status.get("status"))
        .and_then(Value::as_str)
    {
        Some("error") => SpanStatus::Error {
            error: value
                .get("status")
                .and_then(|status| status.get("error"))
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
        id: text("id").unwrap_or_default(),
        trace_id: text("traceId").unwrap_or_default(),
        parent_id: text("parentId"),
        name: text("name").unwrap_or_default(),
        started_ms: value.get("startedMs").map(time_from_value).unwrap_or(0.0),
        ended_ms: value.get("endedMs").map(time_from_value).unwrap_or(0.0),
        attributes: attributes_from_value(value.get("attributes")),
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
                        time_ms: event.get("timeMs").map(time_from_value).unwrap_or(0.0),
                        attributes: attributes_from_value(event.get("attributes")),
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
                        attributes: link
                            .get("attributes")
                            .map(|value| attributes_from_value(Some(value))),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        status,
    }
}

/// `renderTrace` 的模板字面量与 `Object.entries` 边界（TS 录制）。
#[test]
fn render_trace_matches_javascript_template_and_entry_semantics() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/render_edges.json"))
        .expect("render_edges.json");
    let vectors = fixture.as_array().expect("array of vectors");
    assert!(!vectors.is_empty());
    for vector in vectors {
        let name = vector.get("name").and_then(Value::as_str).unwrap_or("?");
        let spans: Vec<StoredSpan> = vector
            .get("spans")
            .and_then(Value::as_array)
            .map(|spans| spans.iter().map(span_from_value).collect())
            .unwrap_or_default();
        let expected = vector.get("expected").and_then(Value::as_str).unwrap_or("");
        assert_eq!(
            kiana_telemetry::render::render_trace(&spans),
            expected,
            "vector {name}"
        );
    }
}

/// 非有限时长/事件时间写成 `NaN` / `Infinity` / `-Infinity`（不是 JSON 的 `null`）。
#[test]
fn non_finite_durations_and_event_times_use_number_to_string() {
    let mut span = StoredSpan {
        id: "s:1".to_string(),
        trace_id: "t".to_string(),
        name: "s".to_string(),
        started_ms: 0.0,
        ended_ms: f64::NAN,
        ..StoredSpan::default()
    };
    assert_eq!(
        kiana_telemetry::render::render_trace(&[span.clone()]),
        "s (NaNms)"
    );
    span.ended_ms = f64::INFINITY;
    assert_eq!(
        kiana_telemetry::render::render_trace(&[span.clone()]),
        "s (Infinityms)"
    );
    span.ended_ms = f64::NEG_INFINITY;
    assert_eq!(
        kiana_telemetry::render::render_trace(&[span.clone()]),
        "s (-Infinityms)"
    );
    span.ended_ms = 10.0;
    span.events = vec![StoredSpanEvent {
        name: "e".to_string(),
        time_ms: f64::NEG_INFINITY,
        attributes: SpanAttributes::new(),
    }];
    assert_eq!(
        kiana_telemetry::render::render_trace(&[span]),
        "s (10ms)\n  · e (+-Infinityms)"
    );
}

// ---------------------------------------------------------------------------
// cursor_edge.json
// ---------------------------------------------------------------------------

/// 可返回任意 f64（含 NaN/Infinity）的脚本时钟。
struct FixedClock {
    now: Mutex<f64>,
    monotonic: Mutex<f64>,
}

impl FixedClock {
    fn new() -> Arc<FixedClock> {
        Arc::new(FixedClock {
            now: Mutex::new(0.0),
            monotonic: Mutex::new(0.0),
        })
    }

    fn set_now(&self, value: f64) {
        *self.now.lock().expect("clock") = value;
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> f64 {
        *self.now.lock().expect("clock")
    }

    fn now_monotonic_ms(&self) -> f64 {
        *self.monotonic.lock().expect("clock")
    }
}

fn external_carrier(trace: &str) -> kiana_telemetry::TelemetryCarrier {
    let mut carrier = kiana_telemetry::TelemetryCarrier::new();
    carrier.insert("kiana-memory-scope".to_string(), "remote".to_string());
    carrier.insert("kiana-memory-span".to_string(), "1".to_string());
    carrier.insert("kiana-memory-trace".to_string(), trace.to_string());
    carrier
}

async fn store_with(clock: &Arc<FixedClock>, entries: &[(&str, f64)]) -> InMemoryTelemetryContext {
    let telemetry = InMemoryTelemetryContext::with_clock(Arc::clone(clock) as Arc<dyn Clock>);
    for (trace, start) in entries {
        let parent = telemetry.extract(Some(&external_carrier(trace)));
        clock.set_now(*start);
        parent
            .start_span_typed(
                SpanOptions {
                    name: (*trace).to_string(),
                    attributes: None,
                    links: None,
                },
                |_| Box::pin(async move {}),
            )
            .await
            .expect("span");
    }
    clock.set_now(0.0);
    telemetry
}

async fn trace_ids(telemetry: &InMemoryTelemetryContext, cursor: Option<&str>) -> Vec<String> {
    let page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            cursor: cursor.map(str::to_string),
            ..TraceQuery::default()
        }))
        .await
        .unwrap_or_else(|error| panic!("cursor {cursor:?}: {error}"));
    page.traces
        .into_iter()
        .map(|trace| trace.trace_id)
        .collect()
}

fn fixture_ids(value: &Value) -> Vec<String> {
    value
        .get("ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// cursor 的 `after[0]`/`after[1]` 走 JS 括号访问与抽象关系比较（TS 录制）。
#[tokio::test]
async fn find_traces_cursor_uses_javascript_bracket_and_relational_semantics() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/cursor_edge.json")).expect("cursor_edge.json");
    let clock = FixedClock::new();
    let telemetry = store_with(
        &clock,
        &[
            ("aaa-trace", 1000.0),
            ("mmm-trace", 1000.0),
            ("zzz-trace", 1000.0),
            ("half-trace", 0.5),
            ("zero-trace", 0.0),
        ],
    )
    .await;

    let all: Vec<String> = fixture
        .get("all")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(trace_ids(&telemetry, None).await, all);

    let cases = fixture
        .get("cursorCases")
        .and_then(Value::as_array)
        .expect("cursorCases");
    assert!(!cases.is_empty());
    for case in cases {
        let cursor = case.get("cursor").and_then(Value::as_str).expect("cursor");
        assert_eq!(
            trace_ids(&telemetry, Some(cursor)).await,
            fixture_ids(case),
            "cursor {cursor}"
        );
    }
}

/// `traceId > after[1]` 在非字符串右侧时按 `ToNumber` 比较（TS 录制）。
#[tokio::test]
async fn find_traces_cursor_compares_numeric_trace_ids_after_to_number() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/cursor_edge.json")).expect("cursor_edge.json");
    let clock = FixedClock::new();
    let telemetry = store_with(&clock, &[("10", 1000.0), ("9", 1000.0), ("abc", 1000.0)]).await;
    let cases = fixture
        .get("numericCases")
        .and_then(Value::as_array)
        .expect("numericCases");
    assert!(!cases.is_empty());
    for case in cases {
        let cursor = case.get("cursor").and_then(Value::as_str).expect("cursor");
        assert_eq!(
            trace_ids(&telemetry, Some(cursor)).await,
            fixture_ids(case),
            "cursor {cursor}"
        );
    }
}

/// 起点相同的 traceId 按 **UTF-16 code unit** 排序与比较（代理对 < U+FFFD）。
#[tokio::test]
async fn find_traces_orders_trace_ids_by_utf16_code_units() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/cursor_edge.json")).expect("cursor_edge.json");
    let clock = FixedClock::new();
    let telemetry = store_with(
        &clock,
        &[
            ("aaa-trace", 1000.0),
            ("\u{FFFD}-trace", 1000.0),
            ("\u{1F600}-trace", 1000.0),
        ],
    )
    .await;

    let expected: Vec<String> = fixture
        .get("astralOrder")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(trace_ids(&telemetry, None).await, expected);
    // 对照：Rust 的 `str` 序会把 😀 排在 U+FFFD 之后，正是需要避免的行为。
    assert!("😀-trace" > "\u{FFFD}-trace");

    for case in fixture
        .get("astralCursors")
        .and_then(Value::as_array)
        .expect("astralCursors")
    {
        let cursor = case.get("cursor").and_then(Value::as_str).expect("cursor");
        assert_eq!(
            trace_ids(&telemetry, Some(cursor)).await,
            fixture_ids(case),
            "cursor {cursor}"
        );
    }
}

/// 非有限 `startedMs` 的 cursor 编码成 `[null, id]`，续页按 JS 的 `===` 语义为空。
///
/// 这里只放两个 trace；NaN 与有限值混排时的**顺序**由 `tests/reviewer_fixes.rs` 的
/// `find_traces_nan_ordering_matches_v8_timsort` / `stored_spans_nan_ordering_matches_v8_timsort`
/// 用 upstream-reference 录制的向量逐条断言（排序已换成 V8 TimSort 的移植，结果与录制向量一致）。
#[tokio::test]
async fn find_traces_encodes_non_finite_starts_as_null_cursor() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/cursor_edge.json")).expect("cursor_edge.json");
    let nan = fixture.get("nan").expect("nan");
    let clock = FixedClock::new();
    let telemetry = store_with(&clock, &[("nan", f64::NAN), ("zero", 0.0)]).await;

    let expected_names = nan.get("names").and_then(Value::as_array).expect("names");
    let expected_cursors = nan
        .get("hasCursor")
        .and_then(Value::as_array)
        .expect("hasCursor");
    assert_eq!(expected_names.len(), 2);
    for (index, limit) in [1u32, 2].into_iter().enumerate() {
        let page = telemetry
            .find_traces(Some(TraceQuery {
                limit: Some(limit),
                ..TraceQuery::default()
            }))
            .await
            .expect("page");
        let names: Vec<String> = page
            .traces
            .iter()
            .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
            .collect();
        let want: Vec<String> = expected_names[index]
            .as_array()
            .expect("names entry")
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        assert_eq!(names, want, "limit {limit}");
        assert_eq!(
            page.cursor.is_some(),
            expected_cursors[index].as_bool().expect("hasCursor entry"),
            "limit {limit} cursor presence"
        );
    }

    let first = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(1),
            ..TraceQuery::default()
        }))
        .await
        .expect("first page");
    let cursor = first.cursor.clone().expect("cursor");
    assert!(
        cursor.starts_with("[null,"),
        "non-finite start encodes as null: {cursor}"
    );
    assert_eq!(
        nan.get("cursorIsNullStarted").and_then(Value::as_bool),
        Some(cursor.starts_with("[null,"))
    );
    let resume: Vec<String> = telemetry
        .find_traces(Some(TraceQuery {
            cursor: Some(cursor),
            ..TraceQuery::default()
        }))
        .await
        .expect("resume")
        .traces
        .iter()
        .map(|trace| trace.trace_id.clone())
        .collect();
    let want: Vec<String> = nan
        .get("resumeNames")
        .and_then(Value::as_array)
        .expect("resumeNames")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let resume_names: Vec<String> = telemetry
        .find_traces(Some(TraceQuery {
            cursor: Some(first.cursor.as_ref().expect("cursor").clone()),
            ..TraceQuery::default()
        }))
        .await
        .expect("resume names")
        .traces
        .iter()
        .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
        .collect();
    assert_eq!(resume_names, want);
    assert_eq!(resume.len(), want.len());
}

/// `findMetrics`/`findTraces` 的属性快照同样经过 `copyAttributes` 的键序。
#[tokio::test]
async fn spans_recorded_with_integer_like_attribute_keys_keep_javascript_order() {
    let clock = FixedClock::new();
    clock.set_now(20_000.0);
    let telemetry = InMemoryTelemetryContext::with_clock(Arc::clone(&clock) as Arc<dyn Clock>);
    let mut attributes = SpanAttributes::new();
    // 调用方给的是「非 JS 序」的插入顺序：`b`, `2`。
    attributes.insert("b".to_string(), AttributeValue::Number(1.0));
    attributes.insert("2".to_string(), AttributeValue::Number(2.0));
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "root".to_string(),
                attributes: Some(attributes),
                links: None,
            },
            |_| Box::pin(async move {}),
        )
        .await
        .expect("span");
    let page = telemetry.find_traces(None).await.expect("traces");
    let span = &page.traces[0].spans[0];
    assert_eq!(
        span.attributes.keys().cloned().collect::<Vec<_>>(),
        vec!["2".to_string(), "b".to_string()]
    );
    assert_eq!(
        kiana_telemetry::render::render_trace(&page.traces[0].spans),
        "root 2=2 b=1 (0ms)"
    );
}

/// 深嵌套 cursor：`JSON.parse` 在 V8 里接受任意深度，128 层的旧上限会在比较之前报错。
///
/// TS 的后续步骤（`after[0]` 的 `ToNumber` → `Array#join`）在约 4650 层处抛
/// `RangeError: Maximum call stack size exceeded`；Rust 侧解析与 `join` 都是迭代的，能算出
/// 同样的文本（空数组逐层 join 出空串 → `ToNumber("") === 0`），于是返回空页而不是错误。
/// 只有敌对 cursor 能到达该深度，此差别记录在 `memory.rs` / `jsjson.rs` 的模块文档里。
#[tokio::test]
async fn deep_cursor_crosses_the_old_recursion_limit_without_a_parse_error() {
    let clock = FixedClock::new();
    let telemetry = store_with(&clock, &[("a", 1000.0), ("b", 2000.0)]).await;

    for depth in [127usize, 128, 129, 1000, 4000] {
        let cursor = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let page = telemetry
            .find_traces(Some(TraceQuery {
                limit: Some(100),
                cursor: Some(cursor.clone()),
                ..TraceQuery::default()
            }))
            .await
            .unwrap_or_else(|error| panic!("depth {depth}: {error}"));
        assert!(
            page.traces.is_empty(),
            "depth {depth}: nested empty arrays ToNumber to 0, so no trace starts before it"
        );
    }

    // 数字嵌在数组里：`[[[2000]]]` 的 ToNumber 仍是 2000 → 严格早于 2000 的 trace 通过。
    let nested = format!("{}2000{}", "[".repeat(500), "]".repeat(500));
    let page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            cursor: Some(nested),
            ..TraceQuery::default()
        }))
        .await
        .expect("nested numeric cursor parses");
    assert_eq!(
        page.traces
            .iter()
            .map(|trace| trace.trace_id.clone())
            .collect::<Vec<_>>(),
        vec!["a".to_string()]
    );

    // 深层对象也不受解析深度限制（对象在 `after[0]` 里 ToPrimitive 成 "[object Object]" → NaN）。
    let object = format!(
        "{}{}",
        "{\"k\":".repeat(200),
        "1".to_string() + &"}".repeat(200)
    );
    let page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            cursor: Some(object),
            ..TraceQuery::default()
        }))
        .await
        .expect("deep object cursor parses");
    assert!(page.traces.is_empty());
}
