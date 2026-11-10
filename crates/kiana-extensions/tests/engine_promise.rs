//! `__kiana` 手写 Promise 协议（设计文档 §2.3、§2.4、§10.5）。
//!
//! TS 对应物：无（Node 的原生 Promise + 宿主回调）。这里覆盖：JS 建 Promise、Rust 存
//! waiter、`__kiana.settle` 回填、完成队列、未知 kind 的【逐字】错误、以及 `Invoke` 回调。

mod support;

use kiana_extensions::engine::{HostCall, HostValue, JsEngine};
use rquickjs::{Function, Persistent};
use support::{Harness, run_local};

fn install_echo(engine: &JsEngine) {
    engine.register_host_kind("echo", |call: HostCall<'_>| {
        let payload: serde_json::Value = call.payload_json()?;
        call.completion_sender()
            .resolve(call.id(), HostValue::Json(payload));
        Ok(())
    });
}

#[test]
fn promise_resolves_from_json_payload_and_preserves_key_order() {
    let harness = Harness::new();
    install_echo(&harness.engine);
    harness.eval::<()>(
        r#"globalThis.__result = null;
           __kiana.promise("echo", { b: 2, a: 1 }).then(value => { __result = value; });"#,
    );
    assert_eq!(harness.engine.pending_count(), 1);
    assert_eq!(harness.engine.try_drain_completions(), 1);
    assert_eq!(harness.engine.pending_count(), 0);
    harness.pump();
    // serde_json 开了 preserve_order：键序 == 传入顺序（§4.3 第 3 条）。
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__result)"),
        "{\"b\":2,\"a\":1}"
    );
}

#[test]
fn promise_rejects_with_error_value() {
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("fail", |call: HostCall<'_>| {
            call.completion_sender()
                .reject_message(call.id(), "host said no");
            Ok(())
        });
    harness.eval::<()>(
        r#"globalThis.__error = null;
           __kiana.promise("fail").catch(error => { __error = error.message; });"#,
    );
    assert_eq!(harness.engine.try_drain_completions(), 1);
    harness.pump();
    assert_eq!(harness.eval::<String>("__error"), "host said no");
}

#[test]
fn unknown_host_kind_throws_synchronously_with_exact_message() {
    let harness = Harness::new();
    // Design §2.3 frozen protocol: `__kiana.promise` creates the Promise, then
    // calls hostStart *outside* the executor, so a host-side throw is
    // synchronous (inside an `async` function it still becomes a rejection).
    let result: String = harness.eval::<String>(
        r#"(function () {
             try {
               const promise = __kiana.promise("nope", 1);
               return "returned:" + (promise instanceof Promise);
             } catch (error) {
               return "threw:" + error.message;
             }
           })()"#,
    );
    assert_eq!(result, "threw:unknown host kind: nope");
    // 未知 kind 不得 leak waiter / pending。
    assert_eq!(harness.eval::<i32>("__kiana.waiters.size"), 0);
    assert_eq!(harness.engine.pending_count(), 0);
}

#[test]
fn settle_removes_waiter_and_pending_entry() {
    let harness = Harness::new();
    install_echo(&harness.engine);
    harness.eval::<()>(
        "globalThis.__value = 0; __kiana.promise('echo', 7).then(v => { __value = v; });",
    );
    assert_eq!(harness.eval::<i32>("__kiana.waiters.size"), 1);
    harness.engine.try_drain_completions();
    harness.pump();
    assert_eq!(harness.eval::<i32>("__kiana.waiters.size"), 0);
    assert_eq!(harness.eval::<i32>("__value"), 7);
    assert_eq!(harness.engine.pending_count(), 0);
}

#[test]
fn duplicate_settle_is_ignored() {
    let harness = Harness::new();
    install_echo(&harness.engine);
    harness.eval::<()>(
        "globalThis.__value = 0; __kiana.promise('echo', 1).then(v => { __value = v; });",
    );
    assert_eq!(harness.engine.try_drain_completions(), 1);
    harness.pump();
    // 再次 settle 同一个 id：JS 返回 false，Rust 侧不 panic。
    let sender = harness.engine.completion_sender();
    sender.resolve(1, HostValue::Number(99.0));
    assert_eq!(harness.engine.try_drain_completions(), 1);
    assert_eq!(harness.eval::<i32>("__value"), 1);
}

#[test]
fn completions_apply_in_queue_order() {
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("order", |call: HostCall<'_>| {
            let value: f64 = call.payload_json()?;
            call.completion_sender()
                .resolve(call.id(), HostValue::Number(value));
            Ok(())
        });
    harness.eval::<()>(
        r#"globalThis.__order = [];
           for (const value of [1, 2, 3]) __kiana.promise("order", value).then(v => __order.push(v));"#,
    );
    assert_eq!(harness.engine.try_drain_completions(), 3);
    harness.pump();
    assert_eq!(harness.eval::<String>("JSON.stringify(__order)"), "[1,2,3]");
}

#[test]
fn invoke_calls_persistent_callback_with_args() {
    let harness = Harness::new();
    let callback: Persistent<Function<'static>> = harness.engine.with_js(|ctx| {
        // Regular function (not an arrow): `arguments` is legal here, so the
        // callback must not throw.
        let function: Function<'_> = ctx
            .eval("(function (a, b) { globalThis.__sum = a + b; globalThis.__argc = arguments.length; })")
            .expect("callback");
        Persistent::save(&ctx, function)
    });
    assert!(harness.engine.completion_sender().invoke(
        callback,
        vec![HostValue::Number(2.0), HostValue::Number(3.0)]
    ));
    assert_eq!(harness.engine.try_drain_completions(), 1);
    assert_eq!(harness.eval::<i32>("__sum"), 5);
    assert_eq!(harness.eval::<i32>("__argc"), 2);
    assert!(
        harness.take_errors().is_empty(),
        "a successful Invoke callback must not record an error"
    );
}

#[test]
fn invoke_callback_exception_is_recorded() {
    let harness = Harness::new();
    let callback: Persistent<Function<'static>> = harness.engine.with_js(|ctx| {
        let function: Function<'_> = ctx
            .eval("() => { throw new Error('invoke boom'); }")
            .expect("callback");
        Persistent::save(&ctx, function)
    });
    assert!(
        harness
            .engine
            .completion_sender()
            .invoke(callback, Vec::new())
    );
    assert_eq!(harness.engine.try_drain_completions(), 1);
    // The exception must surface through the engine error queue instead of
    // being dropped (design §2.3/§2.4).
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, kiana_extensions::engine::JsErrorKind::Host);
    assert_eq!(errors[0].message, "invoke boom");
    assert!(
        errors[0]
            .stack
            .as_deref()
            .unwrap_or("")
            .starts_with("Error: invoke boom\n")
    );
}

#[test]
fn json_payload_keeps_proto_as_an_own_property() {
    // JSON.parse 语义：`"__proto__"` 是自有属性，不会改写原型（逐键 Object::set 会踩）。
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("proto", |call: HostCall<'_>| {
            let payload: serde_json::Value = call.payload_json()?;
            call.completion_sender().resolve(call.id(), payload);
            Ok(())
        });
    harness.eval::<()>(
        r#"globalThis.__proto_value = null;
           __kiana.promise("proto", JSON.parse('{"__proto__": {"polluted": true}, "x": 1}'))
             .then(value => { __proto_value = value; });"#,
    );
    harness.engine.try_drain_completions();
    harness.pump();
    assert!(harness.eval::<bool>("Object.getPrototypeOf(__proto_value) === Object.prototype"));
    assert!(
        harness.eval::<bool>("Object.prototype.hasOwnProperty.call(__proto_value, '__proto__')")
    );
    assert!(harness.eval::<bool>("__proto_value.x === 1"));
    assert!(harness.eval::<bool>("({}).polluted === undefined"));
}

#[test]
fn host_value_variants_materialize() {
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("value", |call: HostCall<'_>| {
            let which: String = call.payload_json()?;
            let value = match which.as_str() {
                "undefined" => HostValue::Undefined,
                "null" => HostValue::Null,
                "bool" => HostValue::Bool(true),
                "number" => HostValue::Number(1.5),
                "error" => HostValue::Error {
                    name: "RangeError".to_owned(),
                    message: "bad".to_owned(),
                },
                _ => HostValue::Json(serde_json::json!({ "a": [1, null, "x"] })),
            };
            call.completion_sender().resolve(call.id(), value);
            Ok(())
        });
    harness.eval::<()>(
        r#"globalThis.__values = null;
           Promise.all(["undefined", "null", "bool", "number", "error", "json"]
             .map(kind => __kiana.promise("value", kind)))
             .then(values => { __values = values; });"#,
    );
    assert_eq!(harness.engine.try_drain_completions(), 6);
    harness.pump();
    assert!(harness.eval::<bool>("__values[0] === undefined"));
    assert!(harness.eval::<bool>("__values[1] === null"));
    assert!(harness.eval::<bool>("__values[2] === true"));
    assert!(harness.eval::<bool>("__values[3] === 1.5"));
    assert!(harness.eval::<bool>(
        "__values[4] instanceof Error && __values[4].name === 'RangeError' && __values[4].message === 'bad'"
    ));
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__values[5])"),
        "{\"a\":[1,null,\"x\"]}"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn promise_settles_from_spawned_host_task() {
    run_local(async {
        let harness = Harness::new();
        harness
            .engine
            .register_host_kind("later", |call: HostCall<'_>| {
                let id = call.id();
                let sender = call.completion_sender();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    sender.resolve(id, HostValue::String("later".to_owned()));
                });
                Ok(())
            });
        harness.eval::<()>(
            "globalThis.__done = null; __kiana.promise('later').then(v => { __done = v; });",
        );
        assert!(harness.engine.pending_count() == 1);
        assert!(
            harness
                .drive_until(support::TEST_TIMEOUT, |h| h.eval::<bool>("__done !== null"))
                .await
        );
        assert_eq!(harness.eval::<String>("__done"), "later");
        assert_eq!(harness.engine.pending_count(), 0);
    })
    .await;
}

#[test]
fn pending_count_tracks_unsettled_host_calls() {
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("never", |_call: HostCall<'_>| Ok(()));
    harness.eval::<()>(
        "globalThis.__promises = [__kiana.promise('never'), __kiana.promise('never')];",
    );
    assert_eq!(harness.engine.pending_count(), 2);
    assert_eq!(harness.eval::<i32>("__kiana.waiters.size"), 2);
}

// ---------------------------------------------------------------------------
// HostValue::Error 的 JS 错误类（设计文档 §2.3、§6.5）
// ---------------------------------------------------------------------------

#[test]
fn host_error_names_materialize_as_node_error_classes() {
    // Node 的 abort 错误是 DOMException（code 20/23），fetch 失败是 TypeError；
    // 扩展代码按 instanceof / error.code / Object.prototype.toString 分支。
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("reject", |call: HostCall<'_>| {
            #[derive(serde::Deserialize)]
            struct Payload {
                name: String,
                message: String,
            }
            let payload: Payload = call.payload_json()?;
            call.completion_sender().reject(
                call.id(),
                HostValue::Error {
                    name: payload.name,
                    message: payload.message,
                },
            );
            Ok(())
        });
    harness.eval::<()>(
        r#"globalThis.__errors = [];
           function capture(name, message) {
             __kiana.promise("reject", { name, message }).catch(error => { __errors.push(error); });
           }
           capture("AbortError", "The operation was aborted");
           capture("TimeoutError", "The operation was aborted due to timeout");
           capture("TypeError", "fetch failed");
           capture("CustomError", "custom");"#,
    );
    for _ in 0..4 {
        harness.engine.try_drain_completions();
        harness.pump();
    }
    assert_eq!(
        harness.eval::<String>(
            "JSON.stringify(__errors.map(error => [\
               error.name, error.code, Object.prototype.toString.call(error),\
               error instanceof DOMException, error instanceof TypeError, error.message\
             ]))"
        ),
        "[[\"AbortError\",20,\"[object DOMException]\",true,false,\"The operation was aborted\"],\
          [\"TimeoutError\",23,\"[object DOMException]\",true,false,\"The operation was aborted due to timeout\"],\
          [\"TypeError\",null,\"[object Error]\",false,true,\"fetch failed\"],\
          [\"CustomError\",null,\"[object Error]\",false,false,\"custom\"]]"
    );
    // DOMException 仍然是 Error（`error instanceof Error` 在 Node 为 true）。
    assert!(harness.eval::<bool>("__errors[0] instanceof Error"));
}

#[tokio::test(flavor = "current_thread")]
async fn captured_callbacks_stream_chunks_across_awaits() {
    // 宿主任务跨 await 持有回调（fetch 流式 chunk、tool onUpdate 的模型）。
    run_local(async {
        let harness = Harness::new();
        harness
            .engine
            .register_host_kind("stream", |call: HostCall<'_>| {
                let function = call
                    .payload()
                    .clone()
                    .into_function()
                    .expect("payload is a JS function");
                let callback = call.completion_sender().capture(function);
                let sender = call.completion_sender();
                let id = call.id();
                tokio::task::spawn_local(async move {
                    for index in 0..3 {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        assert!(callback.invoke(vec![HostValue::Number(f64::from(index))]));
                    }
                    sender.resolve(id, HostValue::Undefined);
                });
                Ok(())
            });
        harness.eval::<()>(
            r#"globalThis.__chunks = [];
               __kiana.promise("stream", value => { __chunks.push(value); });"#,
        );
        // 3 个 chunk 各睡 1 ms 才发出：按断言驱动到全部到达（settle_all 的
        // “无定时器即空闲”启发式会在任务发送前就返回）。
        assert!(
            harness
                .drive_until(support::TEST_TIMEOUT, |h| h.eval::<i32>("__chunks.length")
                    == 3)
                .await
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__chunks)"),
            "[0,1,2]"
        );
        assert_eq!(harness.engine.pending_count(), 0);
    })
    .await;
}
