//! spec 验收清单中「内存适配器」一节的边界向量（由 TS 参考实现在 node 上录制）。
//!
//! fixture `fixtures/memory_edges.json` 覆盖 TS 测试文件未直接断言、但 spec §3.1–§3.8 与
//! §26 验收清单明确要求的行为：
//!
//! - `localIds`：`extract` 的本地父 id 用 JS `Number(span)`（`NaN`、前后空白、`0x` 前缀、
//!   指数、负数、空串），以及 `recorded.parentId`（数字，`NaN` → JSON `null`）与
//!   `StoredSpan.parentId`（`"<scope>:<spanId>"` 字符串）的分歧；
//! - `emptyCarrier`：三个键的值都是 `""` 时仍满足 `typeof === "string"`，因此**识别**为外部
//!   父级（不落入 root）；由于 scope 与 recorder 不同，`recorded.parentId` 为 `null`；
//! - `crossInstance`：模块级身份注册表（TS 的 `WeakMap`）允许另一个 recorder 的 `inject`、
//!   span link 与 histogram exemplar 命中 A 的 span；
//! - `inert`：已结算 span 作为父级时产生惰性 span（不记录、不登记、不可 inject、link 被丢弃），
//!   但仍暴露真实 metrics/propagation 能力；
//! - `selfLink` / `renderSelfParent` / `renderCycle` / `renderEmptyParent`：自链接、自父、环与
//!   空 parentId 的渲染；
//! - `filters` / `traceFilters`：store 属性等值判定是 JS `===`（数组永不匹配标量，
//!   `true !== 1`，`"1" !== 1`）；
//! - `mergeOrder`：`setAttributes` 合并后的 JS 键序（整数样键前置）；
//! - `noop`：no-op propagation 的 `extract` 恒为单例、`inject` 恒为 `undefined`。
//!
//! `startedMs` / `endedMs` 是真实时间，比较模板时用实际值替换（其余字段逐字比较）。

use std::sync::Arc;

use kiana_telemetry::{
    AttributeValue, HistogramOptions, InMemoryTelemetryContext, MetricQuery, SpanAttributes,
    SpanLink, SpanOptions, SpanStatus, StoredMetric, StoredSpan, TelemetryCarrier,
    TelemetryContext, TelemetryContextExt, TracePage, TraceQuery, noop_telemetry_context,
    noop_telemetry_context_as_context, noop_telemetry_propagation,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/memory_edges.json")).expect("memory_edges.json")
}

/// 把随机 scope（UUID v4）归一化成 `<uuid>`，与 fixture 的写法一致。
fn normalize(text: &str, scope: &str) -> String {
    text.replace(scope, "<uuid>")
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture field `{key}`"))
}

fn external(scope: &str, span: &str, trace: &str) -> TelemetryCarrier {
    let mut carrier = TelemetryCarrier::new();
    carrier.insert("kiana-memory-scope".to_string(), scope.to_string());
    carrier.insert("kiana-memory-span".to_string(), span.to_string());
    carrier.insert("kiana-memory-trace".to_string(), trace.to_string());
    carrier
}

fn span_options(name: &str) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        ..SpanOptions::default()
    }
}

fn attrs(pairs: &[(&str, AttributeValue)]) -> SpanAttributes {
    let mut map = SpanAttributes::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

fn stored_span(id: &str, parent: Option<&str>) -> StoredSpan {
    StoredSpan {
        id: id.to_string(),
        trace_id: "t".to_string(),
        parent_id: parent.map(str::to_string),
        name: id.to_string(),
        started_ms: 0.0,
        ended_ms: 1.0,
        attributes: SpanAttributes::new(),
        events: Vec::new(),
        links: Vec::new(),
        status: SpanStatus::Ok,
    }
}

/// fixture 模板里的 `startedMs` / `endedMs` 是录制时的真实时间；用实际值替换后再逐字比较。
fn patch_times(span: &mut Value, started_ms: f64, ended_ms: f64) {
    span["startedMs"] = Value::from(started_ms);
    span["endedMs"] = Value::from(ended_ms);
}

/// `Number(span)` 的本地 id、`RecordedTelemetrySpan` 快照与 `StoredSpan` 快照逐字一致。
#[tokio::test]
async fn local_ids_use_javascript_number_semantics() {
    let fixture = fixture();
    let cases = fixture["localIds"].as_object().expect("localIds");
    assert_eq!(cases.len(), 7, "fixture must cover every Number(span) case");

    for (label, expected) in cases {
        let span_id = match label.as_str() {
            "abc" => "abc",
            "spaced" => " 7 ",
            "hex" => "0x10",
            "empty" => "",
            "exp" => "1e3",
            "negative" => "-1",
            "trailing" => "12abc",
            other => panic!("unexpected local id case `{other}`"),
        };
        let trace_id = format!("T:{label}");
        let telemetry = InMemoryTelemetryContext::new();
        let scope = telemetry.scope().to_string();

        let parent = telemetry.extract(Some(&external(&scope, span_id, &trace_id)));
        parent
            .start_span_typed(span_options(&format!("child-{label}")), |_| {
                Box::pin(async {})
            })
            .await
            .expect("child span");

        let spans = telemetry.get_spans();
        let recorded = &spans[0];
        let recorded_value = kiana_telemetry::json::recorded_span_to_value(recorded);
        assert_eq!(
            normalize(&kiana_json::stringify(&recorded_value), &scope),
            text(expected, "recorded"),
            "recorded snapshot for `{label}`"
        );
        assert_eq!(
            recorded.parent_id.is_some_and(f64::is_nan),
            expected["parentIdIsNaN"].as_bool().expect("parentIdIsNaN"),
            "recorded.parentId is NaN for `{label}`"
        );
        // `JSON.stringify(NaN)` → `null`（`RecordedTelemetrySpan.parentId` 是 `number | null`）。
        assert_eq!(
            kiana_json::stringify(&recorded_value["parentId"]),
            text(expected, "parentIdJson"),
            "JSON parentId for `{label}`"
        );

        let stored = telemetry.get_trace(&trace_id).await.expect("stored trace");
        assert_eq!(stored.spans.len(), 1);
        // `StoredSpan.parentId` 来自父 carrier 的 `<scope>:<spanId>`，即使本地 id 是 NaN 也仍是
        // 有意义的字符串（与 recorded.parentId 的分歧正是本条检查的内容）。
        assert_eq!(
            stored.spans[0].parent_id.as_deref(),
            Some(format!("{scope}:{span_id}")).as_deref(),
            "stored parentId string for `{label}`"
        );
        let mut expected_span: Value =
            serde_json::from_str(&text(expected, "stored").replace("<uuid>", &scope))
                .expect("stored template");
        patch_times(
            &mut expected_span,
            stored.spans[0].started_ms,
            stored.spans[0].ended_ms,
        );
        let stored_value = kiana_telemetry::json::stored_trace_to_value(&stored);
        assert_eq!(
            kiana_json::stringify(&stored_value["spans"][0]),
            kiana_json::stringify(&expected_span),
            "stored span for `{label}`"
        );

        let carrier = telemetry
            .inject(parent.as_ref())
            .expect("extracted parent is injectable");
        assert_eq!(
            normalize(
                &serde_json::to_string(&carrier).expect("carrier json"),
                &scope
            ),
            text(expected, "injectParent"),
            "inject(extracted parent) for `{label}`"
        );
    }
}

/// 空字符串的 carrier 标识仍是字符串 ⇒ 识别为外部父级（**不**回退到 root）。
#[tokio::test]
async fn empty_string_carrier_identifiers_are_recognized() {
    let fixture = fixture();
    let expected = &fixture["emptyCarrier"];
    let telemetry = InMemoryTelemetryContext::new();
    let scope = telemetry.scope().to_string();

    let parent = telemetry.extract(Some(&external("", "", "")));
    // TS 里 `parent === this` 为 false：空串通过了 `typeof === "string"` 检查。
    assert_eq!(
        expected["isRoot"].as_bool(),
        Some(false),
        "fixture records that the empty carrier is not the root"
    );
    assert_ne!(
        parent.span_context_id(),
        telemetry.span_context_id(),
        "an empty-string carrier must not fall back to the root"
    );

    parent
        .start_span_typed(span_options("child"), |_| Box::pin(async {}))
        .await
        .expect("child span");
    let spans = telemetry.get_spans();
    let recorded = &spans[0];
    let recorded_value = kiana_telemetry::json::recorded_span_to_value(recorded);
    assert_eq!(
        normalize(&kiana_json::stringify(&recorded_value), &scope),
        text(expected, "recorded")
    );
    // scope 与 recorder 不同 ⇒ 没有本地父 id（对照 localIds 的 `empty` 用例：那里 scope 相同，
    // `Number("")` 为 0）。
    assert!(
        recorded.parent_id.is_none(),
        "cross-scope carries no local id"
    );
    assert_eq!(
        recorded.parent_context.as_ref(),
        Some(&external("", "", "")),
        "the empty carrier is kept verbatim as parentContext"
    );

    let page = telemetry.find_traces(None).await.expect("traces");
    assert_eq!(page.traces.len(), 1);
    assert_eq!(page.traces[0].trace_id, text(expected, "storedTraceId"));
    let stored_span_value = &page.traces[0].spans[0];
    let mut expected_trace: Value =
        serde_json::from_str(&text(expected, "storedTrace").replace("<uuid>", &scope))
            .expect("storedTrace template");
    patch_times(
        &mut expected_trace["spans"][0],
        stored_span_value.started_ms,
        stored_span_value.ended_ms,
    );
    let actual_trace = kiana_telemetry::json::stored_trace_to_value(&page.traces[0]);
    assert_eq!(
        kiana_json::stringify(&actual_trace),
        kiana_json::stringify(&expected_trace)
    );

    let carrier = telemetry
        .inject(parent.as_ref())
        .expect("extracted parent is injectable");
    assert_eq!(
        serde_json::to_string(&carrier).expect("carrier json"),
        text(expected, "injectParent")
    );
}

/// 模块级身份注册表：跨 recorder 的 inject / link / exemplar 都命中。
#[tokio::test]
async fn identity_registry_is_module_level() {
    let fixture = fixture();
    let expected = &fixture["crossInstance"];
    let a = InMemoryTelemetryContext::new();
    let b = InMemoryTelemetryContext::new();
    let scope_a = a.scope().to_string();
    let scope_b = b.scope().to_string();

    let span_a = a
        .start_span_typed(span_options("a-span"), |span| Box::pin(async move { span }))
        .await
        .expect("a-span");
    let carrier_a = a.inject(span_a.as_ref()).expect("inject A's span");
    assert_eq!(
        normalize(
            &serde_json::to_string(&carrier_a).expect("carrier json"),
            &scope_a
        ),
        normalize(text(expected, "injectFromA"), &scope_a)
    );
    // B 的 inject 命中 A 的 span（进程级注册表）。
    let injected_from_b = b.inject(span_a.as_ref()).expect("B injects A's span");
    assert_eq!(injected_from_b, carrier_a);
    assert_eq!(
        normalize(
            &serde_json::to_string(&injected_from_b).expect("carrier json"),
            &scope_a
        ),
        normalize(text(expected, "injectFromB"), &scope_a)
    );

    // B 的 span 把 A 的 span 记录为 link。
    b.start_span_typed(
        SpanOptions {
            name: "b-linked".to_string(),
            attributes: None,
            links: Some(vec![SpanLink {
                context: span_a.span_context_id(),
                attributes: Some(attrs(&[(
                    "why",
                    AttributeValue::String("cross".to_string()),
                )])),
            }]),
        },
        |_| Box::pin(async {}),
    )
    .await
    .expect("b-linked");
    let spans_b = b.get_spans();
    let linked = &spans_b[0];
    assert_eq!(linked.name, "b-linked");
    assert_eq!(linked.links.len(), 1);
    assert_eq!(linked.links[0].context, carrier_a);
    let linked_value = kiana_telemetry::json::recorded_span_to_value(linked);
    assert_eq!(
        normalize(&kiana_json::stringify(&linked_value["links"]), &scope_a),
        normalize(text(expected, "links"), &scope_a)
    );

    // B 的 histogram exemplar 也接受 A 的 span 上下文。
    let b_host = b.clone();
    let span_for_exemplar = Arc::clone(&span_a);
    b.start_span_typed(span_options("b-host"), move |host| {
        Box::pin(async move {
            let with_boundaries = b_host.histogram(
                "h",
                Some(HistogramOptions {
                    boundaries: Some(vec![1.0]),
                    ..HistogramOptions::default()
                }),
            );
            let exemplar: &dyn TelemetryContext = span_for_exemplar.as_ref();
            with_boundaries.record(1.0, None, Some(exemplar));
            let host_context: &dyn TelemetryContext = host.as_ref();
            b_host
                .histogram("h", None)
                .record(2.0, None, Some(host_context));
        })
    })
    .await
    .expect("b-host");

    let measurements = b.get_metrics();
    assert_eq!(measurements.len(), 2);
    assert_eq!(measurements[0].value, 1.0);
    assert_eq!(measurements[0].boundaries, Some(vec![1.0]));
    assert_eq!(measurements[0].context.as_ref(), Some(&carrier_a));
    assert_eq!(measurements[1].value, 2.0);
    let exemplar_pairs: Vec<(f64, Option<&TelemetryCarrier>)> = measurements
        .iter()
        .map(|row| (row.value, row.context.as_ref()))
        .collect();
    let exemplar_json =
        kiana_json::stringify(&serde_json::to_value(&exemplar_pairs).expect("exemplar pairs json"));
    assert_eq!(
        normalize(&normalize(&exemplar_json, &scope_a), &scope_b),
        text(expected, "exemplarContexts")
    );

    // A 的 extract 父上下文同样可被 B 的 inject 命中（注册表按 carrier 身份而非对象身份）。
    let extracted = a.extract(Some(&carrier_a));
    let injected_extracted = b.inject(extracted.as_ref()).expect("inject extracted");
    assert_eq!(injected_extracted, carrier_a);
    assert_eq!(
        normalize(
            &serde_json::to_string(&injected_extracted).expect("carrier json"),
            &scope_a
        ),
        normalize(text(expected, "injectExtractedFromB"), &scope_a)
    );

    // root 自身不可注入。
    assert!(a.inject(&a).is_none(), "root is never injected");
    assert!(b.inject(&b).is_none(), "root is never injected");
    assert_eq!(
        expected["rootInjectIsUndefined"].as_bool(),
        Some(true),
        "fixture records undefined for root inject"
    );
}

/// 已结算 span 作为父级 ⇒ 惰性 span：不记录、不登记、link 被丢弃、能力仍是真实适配器的。
#[tokio::test]
async fn settled_parents_yield_inert_spans() {
    let fixture = fixture();
    let expected = &fixture["inert"];
    let telemetry = InMemoryTelemetryContext::new();

    let settled = telemetry
        .start_span_typed(span_options("settled"), |span| {
            Box::pin(async move { span })
        })
        .await
        .expect("settled span");

    let (inert_child, nested) = settled
        .start_span_typed(span_options("inert-child"), move |child| {
            Box::pin(async move {
                let nested = child
                    .start_span_typed(span_options("nested-inert"), |nested| {
                        Box::pin(async move { nested })
                    })
                    .await
                    .expect("nested inert span");
                (child, nested)
            })
        })
        .await
        .expect("inert child");

    // 惰性 span 是展开 no-op 的新对象（TS 里不是 `NOOP_TELEMETRY_CONTEXT` 单例）。
    let child_is_noop = Arc::ptr_eq(&inert_child, &noop_telemetry_context());
    assert_eq!(
        child_is_noop,
        expected["childIsNoopContext"].as_bool().expect("flag"),
        "inert child identity"
    );
    let nested_is_noop = Arc::ptr_eq(&nested, &noop_telemetry_context());
    assert_eq!(
        nested_is_noop,
        expected["nestedIsNoopContext"].as_bool().expect("flag"),
        "nested inert identity"
    );

    // 能力句柄与 recorder 相同（`inert.metrics === t.metrics`）。
    let recorder_metrics = telemetry.metrics().expect("recorder metrics");
    let recorder_propagation = telemetry.propagation().expect("recorder propagation");
    let same_metrics = Arc::ptr_eq(
        &inert_child.metrics().expect("inert metrics"),
        &recorder_metrics,
    );
    assert_eq!(
        same_metrics,
        expected["metricsSameAsRecorder"].as_bool().expect("flag")
    );
    let same_propagation = Arc::ptr_eq(
        &inert_child.propagation().expect("inert propagation"),
        &recorder_propagation,
    );
    assert_eq!(
        same_propagation,
        expected["propagationSameAsRecorder"]
            .as_bool()
            .expect("flag")
    );

    // 惰性 span 不登记 ⇒ inject 为 undefined。
    assert!(
        telemetry.inject(inert_child.as_ref()).is_none(),
        "inert spans are never injectable"
    );

    // 指向惰性 span 的 link 被静默丢弃。
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "linker".to_string(),
                attributes: None,
                links: Some(vec![
                    SpanLink {
                        context: inert_child.span_context_id(),
                        attributes: None,
                    },
                    SpanLink {
                        context: nested.span_context_id(),
                        attributes: None,
                    },
                ]),
            },
            |_| Box::pin(async {}),
        )
        .await
        .expect("linker");

    // 惰性 span 上的记录调用全部无效。
    inert_child.set_attributes(attrs(&[("x", AttributeValue::Number(1.0))]));
    inert_child.add_event("e", Some(attrs(&[("y", AttributeValue::Number(2.0))])));
    inert_child.set_status(SpanStatus::Error { error: None });
    inert_child.add_link(SpanLink {
        context: inert_child.span_context_id(),
        attributes: None,
    });

    let spans = telemetry.get_spans();
    let span_names: Vec<(String, bool)> = spans
        .iter()
        .map(|span| (span.name.clone(), span.settled))
        .collect();
    assert_eq!(
        serde_json::to_string(&span_names).expect("span names json"),
        text(expected, "spanNames")
    );
    assert_eq!(spans.len(), 2, "inert spans are never recorded");
    let linker = spans
        .iter()
        .find(|span| span.name == "linker")
        .expect("linker span");
    assert!(linker.links.is_empty(), "links to inert spans are dropped");
    assert_eq!(
        serde_json::to_string(&linker.links).expect("links json"),
        text(expected, "linkerLinks")
    );
}

/// 自链接被记录；自父与环在 `renderTrace` 里静默丢弃（不递归、不崩溃）。
#[tokio::test]
async fn self_links_and_cyclic_parents_match_typescript() {
    let fixture = fixture();
    let telemetry = InMemoryTelemetryContext::new();
    let scope = telemetry.scope().to_string();

    telemetry
        .start_span_typed(span_options("self"), |span| {
            Box::pin(async move {
                span.add_link(SpanLink {
                    context: span.span_context_id(),
                    attributes: None,
                });
            })
        })
        .await
        .expect("self span");

    let spans = telemetry.get_spans();
    let recorded = kiana_telemetry::json::recorded_span_to_value(&spans[0]);
    assert_eq!(
        normalize(&kiana_json::stringify(&recorded["links"]), &scope),
        normalize(text(&fixture, "selfLink"), &scope)
    );

    let self_parent = [stored_span("a", Some("a")), stored_span("b", Some("a"))];
    assert_eq!(
        kiana_telemetry::render::render_trace(&self_parent),
        text(&fixture, "renderSelfParent")
    );

    let cycle = [
        stored_span("x", Some("y")),
        stored_span("y", Some("x")),
        stored_span("root", None),
        stored_span("c", Some("root")),
    ];
    assert_eq!(
        kiana_telemetry::render::render_trace(&cycle),
        text(&fixture, "renderCycle")
    );

    let empty_parent = [stored_span("p", Some("")), stored_span("q", Some("p"))];
    assert_eq!(
        kiana_telemetry::render::render_trace(&empty_parent),
        text(&fixture, "renderEmptyParent")
    );
}

/// store 的属性过滤是 JS `===`：数组永不匹配标量、`true !== 1`、`"1" !== 1`。
#[tokio::test]
async fn attribute_filters_use_javascript_strict_equality() {
    let fixture = fixture();
    let expected = &fixture["filters"];
    let telemetry = InMemoryTelemetryContext::new();
    let counter = telemetry.counter("m", None);
    counter.add(
        1.0,
        Some(attrs(&[(
            "tags",
            AttributeValue::StringArray(vec!["a".to_string()]),
        )])),
    );
    counter.add(
        2.0,
        Some(attrs(&[("tags", AttributeValue::String("a".to_string()))])),
    );
    counter.add(4.0, Some(attrs(&[("v", AttributeValue::Boolean(true))])));
    counter.add(8.0, Some(attrs(&[("v", AttributeValue::Number(1.0))])));
    counter.add(
        16.0,
        Some(attrs(&[("v", AttributeValue::String("1".to_string()))])),
    );

    let sums =
        |rows: Vec<StoredMetric>| -> Vec<f64> { rows.into_iter().map(|row| row.sum).collect() };
    let query = |filter: SpanAttributes| MetricQuery {
        name: "m".to_string(),
        attributes: Some(filter),
        ..MetricQuery::default()
    };
    let expected_sums =
        |key: &str| -> Vec<f64> { serde_json::from_value(expected[key].clone()).expect("sums") };

    assert_eq!(
        sums(
            telemetry
                .find_metrics(query(attrs(&[(
                    "tags",
                    AttributeValue::String("a".to_string()),
                )])))
                .await
        ),
        expected_sums("stringTag")
    );
    assert_eq!(
        sums(
            telemetry
                .find_metrics(query(attrs(&[(
                    "tags",
                    AttributeValue::StringArray(vec!["a".to_string()]),
                )])))
                .await
        ),
        expected_sums("arrayTag")
    );
    assert_eq!(
        sums(
            telemetry
                .find_metrics(query(attrs(&[("v", AttributeValue::Boolean(true))])))
                .await
        ),
        expected_sums("bool")
    );
    assert_eq!(
        sums(
            telemetry
                .find_metrics(query(attrs(&[("v", AttributeValue::Number(1.0))])))
                .await
        ),
        expected_sums("number")
    );
    assert_eq!(
        sums(
            telemetry
                .find_metrics(query(attrs(&[(
                    "v",
                    AttributeValue::String("1".to_string())
                )])))
                .await
        ),
        expected_sums("numberString")
    );

    // trace 过滤器同样如此（同一 `===` 语义）。
    let trace_fixture = &fixture["traceFilters"];
    let traces = InMemoryTelemetryContext::new();
    traces
        .start_span_typed(
            SpanOptions {
                name: "arr".to_string(),
                attributes: Some(attrs(&[
                    ("tags", AttributeValue::StringArray(vec!["a".to_string()])),
                    ("v", AttributeValue::Boolean(true)),
                    ("n", AttributeValue::Number(1.0)),
                ])),
                links: None,
            },
            |_| Box::pin(async {}),
        )
        .await
        .expect("arr span");

    let names = |page: TracePage| -> Vec<String> {
        page.traces
            .into_iter()
            .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
            .collect()
    };
    let trace_query = |filter: SpanAttributes| TraceQuery {
        attributes: Some(filter),
        ..TraceQuery::default()
    };
    let expected_names = |key: &str| -> Vec<String> {
        serde_json::from_value(trace_fixture[key].clone()).expect("trace names")
    };

    assert_eq!(
        names(
            traces
                .find_traces(Some(trace_query(attrs(&[(
                    "tags",
                    AttributeValue::StringArray(vec!["a".to_string()]),
                )]))))
                .await
                .expect("traces")
        ),
        expected_names("arrayTag")
    );
    assert_eq!(
        names(
            traces
                .find_traces(Some(trace_query(attrs(&[(
                    "v",
                    AttributeValue::Boolean(true)
                )]))))
                .await
                .expect("traces")
        ),
        expected_names("bool")
    );
    assert_eq!(
        names(
            traces
                .find_traces(Some(trace_query(attrs(&[(
                    "v",
                    AttributeValue::Number(1.0)
                )]))))
                .await
                .expect("traces")
        ),
        expected_names("number")
    );
    assert_eq!(
        names(
            traces
                .find_traces(Some(trace_query(attrs(&[(
                    "v",
                    AttributeValue::String("1".to_string())
                )]))))
                .await
                .expect("traces")
        ),
        expected_names("numberString")
    );
    assert_eq!(
        names(
            traces
                .find_traces(Some(trace_query(attrs(&[(
                    "tags",
                    AttributeValue::String("a".to_string())
                )]))))
                .await
                .expect("traces")
        ),
        expected_names("stringTag")
    );
    assert_eq!(
        names(
            traces
                .find_traces(Some(trace_query(attrs(&[(
                    "n",
                    AttributeValue::Number(1.0)
                )]))))
                .await
                .expect("traces")
        ),
        expected_names("numberAttr")
    );
}

/// `setAttributes` 合并后按 JS 键序枚举（整数样键升序前置，其余保持插入序）。
#[tokio::test]
async fn set_attributes_merges_with_javascript_key_order() {
    let fixture = fixture();
    let telemetry = InMemoryTelemetryContext::new();
    telemetry
        .start_span_typed(
            SpanOptions {
                name: "attrs".to_string(),
                attributes: Some(attrs(&[
                    ("z", AttributeValue::Number(1.0)),
                    ("2", AttributeValue::Number(2.0)),
                    ("a", AttributeValue::Number(3.0)),
                ])),
                links: None,
            },
            |span| {
                Box::pin(async move {
                    span.set_attributes(attrs(&[
                        ("1", AttributeValue::Number(4.0)),
                        ("b", AttributeValue::Number(5.0)),
                    ]));
                    span.set_attributes(attrs(&[("z", AttributeValue::Number(6.0))]));
                })
            },
        )
        .await
        .expect("attrs span");

    let spans = telemetry.get_spans();
    let attributes = serde_json::to_value(&spans[0].attributes).expect("attributes json");
    assert_eq!(
        kiana_json::stringify(&attributes),
        text(&fixture, "mergeOrder")
    );
}

/// no-op propagation：`inject` 恒 `undefined`、`extract` 恒返回单例。
#[test]
fn noop_propagation_matches_typescript() {
    let fixture = fixture();
    let expected = &fixture["noop"];
    let noop = noop_telemetry_context();
    let noop_context: Arc<dyn TelemetryContext> = noop.clone();
    let propagation = noop_telemetry_propagation();

    let inject_is_undefined = propagation.inject(noop_context.as_ref()).is_none();
    assert_eq!(
        inject_is_undefined,
        expected["injectIsUndefined"].as_bool().expect("flag")
    );

    let extract_undefined_is_singleton = Arc::ptr_eq(
        &propagation.extract(None),
        &noop_telemetry_context_as_context(),
    );
    assert_eq!(
        extract_undefined_is_singleton,
        expected["extractUndefinedIsSingleton"]
            .as_bool()
            .expect("flag")
    );

    let mut unknown = TelemetryCarrier::new();
    unknown.insert("unknown".to_string(), "x".to_string());
    let extract_unknown_is_singleton = Arc::ptr_eq(
        &propagation.extract(Some(&unknown)),
        &noop_telemetry_context_as_context(),
    );
    assert_eq!(
        extract_unknown_is_singleton,
        expected["extractUnknownIsSingleton"]
            .as_bool()
            .expect("flag")
    );
}

// ---------------------------------------------------------------------------
// `Number(span)` 的正确舍入（TS 录制于 fixtures/hex_parent_ids.json）
// ---------------------------------------------------------------------------

/// `extract` 的本地父 id 用 `Number(span)`；超过 32 个十六进制位的字面量在旧实现里逐位
/// f64 累加，会低 1 ULP（review 发现的 `0x2cc8…20db` 即其一）。这里逐条比对 TS 录制的
/// `RecordedTelemetrySpan.parentId` 文本（`JSON.stringify` 语义：非有限 → `null`）。
#[tokio::test]
async fn local_parent_ids_use_correctly_rounded_number_semantics() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/hex_parent_ids.json"))
        .expect("hex_parent_ids.json");
    let cases = fixture.as_object().expect("cases");
    assert!(cases.len() >= 10);

    let telemetry = InMemoryTelemetryContext::new();
    let scope = telemetry.scope().to_string();
    for (label, expected) in cases {
        let span_id = expected["spanId"].as_str().expect("spanId");
        let trace_id = format!("S:{label}");
        let parent = telemetry.extract(Some(&external(&scope, span_id, &trace_id)));
        parent
            .start_span_typed(span_options(&format!("same-{label}")), |_| {
                Box::pin(async {})
            })
            .await
            .expect("child span");
        let spans = telemetry.get_spans();
        let recorded = spans
            .iter()
            .find(|span| span.name == format!("same-{label}"))
            .expect("recorded span");
        let value = serde_json::to_value(recorded).expect("recorded span serializes");
        assert_eq!(
            kiana_json::stringify(&value["parentId"]),
            expected["recordedParentIdJson"]
                .as_str()
                .expect("recordedParentIdJson"),
            "JSON parentId for `{label}` ({span_id})"
        );
    }
}
