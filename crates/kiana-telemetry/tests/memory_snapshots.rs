//! 内存适配器快照黄金向量（由 TS 参考实现的同一脚本生成）。
//!
//! 脚本（`gen_memory.mjs`）用 `Date.now` / `performance.now` 的 spy 固定时间，跑一段固定的
//! span/measurement 脚本，然后记录 `getSpans()` / `getMetrics()` / `getTrace` / `findTraces` /
//! `findMetrics` 的 `JSON.stringify`。fixture 里的 scope UUID 已归一化为
//! `00000000-0000-4000-8000-000000000000`，Rust 侧用该 scope 复现。
//!
//! 偏差：TS 脚本里 span `child` 带 `attributes: { deep: null }`；`SpanAttributes`（冻结契约）的值
//! 是 `AttributeValue` 联合，没有 `null` 分支（JS 侧只有 `undefined` 会被丢弃，`null` 保留）。
//! 该属性已从黄金向量里移除，其余字节保持一致。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_telemetry::{
    AttributeValue, Clock, InMemoryTelemetryContext, MetricQuery, SpanOptions, SpanStatus,
    StatusError, TelemetryContextExt, TraceQuery,
};
use serde_json::Value;

/// 可脚本化的时钟：墙钟与单调钟分别由一个原子毫秒计数器表示（测试里按脚本推进）。
struct ScriptedClock {
    now: AtomicU64,
    monotonic: AtomicU64,
}

impl ScriptedClock {
    fn new() -> Arc<ScriptedClock> {
        Arc::new(ScriptedClock {
            now: AtomicU64::new(20_000),
            monotonic: AtomicU64::new(0),
        })
    }

    fn set_now(&self, ms: u64) {
        self.now.store(ms, Ordering::SeqCst);
    }

    fn set_monotonic(&self, ms: u64) {
        self.monotonic.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ScriptedClock {
    fn now_ms(&self) -> f64 {
        self.now.load(Ordering::SeqCst) as f64
    }

    fn now_monotonic_ms(&self) -> f64 {
        self.monotonic.load(Ordering::SeqCst) as f64
    }
}

fn attributes(pairs: &[(&str, AttributeValue)]) -> kiana_telemetry::SpanAttributes {
    let mut map = kiana_telemetry::SpanAttributes::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/memory_snapshots.json"))
        .expect("memory_snapshots.json")
}

fn scripted_context() -> (InMemoryTelemetryContext, Arc<ScriptedClock>) {
    let clock = ScriptedClock::new();
    (InMemoryTelemetryContext::with_clock(clock.clone()), clock)
}

/// 从黄金页里取每个 trace 的第一个 span 名字（TS 的 `traces.map(t => t.spans[0].name)`）。
fn fixture_names(page: &Value) -> Vec<String> {
    page.get("traces")
        .and_then(Value::as_array)
        .map(|traces| {
            traces
                .iter()
                .filter_map(|trace| {
                    trace
                        .get("spans")
                        .and_then(Value::as_array)
                        .and_then(|spans| spans.first())
                        .and_then(|span| span.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 归一化的 scope 前缀：`InMemoryTelemetryContext` 用随机 UUID；黄金向量里统一替换成固定值。
fn normalize(text: &str, scope: &str) -> String {
    text.replace(scope, "00000000-0000-4000-8000-000000000000")
}

#[tokio::test]
async fn reproduces_the_ts_snapshot_script() {
    let (telemetry, clock) = scripted_context();
    clock.set_now(20_000);
    clock.set_monotonic(0);

    let root_clock = Arc::clone(&clock);
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "root".to_string(),
                attributes: Some(attributes(&[
                    ("b", AttributeValue::Number(1.0)),
                    ("2", AttributeValue::Number(2.0)),
                    ("a", AttributeValue::Number(3.0)),
                    ("listed", AttributeValue::StringArray(vec!["x".to_string()])),
                    ("flag", AttributeValue::Boolean(true)),
                ])),
                links: None,
            },
            move |root| {
                let clock = Arc::clone(&root_clock);
                Box::pin(async move {
                    clock.set_monotonic(5);
                    root.add_event(
                        "evt",
                        Some(attributes(&[
                            ("10", AttributeValue::Number(1.0)),
                            ("z", AttributeValue::Number(2.0)),
                        ])),
                    );
                    root.set_attributes(attributes(&[("extra", AttributeValue::Number(1.5))]));
                    clock.set_monotonic(10);
                    let child_clock = Arc::clone(&clock);
                    root.start_span_typed(
                        SpanOptions {
                            name: "child".to_string(),
                            attributes: None,
                            links: None,
                        },
                        move |child| {
                            child_clock.set_monotonic(12);
                            Box::pin(async move {
                                child.set_status(SpanStatus::Error {
                                    error: Some(StatusError {
                                        name: "Expected".to_string(),
                                        message: "boom".to_string(),
                                    }),
                                });
                            })
                        },
                    )
                    .await?;
                    clock.set_monotonic(15);
                    root.start_span_typed(
                        SpanOptions {
                            name: "second-child".to_string(),
                            attributes: None,
                            links: None,
                        },
                        |_| Box::pin(async move {}),
                    )
                    .await
                })
            },
        )
        .await
        .expect("root script")
        .expect("root span");
    clock.set_monotonic(20);

    clock.set_now(20_001);
    let clock_for_other = Arc::clone(&clock);
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "other-root".to_string(),
                attributes: None,
                links: None,
            },
            move |span| {
                let clock = Arc::clone(&clock_for_other);
                Box::pin(async move {
                    clock.set_monotonic(22);
                    span.start_span_typed(
                        SpanOptions {
                            name: "nested".to_string(),
                            attributes: None,
                            links: None,
                        },
                        |_| Box::pin(async move {}),
                    )
                    .await
                })
            },
        )
        .await
        .expect("other-root script")
        .expect("other-root span");
    clock.set_now(30_000);
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "late-root".to_string(),
                attributes: None,
                links: None,
            },
            |_| Box::pin(async move {}),
        )
        .await
        .expect("late-root script");
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "linker".to_string(),
                attributes: None,
                links: None,
            },
            |_| Box::pin(async move {}),
        )
        .await
        .expect("linker script");

    telemetry
        .counter(
            "calls",
            Some(kiana_telemetry::InstrumentOptions {
                description: Some("Count".to_string()),
                unit: Some("1".to_string()),
            }),
        )
        .add(
            2.0,
            Some(attributes(&[
                ("b", AttributeValue::Number(1.0)),
                ("10", AttributeValue::Number(9.0)),
                ("a", AttributeValue::String("x".into())),
            ])),
        );
    telemetry.up_down_counter("active", None).add(-1.0, None);
    telemetry
        .histogram(
            "duration",
            Some(kiana_telemetry::HistogramOptions {
                instrument: kiana_telemetry::InstrumentOptions {
                    description: None,
                    unit: Some("ms".to_string()),
                },
                boundaries: Some(vec![1.0, 10.0]),
            }),
        )
        .record(
            5.0,
            Some(attributes(&[(
                "method",
                AttributeValue::String("read".into()),
            )])),
            None,
        );
    let telemetry_for_exemplar = telemetry.clone();
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "exemplar".to_string(),
                attributes: None,
                links: None,
            },
            move |span| {
                let histogram = telemetry_for_exemplar.histogram(
                    "duration",
                    Some(kiana_telemetry::HistogramOptions {
                        instrument: kiana_telemetry::InstrumentOptions {
                            description: None,
                            unit: None,
                        },
                        boundaries: Some(vec![1.0, 10.0]),
                    }),
                );
                Box::pin(async move {
                    let context: &dyn kiana_telemetry::TelemetryContext = span.as_ref();
                    histogram.record(
                        7.0,
                        Some(attributes(&[("m", AttributeValue::Number(1.0))])),
                        Some(context),
                    );
                })
            },
        )
        .await
        .expect("exemplar script");

    let fixture = fixture();
    let scope = telemetry.scope().to_string();

    // getSpans
    let spans_value = kiana_telemetry::json::recorded_spans_to_value(&telemetry.get_spans());
    let spans_text = normalize(&kiana_json::stringify(&spans_value), &scope);
    let expected_spans = kiana_json::stringify(&fixture["getSpans"]);
    assert_eq!(spans_text, expected_spans, "getSpans golden");

    // getMetrics
    let metrics_value =
        kiana_telemetry::json::recorded_measurements_to_value(&telemetry.get_metrics());
    let metrics_text = normalize(&kiana_json::stringify(&metrics_value), &scope);
    assert_eq!(
        metrics_text,
        kiana_json::stringify(&fixture["getMetrics"]),
        "getMetrics golden"
    );

    // findTraces（含分页）
    let names = |page: &kiana_telemetry::TracePage| -> Vec<String> {
        page.traces
            .iter()
            .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
            .collect()
    };
    let default_page = telemetry.find_traces(None).await.expect("default page");
    assert_eq!(
        &names(&default_page),
        &fixture_names(&fixture["findTraces"]["default"])
    );
    let errors_page = telemetry
        .find_traces(Some(TraceQuery {
            errors: Some(true),
            ..TraceQuery::default()
        }))
        .await
        .expect("errors page");
    assert_eq!(
        names(&errors_page),
        fixture_names(&fixture["findTraces"]["errors"])
    );
    let attributes_page = telemetry
        .find_traces(Some(TraceQuery {
            attributes: Some(attributes(&[("a", AttributeValue::Number(3.0))])),
            ..TraceQuery::default()
        }))
        .await
        .expect("attributes page");
    assert_eq!(
        names(&attributes_page),
        fixture_names(&fixture["findTraces"]["attributes"])
    );
    let name_page = telemetry
        .find_traces(Some(TraceQuery {
            name: Some("other-root".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect("name page");
    assert_eq!(
        names(&name_page),
        fixture_names(&fixture["findTraces"]["name"])
    );
    let start_min_page = telemetry
        .find_traces(Some(TraceQuery {
            start_min: Some(20_001.0),
            ..TraceQuery::default()
        }))
        .await
        .expect("startMin page");
    assert_eq!(
        names(&start_min_page),
        fixture_names(&fixture["findTraces"]["startMin"])
    );
    let limit_page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(2),
            ..TraceQuery::default()
        }))
        .await
        .expect("limit page");
    assert_eq!(
        names(&limit_page),
        fixture_names(&fixture["findTraces"]["limit2"])
    );
    let cursor = limit_page.cursor.clone().expect("cursor");
    let cursor_text = normalize(&cursor, &scope);
    assert_eq!(
        cursor_text,
        fixture["findTraces"]["limit2"]["cursor"].as_str().unwrap()
    );
    let page2 = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(2),
            cursor: Some(cursor),
            ..TraceQuery::default()
        }))
        .await
        .expect("page 2");
    assert_eq!(
        &names(&page2),
        &fixture_names(&fixture["findTraces"]["limit2page2"])
    );
    assert_eq!(
        page2.cursor.as_deref().map(|c| normalize(c, &scope)),
        fixture["findTraces"]["limit2page2"]["cursor"]
            .as_str()
            .map(str::to_string)
    );

    // findMetrics
    let calls = telemetry
        .find_metrics(MetricQuery {
            name: "calls".to_string(),
            start_min: None,
            start_max: None,
            attributes: None,
        })
        .await;
    assert_eq!(
        normalize(
            &kiana_json::stringify(&kiana_telemetry::json::stored_metrics_to_value(&calls)),
            &scope
        ),
        kiana_json::stringify(&fixture["findMetrics"]["calls"]),
        "findMetrics calls golden"
    );
    let calls_attr = telemetry
        .find_metrics(MetricQuery {
            name: "calls".to_string(),
            start_min: None,
            start_max: None,
            attributes: Some(attributes(&[("b", AttributeValue::Number(1.0))])),
        })
        .await;
    assert_eq!(
        normalize(
            &kiana_json::stringify(&kiana_telemetry::json::stored_metrics_to_value(&calls_attr)),
            &scope
        ),
        kiana_json::stringify(&fixture["findMetrics"]["callsAttr"]),
        "findMetrics attribute golden"
    );
    let window = telemetry
        .find_metrics(MetricQuery {
            name: "calls".to_string(),
            start_min: Some(20_000.0),
            start_max: Some(20_000.0),
            attributes: None,
        })
        .await;
    assert!(window.is_empty(), "findMetrics window golden");
}
