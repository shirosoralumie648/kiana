//! store.test.ts 的 Rust 移植。
//!
//! `telemetryStoreConformance("in-memory telemetry store conformance", factory)` 在 Rust 侧是
//! `kiana_telemetry::store_conformance::telemetry_store_conformance(factory)`，返回 6 个 case；
//! 与 TS 一样按 describe 名注册（这里用一个 `#[test]` 跑完全部 case，并逐条保留 name）。
//!
//! `describe("in-memory store snapshots")` 下的 3 个 `it` 标题保留既有文案（稳定契约）。
//!
//! 时钟注入：TS 用 `vi.spyOn(Date, "now")` / `vi.spyOn(performance, "now")`；Rust 用
//! [`Clock`]（见 `memory.rs` 模块文档）。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_telemetry::store_conformance::{
    StoreFixture, StoreFixtureFactory, telemetry_store_conformance,
};
use kiana_telemetry::{
    AttributeValue, Clock, InMemoryTelemetryContext, MetricQuery, SpanOptions, SpanStatus,
    TelemetryCarrier, TelemetryContext, TelemetryContextExt, TelemetrySpan, TelemetryStore,
    TraceQuery,
};

/// 可脚本化的时钟：`now_ms` 是 `Date.now()`，`now_monotonic_ms` 是 `performance.now()`。
#[derive(Default)]
struct ScriptedClock {
    now_ms: AtomicU64,
    monotonic_ms: AtomicU64,
}

impl Clock for ScriptedClock {
    fn now_ms(&self) -> f64 {
        self.now_ms.load(Ordering::SeqCst) as f64
    }

    fn now_monotonic_ms(&self) -> f64 {
        self.monotonic_ms.load(Ordering::SeqCst) as f64
    }
}

fn store_factory() -> StoreFixtureFactory {
    Arc::new(|| {
        Box::pin(async move {
            let scripted = Arc::new(ScriptedClock::default());
            let clock: Arc<dyn Clock> = scripted.clone();
            let context = Arc::new(InMemoryTelemetryContext::with_clock(clock));
            let context_for_set_now = Arc::clone(&scripted);
            let context_for_store: Arc<dyn TelemetryStore> = context.clone();
            let context_for_fixture: Arc<dyn TelemetryContext> = context.clone();
            // `{...carrier, [key]: 42}`：内存适配器用 `extract_json`（逐键 typeof 判定）覆盖
            // 这条 TS 分支；`TelemetryCarrier` 本身表达不了非字符串值。
            let context_for_non_string = Arc::clone(&context);
            Ok(StoreFixture {
                context: context_for_fixture,
                store: context_for_store,
                flush: Arc::new(|| Box::pin(async {})),
                dispose: Arc::new(|| Box::pin(async {})),
                set_now: Arc::new(move |ms| {
                    context_for_set_now
                        .now_ms
                        .store(ms as u64, Ordering::SeqCst);
                }),
                extract_non_string: Some(Arc::new(
                    move |carrier: &kiana_telemetry::TelemetryCarrier, key: &str| {
                        let mut value = serde_json::Map::new();
                        for (name, text) in carrier {
                            if name == key {
                                value.insert(name.clone(), serde_json::Value::from(42));
                            } else {
                                value.insert(name.clone(), serde_json::Value::String(text.clone()));
                            }
                        }
                        Some(context_for_non_string.extract_json(&serde_json::Value::Object(value)))
                    },
                )),
            })
        })
    })
}

fn options(name: &str) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        ..SpanOptions::default()
    }
}

/// `telemetryStoreConformance("in-memory telemetry store conformance", ...)`。
#[tokio::test]
async fn in_memory_telemetry_store_conformance() {
    let cases = telemetry_store_conformance(store_factory());
    assert_eq!(cases.len(), 6);
    let expected = [
        "returns only settled traces, complete span snapshots, and detached results",
        "filters by first-span name and start, any error, and all attributes on one span",
        "pages complete traces without duplicates when trace starts tie",
        "inherits remote trace IDs and preserves remote parents and cross-trace links",
        "accepts external string identities without imposing an ID format",
        "queries metric names, inclusive interval bounds, typed attribute equality, and detached values",
    ];
    for (index, (case, name)) in cases.iter().zip(expected).enumerate() {
        assert_eq!(case.group, "store", "case #{index} group");
        assert_eq!(case.name, name, "case #{index} name");
        (case.run)()
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

/// `inherits remote trace IDs …` 的 fallback 计数是固定契约：每个 carrier 键两条
/// （删除该键 + 该键的值换成数字 42），共 `keys * 2`；两条都回退成根 span。
///
/// 这里额外计数 `extract_non_string` 的调用次数，证明套件确实走了「非字符串值」那半条分支
/// （TS: `expect(fallbacks).toHaveLength(Object.keys(carrier).length * 2)`）。
#[tokio::test]
async fn remote_id_fallbacks_cover_deleted_and_non_string_carrier_values() {
    let calls = Arc::new(AtomicU64::new(0));
    let calls_for_factory = Arc::clone(&calls);
    let base = store_factory();
    let factory: StoreFixtureFactory = Arc::new(move || {
        let base = Arc::clone(&base);
        let calls = Arc::clone(&calls_for_factory);
        Box::pin(async move {
            let mut fixture = base().await?;
            if let Some(extract) = fixture.extract_non_string.take() {
                fixture.extract_non_string =
                    Some(Arc::new(move |carrier: &TelemetryCarrier, key: &str| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        extract(carrier, key)
                    }));
            }
            Ok(fixture)
        })
    });
    let cases = telemetry_store_conformance(factory);
    let case = cases
        .iter()
        .find(|case| case.name.starts_with("inherits remote trace IDs"))
        .expect("remote ids case");
    (case.run)().await.expect("remote ids case runs");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "one non-string extract per carrier key"
    );
}

/// `describe("in-memory store snapshots") > it("records wall-clock boundaries and relative event
/// times without changing normalized event snapshots")`。
#[tokio::test]
async fn records_wall_clock_boundaries_and_relative_event_times_without_changing_normalized_event_snapshots()
 {
    let scripted = Arc::new(ScriptedClock::default());
    let clock: Arc<dyn Clock> = scripted.clone();
    scripted.now_ms.store(1000, Ordering::SeqCst);
    let clock_for_read = Arc::clone(&scripted);
    scripted.monotonic_ms.store(100, Ordering::SeqCst);
    let telemetry = Arc::new(InMemoryTelemetryContext::with_clock(clock));

    let (id_tx, id_rx) = tokio::sync::oneshot::channel::<String>();
    let telemetry_for_span = Arc::clone(&telemetry);
    let telemetry_for_callback = Arc::clone(&telemetry_for_span);
    let clock_for_span = Arc::clone(&clock_for_read);
    telemetry_for_span
        .start_span_typed(options("timed"), move |span| {
            Box::pin(async move {
                let span_context: &dyn TelemetryContext = span.as_ref();
                let carrier = telemetry_for_callback
                    .inject(span_context)
                    .expect("injected");
                let id = format!(
                    "{}:{}",
                    carrier.get("kiana-memory-scope").expect("scope"),
                    carrier.get("kiana-memory-span").expect("span")
                );
                let _ = id_tx.send(id);
                clock_for_span.monotonic_ms.store(110, Ordering::SeqCst);
                span.add_event(
                    "event",
                    Some(kiana_telemetry::testing::attributes(&[(
                        "count",
                        AttributeValue::Number(1.0),
                    )])),
                );
                clock_for_span.monotonic_ms.store(125, Ordering::SeqCst);
            })
        })
        .await
        .expect("timed span");
    let id = id_rx.await.expect("id");

    let trace = telemetry.get_trace(&id).await.expect("stored trace");
    assert_eq!(
        trace,
        kiana_telemetry::StoredTrace {
            trace_id: id.clone(),
            spans: vec![kiana_telemetry::StoredSpan {
                id: id.clone(),
                trace_id: id.clone(),
                parent_id: None,
                name: "timed".to_string(),
                started_ms: 1000.0,
                ended_ms: 1025.0,
                attributes: kiana_telemetry::SpanAttributes::new(),
                events: vec![kiana_telemetry::StoredSpanEvent {
                    name: "event".to_string(),
                    time_ms: 10.0,
                    attributes: kiana_telemetry::testing::attributes(&[(
                        "count",
                        AttributeValue::Number(1.0)
                    )]),
                }],
                links: Vec::new(),
                status: SpanStatus::Ok,
            }],
        }
    );

    // `getSpans()` 的事件不含相对时间（`timeMs` 只出现在 store 快照里）。
    let recorded = telemetry.get_spans();
    assert_eq!(recorded[0].events.len(), 1);
    assert_eq!(recorded[0].events[0].name, "event");
    assert_eq!(
        recorded[0].events[0].attributes,
        kiana_telemetry::testing::attributes(&[("count", AttributeValue::Number(1.0))])
    );
    // `expect(...).not.toHaveProperty("startedMs")`：`RecordedTelemetrySpan` 没有该字段，
    // 这里用 JSON 视图断言（序列化后不出现 `startedMs`）。
    let recorded_json = kiana_telemetry::json::recorded_span_to_value(&recorded[0]);
    assert!(recorded_json.get("startedMs").is_none());
    assert!(recorded_json.get("endedMs").is_none());
}

/// `describe("in-memory store snapshots") > it("maps raw metric measurements to count-one rows
/// without exposing private timing in getMetrics")`。
#[tokio::test]
async fn maps_raw_metric_measurements_to_count_one_rows_without_exposing_private_timing_in_get_metrics()
 {
    let scripted = Arc::new(ScriptedClock::default());
    scripted.now_ms.store(20_001, Ordering::SeqCst);
    let clock: Arc<dyn Clock> = scripted;
    let telemetry = Arc::new(InMemoryTelemetryContext::with_clock(clock));
    telemetry.counter("calls", None).add(2.0, None);
    telemetry.counter("calls", None).add(3.0, None);

    let rows: Vec<(f64, f64, f64)> = telemetry
        .find_metrics(MetricQuery {
            name: "calls".to_string(),
            ..MetricQuery::default()
        })
        .await
        .into_iter()
        .map(|row| (row.interval_ms, row.count, row.sum))
        .collect();
    assert_eq!(rows, vec![(20_000.0, 1.0, 2.0), (20_000.0, 1.0, 3.0)]);

    let measurements = telemetry.get_metrics();
    assert_eq!(measurements.len(), 2);
    for (index, measurement) in measurements.iter().enumerate() {
        assert_eq!(measurement.kind, kiana_telemetry::MetricKind::Counter);
        assert_eq!(measurement.name, "calls");
        assert_eq!(measurement.description, None);
        assert_eq!(measurement.unit, None);
        assert_eq!(measurement.boundaries, None);
        assert_eq!(measurement.value, [2.0, 3.0][index]);
        assert_eq!(
            measurement.attributes,
            kiana_telemetry::SpanAttributes::new()
        );
        assert_eq!(measurement.context, None);
    }
}

/// `describe("in-memory store snapshots") > it("detaches link attributes and error details")`。
#[tokio::test]
async fn detaches_link_attributes_and_error_details() {
    let telemetry = InMemoryTelemetryContext::new();
    let (target_tx, target_rx) = tokio::sync::oneshot::channel::<Arc<dyn TelemetrySpan>>();
    let telemetry_for_target = telemetry.clone();
    telemetry_for_target
        .start_span_typed(options("target"), move |span| {
            Box::pin(async move {
                let _ = target_tx.send(span);
            })
        })
        .await
        .expect("target span");
    let target = target_rx.await.expect("target");

    let telemetry_for_source = telemetry.clone();
    telemetry_for_source
        .start_span_typed(
            SpanOptions {
                name: "source".to_string(),
                links: Some(vec![kiana_telemetry::SpanLink {
                    context: target.span_context_id(),
                    attributes: Some(kiana_telemetry::testing::attributes(&[(
                        "tags",
                        AttributeValue::StringArray(vec!["original".to_string()]),
                    )])),
                }]),
                attributes: None,
            },
            |span| {
                Box::pin(async move {
                    span.set_status(SpanStatus::Error {
                        error: Some(kiana_telemetry::StatusError {
                            name: "Expected".to_string(),
                            message: "original".to_string(),
                        }),
                    });
                })
            },
        )
        .await
        .expect("source span");

    let page = telemetry
        .find_traces(Some(TraceQuery {
            name: Some("source".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect("find traces");
    let trace = page.traces.first().expect("trace").clone();
    let mut span = trace.spans.first().expect("source span").clone();
    span.links[0]
        .attributes
        .as_mut()
        .expect("link attributes")
        .insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["changed".to_string()]),
        );
    if let SpanStatus::Error {
        error: Some(error), ..
    } = &mut span.status
    {
        error.message = "changed".to_string();
    }

    let fresh = telemetry
        .get_trace(&trace.trace_id)
        .await
        .expect("stored trace");
    let fresh_span = fresh.spans.first().expect("fresh span");
    assert_eq!(
        fresh_span.links[0]
            .attributes
            .as_ref()
            .expect("attributes")
            .get("tags"),
        Some(&AttributeValue::StringArray(vec!["original".to_string()]))
    );
    assert_eq!(
        fresh_span.status,
        SpanStatus::Error {
            error: Some(kiana_telemetry::StatusError {
                name: "Expected".to_string(),
                message: "original".to_string(),
            }),
        }
    );
}

// ---------------------------------------------------------------------------
// 套件自身的回归：断言确实存在、额外 fixture 确实被释放。
// ---------------------------------------------------------------------------

/// `remote_ids_case` 的 `await using sender/receiver` 必须释放两个额外 fixture（含失败路径）。
#[tokio::test]
async fn remote_ids_case_disposes_its_extra_fixtures() {
    let disposes = Arc::new(AtomicU64::new(0));
    let disposes_for_factory = Arc::clone(&disposes);
    let base = store_factory();
    let factory: StoreFixtureFactory = Arc::new(move || {
        let base = Arc::clone(&base);
        let disposes = Arc::clone(&disposes_for_factory);
        Box::pin(async move {
            let mut fixture = base().await?;
            let previous = Arc::clone(&fixture.dispose);
            fixture.dispose = Arc::new(move || {
                let previous = Arc::clone(&previous);
                let disposes = Arc::clone(&disposes);
                Box::pin(async move {
                    disposes.fetch_add(1, Ordering::SeqCst);
                    previous().await;
                })
            });
            Ok(fixture)
        })
    });
    let cases = telemetry_store_conformance(factory);
    let case = cases
        .iter()
        .find(|case| case.name.starts_with("inherits remote trace IDs"))
        .expect("remote ids case");
    (case.run)().await.expect("remote ids case runs");
    // runner 释放主 fixture；case 自己释放 sender 与 receiver（`await using` 的等价物）。
    assert_eq!(disposes.load(Ordering::SeqCst), 3);
}

/// 把 `getTrace` 里的继续 span 的 id 改写成父 id（= 发送方根 span 的 id）的 store 包装，
/// 用来证明 `remote_ids_case` 确实断言了「继续的 span 得到新的本地 id」。
struct ReusingIdStore {
    inner: Arc<dyn TelemetryStore>,
}

impl TelemetryStore for ReusingIdStore {
    fn find_traces(
        &self,
        query: Option<TraceQuery>,
    ) -> kiana_telemetry::index::BoxFuture<
        '_,
        Result<kiana_telemetry::TracePage, kiana_telemetry::TelemetryError>,
    > {
        self.inner.find_traces(query)
    }

    fn get_trace(
        &self,
        trace_id: &str,
    ) -> kiana_telemetry::index::BoxFuture<
        '_,
        Result<Option<kiana_telemetry::StoredTrace>, kiana_telemetry::TelemetryError>,
    > {
        let trace_id = trace_id.to_string();
        Box::pin(async move {
            let mut trace = self.inner.get_trace(&trace_id).await?;
            if let Some(trace) = trace.as_mut() {
                for span in &mut trace.spans {
                    if let Some(parent_id) = span.parent_id.clone() {
                        span.id = parent_id;
                    }
                }
            }
            Ok(trace)
        })
    }

    fn find_metrics(
        &self,
        query: MetricQuery,
    ) -> kiana_telemetry::index::BoxFuture<
        '_,
        Result<Vec<kiana_telemetry::StoredMetric>, kiana_telemetry::TelemetryError>,
    > {
        self.inner.find_metrics(query)
    }
}

/// 继续的 span 若复用发送方的 id，套件必须失败（TS store-conformance.ts:148 的断言）。
#[tokio::test]
async fn remote_ids_case_rejects_a_reused_continued_span_id() {
    let base = store_factory();
    let factory: StoreFixtureFactory = Arc::new(move || {
        let base = Arc::clone(&base);
        Box::pin(async move {
            let mut fixture = base().await?;
            fixture.store = Arc::new(ReusingIdStore {
                inner: Arc::clone(&fixture.store),
            });
            Ok(fixture)
        })
    });
    let cases = telemetry_store_conformance(factory);
    let case = cases
        .iter()
        .find(|case| case.name.starts_with("inherits remote trace IDs"))
        .expect("remote ids case");
    let error = (case.run)()
        .await
        .expect_err("the suite must reject a reused id");
    assert!(
        error.message.contains("fresh local id"),
        "unexpected error {error}"
    );
}

/// 第 1 条 conformance case 的前提：断言「无 settled trace」时，必须真的存在一条**已记录但未
/// 结算**的 span（否则空页只说明没有 span，`storedSpans` 的 `settled` 过滤没被检验）。
///
/// TS: `const active = f.context.startSpan(...)` 同步进入回调并 push recorded span，随后
/// `expect((await f.store.findTraces()).traces).toEqual([])` —— 被断言的是 unsettled span。
#[tokio::test]
async fn settled_case_holds_a_recorded_but_unsettled_span_before_its_first_assertion() {
    let scripted = Arc::new(ScriptedClock::default());
    let clock: Arc<dyn Clock> = scripted.clone();
    scripted.now_ms.store(20_000, Ordering::SeqCst);
    let context = Arc::new(InMemoryTelemetryContext::with_clock(clock));

    // 与 conformance case 同形状：回调记录事件后停在 gate 上。
    let mut active = Box::pin(context.start_span_typed(options("root"), |root| {
        Box::pin(async move {
            root.add_event("event", None);
            std::future::pending::<()>().await;
        })
    }));
    assert!(
        kiana_telemetry::testing::poll_once(&mut active).is_pending(),
        "the callback waits"
    );

    // span 已经记录（getSpans 看得到），但未结算。
    let spans = context.get_spans();
    assert_eq!(spans.len(), 1, "the span is recorded before it settles");
    assert!(!spans[0].settled);
    assert_eq!(spans[0].end_sequence, None);

    // 而 store 看不到它（`storedSpans().filter(settled)`）—— 这正是 case 的断言内容。
    assert!(
        context
            .find_traces(None)
            .await
            .expect("traces")
            .traces
            .is_empty(),
        "the recorded-but-unsettled span is filtered out"
    );

    drop(active);
    // 取消后同样不结算（TS 的 abandoned promise）。
    assert!(!context.get_spans()[0].settled);
    assert!(
        context
            .find_traces(None)
            .await
            .expect("traces")
            .traces
            .is_empty()
    );
}
