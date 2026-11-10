//! telemetry.test.ts 的 Rust 移植。
//!
//! TS 的 5 个 `it`：其中若干断言是编译期类型检查（`expectTypeOf`），Rust 无法表达，
//! 这里保留其运行时部分（schema 恒等 / 可序列化、typed starter 的父子关系、no-op 行为），
//! 并在注释里标注被丢弃的编译期断言（spec §25 第 4 条：仅类型层面的保证）。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_telemetry::testing::{expect, poll_once};
use kiana_telemetry::{
    AttributeValue, Cardinality, InMemoryTelemetryContext, SpanAttributes, SpanOptions, SpanStatus,
    TelemetryAttributeDefinition, TelemetryAttributeMetadata, TelemetryContext,
    TelemetryContextExt, TelemetryEventDefinition, TelemetryParentDefinition,
    TelemetryRequiredAttributeDefinition, TelemetrySchemaDefinition, TelemetrySpan,
    TelemetrySpanDefinition, TelemetryStatusDefinition, TypedSpanStarter,
    create_typed_span_starter, define_telemetry_schema, noop_telemetry_context,
    noop_telemetry_metrics, noop_telemetry_propagation,
};
use serde_json::Value;

fn required(def: TelemetryAttributeDefinition) -> TelemetryRequiredAttributeDefinition {
    TelemetryRequiredAttributeDefinition {
        definition: def,
        required: true,
    }
}

fn string_definition(
    description: &str,
    values: Option<Vec<&str>>,
    cardinality: Option<Cardinality>,
) -> TelemetryAttributeDefinition {
    TelemetryAttributeDefinition::String {
        meta: TelemetryAttributeMetadata {
            description: description.to_string(),
            sensitive: None,
            cardinality,
        },
        values: values.map(|values| values.into_iter().map(str::to_string).collect()),
        examples: None,
    }
}

fn operation_schema() -> TelemetrySchemaDefinition {
    let mut start_attributes = IndexMap::new();
    start_attributes.insert(
        "kind".to_string(),
        required(string_definition("Kind", Some(vec!["read", "write"]), None)),
    );
    let mut event_attributes = IndexMap::new();
    event_attributes.insert(
        "outcome".to_string(),
        required(string_definition(
            "Outcome",
            Some(vec!["ok", "error"]),
            None,
        )),
    );
    let mut events = IndexMap::new();
    events.insert(
        "result".to_string(),
        TelemetryEventDefinition {
            description: "Result".to_string(),
            attributes: event_attributes,
        },
    );
    let mut spans = IndexMap::new();
    spans.insert(
        "operation".to_string(),
        TelemetrySpanDefinition {
            description: "Test operation".to_string(),
            parents: Some(TelemetryParentDefinition::Any),
            start_attributes,
            end_attributes: IndexMap::new(),
            events: Some(events),
            status: TelemetryStatusDefinition {
                default: "ok".to_string(),
                error_when: "The operation fails".to_string(),
            },
        },
    );
    define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1.0,
        spans,
        metrics: None,
    })
}

// TS: describe("telemetry schemas") > it("preserves serializable definitions and infers exact attributes")
#[test]
fn preserves_serializable_definitions_and_infers_exact_attributes() {
    let definition = operation_schema();
    // `expect(schema).toBe(definition)`：Rust 的 `define_telemetry_schema` 是恒等函数（按值）。
    let schema = define_telemetry_schema(definition.clone());
    assert_eq!(schema, definition);
    let value = serde_json::to_value(&schema).expect("schema serializes");
    assert!(value.is_object());
    // 编译期断言（`expectTypeOf`/`@ts-expect-error`）在 Rust 中丢弃：类型化 span 的
    // 精确属性校验属于 TS 类型系统，见 spec §25 第 4 条。
}

// TS: describe("telemetry schemas") > it("combines schema vocabularies and binds child starters to their parent spans")
#[tokio::test]
async fn combines_schema_vocabularies_and_binds_child_starters_to_their_parent_spans() {
    let operation = operation_schema();
    let request = {
        let mut start_attributes = IndexMap::new();
        start_attributes.insert(
            "provider".to_string(),
            required(string_definition("Provider", None, None)),
        );
        let mut end_attributes = IndexMap::new();
        end_attributes.insert(
            "response".to_string(),
            string_definition("Response kind", None, None),
        );
        let mut spans = IndexMap::new();
        spans.insert(
            "request".to_string(),
            TelemetrySpanDefinition {
                description: "Request".to_string(),
                parents: Some(TelemetryParentDefinition::Spans {
                    spans: vec!["operation".to_string()],
                }),
                start_attributes,
                end_attributes,
                events: None,
                status: TelemetryStatusDefinition {
                    default: "ok".to_string(),
                    error_when: "The request fails".to_string(),
                },
            },
        );
        define_telemetry_schema(TelemetrySchemaDefinition {
            version: 3.0,
            spans,
            metrics: None,
        })
    };
    let telemetry_context = InMemoryTelemetryContext::new();
    let start_span: TypedSpanStarter = create_typed_span_starter(
        Arc::new(telemetry_context.clone()),
        &[operation.clone(), request],
    );

    let result: i32 = start_span
        .start(
            "operation",
            attributes(&[("kind", AttributeValue::String("read".into()))]),
            |_operation_span, start_child_span| {
                Box::pin(async move {
                    start_child_span
                        .start(
                            "request",
                            attributes(&[("provider", AttributeValue::String("example".into()))]),
                            |request_span, _| {
                                Box::pin(async move {
                                    request_span.set_attributes(attributes(&[(
                                        "response",
                                        AttributeValue::String("cached".into()),
                                    )]));
                                    42
                                })
                            },
                        )
                        .await
                        .expect("child starter")
                })
            },
        )
        .await
        .expect("typed starter");
    assert_eq!(result, 42);

    let spans = telemetry_context.get_spans();
    let operation_span = spans
        .iter()
        .find(|span| span.name == "operation")
        .expect("operation span");
    let request_span = spans
        .iter()
        .find(|span| span.name == "request")
        .expect("request span");
    assert_eq!(operation_span.parent_id, None);
    assert_eq!(request_span.parent_id, Some(operation_span.id));

    // 同值 rejection：Rust 的「throw」只有擦除形式 `start_span`（回调返回 `Err`）能表达；
    // 冻结契约把 rejection 值固定为 TelemetryError，因此按同一错误比较。
    for name in ["operation", "request"] {
        let sync_error = kiana_telemetry::TelemetryError::new("sync");
        let expected = sync_error.clone();
        let result = start_span
            .context()
            .start_span(
                SpanOptions {
                    name: name.to_string(),
                    attributes: Some(attributes(&[(
                        "kind",
                        AttributeValue::String("write".into()),
                    )])),
                    links: None,
                },
                Box::new(move |_| Box::pin(async move { Err(sync_error) })),
            )
            .await
            .expect_err("sync rejection");
        assert_eq!(result, expected);
    }
}

fn attributes(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
    let mut map = IndexMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

// TS: describe("NOOP_TELEMETRY_CONTEXT") > it("admits callbacks synchronously and reuses one inert span")
#[tokio::test]
async fn noop_admits_callbacks_synchronously_and_reuses_one_inert_span() {
    let noop = noop_telemetry_context();
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let admitted_for_cb = Arc::clone(&admitted);
    let first_span = Arc::new(std::sync::Mutex::new(None::<Arc<dyn TelemetrySpan>>));
    let first_span_for_cb = Arc::clone(&first_span);
    let mut future = Box::pin(noop.start_span_typed(
        SpanOptions {
            name: "first".to_string(),
            attributes: None,
            links: None,
        },
        move |span| {
            admitted_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            let span_for_child = Arc::clone(&span);
            *first_span_for_cb.lock().unwrap() = Some(span);
            Box::pin(async move {
                let child = span_for_child
                    .start_span_typed(
                        SpanOptions {
                            name: "child".to_string(),
                            attributes: None,
                            links: None,
                        },
                        |child_span| Box::pin(async move { child_span }),
                    )
                    .await
                    .expect("noop child");
                // `child === span`
                assert!(Arc::ptr_eq(&child, &span_for_child));
                42
            })
        },
    ));
    let first_poll = poll_once(&mut future);
    assert!(admitted.load(std::sync::atomic::Ordering::SeqCst));
    let result = match first_poll {
        std::task::Poll::Pending => future.await,
        std::task::Poll::Ready(value) => value,
    };
    assert_eq!(result.expect("noop result"), 42);

    let captured = first_span.lock().unwrap().take().expect("first span");
    // `Object.isFrozen(firstSpan)` → Rust 里等价的可观察性质是「与单例共享同一分配」。
    let singleton = noop_telemetry_context();
    assert!(Arc::ptr_eq(&captured, &singleton));
    let captured_context: Arc<dyn TelemetryContext> = captured.clone();
    assert!(Arc::ptr_eq(
        &captured_context.metrics().expect("metrics"),
        &noop_telemetry_metrics()
    ));
    assert!(Arc::ptr_eq(
        &captured_context.propagation().expect("propagation"),
        &noop_telemetry_propagation()
    ));
}

// TS: describe("NOOP_TELEMETRY_CONTEXT") > it("preserves synchronous and asynchronous rejection values")
#[tokio::test]
async fn noop_preserves_synchronous_and_asynchronous_rejection_values() {
    let noop = noop_telemetry_context();
    let sync_error = kiana_telemetry::TelemetryError::new("sync");
    let expected = sync_error.clone();
    let error = noop
        .start_span(
            SpanOptions {
                name: "sync".to_string(),
                attributes: None,
                links: None,
            },
            Box::new(move |_| Box::pin(async move { Err(sync_error) })),
        )
        .await
        .expect_err("sync rejection");
    assert_eq!(error, expected);

    let async_error = kiana_telemetry::TelemetryError::new("async");
    let expected = async_error.clone();
    let error = noop
        .start_span(
            SpanOptions {
                name: "async".to_string(),
                attributes: None,
                links: None,
            },
            Box::new(move |_| Box::pin(async move { Err(async_error) })),
        )
        .await
        .expect_err("async rejection");
    assert_eq!(error, expected);
}

// TS: describe("NOOP_TELEMETRY_CONTEXT") > it("ignores typed recording payloads")
#[tokio::test]
async fn noop_ignores_typed_recording_payloads() {
    let noop = noop_telemetry_context();
    let result = noop
        .start_span_typed(
            SpanOptions {
                name: "operation".to_string(),
                attributes: Some(attributes(&[(
                    "kind",
                    AttributeValue::String("test".into()),
                )])),
                links: None,
            },
            |span| {
                Box::pin(async move {
                    span.add_event(
                        "event",
                        Some(attributes(&[("count", AttributeValue::Number(1.0))])),
                    );
                    span.set_attributes(attributes(&[("complete", AttributeValue::Boolean(true))]));
                    span.set_status(SpanStatus::Error {
                        error: Some(kiana_telemetry::StatusError {
                            name: "Expected".to_string(),
                            message: "test".to_string(),
                        }),
                    });
                    span.add_link(kiana_telemetry::SpanLink {
                        context: span.span_context_id(),
                        attributes: None,
                    });
                    42
                })
            },
        )
        .await
        .expect("noop result");
    assert_eq!(result, 42);
}

// ---------------------------------------------------------------------------
// schema 黄金向量（TS 的 AI_TELEMETRY_SCHEMA / DURABLE_TELEMETRY_SCHEMA）
// ---------------------------------------------------------------------------

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/schema_json.json")).expect("schema_json.json")
}

#[test]
fn schema_json_matches_ts_for_ai_and_durable_schemas() {
    // schema 的 `JSON.stringify` 文本由 tests/schema_golden.rs 里的 serde 结构体逐字比对；
    // 这里只验证 fixture 自身结构，避免重复解析。
    let fixture = fixture();
    for key in ["ai", "durable"] {
        let text = fixture
            .get(key)
            .and_then(Value::as_str)
            .expect("schema text");
        let parsed: Value = serde_json::from_str(text).expect("schema JSON");
        assert!(parsed.get("version").is_some(), "{key}");
        assert!(parsed.get("spans").is_some(), "{key}");
    }
}

#[test]
fn expect_helper_reports_the_verbatim_message() {
    let error = expect(false, "Expected recorded span x").expect_err("expect should fail");
    assert_eq!(error.message, "Expected recorded span x");
}

/// spec §27.1：`testing/conformance.ts` 的 6 条逐字错误文本。前两条（`findSpan`、
/// `rejectsWithSameValue` 的失败分支）可以直接触发；其余四条是 case 内部的断言消息，这里逐字
/// 核对源码里确实存在（避免重构时被改写）。
#[tokio::test]
async fn conformance_helpers_use_the_verbatim_spec_27_1_messages() {
    // `findSpan(spans, name)` → ``Expected recorded span ${name}``。
    let missing = kiana_telemetry::testing::find_span(&[], "request").expect_err("no spans");
    assert_eq!(missing.message, "Expected recorded span request");

    // `rejectsWithSameValue(operation, expected)` 在操作成功时 → `Expected operation to reject`。
    let expected = kiana_telemetry::TelemetryError::new("boom");
    let resolved = kiana_telemetry::testing::rejects_with_same_value(
        Box::pin(async { Ok(Box::new(()) as kiana_telemetry::index::ErasedValue) }),
        &expected,
    )
    .await
    .expect_err("the operation must reject");
    assert_eq!(resolved.message, "Expected operation to reject");

    // 其余四条由 conformance 套件在断言失败时产生；逐字存在于 `testing.rs`。
    for message in [
        "Expected target span",
        "Expected source span",
        "Expected propagation on each fixture",
        "Expected callback span",
    ] {
        assert!(
            include_str!("../src/testing.rs").contains(message),
            "missing verbatim message {message:?}"
        );
    }
}

/// spec §19.2 的 59 个 `index.ts` 导出名 + `render`/`testing` 子模块名必须在 Rust 侧存在。
///
/// 其中 17 个在 TS 里只参与类型（`SchemaTelemetry*`、`Infer*`、`ExactTelemetryAttributes`、
/// `TelemetrySchemaSpan*` 等）；Rust 提供运行时等价类型别名（见 `index.rs` 末尾），本测试
/// 用 `use` 证明它们可以被逐名引用。
#[test]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn exports_every_spec_19_2_name() {
    use kiana_telemetry::index::{
        ExactTelemetryAttributes, InferEventAttributes, InferOptionalAttributes,
        InferRequiredAndOptionalAttributes, InferStartAttributes, SchemaTelemetryCounter,
        SchemaTelemetryHistogram, SchemaTelemetrySpan, TelemetryEventAttributeDefinition,
        TelemetrySchemaSpanEndAttributes, TelemetrySchemaSpanEventAttributes,
        TelemetrySchemaSpanEventName, TelemetrySchemaSpanName, TelemetrySchemaSpanStartAttributes,
        TelemetrySchemaSpanUnion, TelemetryStartAttributeDefinition,
    };
    use kiana_telemetry::metrics::TypedTelemetryMetrics;
    use kiana_telemetry::render::render_trace;
    use kiana_telemetry::store_conformance::{
        StoreConformanceCase, StoreFixture, StoreFixtureFactory, telemetry_store_conformance,
    };
    use kiana_telemetry::testing::{
        TelemetryAdapterConformanceCase, TelemetryAdapterFixture, TelemetryAdapterFixtureFactory,
        create_telemetry_adapter_conformance,
    };
    use kiana_telemetry::{
        AttributeValue, Cardinality, HistogramOptions, InMemoryTelemetryContext, InstrumentOptions,
        MetricKind, MetricQuery, NOOP_TELEMETRY_CONTEXT_SENTINEL, RecordedTelemetryEvent,
        RecordedTelemetryLink, RecordedTelemetryMeasurement, RecordedTelemetrySpan, SpanAttributes,
        SpanContextId, SpanLink, SpanLinkContext, SpanOptions, SpanStatus, StatusError,
        StoredMetric, StoredSpan, StoredSpanEvent, StoredSpanLink, StoredTrace,
        TelemetryAttributeDefinition, TelemetryAttributeMetadata, TelemetryAttributeType,
        TelemetryCarrier, TelemetryContext, TelemetryContextExt, TelemetryCounter, TelemetryError,
        TelemetryEventDefinition, TelemetryHistogram, TelemetryMetricAttributeDefinition,
        TelemetryMetricDefinition, TelemetryMetrics, TelemetryParentDefinition,
        TelemetryPropagation, TelemetryRequiredAttributeDefinition, TelemetrySchemaDefinition,
        TelemetrySpan, TelemetrySpanDefinition, TelemetryStatusDefinition, TelemetryStore,
        TelemetryUpDownCounter, TracePage, TraceQuery, TypedMetrics, TypedSpanStarter,
        define_telemetry_schema, noop_telemetry_context, noop_telemetry_metrics,
        noop_telemetry_propagation, start_span_dyn,
    };
    use std::sync::Arc;

    // 仅类型层面的别名在运行时是其目标类型。
    let attributes: ExactTelemetryAttributes = SpanAttributes::new();
    let _: InferRequiredAndOptionalAttributes = attributes.clone();
    let _: InferStartAttributes = attributes.clone();
    let _: InferOptionalAttributes = attributes.clone();
    let _: InferEventAttributes = attributes.clone();
    let _: TelemetrySchemaSpanStartAttributes = attributes.clone();
    let _: TelemetrySchemaSpanEndAttributes = attributes.clone();
    let _: TelemetrySchemaSpanEventAttributes = attributes;
    let _: TelemetrySchemaSpanName = "request";
    let _: TelemetrySchemaSpanEventName = "checkpoint";
    let _: Option<SchemaTelemetryCounter> = None;
    let _: Option<SchemaTelemetryHistogram> = None;
    let _: Option<SchemaTelemetrySpan> = None;
    let _: Option<TelemetrySchemaSpanUnion> = None;
    let _: Option<TelemetryStartAttributeDefinition> = None;
    let _: Option<TelemetryEventAttributeDefinition> = None;
    let _: Option<TypedTelemetryMetrics> = None;

    // 其余名字（函数、常量、trait object）只是引用一次。
    let _ = render_trace(&[]);
    let _ = create_telemetry_adapter_conformance(Arc::new(|| {
        Box::pin(async { Err(TelemetryError::new("unused")) })
    }));
    let _ = telemetry_store_conformance(Arc::new(|| {
        Box::pin(async { Err(TelemetryError::new("unused")) })
    }));
    let _: Option<TelemetryAdapterConformanceCase> = None;
    let _: Option<TelemetryAdapterFixture> = None;
    let _: Option<TelemetryAdapterFixtureFactory> = None;
    let _: Option<StoreConformanceCase> = None;
    let _: Option<StoreFixture> = None;
    let _: Option<StoreFixtureFactory> = None;
    let _ = NOOP_TELEMETRY_CONTEXT_SENTINEL.clone();
    let _ = noop_telemetry_context();
    let _ = noop_telemetry_metrics();
    let _ = noop_telemetry_propagation();
    let _ = define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1.0,
        spans: indexmap::IndexMap::new(),
        metrics: None,
    });
    let _ = Arc::new(InMemoryTelemetryContext::new())
        .scope()
        .to_string();
    let _: Option<Arc<dyn TelemetryContext>> = None;
    fn _uses_traits<T: TelemetryStore + TelemetryPropagation + TelemetryMetrics>() {}
    fn _uses_span(_: &dyn TelemetrySpan) {}
    fn _uses_context(_: &dyn TelemetryContext) {}
    fn _uses_metrics(_: &dyn TelemetryMetrics) {}
    fn _uses_counter(_: &dyn TelemetryCounter) {}
    fn _uses_up_down(_: &dyn TelemetryUpDownCounter) {}
    fn _uses_histogram(_: &dyn TelemetryHistogram) {}
    fn _uses_types(
        _: SpanOptions,
        _: SpanAttributes,
        _: SpanLink,
        _: SpanLinkContext,
        _: SpanContextId,
        _: SpanStatus,
        _: StatusError,
        _: StoredSpan,
        _: StoredSpanEvent,
        _: StoredSpanLink,
        _: StoredTrace,
        _: StoredMetric,
        _: TraceQuery,
        _: MetricQuery,
        _: TracePage,
        _: TelemetryCarrier,
        _: RecordedTelemetrySpan,
        _: RecordedTelemetryEvent,
        _: RecordedTelemetryLink,
        _: RecordedTelemetryMeasurement,
        _: MetricKind,
        _: InstrumentOptions,
        _: HistogramOptions,
        _: TelemetryMetricDefinition,
        _: TelemetryMetricAttributeDefinition,
        _: TelemetryAttributeDefinition,
        _: TelemetryAttributeMetadata,
        _: TelemetryAttributeType,
        _: TelemetryEventDefinition,
        _: TelemetryParentDefinition,
        _: TelemetrySpanDefinition,
        _: TelemetryStatusDefinition,
        _: TelemetryRequiredAttributeDefinition,
        _: Cardinality,
        _: TypedMetrics,
        _: TypedSpanStarter,
        _: AttributeValue,
    ) {
    }
    fn _uses_functions(
        _: fn(Arc<dyn TelemetryContext>, &[TelemetrySchemaDefinition]) -> TypedSpanStarter,
        _: fn(
            kiana_telemetry::metrics::TelemetryMetricDefinitions,
            Arc<dyn TelemetryMetrics>,
        ) -> TypedMetrics,
        _: for<'a> fn(
            &'a dyn TelemetryContext,
            SpanOptions,
            kiana_telemetry::SpanCallback<'a>,
        ) -> kiana_telemetry::index::BoxFuture<
            'a,
            Result<kiana_telemetry::index::ErasedValue, TelemetryError>,
        >,
        _: fn(TelemetrySchemaDefinition) -> TelemetrySchemaDefinition,
    ) {
    }
    fn _uses_ext<T: TelemetryContextExt>() {}
    let _ = _uses_traits::<InMemoryTelemetryContext>;
    let _ = _uses_ext::<InMemoryTelemetryContext>;
    let _ = _uses_span;
    let _ = _uses_context;
    let _ = _uses_metrics;
    let _ = _uses_counter;
    let _ = _uses_up_down;
    let _ = _uses_histogram;
    let _ = _uses_types;
    let _ = _uses_functions;
    let _ = start_span_dyn;
    // 两个构造器在 `_uses_functions` 里以函数指针形式引用。
    let _ = create_typed_span_starter
        as fn(Arc<dyn TelemetryContext>, &[TelemetrySchemaDefinition]) -> TypedSpanStarter;
    let _ = kiana_telemetry::create_typed_metrics
        as fn(
            kiana_telemetry::metrics::TelemetryMetricDefinitions,
            Arc<dyn TelemetryMetrics>,
        ) -> TypedMetrics;
}
