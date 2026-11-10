//! conformance.test.ts 的 Rust 移植。
//!
//! TS 里 `createTelemetryAdapterConformance(...)` 的每个 case 在
//! `describe("InMemoryTelemetryContext conformance") > describe(group) > it(name)` 下注册。
//! Rust 的 `#[test]` 没有嵌套 describe，因此：
//! - 每个 case 一个 `#[test]`，函数名是 `it` 标题的 snake_case，doc 注释保留 `describe > it` 原文；
//! - `registers_the_verbatim_describe_and_it_titles` 断言 group / name 字符串保持既有稳定文案
//!   （即 describe/it 标题本身没有被改写）。
//!
//! 额外保留 `it("returns detached snapshots without exposing mutable recording state")`。

use std::sync::Arc;

use kiana_telemetry::testing::{
    TelemetryAdapterFixture, TelemetryAdapterFixtureFactory, create_telemetry_adapter_conformance,
};
use kiana_telemetry::{
    AttributeValue, InMemoryTelemetryContext, RecordedTelemetryMeasurement, RecordedTelemetrySpan,
    SpanOptions, TelemetryContext, TelemetryContextExt,
};

fn factory() -> TelemetryAdapterFixtureFactory {
    Arc::new(|| {
        Box::pin(async move {
            let context = Arc::new(InMemoryTelemetryContext::new());
            let context_for_metrics: Arc<dyn TelemetryContext> = context.clone();
            let context_for_propagation: Arc<dyn TelemetryContext> = context.clone();
            let context_for_metrics_reader = Arc::clone(&context);
            let context_for_spans_reader = Arc::clone(&context);
            Ok(TelemetryAdapterFixture {
                context: context_for_metrics.clone(),
                propagation: context_for_propagation.propagation(),
                metrics: context_for_metrics.metrics(),
                get_metrics: Some(Arc::new(move || {
                    let context = Arc::clone(&context_for_metrics_reader);
                    Box::pin(async move { context.get_metrics() })
                })),
                get_spans: Arc::new(move || {
                    let context = Arc::clone(&context_for_spans_reader);
                    Box::pin(async move { context.get_spans() })
                }),
                dispose: Some(Arc::new(|| Box::pin(async {}))),
            })
        })
    })
}

async fn run_case(name: &str) {
    let cases = create_telemetry_adapter_conformance(factory());
    let case = cases
        .iter()
        .find(|candidate| candidate.name == name)
        .unwrap_or_else(|| panic!("missing conformance case {name:?}"));
    (case.run)()
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));
}

/// `describe("capabilities") > it(...)`。
#[tokio::test]
async fn carries_adapter_capabilities_through_contexts_and_spans() {
    run_case("carries adapter capabilities through contexts and spans").await;
}

/// `describe("metrics") > it(...)`。
#[tokio::test]
async fn records_ordered_measurements_and_optional_histogram_exemplars() {
    run_case("records ordered measurements and optional histogram exemplars").await;
}

/// `describe("metrics") > it(...)`。
#[tokio::test]
async fn copies_instrument_metadata_and_measurement_attributes() {
    run_case("copies instrument metadata and measurement attributes").await;
}

/// `describe("links") > it(...)`。
#[tokio::test]
async fn records_initial_and_late_links_without_changing_parents() {
    run_case("records initial and late links without changing parents").await;
}

/// `describe("links") > it(...)`。
#[tokio::test]
async fn ignores_links_added_after_source_settlement() {
    run_case("ignores links added after source settlement").await;
}

/// `describe("propagation") > it(...)`。
#[tokio::test]
async fn roundtrips_parent_identity_through_json_without_retaining_the_carrier() {
    run_case("roundtrips parent identity through JSON without retaining the carrier").await;
}

/// `describe("propagation") > it(...)`。
#[tokio::test]
async fn absent_and_unknown_carriers_produce_recorded_roots() {
    run_case("absent and unknown carriers produce recorded roots").await;
}

/// `describe("propagation") > it(...)`。
#[tokio::test]
async fn propagates_across_fresh_adapters_and_multiple_hops() {
    run_case("propagates across fresh adapters and multiple hops").await;
}

/// `describe("callback lifecycle") > it(...)`。
#[tokio::test]
async fn admits_once_synchronously_and_preserves_the_result() {
    run_case("admits once synchronously and preserves the result").await;
}

/// `describe("callback lifecycle") > it(...)`。
#[tokio::test]
async fn preserves_synchronous_and_asynchronous_rejection_values() {
    run_case("preserves synchronous and asynchronous rejection values").await;
}

/// `describe("status") > it(...)`。
#[tokio::test]
async fn uses_last_explicit_status_without_automatic_overwrite() {
    run_case("uses last explicit status without automatic overwrite").await;
}

/// `describe("recording") > it(...)`。
#[tokio::test]
async fn merges_attributes_and_records_ordered_events() {
    run_case("merges attributes and records ordered events").await;
}

/// `describe("recording") > it(...)`。
#[tokio::test]
async fn makes_calls_after_settlement_inert() {
    run_case("makes calls after settlement inert").await;
}

/// `describe("parentage") > it(...)`。
#[tokio::test]
async fn records_nested_and_concurrent_child_relationships() {
    run_case("records nested and concurrent child relationships").await;
}

/// describe / it 标题逐字校验（对应 TS 的 `new Set(conformance.map(...))` 分组）。
#[test]
fn registers_the_verbatim_describe_and_it_titles() {
    let cases = create_telemetry_adapter_conformance(factory());
    assert_eq!(cases.len(), 14);
    let expected = [
        (
            "capabilities",
            "carries adapter capabilities through contexts and spans",
        ),
        (
            "metrics",
            "records ordered measurements and optional histogram exemplars",
        ),
        (
            "metrics",
            "copies instrument metadata and measurement attributes",
        ),
        (
            "links",
            "records initial and late links without changing parents",
        ),
        ("links", "ignores links added after source settlement"),
        (
            "propagation",
            "roundtrips parent identity through JSON without retaining the carrier",
        ),
        (
            "propagation",
            "absent and unknown carriers produce recorded roots",
        ),
        (
            "propagation",
            "propagates across fresh adapters and multiple hops",
        ),
        (
            "callback lifecycle",
            "admits once synchronously and preserves the result",
        ),
        (
            "callback lifecycle",
            "preserves synchronous and asynchronous rejection values",
        ),
        (
            "status",
            "uses last explicit status without automatic overwrite",
        ),
        ("recording", "merges attributes and records ordered events"),
        ("recording", "makes calls after settlement inert"),
        (
            "parentage",
            "records nested and concurrent child relationships",
        ),
    ];
    for (index, (group, name)) in expected.iter().enumerate() {
        assert_eq!(cases[index].group, *group, "group #{index}");
        assert_eq!(cases[index].name, *name, "it #{index}");
    }
    // 分组顺序与首次出现顺序一致（TS 用 `new Set(...)` 去重后按插入序注册）。
    let groups: Vec<&str> = cases.iter().map(|case| case.group).collect::<Vec<_>>();
    let mut unique = Vec::new();
    for group in groups {
        if !unique.contains(&group) {
            unique.push(group);
        }
    }
    assert_eq!(
        unique,
        vec![
            "capabilities",
            "metrics",
            "links",
            "propagation",
            "callback lifecycle",
            "status",
            "recording",
            "parentage"
        ]
    );
}

/// TS 的额外 `it`（不属于 conformance 套件，留在同一个 describe 下）。
#[tokio::test]
async fn returns_detached_snapshots_without_exposing_mutable_recording_state() {
    let context = InMemoryTelemetryContext::new();
    let (open_tx, open_rx) = tokio::sync::oneshot::channel::<(Option<bool>, Option<f64>)>();
    let context_for_span = context.clone();
    context
        .start_span_typed(
            SpanOptions {
                name: "snapshot".to_string(),
                attributes: Some(kiana_telemetry::testing::attributes(&[(
                    "tags",
                    AttributeValue::StringArray(vec!["initial".to_string()]),
                )])),
                links: None,
            },
            move |span| {
                let open_tx = std::sync::Mutex::new(Some(open_tx));
                Box::pin(async move {
                    span.add_event(
                        "event",
                        Some(kiana_telemetry::testing::attributes(&[(
                            "value",
                            AttributeValue::Number(1.0),
                        )])),
                    );
                    let spans = context_for_span.get_spans();
                    let open = spans.first();
                    if let Some(tx) = open_tx.lock().expect("open sender").take() {
                        let _ = tx.send((
                            open.map(|open| open.settled),
                            open.and_then(|open| open.end_sequence),
                        ));
                    }
                })
            },
        )
        .await
        .expect("span");

    let (open_settled, open_end_sequence) = open_rx.await.expect("open snapshot");
    assert_eq!(open_settled, Some(false));
    assert_eq!(
        open_end_sequence, None,
        "endSequence is undefined before settlement"
    );

    let mut first = context.get_spans().into_iter().next().expect("first");
    assert!(first.settled);
    assert_eq!(first.end_sequence, Some(1.0));

    // 篡改快照后重新读取，recorder 状态不受影响。
    first.attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["mutated".to_string()]),
    );
    first
        .events
        .first_mut()
        .expect("event")
        .attributes
        .insert("value".to_string(), AttributeValue::Number(2.0));

    let second = context.get_spans().into_iter().next().expect("second");
    assert_eq!(
        second.attributes,
        kiana_telemetry::testing::attributes(&[(
            "tags",
            AttributeValue::StringArray(vec!["initial".to_string()])
        )])
    );
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].name, "event");
    assert_eq!(
        second.events[0].attributes,
        kiana_telemetry::testing::attributes(&[("value", AttributeValue::Number(1.0))])
    );
}

// 让未使用的导入在 `--all-targets` 下不产生 warning（fixture 读取器类型在断言里用到）。
#[allow(dead_code)]
fn _assert_reader_types(
    _metrics: Option<Vec<RecordedTelemetryMeasurement>>,
    _spans: Vec<RecordedTelemetrySpan>,
) {
}

// ---------------------------------------------------------------------------
// `capabilities` case：`inject` 返回 `undefined` 时仍然 extract 并启动
// `capabilities.extracted`（TS conformance.ts:65-69 无条件执行这一分支）。
// ---------------------------------------------------------------------------

/// 一个「传播不可用」的适配器：`inject` 恒为 `None`（无法把 span 序列化成 carrier），
/// `extract(undefined)` 回退到根。所有上下文/span 都是同一个 `Arc<Adapter>`（与
/// `NOOP_TELEMETRY_CONTEXT` 一样复用惰性 span），因此能力恒等断言成立。
struct InjectlessAdapter {
    me: std::sync::OnceLock<std::sync::Weak<InjectlessAdapter>>,
    started: std::sync::Mutex<Vec<String>>,
    extracts: std::sync::atomic::AtomicUsize,
}

impl InjectlessAdapter {
    fn new() -> Arc<InjectlessAdapter> {
        Arc::new_cyclic(|weak| InjectlessAdapter {
            me: std::sync::OnceLock::from(weak.clone()),
            started: std::sync::Mutex::new(Vec::new()),
            extracts: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn handle(&self) -> Arc<InjectlessAdapter> {
        self.me
            .get()
            .expect("cyclic weak is set at construction")
            .upgrade()
            .expect("the adapter outlives its contexts")
    }
}

impl kiana_telemetry::TelemetryMetrics for InjectlessAdapter {
    fn counter(
        &self,
        _name: &str,
        _options: Option<kiana_telemetry::InstrumentOptions>,
    ) -> Arc<dyn kiana_telemetry::TelemetryCounter> {
        kiana_telemetry::noop_telemetry_metrics().counter("", None)
    }

    fn up_down_counter(
        &self,
        _name: &str,
        _options: Option<kiana_telemetry::InstrumentOptions>,
    ) -> Arc<dyn kiana_telemetry::TelemetryUpDownCounter> {
        kiana_telemetry::noop_telemetry_metrics().up_down_counter("", None)
    }

    fn histogram(
        &self,
        _name: &str,
        _options: Option<kiana_telemetry::HistogramOptions>,
    ) -> Arc<dyn kiana_telemetry::TelemetryHistogram> {
        kiana_telemetry::noop_telemetry_metrics().histogram("", None)
    }
}

impl kiana_telemetry::TelemetryPropagation for InjectlessAdapter {
    fn inject(
        &self,
        _context: &dyn kiana_telemetry::TelemetryContext,
    ) -> Option<kiana_telemetry::TelemetryCarrier> {
        None
    }

    fn extract(
        &self,
        _carrier: Option<&kiana_telemetry::TelemetryCarrier>,
    ) -> Arc<dyn kiana_telemetry::TelemetryContext> {
        self.extracts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let handle: Arc<dyn kiana_telemetry::TelemetryContext> = self.handle();
        handle
    }
}

impl kiana_telemetry::TelemetryContext for InjectlessAdapter {
    fn metrics(&self) -> Option<Arc<dyn kiana_telemetry::TelemetryMetrics>> {
        let handle: Arc<dyn kiana_telemetry::TelemetryMetrics> = self.handle();
        Some(handle)
    }

    fn propagation(&self) -> Option<Arc<dyn kiana_telemetry::TelemetryPropagation>> {
        let handle: Arc<dyn kiana_telemetry::TelemetryPropagation> = self.handle();
        Some(handle)
    }

    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: kiana_telemetry::index::SpanCallback<'a>,
    ) -> kiana_telemetry::index::BoxFuture<
        'a,
        Result<kiana_telemetry::index::ErasedValue, kiana_telemetry::TelemetryError>,
    > {
        self.started
            .lock()
            .expect("started spans")
            .push(options.name.clone());
        let handle: Arc<dyn kiana_telemetry::TelemetrySpan> = self.handle();
        callback(handle)
    }

    fn span_context_id(&self) -> kiana_telemetry::SpanLinkContext {
        kiana_telemetry::SpanLinkContext::default()
    }
}

impl kiana_telemetry::TelemetrySpan for InjectlessAdapter {
    fn add_link(&self, _link: kiana_telemetry::SpanLink) {}
    fn add_event(&self, _name: &str, _attributes: Option<kiana_telemetry::SpanAttributes>) {}
    fn set_attributes(&self, _attributes: kiana_telemetry::SpanAttributes) {}
    fn set_status(&self, _status: kiana_telemetry::SpanStatus) {}
}

/// `inject` 返回 `None` 时，`capabilities` case 仍必须 `extract` 并启动
/// `capabilities.extracted`（并对其断言能力）。
#[tokio::test]
async fn capabilities_case_extracts_even_when_inject_returns_none() {
    let tracker = Arc::new(std::sync::Mutex::new(None::<Arc<InjectlessAdapter>>));
    let tracker_for_factory = Arc::clone(&tracker);
    let factory: TelemetryAdapterFixtureFactory = Arc::new(move || {
        let tracker = Arc::clone(&tracker_for_factory);
        Box::pin(async move {
            let adapter = InjectlessAdapter::new();
            *tracker.lock().expect("tracker") = Some(Arc::clone(&adapter));
            let context: Arc<dyn kiana_telemetry::TelemetryContext> = adapter.clone();
            let metrics: Arc<dyn kiana_telemetry::TelemetryMetrics> = adapter.clone();
            let propagation: Arc<dyn kiana_telemetry::TelemetryPropagation> = adapter;
            Ok(TelemetryAdapterFixture {
                context,
                propagation: Some(propagation),
                metrics: Some(metrics),
                get_metrics: None,
                get_spans: Arc::new(|| Box::pin(async { Vec::new() })),
                dispose: None,
            })
        })
    });
    let cases = create_telemetry_adapter_conformance(factory);
    let case = cases
        .iter()
        .find(|case| case.name == "carries adapter capabilities through contexts and spans")
        .expect("capabilities case");
    (case.run)().await.expect("capabilities case runs");
    let adapter = tracker
        .lock()
        .expect("tracker")
        .clone()
        .expect("factory ran");
    // `extract` 被调用（TS 无条件执行 `propagation.extract(propagation.inject(span))`）。
    assert_eq!(
        adapter.extracts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "extract must run even though inject returned None"
    );
    // `capabilities.extracted` 被启动（回退到根之后）。
    let started = adapter.started.lock().expect("started spans").clone();
    for name in [
        "capabilities",
        "capabilities.child",
        "capabilities.extracted",
        "capabilities.inert",
    ] {
        assert!(
            started.iter().any(|candidate| candidate == name),
            "missing span {name}: {started:?}"
        );
    }
}
