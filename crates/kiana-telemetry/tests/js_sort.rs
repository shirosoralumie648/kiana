//! `Array#sort` 的 V8 TimSort 语义（render / store 排序路径的回归向量）。
//!
//! TS 的 `render.ts` / `memory.ts` 直接用 `Array.prototype.sort` 与「可能返回 NaN」的比较器；
//! Rust 的 `slice::sort_by` 对非全序比较器会 panic
//! （`user-provided comparison function does not correctly implement a total order`），
//! 因此 `src/timsort.rs` 逐宏移植了 Node 26 的 V8 TimSort。
//!
//! `fixtures/sort_vectors.json` 由 Node 26（V8 14.6）的 `Array.prototype.sort((a, b) => a - b)`
//! 生成：`input` / `expected` 里的 `"__NaN"` / `"__Infinity"` / `"__-Infinity"` 是标记。

use std::sync::{Arc, Mutex};

use kiana_telemetry::{
    Clock, InMemoryTelemetryContext, SpanOptions, StoredSpan, TelemetryContextExt, TraceQuery,
};
use serde_json::Value;

/// `render_trace` 对含 NaN 起点的大数组**不得 panic**（评审给出的崩溃向量）。
#[test]
fn render_trace_does_not_panic_on_nan_started_ms() {
    let starts = [
        93.0,
        29.0,
        5.0,
        31.0,
        72.0,
        53.0,
        11.0,
        11.0,
        82.0,
        48.0,
        1.0,
        51.0,
        83.0,
        67.0,
        81.0,
        f64::NAN,
        21.0,
        42.0,
        88.0,
        17.0,
        79.0,
    ];
    let spans: Vec<StoredSpan> = starts
        .iter()
        .enumerate()
        .map(|(index, started)| StoredSpan {
            id: format!("s:{index}"),
            trace_id: "t".to_string(),
            name: format!("n{index}"),
            started_ms: *started,
            ended_ms: *started + 1.0,
            ..StoredSpan::default()
        })
        .collect();
    let rendered = kiana_telemetry::render::render_trace(&spans);
    assert_eq!(rendered.lines().count(), starts.len());
    // 与 V8 一致：NaN 起点排到最后。
    assert!(rendered.ends_with("n15 (NaNms)"), "{rendered}");
}

/// 起点含 NaN 的 span 集合：`get_spans`/`find_traces`/`get_trace` 都不得 panic。
#[tokio::test]
async fn store_queries_do_not_panic_on_nan_started_ms() {
    struct ScriptedClock {
        now: Mutex<f64>,
    }

    impl Clock for ScriptedClock {
        fn now_ms(&self) -> f64 {
            *self.now.lock().expect("clock")
        }

        fn now_monotonic_ms(&self) -> f64 {
            0.0
        }
    }

    let clock = Arc::new(ScriptedClock {
        now: Mutex::new(0.0),
    });
    let telemetry = InMemoryTelemetryContext::with_clock(clock.clone());
    for index in 0..30 {
        *clock.now.lock().expect("clock") = if index % 3 == 2 {
            f64::NAN
        } else {
            index as f64
        };
        telemetry
            .start_span_typed(
                SpanOptions {
                    name: format!("s{index}"),
                    ..SpanOptions::default()
                },
                |_| Box::pin(async move {}),
            )
            .await
            .expect("span");
    }

    let spans = telemetry.get_spans();
    assert_eq!(spans.len(), 30);
    let traces = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            ..TraceQuery::default()
        }))
        .await
        .expect("find_traces");
    assert_eq!(traces.traces.len(), 30);
    let trace_id = traces.traces[0].trace_id.clone();
    let trace = telemetry.get_trace(&trace_id).await.expect("get_trace");
    assert_eq!(trace.spans.len(), 1);
}

fn marker_number(value: &Value) -> f64 {
    match value {
        Value::String(text) => match text.as_str() {
            "__NaN" => f64::NAN,
            "__Infinity" => f64::INFINITY,
            "__-Infinity" => f64::NEG_INFINITY,
            other => panic!("unexpected marker {other}"),
        },
        other => other.as_f64().expect("number"),
    }
}

fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(marker_number)
        .collect()
}

fn same_bits(left: f64, right: f64) -> bool {
    left.to_bits() == right.to_bits() || (left.is_nan() && right.is_nan())
}

/// `Array.prototype.sort((a, b) => a - b)` 的逐元素黄金向量（Node 26 录制）。
///
/// 覆盖：`< 8` 的二分插入排序路径、TimSort 的 run/gallop/merge 路径、任意位置的 `NaN`、
/// 重复值、`±Infinity`、`-0`。
#[test]
fn matches_v8_array_sort_vectors() {
    let vectors: Value = serde_json::from_str(include_str!("fixtures/sort_vectors.json"))
        .expect("sort_vectors.json");
    let cases = vectors.as_array().expect("array of cases");
    assert!(cases.len() >= 100);
    for (index, case) in cases.iter().enumerate() {
        let input = numbers(case.get("input").expect("input"));
        let expected = numbers(case.get("expected").expect("expected"));
        // 经 render_trace 的排序路径：把每个数字当成一个 span 的 startedMs。
        let spans: Vec<StoredSpan> = input
            .iter()
            .enumerate()
            .map(|(position, started)| StoredSpan {
                id: format!("s:{position}"),
                trace_id: "t".to_string(),
                name: format!("n{position}"),
                started_ms: *started,
                ended_ms: *started,
                ..StoredSpan::default()
            })
            .collect();
        let rendered = kiana_telemetry::render::render_trace(&spans);
        let rendered_order: Vec<f64> = rendered
            .lines()
            .map(|line| {
                let name = line.split(' ').next().expect("name");
                let position: usize = name[1..].parse().expect("index");
                input[position]
            })
            .collect();
        assert_eq!(rendered_order.len(), expected.len(), "case {index}");
        for (left, right) in rendered_order.iter().zip(expected.iter()) {
            assert!(
                same_bits(*left, *right),
                "case {index}: render order {rendered_order:?} != {expected:?}"
            );
        }
    }
}
