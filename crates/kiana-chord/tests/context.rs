//! `packages/chord/test/context.test.ts` 的 Rust 端口（8 条 `it`）。
//!
//! 夹具 `fixtures/context_golden.json` 由 `scratch/kiana-chord__core/gen_context2.mjs` 在 TS 参考
//! 实现上生成：空根上下文的字符串、分层值的 `toString()`、取消传播的 reason、`abortError`
//! 的三种形态（缺省 DOMException / 非 Error 值回落 / Error 原样）、以及 `traceIn` 的 span 树。
//!
//! TS 测试用 `vi.fn()` 数监听器调用次数；Rust 用 `Arc<AtomicUsize>`。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_chord::context::{
    AbortError, AbortReason, AbortSignal, BACKGROUND_CONTEXT, ContextRef, TODO_CONTEXT,
    await_with_context, context_string, create_context_key, create_context_key_anonymous, trace_in,
    with_abort_signal, with_cancel, with_context_value, with_telemetry, without_abort_signal,
};
use kiana_telemetry::InMemoryTelemetryContext;
use kiana_telemetry::index::{BoxFuture, SpanOptions, TelemetryContextExt, TelemetrySpan};
use serde_json::{Value, json};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/context_golden.json")).expect("context golden")
}

fn background() -> ContextRef {
    ContextRef::clone(&BACKGROUND_CONTEXT)
}

fn options(name: &str) -> SpanOptions {
    SpanOptions {
        name: name.to_owned(),
        ..Default::default()
    }
}

/// 在 in-memory 上下文里起一个 span，在它**仍开着**的时候跑 `f`（TS 的
/// `telemetry.startSpan({name}, async (span) => …)`）。
async fn with_span<T, F>(telemetry: &InMemoryTelemetryContext, name: &str, f: F) -> T
where
    T: Send + 'static,
    F: FnOnce(Arc<dyn TelemetrySpan>) -> BoxFuture<'static, T> + Send + 'static,
{
    telemetry
        .start_span_typed(options(name), f)
        .await
        .expect("span")
}

// ── it("provides distinct empty root contexts") ───────────────────────────────────

#[test]
fn provides_distinct_empty_root_contexts() {
    let golden = golden();
    let key = create_context_key::<String>("value");

    assert_eq!(
        golden["empty"]["distinct"],
        json!(!Arc::ptr_eq(&BACKGROUND_CONTEXT, &TODO_CONTEXT))
    );
    assert_eq!(
        golden["empty"]["todoSignal"],
        json!(TODO_CONTEXT.abort_signal().is_none())
    );
    assert_eq!(
        golden["empty"]["todoNoop"],
        json!(Arc::ptr_eq(
            &TODO_CONTEXT.telemetry(),
            &kiana_telemetry::noop::noop_telemetry_context()
        )),
        "the empty contexts return the NOOP singleton"
    );
    assert_eq!(
        golden["empty"]["backgroundNoop"],
        json!(Arc::ptr_eq(
            &BACKGROUND_CONTEXT.telemetry(),
            &kiana_telemetry::noop::noop_telemetry_context()
        ))
    );
    assert!(key.get(TODO_CONTEXT.as_ref()).is_none());
    assert_eq!(
        context_string(BACKGROUND_CONTEXT.as_ref()),
        golden["empty"]["backgroundString"]
            .as_str()
            .expect("golden")
    );
    assert_eq!(
        context_string(TODO_CONTEXT.as_ref()),
        golden["empty"]["todoString"].as_str().expect("golden")
    );

    // 匿名键在 `toString()` 里显示为 `anonymous`（`Symbol(undefined).description`）。
    let anonymous = create_context_key_anonymous::<u8>();
    assert_eq!(anonymous.description(), None);
    assert_eq!(
        context_string(with_context_value(anonymous, 1u8, background()).as_ref()),
        "[Context BACKGROUND_CONTEXT].WithValue(anonymous)"
    );
}

// ── it("layers typed values without modifying parents") ───────────────────────────

#[test]
fn layers_typed_values_without_modifying_parents() {
    let golden = golden();
    let first_key = create_context_key::<String>("first");
    let second_key = create_context_key::<u32>("second");
    let first = with_context_value(first_key.clone(), "one".to_owned(), background());
    let second = with_context_value(second_key.clone(), 2u32, ContextRef::clone(&first));
    let replaced = with_context_value(
        first_key.clone(),
        "updated".to_owned(),
        ContextRef::clone(&second),
    );

    assert_eq!(
        golden["layer"]["bgFirst"],
        json!(first_key.get(BACKGROUND_CONTEXT.as_ref()))
    );
    assert_eq!(
        golden["layer"]["firstFirst"],
        json!(first_key.get(first.as_ref()).expect("one"))
    );
    assert_eq!(
        golden["layer"]["firstSecond"],
        json!(second_key.get(first.as_ref()))
    );
    assert_eq!(
        golden["layer"]["secondFirst"],
        json!(first_key.get(second.as_ref()).expect("one"))
    );
    assert_eq!(
        golden["layer"]["secondSecond"],
        json!(second_key.get(second.as_ref()))
    );
    assert_eq!(
        golden["layer"]["replacedFirst"],
        json!(first_key.get(replaced.as_ref()).expect("updated"))
    );
    assert_eq!(
        golden["layer"]["secondFirstStill"],
        json!(first_key.get(second.as_ref()).expect("one")),
        "the parent is not modified"
    );
    assert_eq!(
        context_string(replaced.as_ref()),
        golden["layer"]["string"].as_str().expect("golden")
    );
}

// ── it("inherits parent cancellation and isolates child cancellation") ────────────

#[test]
fn inherits_parent_cancellation_and_isolates_child_cancellation() {
    let golden = golden();
    let parent_controller = AbortSignal::new();
    let parent = with_abort_signal(parent_controller.clone(), background());
    let child = with_cancel(ContextRef::clone(&parent));
    let sibling = with_cancel(ContextRef::clone(&parent));
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let _subscription = child
        .context
        .abort_signal()
        .expect("child signal")
        .on_abort(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        });

    child.cancel_with(AbortReason::value(json!("child")));
    let child_signal = child.context.abort_signal().expect("child signal");
    assert_eq!(
        golden["cancel"]["childAborted"],
        json!(child_signal.is_aborted())
    );
    assert_eq!(
        golden["cancel"]["childReason"],
        child_signal.reason().expect("reason").to_json()
    );
    assert_eq!(
        golden["cancel"]["siblingBefore"],
        json!(
            sibling
                .context
                .abort_signal()
                .expect("sibling signal")
                .is_aborted()
        )
    );
    assert_eq!(
        golden["cancel"]["parentBefore"],
        json!(parent_controller.is_aborted())
    );
    assert_eq!(
        golden["cancel"]["listenerCalls"],
        json!(calls.load(Ordering::SeqCst))
    );

    parent_controller.abort(Some(AbortReason::value(json!("parent"))));
    let sibling_signal = sibling.context.abort_signal().expect("sibling signal");
    assert_eq!(
        golden["cancel"]["siblingAborted"],
        json!(sibling_signal.is_aborted()),
        "the parent's cancellation reaches the sibling"
    );
    assert_eq!(
        golden["cancel"]["siblingReason"],
        sibling_signal.reason().expect("reason").to_json()
    );
}

// ── it("masks caller cancellation for mandatory cleanup") ─────────────────────────

#[test]
fn masks_caller_cancellation_for_mandatory_cleanup() {
    let golden = golden();
    let controller = AbortSignal::new();
    let key = create_context_key::<String>("value");
    let context = with_context_value(
        key.clone(),
        "preserved".to_owned(),
        with_abort_signal(controller.clone(), background()),
    );
    let cleanup = without_abort_signal(ContextRef::clone(&context));

    controller.abort_without_reason();
    assert_eq!(
        golden["mask"]["baseAborted"],
        json!(context.abort_signal().expect("signal").is_aborted())
    );
    assert_eq!(
        golden["mask"]["cleanupHasSignal"],
        json!(cleanup.abort_signal().is_some())
    );
    assert_eq!(
        golden["mask"]["value"],
        json!(key.get(cleanup.as_ref()).expect("preserved"))
    );
}

// ── it("derives telemetry independently from values and cancellation") ────────────

#[tokio::test]
async fn derives_telemetry_independently_from_values_and_cancellation() {
    let golden = golden();
    let telemetry = InMemoryTelemetryContext::new();
    let recorder = telemetry.clone();
    with_span(&telemetry, "caller", move |caller_span| {
        Box::pin(async move {
            let other = InMemoryTelemetryContext::new();
            with_span(&other, "other", move |other_span| {
                Box::pin(async move {
                    let key = create_context_key::<String>("principal");
                    let controller = AbortSignal::new();
                    let base = with_context_value(
                        key.clone(),
                        "owner".to_owned(),
                        with_abort_signal(controller.clone(), background()),
                    );
                    let context =
                        with_telemetry(Arc::clone(&caller_span), ContextRef::clone(&base));
                    let replaced =
                        with_telemetry(Arc::clone(&other_span), ContextRef::clone(&context));

                    assert!(Arc::ptr_eq(
                        &base.telemetry(),
                        &kiana_telemetry::noop::noop_telemetry_context()
                    ));
                    assert!(Arc::ptr_eq(&context.telemetry(), &caller_span));
                    assert!(Arc::ptr_eq(&replaced.telemetry(), &other_span));
                    assert_eq!(key.get(replaced.as_ref()), Some("owner".to_owned()));
                    assert!(
                        replaced
                            .abort_signal()
                            .expect("signal")
                            .same_signal(&base.abort_signal().expect("signal")),
                        "the abort signal is unchanged"
                    );
                    assert!(Arc::ptr_eq(
                        &without_abort_signal(ContextRef::clone(&context)).telemetry(),
                        &caller_span
                    ));

                    let outer_context = ContextRef::clone(&context);
                    let key_outer = key.clone();
                    trace_in(
                        options("outer"),
                        move |child| {
                            let key = key_outer;
                            Box::pin(async move {
                                assert_eq!(key.get(child.as_ref()), Some("owner".to_owned()));
                                assert!(
                                    child.abort_signal().expect("signal").same_signal(
                                        &outer_context.abort_signal().expect("signal")
                                    )
                                );
                                child.telemetry().add_event("started", None);
                                let inner_parent = ContextRef::clone(&child);
                                let inner = trace_in(
                                    options("inner"),
                                    move |nested| {
                                        let link_parent = ContextRef::clone(&inner_parent);
                                        Box::pin(async move {
                                            nested.telemetry().add_link(
                                                kiana_telemetry::SpanLink {
                                                    context: link_parent
                                                        .telemetry()
                                                        .span_context_id(),
                                                    attributes: None,
                                                },
                                            );
                                            42
                                        })
                                    },
                                    ContextRef::clone(&child),
                                )
                                .await
                                .expect("inner trace");
                                assert_eq!(inner, 42);
                            })
                        },
                        ContextRef::clone(&context),
                    )
                    .await
                    .expect("outer trace");

                    // 夹具：caller → outer → inner，inner 有一条指向 outer 的 link。
                    let spans = recorder.get_spans();
                    let summary: Vec<Value> = spans
                        .iter()
                        .map(|span| {
                            json!({
                                "name": span.name,
                                "parentId": span.parent_id.map(|id| id as i64),
                                "linkCount": span.links.len(),
                            })
                        })
                        .collect();
                    assert_eq!(Value::Array(summary), golden["spans"]);
                    let caller = &spans[0];
                    let outer = &spans[1];
                    let inner = &spans[2];
                    assert_eq!(outer.parent_id, Some(caller.id));
                    assert_eq!(inner.parent_id, Some(outer.id));
                    assert_eq!(inner.links[0].context, outer.context);

                    assert!(Arc::ptr_eq(&context.telemetry(), &caller_span));
                    controller.abort_without_reason();
                    assert!(context.abort_signal().expect("signal").is_aborted());
                })
            })
            .await;
        })
    })
    .await;
}

// ── it("isolates concurrent traceIn branches and preserves results and errors") ────

#[tokio::test]
async fn isolates_concurrent_trace_in_branches_and_preserves_results_and_errors() {
    let telemetry = InMemoryTelemetryContext::new();
    let recorder = telemetry.clone();
    with_span(&telemetry, "caller", move |parent| {
        Box::pin(async move {
            let context = with_telemetry(Arc::clone(&parent), background());
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let gate = Arc::new(tokio::sync::Mutex::new(Some(rx)));

            let first_gate = Arc::clone(&gate);
            let first = trace_in(
                options("first"),
                move |child| {
                    Box::pin(async move {
                        let rx = first_gate.lock().await.take().expect("gate");
                        rx.await.ok();
                        trace_in(
                            options("first.child"),
                            |_| Box::pin(async { "first" }),
                            child,
                        )
                        .await
                        .expect("first child")
                    })
                },
                ContextRef::clone(&context),
            );
            let second = trace_in(
                options("second"),
                move |child| {
                    Box::pin(async move {
                        let result = trace_in(
                            options("second.child"),
                            |_| Box::pin(async { "second" }),
                            ContextRef::clone(&child),
                        )
                        .await
                        .expect("second child");
                        tx.send(()).ok();
                        result
                    })
                },
                ContextRef::clone(&context),
            );
            let (a, b) = tokio::join!(first, second);
            assert_eq!((a.unwrap(), b.unwrap()), ("first", "second"));

            // 并发分支各自挂在自己的父 span 下（不是串成一串）。
            let spans = recorder.get_spans();
            for name in ["first", "second"] {
                let parent = spans
                    .iter()
                    .find(|span| span.name == name)
                    .expect("parent span");
                let child = spans
                    .iter()
                    .find(|span| span.name == format!("{name}.child"))
                    .expect("child span");
                assert_eq!(child.parent_id, Some(parent.id));
            }

            // 无 telemetry 的上下文也走 startSpan（没有 no-telemetry 分支）。
            let value = trace_in(
                options("noop"),
                |child| {
                    Box::pin(async move {
                        child.telemetry().add_link(kiana_telemetry::SpanLink {
                            context: child.telemetry().span_context_id(),
                            attributes: None,
                        });
                        42
                    })
                },
                background(),
            )
            .await
            .expect("noop trace");
            assert_eq!(golden()["noopTrace"], json!(value));
        })
    })
    .await;
}

// ── it("stops waiting when the invocation is cancelled") ──────────────────────────

#[tokio::test]
async fn stops_waiting_when_the_invocation_is_cancelled() {
    let golden = golden();
    let controller = AbortSignal::new();
    let context = with_abort_signal(controller.clone(), background());
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let handle = tokio::spawn(async move { rx.await.unwrap_or_default() });
    let waiting = kiana_chord::context::await_handle_with_context(handle, context.as_ref());

    #[derive(Debug)]
    struct Cancelled;
    impl std::fmt::Display for Cancelled {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("cancelled")
        }
    }
    impl std::error::Error for Cancelled {}

    controller.abort(Some(AbortReason::error(Cancelled)));
    let error = waiting.await.expect_err("cancelled");
    // `rejects.toBe(cancellation)`：原因原样传出。
    assert!(matches!(error, AbortError::Reason(_)));
    assert_eq!(error.to_string(), "cancelled");

    // 底层 promise 继续跑（任务没被取消）。
    tx.send("completed later".to_owned()).ok();
    assert_eq!(
        await_with_context(async { "completed" }, BACKGROUND_CONTEXT.as_ref())
            .await
            .expect("no signal"),
        "completed"
    );

    // `abortError` 的三种形态（夹具 abortCases）。
    let cases = &golden["abortCases"];
    let check = |error: AbortError, key: &str| {
        assert_eq!(
            error.name(),
            cases[key]["name"].as_str().expect("name"),
            "{key}"
        );
        assert_eq!(
            error.message(),
            cases[key]["message"].as_str().expect("message"),
            "{key}"
        );
        // `String(error)`：DOMException 分支逐字一致；`Error` 分支在 JS 里是
        // `` `${error.name}: ${error.message}` ``，而 Rust 的 `dyn Error` 没有 `name`，
        // `Display` 就是 message（见模块文档"与 TS 的差异"）。
        if error.name() == "AbortError" {
            assert_eq!(
                error.to_string(),
                cases[key]["string"].as_str().expect("string"),
                "{key}"
            );
        }
    };

    // 缺省原因：`DOMException("This operation was aborted", "AbortError")` 原样返回。
    let signal = AbortSignal::new();
    signal.abort_without_reason();
    let context = with_abort_signal(signal, background());
    let error = await_with_context(async { 1 }, context.as_ref())
        .await
        .expect_err("aborted");
    check(error, "noReason");

    for (key, reason) in [
        ("nullValue", json!(null)),
        ("stringValue", json!("plain string")),
        ("zeroValue", json!(0)),
    ] {
        let signal = AbortSignal::new();
        signal.abort(Some(AbortReason::value(reason)));
        let context = with_abort_signal(signal, background());
        let error = await_with_context(async { 1 }, context.as_ref())
            .await
            .expect_err("aborted");
        check(error, key);
    }

    // Error 原因原样返回（`name` 来自错误本身；TS 的 `new Error("boom")`）。
    #[derive(Debug)]
    struct Boom;
    impl std::fmt::Display for Boom {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("boom")
        }
    }
    impl std::error::Error for Boom {}

    let signal = AbortSignal::new();
    signal.abort(Some(AbortReason::error(Boom)));
    let context = with_abort_signal(signal, background());
    let error = await_with_context(async { 1 }, context.as_ref())
        .await
        .expect_err("aborted");
    assert_eq!(
        error.to_string(),
        "boom",
        "Rust's `dyn Error` has no name to prefix"
    );
    check(error, "plainError");

    // `AbortSignal.any` 的首个原因胜出（withAbortSignal 用它组合父子信号）。
    let parent = AbortSignal::new();
    let child = AbortSignal::new();
    let combined = AbortSignal::any(&[parent.clone(), child.clone()]);
    parent.abort(Some(AbortReason::value(json!("parent"))));
    child.abort(Some(AbortReason::value(json!("child"))));
    assert!(combined.is_aborted());
    assert_eq!(
        combined.reason().map(|reason| reason.to_json()),
        Some(json!("parent")),
        "the first reason wins"
    );
}
