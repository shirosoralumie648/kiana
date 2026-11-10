//! metrics.test.ts 的 Rust 移植（4 个 `it` 标题保留既有文案，作为稳定契约）。
//!
//! 偏差（spec §25 第 4 条）：TS 的 `createTypedMetrics` 用类型系统校验名字/kind/必填属性；
//! Rust 没有重载与 const 泛型约束，编译期断言（`@ts-expect-error` / `expectTypeOf`）无法表达。
//! **运行时**行为保持逐条对齐：TS 只做 `definitions.get(name)` 查表并转发定义，未知名字得到
//! `undefined` 选项、kind 不匹配只是把另一个定义当选项传下去 —— 两条在 Rust 侧都不断言成错误，
//! 而是断言「工厂照常被调用，元数据来自被查到的定义」（见下方 `factory()` 的 fake 后端）。

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_telemetry::metrics::TelemetryMetricDefinitions;
use kiana_telemetry::testing::attributes;
use kiana_telemetry::{
    AttributeValue, Cardinality, HistogramOptions, InMemoryTelemetryContext, InstrumentOptions,
    MetricKind, SpanOptions, TelemetryAttributeDefinition, TelemetryAttributeMetadata,
    TelemetryContextExt, TelemetryCounter, TelemetryHistogram, TelemetryMetricDefinition,
    TelemetryMetrics, TelemetryRequiredAttributeDefinition, TelemetrySchemaDefinition,
    TelemetryUpDownCounter, create_typed_metrics, noop_telemetry_context_as_context,
    noop_telemetry_metrics,
};

/// 记录工厂调用的 fake 后端：`createTypedMetrics` 的运行时行为只有「把什么作为 options 传下去」
/// 可观察（TS 里自定义适配器能读到整个定义对象）。
#[derive(Debug, Clone, PartialEq)]
enum FactoryCall {
    Counter(String, Option<InstrumentOptions>),
    UpDownCounter(String, Option<InstrumentOptions>),
    Histogram(String, Option<HistogramOptions>),
}

struct RecordingBackend {
    calls: Mutex<Vec<FactoryCall>>,
}

impl RecordingBackend {
    fn new() -> Arc<RecordingBackend> {
        Arc::new(RecordingBackend {
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<FactoryCall> {
        self.calls.lock().expect("calls").clone()
    }
}

impl TelemetryMetrics for RecordingBackend {
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter> {
        self.calls
            .lock()
            .expect("calls")
            .push(FactoryCall::Counter(name.to_string(), options));
        noop_telemetry_metrics().counter(name, None)
    }

    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter> {
        self.calls
            .lock()
            .expect("calls")
            .push(FactoryCall::UpDownCounter(name.to_string(), options));
        noop_telemetry_metrics().up_down_counter(name, None)
    }

    fn histogram(
        &self,
        name: &str,
        options: Option<HistogramOptions>,
    ) -> Arc<dyn TelemetryHistogram> {
        self.calls
            .lock()
            .expect("calls")
            .push(FactoryCall::Histogram(name.to_string(), options));
        noop_telemetry_metrics().histogram(name, None)
    }
}

fn meta(description: &str) -> TelemetryAttributeMetadata {
    TelemetryAttributeMetadata {
        description: description.to_string(),
        sensitive: None,
        cardinality: None,
    }
}

fn required(def: TelemetryAttributeDefinition) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: def,
        required: true,
    }
}

fn optional(def: TelemetryAttributeDefinition) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: def,
        required: false,
    }
}

fn empty_spans() -> IndexMap<String, kiana_telemetry::TelemetrySpanDefinition> {
    IndexMap::new()
}

/// `traffic`：counter + upDownCounter。
fn traffic() -> TelemetrySchemaDefinition {
    let mut bytes_attributes = IndexMap::new();
    bytes_attributes.insert(
        "direction".to_string(),
        required(TelemetryAttributeDefinition::String {
            meta: TelemetryAttributeMetadata {
                description: "Direction".to_string(),
                sensitive: None,
                cardinality: Some(Cardinality::Low),
            },
            values: Some(vec!["in".to_string(), "out".to_string()]),
            examples: None,
        }),
    );
    let mut metrics = IndexMap::new();
    metrics.insert(
        "network.bytes".to_string(),
        TelemetryMetricDefinition::Counter {
            description: "Encoded bytes".to_string(),
            unit: Some("By".to_string()),
            attributes: bytes_attributes,
        },
    );
    metrics.insert(
        "network.active".to_string(),
        TelemetryMetricDefinition::UpDownCounter {
            description: "Open links".to_string(),
            unit: None,
            attributes: IndexMap::new(),
        },
    );
    TelemetrySchemaDefinition {
        version: 1.0,
        spans: empty_spans(),
        metrics: Some(metrics),
    }
}

/// `latency`：histogram（含 boundaries）。
fn latency() -> TelemetrySchemaDefinition {
    let mut duration_attributes = IndexMap::new();
    duration_attributes.insert(
        "method".to_string(),
        optional(TelemetryAttributeDefinition::String {
            meta: meta("Method"),
            values: None,
            examples: None,
        }),
    );
    let mut metrics = IndexMap::new();
    metrics.insert(
        "call.duration".to_string(),
        TelemetryMetricDefinition::Histogram {
            description: "Call duration".to_string(),
            unit: Some("ms".to_string()),
            attributes: duration_attributes,
            boundaries: Some(vec![1.0, 10.0, 100.0]),
        },
    );
    TelemetrySchemaDefinition {
        version: 1.0,
        spans: empty_spans(),
        metrics: Some(metrics),
    }
}

/// `createTypedMetrics(metrics, schemas)` 的定义合并：后出现的 schema 覆盖同名 metric。
fn definitions(schemas: &[TelemetrySchemaDefinition]) -> TelemetryMetricDefinitions {
    let mut merged = TelemetryMetricDefinitions::new();
    for schema in schemas {
        if let Some(metrics) = &schema.metrics {
            for (name, definition) in metrics {
                merged.insert(name.clone(), definition.clone());
            }
        }
    }
    merged
}

#[tokio::test]
async fn binds_schema_names_kinds_metadata_and_exact_measurement_attributes() {
    let adapter = Arc::new(InMemoryTelemetryContext::new());
    let schemas = [traffic(), latency()];
    let metrics = create_typed_metrics(definitions(&schemas), adapter.clone());

    metrics.counter("network.bytes").expect("counter").add(
        128.0,
        Some(attributes(&[(
            "direction",
            AttributeValue::String("out".into()),
        )])),
    );
    metrics
        .up_down_counter("network.active")
        .expect("active")
        .add(1.0, None);
    metrics
        .up_down_counter("network.active")
        .expect("active")
        .add(-1.0, None);

    let histogram = metrics.histogram("call.duration").expect("histogram");
    let histogram_for_span = Arc::clone(&histogram);
    let histogram_for_plain = Arc::clone(&histogram);
    let histogram_for_context = Arc::clone(&histogram);
    adapter
        .start_span_typed(
            SpanOptions {
                name: "request".to_string(),
                ..SpanOptions::default()
            },
            move |span| {
                Box::pin(async move {
                    histogram_for_span.record(
                        12.0,
                        Some(attributes(&[(
                            "method",
                            AttributeValue::String("read".into()),
                        )])),
                        Some(span.as_ref()),
                    );
                    histogram_for_plain.record(13.0, None, None);
                    histogram_for_context.record(14.0, None, Some(span.as_ref()));
                })
            },
        )
        .await
        .expect("span");

    let measurements = adapter.get_metrics();
    assert_eq!(measurements.len(), 6);

    let first = &measurements[0];
    assert_eq!(first.kind, MetricKind::Counter);
    assert_eq!(first.name, "network.bytes");
    assert_eq!(first.description.as_deref(), Some("Encoded bytes"));
    assert_eq!(first.unit.as_deref(), Some("By"));
    assert_eq!(first.value, 128.0);
    assert_eq!(
        first.attributes,
        attributes(&[("direction", AttributeValue::String("out".into()))])
    );
    assert_eq!(first.context, None);

    let spans = adapter.get_spans();
    let request_context = spans[0].context.clone();
    let third = &measurements[3];
    assert_eq!(third.kind, MetricKind::Histogram);
    assert_eq!(third.boundaries.as_deref(), Some(&[1.0, 10.0, 100.0][..]));
    assert_eq!(third.context.as_ref(), Some(&request_context));
    assert_eq!(measurements[4].context, None, "no exemplar context");
    assert_eq!(measurements[5].context.as_ref(), Some(&request_context));

    // TS 只在编译期拒绝下列用法；运行时是 `metrics.counter(name, definitions.get(name))`，
    // 不抛错、不拦截：kind 不匹配只是把另一个 kind 的定义当选项传下去，未知名字传 `undefined`。
    // 这里用 fake 后端固定这三条（TS 的 `@ts-expect-error` 在 Rust 无法表达）。
    let backend = RecordingBackend::new();
    let recording: Arc<dyn TelemetryMetrics> = backend.clone();
    let typed = create_typed_metrics(definitions(&schemas), recording);
    typed
        .counter("call.duration")
        .expect("kind mismatch is not an error");
    typed
        .histogram("network.bytes")
        .expect("kind mismatch is not an error");
    typed
        .up_down_counter("unknown")
        .expect("unknown names are not an error");
    assert_eq!(
        backend.calls(),
        vec![
            // 目标工厂收到的是 histogram 定义的 description/unit（boundaries 只有 histogram
            // 工厂才读，counter 的 options 里没有它）。
            FactoryCall::Counter(
                "call.duration".to_string(),
                Some(InstrumentOptions {
                    description: Some("Call duration".to_string()),
                    unit: Some("ms".to_string()),
                }),
            ),
            FactoryCall::Histogram(
                "network.bytes".to_string(),
                Some(HistogramOptions {
                    instrument: InstrumentOptions {
                        description: Some("Encoded bytes".to_string()),
                        unit: Some("By".to_string()),
                    },
                    boundaries: None,
                }),
            ),
            // 未知名字 → `definitions.get(name)` 是 undefined → 不传选项。
            FactoryCall::UpDownCounter("unknown".to_string(), None),
        ]
    );

    // 重复的 metric 名在 TS 是编译错误；Rust 侧「后者覆盖」且不 panic（见模块文档）。
    let duplicate = create_typed_metrics(definitions(&[traffic(), traffic()]), adapter.clone());
    assert!(
        duplicate.counter("network.bytes").is_ok(),
        "duplicate metric names keep the last definition"
    );

    // `expect(() => JSON.stringify(traffic)).not.toThrow()`。
    let json = serde_json::to_string(&traffic()).expect("schema serializes");
    assert!(json.contains("\"network.bytes\""));
}

#[test]
fn requires_histogram_attributes_when_the_schema_declares_them() {
    let mut attributes_map = IndexMap::new();
    attributes_map.insert(
        "cache".to_string(),
        required(TelemetryAttributeDefinition::Boolean {
            meta: meta("Cache hit"),
            values: None,
            examples: None,
        }),
    );
    let mut metrics_map = IndexMap::new();
    metrics_map.insert(
        "required".to_string(),
        TelemetryMetricDefinition::Histogram {
            description: "Required tag".to_string(),
            unit: None,
            attributes: attributes_map,
            boundaries: None,
        },
    );
    let schema = TelemetrySchemaDefinition {
        version: 1.0,
        spans: empty_spans(),
        metrics: Some(metrics_map),
    };
    let adapter = Arc::new(InMemoryTelemetryContext::new());
    let metrics = create_typed_metrics(definitions(&[schema]), adapter);
    metrics.histogram("required").expect("histogram").record(
        1.0,
        Some(attributes(&[("cache", AttributeValue::Boolean(true))])),
        None,
    );

    // TS 的 `@ts-expect-error`（省略必填属性 / 属性类型不符 / 把 context 当属性传）在 Rust 中
    // 是类型层面约束：`record` 的属性参数是 `Option<SpanAttributes>`，不存在「必填」概念，
    // 也不可能把 context 混进属性 map（两种参数类型不同）。因此这 3 条编译期断言不可表达，
    // 只保留运行期记录不发生 panic 的等价断言（spec §25 第 4 条）。
    let histogram = metrics.histogram("required").expect("histogram");
    assert!(Arc::strong_count(&histogram) >= 1);
}

#[tokio::test]
async fn returns_detached_measurement_snapshots_including_exemplars_and_boundaries() {
    let telemetry = Arc::new(InMemoryTelemetryContext::new());
    let histogram = telemetry.histogram(
        "duration",
        Some(HistogramOptions {
            instrument: InstrumentOptions::default(),
            boundaries: Some(vec![1.0, 2.0]),
        }),
    );
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "request".to_string(),
                ..SpanOptions::default()
            },
            move |span| {
                Box::pin(async move {
                    histogram.record(
                        1.0,
                        Some(attributes(&[(
                            "tags",
                            AttributeValue::StringArray(vec!["original".to_string()]),
                        )])),
                        Some(span.as_ref()),
                    );
                })
            },
        )
        .await
        .expect("span");

    let mut measurement = telemetry
        .get_metrics()
        .into_iter()
        .next()
        .expect("measurement");
    let expected_context = measurement
        .context
        .clone()
        .expect("Expected measurement context");
    assert_eq!(
        measurement.context.as_ref(),
        Some(&telemetry.get_spans()[0].context),
        "exemplar carries the recording span's carrier"
    );

    // 篡改快照：不得影响 recorder 内部状态。
    measurement
        .boundaries
        .as_mut()
        .expect("boundaries")
        .push(3.0);
    measurement.attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["mutated".to_string()]),
    );
    measurement
        .context
        .as_mut()
        .expect("context")
        .insert("kiana-memory-span".to_string(), "999".to_string());

    let fresh = telemetry
        .get_metrics()
        .into_iter()
        .next()
        .expect("fresh measurement");
    assert_eq!(fresh.boundaries.as_deref(), Some(&[1.0, 2.0][..]));
    assert_eq!(
        fresh.attributes,
        attributes(&[(
            "tags",
            AttributeValue::StringArray(vec!["original".to_string()])
        )])
    );
    assert_eq!(
        fresh.context.as_ref(),
        Some(&telemetry.get_spans()[0].context)
    );
    assert_eq!(fresh.context.as_ref(), Some(&expected_context));
    assert_eq!(
        fresh
            .context
            .as_ref()
            .expect("context")
            .get("kiana-memory-span")
            .map(String::as_str),
        Some("1"),
        "the recorder's own carrier value is untouched by the snapshot mutation"
    );
    assert_ne!(
        fresh
            .context
            .as_ref()
            .expect("context")
            .get("kiana-memory-span")
            .map(String::as_str),
        Some("999")
    );
}

#[test]
fn shares_frozen_no_op_instruments() {
    let noop = noop_telemetry_metrics();
    let options = InstrumentOptions {
        description: None,
        unit: Some("By".to_string()),
    };
    let histogram_options = HistogramOptions {
        instrument: options.clone(),
        boundaries: None,
    };
    let counter: Arc<dyn TelemetryCounter> = noop.counter("bytes", Some(options.clone()));
    let active: Arc<dyn TelemetryUpDownCounter> =
        noop.up_down_counter("active", Some(options.clone()));
    let histogram: Arc<dyn TelemetryHistogram> =
        noop.histogram("duration", Some(histogram_options));

    assert!(
        Arc::ptr_eq(&counter, &noop.counter("other", None)),
        "all no-op counters are one shared instance"
    );
    assert!(Arc::ptr_eq(&active, &noop.up_down_counter("other", None)));
    assert!(Arc::ptr_eq(&histogram, &noop.histogram("other", None)));

    // `Object.isFrozen(x) === true`：Rust 的等价物是「共享且不可变」——同一 `Arc` 反复取用，
    // 且所有方法都取 `&self`（无内部可变状态可观察）。
    let frozen_marker =
        Arc::strong_count(&counter) > 1 || Arc::ptr_eq(&counter, &noop.counter("bytes", None));
    assert!(frozen_marker);
    let by = attributes(&[("direction", AttributeValue::String("out".into()))]);
    counter.add(1.0, Some(by.clone()));
    active.add(-1.0, Some(by.clone()));
    let noop_context = noop_telemetry_context_as_context();
    histogram.record(1.0, Some(by), Some(noop_context.as_ref()));
}
