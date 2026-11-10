//! propagation.test.ts 的 Rust 移植（6 个 `it` 标题保留既有文案作为稳定契约，describe 为
//! "propagation and links"）。
//!
//! `continues a parent in another Node process...`：TS 用
//! `execFileSync(node, propagation-worker.ts, [JSON.stringify(carrier)])`。Rust 的等价物是重新
//! 执行当前测试二进制（`std::env::current_exe()`）并只跑 `#[test] propagation_worker`，carrier
//! JSON 通过环境变量 `KIANA_TELEMETRY_PROPAGATION_CARRIER` 传入；worker 把 `getSpans()` 的 JSON
//! 打到 stdout（`kiana_telemetry::json::recorded_spans_to_json`，JS 数字/键序语义）。
//!
//! 对象身份：TS 的 `telemetry.extract(carrier) === telemetry`（`readonly metrics = this`）
//! 在 Rust 侧由 `memory::RecorderIdentity` 复刻 —— recorder 对象同时实现
//! `TelemetryContext` / `TelemetryPropagation` / `TelemetryMetrics`，`Inner` 持有唯一的
//! `Arc`，所有 `metrics()` / `propagation()` / `extract(absent)` 都返回它。测试用
//! [`allocation`]（trait object 的 data 指针）断言与 `toBe` 同等强度的身份。

use std::sync::{Arc, Mutex};

use kiana_telemetry::testing::{attributes, json_roundtrip_carrier, same_allocation};
use kiana_telemetry::{
    AttributeValue, InMemoryTelemetryContext, RecordedTelemetryLink, SpanLink, SpanOptions,
    TelemetryCarrier, TelemetryContext, TelemetryContextExt, TelemetrySpan, noop_telemetry_context,
    noop_telemetry_context_as_context, noop_telemetry_propagation,
};
use serde_json::Value;

/// `Arc<T>` 指向的分配（trait object 取 data 指针），用于 `toBe` 式的身份断言。
fn allocation<T: ?Sized>(value: &Arc<T>) -> *const () {
    Arc::as_ptr(value) as *const ()
}

fn carrier(pairs: &[(&str, &str)]) -> TelemetryCarrier {
    let mut map = TelemetryCarrier::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), (*value).to_string());
    }
    map
}

fn carrier_value(carrier: &TelemetryCarrier) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in carrier {
        map.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(map)
}

fn options(name: &str, links: Option<Vec<SpanLink>>) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        attributes: None,
        links,
    }
}

fn link_from_context(
    context: &Arc<dyn TelemetryContext>,
    attributes: Option<kiana_telemetry::SpanAttributes>,
) -> SpanLink {
    SpanLink {
        context: context.span_context_id(),
        attributes,
    }
}

/// `propagation-worker.ts` 的对应物：从环境变量读 carrier，记录一个 "remote" span 并打印
/// `JSON.stringify(getSpans())`。
///
/// 普通 `cargo test` 下它没有环境变量，直接返回（trivially pass）。
#[test]
fn propagation_worker() {
    let Ok(raw) = std::env::var("KIANA_TELEMETRY_PROPAGATION_CARRIER") else {
        return;
    };
    let carrier: TelemetryCarrier = match kiana_json::parse(&raw) {
        Ok(Value::Object(map)) => map
            .into_iter()
            .filter_map(|(key, value)| match value {
                Value::String(text) => Some((key, text)),
                _ => None,
            })
            .collect(),
        _ => TelemetryCarrier::new(),
    };
    let telemetry = InMemoryTelemetryContext::new();
    let parent = telemetry.extract(Some(&carrier));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("worker runtime");
    runtime.block_on(async {
        parent
            .start_span_typed(options("remote", None), |_| Box::pin(async move {}))
            .await
            .expect("remote span");
    });
    let spans = telemetry.get_spans();
    use std::io::Write;
    print!("{}", kiana_telemetry::json::recorded_spans_to_json(&spans));
    std::io::stdout().flush().expect("flush stdout");
}

#[test]
fn continues_a_parent_in_another_node_process_without_shared_recorder_state() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let telemetry = InMemoryTelemetryContext::new();
    let carrier_slot: Arc<Mutex<Option<Option<TelemetryCarrier>>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&carrier_slot);
    let telemetry_for_span = telemetry.clone();
    runtime.block_on(async move {
        let telemetry_for_callback = telemetry_for_span.clone();
        telemetry_for_span
            .start_span_typed(options("caller", None), move |span| {
                Box::pin(async move {
                    let span_context: &dyn TelemetryContext = span.as_ref();
                    *slot.lock().expect("slot") = Some(telemetry_for_callback.inject(span_context));
                })
            })
            .await
            .expect("caller span");
    });
    let carrier = carrier_slot
        .lock()
        .expect("slot")
        .take()
        .expect("callback ran")
        .expect("injected carrier");
    let carrier_json = kiana_json::js_object::stringify(&carrier_value(&carrier));

    let exe = std::env::current_exe().expect("current exe");
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg("propagation_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("KIANA_TELEMETRY_PROPAGATION_CARRIER", &carrier_json)
        .output()
        .expect("worker process");
    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let start = stdout.find('[').expect("worker prints a JSON array");
    let end = stdout.rfind(']').expect("worker prints a JSON array");
    let recorded: Vec<Value> =
        serde_json::from_str(&stdout[start..=end]).expect("recorded spans JSON");
    assert_eq!(recorded.len(), 1, "recorded: {recorded:?}");
    let expected = carrier_value(&carrier);
    assert_eq!(
        recorded[0].get("parentContext"),
        Some(&expected),
        "the remote span keeps the extracted parent carrier"
    );
    assert!(
        recorded[0].get("parentId").is_some_and(Value::is_null),
        "parentId is null for a remote parent"
    );
    assert_ne!(
        recorded[0].get("context"),
        Some(&expected),
        "the child context differs from the carrier"
    );
}

#[tokio::test]
async fn isolates_crossed_concurrent_remote_parents_and_links_shared_roots() {
    let caller = Arc::new(InMemoryTelemetryContext::new());
    let provider = Arc::new(InMemoryTelemetryContext::new());
    let activation: Arc<Mutex<Option<Arc<dyn TelemetrySpan>>>> = Arc::new(Mutex::new(None));
    // `firstGate`（later → first）与 `shared` 完成信号（first → later）。
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let gate_rx = Arc::new(Mutex::new(Some(gate_rx)));
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));

    let first = {
        let caller = Arc::clone(&caller);
        let provider = Arc::clone(&provider);
        let activation = Arc::clone(&activation);
        let gate_rx = Arc::clone(&gate_rx);
        let done_tx = Arc::clone(&done_tx);
        async move {
            let caller_for_callback = Arc::clone(&caller);
            caller
                .start_span_typed(options("first", None), move |trigger| {
                    let caller = Arc::clone(&caller_for_callback);
                    let provider = Arc::clone(&provider);
                    let activation = Arc::clone(&activation);
                    let gate_rx = Arc::clone(&gate_rx);
                    let done_tx = Arc::clone(&done_tx);
                    Box::pin(async move {
                        let trigger_context: &dyn TelemetryContext = trigger.as_ref();
                        let injected = caller.inject(trigger_context);
                        let parent = provider.extract(injected.as_ref());
                        let provider_for_method = Arc::clone(&provider);
                        parent
                            .start_span_typed(options("first.method", None), move |getter| {
                                let activation = Arc::clone(&activation);
                                let gate_rx = Arc::clone(&gate_rx);
                                let done_tx = Arc::clone(&done_tx);
                                Box::pin(async move {
                                    // TS: `shared = provider.startSpan(...)` 会同步执行到 `await gate`；
                                    // Rust 的 future 惰性，因此先 poll 一次让 activation 被接纳并停住。
                                    let gate = gate_rx.lock().expect("gate").take().expect("gate");
                                    let activation_slot = Arc::clone(&activation);
                                    let getter_context: Arc<dyn TelemetryContext> = getter.clone();
                                    let mut activation_future =
                                        Box::pin(provider_for_method.start_span_typed(
                                            options(
                                                "activation",
                                                Some(vec![link_from_context(
                                                    &getter_context,
                                                    None,
                                                )]),
                                            ),
                                            move |span| {
                                                *activation_slot.lock().expect("activation") =
                                                    Some(span);
                                                Box::pin(async move {
                                                    let _ = gate.await;
                                                })
                                            },
                                        ));
                                    assert!(
                                        kiana_telemetry::testing::poll_once(&mut activation_future)
                                            .is_pending(),
                                        "activation waits for the gate"
                                    );
                                    let current = activation
                                        .lock()
                                        .expect("activation")
                                        .clone()
                                        .expect("activation span was admitted");
                                    getter.add_link(SpanLink {
                                        context: current.span_context_id(),
                                        attributes: None,
                                    });
                                    activation_future.await.expect("activation span");
                                    if let Some(done) = done_tx.lock().expect("done").take() {
                                        let _ = done.send(());
                                    }
                                })
                            })
                            .await
                            .expect("first.method span");
                    })
                })
                .await
                .expect("first caller span");
        }
    };

    let later = {
        let caller = Arc::clone(&caller);
        let provider = Arc::clone(&provider);
        let activation = Arc::clone(&activation);
        let gate_tx = Arc::new(Mutex::new(Some(gate_tx)));
        async move {
            let caller_for_callback = Arc::clone(&caller);
            caller
                .start_span_typed(options("later", None), move |trigger| {
                    let caller = Arc::clone(&caller_for_callback);
                    let provider = Arc::clone(&provider);
                    let activation = Arc::clone(&activation);
                    let gate_tx = Arc::clone(&gate_tx);
                    Box::pin(async move {
                        let trigger_context: &dyn TelemetryContext = trigger.as_ref();
                        let injected = caller.inject(trigger_context);
                        let parent = provider.extract(injected.as_ref());
                        parent
                            .start_span_typed(options("later.method", None), move |getter| {
                                let activation = Arc::clone(&activation);
                                let gate_tx = Arc::clone(&gate_tx);
                                Box::pin(async move {
                                    let current = activation
                                        .lock()
                                        .expect("activation")
                                        .clone()
                                        .expect("first.method admitted activation");
                                    getter.add_link(SpanLink {
                                        context: current.span_context_id(),
                                        attributes: None,
                                    });
                                    if let Some(gate) = gate_tx.lock().expect("gate").take() {
                                        let _ = gate.send(());
                                    }
                                    let _ = done_rx.await;
                                })
                            })
                            .await
                            .expect("later.method span");
                    })
                })
                .await
                .expect("later caller span");
        }
    };

    tokio::join!(first, later);

    let spans = provider.get_spans();
    let find = |name: &str| {
        spans
            .iter()
            .find(|span| span.name == name)
            .unwrap_or_else(|| panic!("missing span {name}"))
    };
    let root = find("activation");
    let first_method = find("first.method");
    let later_method = find("later.method");
    assert_eq!(root.parent_context, None);
    let caller_spans = caller.get_spans();
    assert_eq!(
        first_method.parent_context.as_ref(),
        Some(&caller_spans[0].context)
    );
    assert_eq!(
        later_method.parent_context.as_ref(),
        Some(&caller_spans[1].context)
    );
    assert_eq!(
        root.links.first().expect("root link").context,
        first_method.context
    );
    assert_eq!(
        later_method.links.first().expect("later link").context,
        root.context
    );
}

#[tokio::test]
async fn detaches_carrier_and_link_snapshots_and_retains_settled_target_identities() {
    let telemetry = InMemoryTelemetryContext::new();
    let (target_tx, target_rx) = tokio::sync::oneshot::channel::<Arc<dyn TelemetrySpan>>();
    let telemetry_for_span = telemetry.clone();
    telemetry_for_span
        .start_span_typed(options("target", None), move |span| {
            Box::pin(async move {
                let _ = target_tx.send(span);
            })
        })
        .await
        .expect("target span");
    let target = target_rx.await.expect("Expected target");
    let target_context: &dyn TelemetryContext = target.as_ref();
    let original = telemetry.inject(target_context);
    let mut injected = telemetry.inject(target_context).expect("Expected carrier");
    injected.insert("kiana-memory-span".to_string(), "999".to_string());
    assert_eq!(
        telemetry.inject(target_context),
        original,
        "inject returns a detached copy each time"
    );

    let telemetry_for_link = telemetry.clone();
    let target_for_link = Arc::clone(&target);
    telemetry_for_link
        .start_span_typed(
            options(
                "linked",
                Some(vec![SpanLink {
                    context: target_for_link.span_context_id(),
                    attributes: Some(attributes(&[(
                        "tags",
                        AttributeValue::StringArray(vec!["original".to_string()]),
                    )])),
                }]),
            ),
            |_| Box::pin(async move {}),
        )
        .await
        .expect("linked span");

    let mut spans = telemetry.get_spans();
    assert_eq!(spans.len(), 2);
    let linked = spans.remove(1);
    let mut link = linked
        .links
        .first()
        .expect("Expected spans and link")
        .clone();
    spans[0]
        .context
        .insert("kiana-memory-span".to_string(), "123".to_string());
    link.context
        .insert("kiana-memory-span".to_string(), "456".to_string());
    link.attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["mutated".to_string()]),
    );

    let fresh = telemetry.get_spans();
    let expected = RecordedTelemetryLink {
        context: original.expect("original carrier"),
        attributes: attributes(&[(
            "tags",
            AttributeValue::StringArray(vec!["original".to_string()]),
        )]),
    };
    assert_eq!(fresh[1].links, vec![expected]);
}

#[tokio::test]
async fn links_a_new_root_to_an_extracted_remote_target() {
    let sender = InMemoryTelemetryContext::new();
    let receiver = InMemoryTelemetryContext::new();
    let (carrier_tx, carrier_rx) = tokio::sync::oneshot::channel::<Option<TelemetryCarrier>>();
    let sender_for_span = sender.clone();
    sender
        .start_span_typed(options("trigger", None), move |span| {
            Box::pin(async move {
                let span_context: &dyn TelemetryContext = span.as_ref();
                let _ = carrier_tx.send(sender_for_span.inject(span_context));
            })
        })
        .await
        .expect("trigger span");
    let carrier = carrier_rx
        .await
        .expect("carrier")
        .expect("injected carrier");

    let target = receiver.extract(Some(&json_roundtrip_carrier(&carrier)));
    let target_for_late = Arc::clone(&target);
    receiver
        .start_span_typed(
            options(
                "background",
                Some(vec![link_from_context(
                    &target,
                    Some(attributes(&[(
                        "reason",
                        AttributeValue::String("restart".to_string()),
                    )])),
                )]),
            ),
            move |span| {
                Box::pin(async move {
                    span.add_link(link_from_context(&target_for_late, None));
                })
            },
        )
        .await
        .expect("background span");

    let recorded = receiver
        .get_spans()
        .into_iter()
        .next()
        .expect("recorded span");
    assert_eq!(recorded.parent_context, None);
    assert_eq!(
        recorded.links,
        vec![
            RecordedTelemetryLink {
                context: carrier.clone(),
                attributes: attributes(&[(
                    "reason",
                    AttributeValue::String("restart".to_string())
                )]),
            },
            RecordedTelemetryLink {
                context: carrier,
                attributes: kiana_telemetry::SpanAttributes::new(),
            },
        ]
    );
}

#[tokio::test]
async fn uses_the_root_for_absent_and_unknown_carriers() {
    let telemetry = InMemoryTelemetryContext::new();
    let root = telemetry.extract(None);
    let absent = telemetry.extract(None);
    assert!(
        same_allocation(&root, &absent),
        "every absent carrier falls back to one root allocation"
    );
    for unknown in [carrier(&[]), carrier(&[("unknown", "format")])] {
        let extracted = telemetry.extract(Some(&unknown));
        assert!(
            same_allocation(&extracted, &root),
            "absent/unknown carriers produce the root"
        );
    }
    // TS: `expect(telemetry.extract(carrier)).toBe(telemetry)` —— recorder、它的 metrics 能力、
    // 它的 propagation 能力、以及 extract 的根是**同一个对象**（`readonly metrics = this`）。
    // Rust 的等价物是同一个分配：trait object 的 data 指针相等。
    let recorder = allocation(&root);
    assert_eq!(
        allocation(&telemetry.extract(Some(&carrier(&[("unknown", "format")])))),
        recorder,
        "extract(unknown) returns the recorder object itself"
    );
    assert_eq!(
        allocation(&telemetry.metrics().expect("recorder metrics")),
        recorder,
        "context.metrics is the recorder object itself"
    );
    assert_eq!(
        allocation(&telemetry.propagation().expect("recorder propagation")),
        recorder,
        "context.propagation is the recorder object itself"
    );
    assert_eq!(
        allocation(&root.metrics().expect("root metrics")),
        recorder,
        "root.metrics is the recorder object itself"
    );
    assert_eq!(
        allocation(&root.propagation().expect("root propagation")),
        recorder,
        "root.propagation is the recorder object itself"
    );

    let noop_propagation = noop_telemetry_propagation();
    let noop_context: Arc<dyn TelemetryContext> = noop_telemetry_context_as_context();
    assert_eq!(noop_propagation.inject(noop_context.as_ref()), None);
    let unknown = carrier(&[("unknown", "format")]);
    let extracted = noop_propagation.extract(Some(&unknown));
    let shared: Arc<dyn TelemetryContext> = noop_telemetry_context();
    assert!(
        same_allocation(&extracted, &shared),
        "noop extract returns the shared no-op context"
    );

    noop_telemetry_context()
        .start_span_typed(options("noop", None), |span| {
            Box::pin(async move {
                span.add_link(SpanLink {
                    context: span.span_context_id(),
                    attributes: None,
                });
            })
        })
        .await
        .expect("noop span");
}

#[tokio::test]
async fn treats_recognized_carrier_identifiers_as_opaque_strings() {
    let telemetry = InMemoryTelemetryContext::new();
    let remote = carrier(&[
        ("kiana-memory-scope", "external-recorder"),
        ("kiana-memory-span", "external-span"),
        ("kiana-memory-trace", "external-trace"),
    ]);
    let parent = telemetry.extract(Some(&remote));
    parent
        .start_span_typed(options("receiver", None), |_| Box::pin(async move {}))
        .await
        .expect("receiver span");
    let spans = telemetry.get_spans();
    assert_eq!(spans[0].parent_context.as_ref(), Some(&remote));
}

// ---------------------------------------------------------------------------
// 非字符串 carrier 值（`{...carrier, [key]: 42}`）→ 回退到根
//（TS memory.ts:314 `typeof scope !== "string"` 的判据）。
// ---------------------------------------------------------------------------

/// `extract_json` 是 `TelemetryCarrier`（字符串 map）无法表达的「键存在但值非字符串」入口。
///
/// TS 实测：三个必需键中任一个是数字时 `extract` 返回根；额外键是数字时**不影响**识别
/// （只看三个必需键）。两种情形都是当前稳定契约，改动需同步测试。
#[tokio::test]
async fn non_string_carrier_values_fall_back_to_the_root() {
    use serde_json::json;

    let telemetry = InMemoryTelemetryContext::new();
    let root = telemetry.extract(None);

    let mut real = carrier(&[("kiana-memory-scope", "remote")]);
    real.insert("kiana-memory-span".to_string(), "1".to_string());
    real.insert("kiana-memory-trace".to_string(), "T".to_string());
    let carrier_json = json!({
        "kiana-memory-scope": real["kiana-memory-scope"],
        "kiana-memory-span": real["kiana-memory-span"],
        "kiana-memory-trace": real["kiana-memory-trace"],
    });
    assert!(
        !same_allocation(&telemetry.extract_json(&carrier_json), &root),
        "a fully string carrier is recognized"
    );

    for key in [
        "kiana-memory-scope",
        "kiana-memory-span",
        "kiana-memory-trace",
    ] {
        let mut value = carrier_json.clone();
        value[key] = json!(42);
        assert!(
            same_allocation(&telemetry.extract_json(&value), &root),
            "numeric {key} falls back to the root"
        );
        // 其他非字符串形态同样回退（null / bool / 数组 / 对象 / undefined=缺失）。
        for replacement in [json!(null), json!(true), json!([1]), json!({"a": 1})] {
            let mut value = carrier_json.clone();
            value[key] = replacement.clone();
            assert!(
                same_allocation(&telemetry.extract_json(&value), &root),
                "{replacement} {key} falls back to the root"
            );
        }
        let mut missing = carrier_json.clone();
        missing.as_object_mut().expect("object").shift_remove(key);
        assert!(
            same_allocation(&telemetry.extract_json(&missing), &root),
            "missing {key} falls back to the root"
        );
    }

    // 额外键不是字符串：TS 不检查它们（只看三个必需键），因此仍是同一个 carrier。
    let mut with_extra = carrier_json.clone();
    with_extra["extra"] = json!(42);
    let extracted = telemetry.extract_json(&with_extra);
    assert!(
        !same_allocation(&extracted, &root),
        "extra keys are ignored, matching TS"
    );
    // 非对象输入（数字/数组/字符串/null）→ 根。
    for value in [json!(42), json!([1, 2]), json!("text"), json!(null)] {
        assert!(
            same_allocation(&telemetry.extract_json(&value), &root),
            "{value} is not a carrier"
        );
    }
}

// ---------------------------------------------------------------------------
// TS-faithful adapter fixture：`metrics`/`propagation` 就是 context 对象本身
// ---------------------------------------------------------------------------

/// TS 的 `test/conformance.test.ts` fixture 是
/// `{ context, propagation: context, metrics: context, … }` —— recorder 对象同时充当三个角色。
/// 一致性套件的 `assertCapabilities` 是 `strictEqual(context.metrics, metrics)`，所以这个 fixture
/// 只有在「recorder 就是能力对象」时才能通过。内存适配器现在满足它（`RecorderIdentity`），
/// 这里把同一个 fixture 形状跑一遍全部 14 个 case 作为回归。
#[tokio::test]
async fn conformance_passes_with_the_recorder_as_every_capability() {
    use kiana_telemetry::testing::{
        TelemetryAdapterFixture, TelemetryAdapterFixtureFactory,
        create_telemetry_adapter_conformance,
    };

    let factory: TelemetryAdapterFixtureFactory = Arc::new(|| {
        Box::pin(async move {
            let context = Arc::new(InMemoryTelemetryContext::new());
            // `context.metrics` / `context.propagation` 都是 recorder 本身。
            let metrics = context.metrics().expect("metrics");
            let propagation = context.propagation().expect("propagation");
            let reader_metrics = Arc::clone(&context);
            let reader_spans = Arc::clone(&context);
            Ok(TelemetryAdapterFixture {
                context: context as Arc<dyn TelemetryContext>,
                propagation: Some(propagation),
                metrics: Some(metrics),
                get_metrics: Some(Arc::new(move || {
                    let context = Arc::clone(&reader_metrics);
                    Box::pin(async move { context.get_metrics() })
                })),
                get_spans: Arc::new(move || {
                    let context = Arc::clone(&reader_spans);
                    Box::pin(async move { context.get_spans() })
                }),
                dispose: Some(Arc::new(|| Box::pin(async {}))),
            })
        })
    });

    let cases = create_telemetry_adapter_conformance(factory);
    assert_eq!(cases.len(), 14);
    for case in cases {
        (case.run)()
            .await
            .unwrap_or_else(|error| panic!("{} > {}: {error}", case.group, case.name));
    }
}
