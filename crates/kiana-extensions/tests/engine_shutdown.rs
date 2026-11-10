//! 关闭顺序与代际重载（设计文档 §2.8、§10.5）。
//!
//! TS 对应物：无（Node 的 `vm` context 由 GC 回收；`/reload` 由 jiti 的模块缓存失效实现）。
//!
//! 这一组测试保护的是“会 abort 进程”的硬约束：
//! 任何一个 `Persistent` 活过 `Runtime` 会触发
//! `JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)' failed`。
//! 所以 `Drop` 必须先清空 pending/handles/persistents/errors，再泵 job、`run_gc`，
//! 最后按字段顺序析构 `ctx` → `rt`（`engine/shutdown.rs`）。

mod support;

use kiana_extensions::engine::{HostValue, JsEngine};
use rquickjs::{Function, Object, Persistent};
use support::{Harness, run_local};

/// 设计文档 §4.3 第 5 条的【逐字】stale 文案（`api.js` 的 `invalidate()` 使用）。
const STALE: &str = "This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().";

#[test]
fn engine_drop_is_clean_with_live_handles_and_persistents() {
    for _ in 0..100 {
        let harness = Harness::new();

        // 1. 句柄（ui/adapter.rs 的组件对象）。
        let handle = harness.engine.with_js(|ctx| {
            let object = Object::new(ctx.clone()).expect("object");
            harness.engine.store_handle(object)
        });
        // 2. 通用持久槽（theme 回调等）。
        harness.engine.with_js(|ctx| {
            let callback: Function<'_> = ctx.eval("(() => 1)").expect("function");
            harness
                .engine
                .put_persistent("theme.fg", Persistent::save(&ctx, callback));
        });
        // 3. 未处理拒绝（错误队列里的 Persistent）。
        harness.eval::<()>("Promise.reject(new Error('leak check'));");
        harness.pump();
        // 4. 未完成的宿主 promise（pending 里的 Persistent）。
        harness
            .engine
            .register_host_kind("never", |_call: kiana_extensions::engine::HostCall<'_>| {
                Ok(())
            });
        harness.eval::<()>("__kiana.promise('never');");
        // 5. 挂着的定时器与未完成的 interval。
        harness.eval::<()>("setTimeout(() => {}, 60000); setInterval(() => {}, 60000);");

        assert!(harness.engine.drop_handle(handle));
        assert_eq!(harness.engine.pending_count(), 1);
        assert_eq!(harness.engine.persistent_slot_count(), 1);

        drop(harness.engine);
    }
}

#[test]
fn drop_with_unpumped_rejecting_microtasks_is_clean() {
    // 回归（设计文档 §2.8/§10.5）：Drop 里泵 job 产生的未处理拒绝会被 rejection
    // tracker 登记成新的 `Persistent`。若只在泵之前清一次 `errors`，这个 Persistent
    // 会活过 `Runtime`，触发
    // `JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)' failed` 并 abort
    // 整个测试进程（exit 134）。所以这里故意不 pump 就 drop。
    for source in [
        "Promise.resolve().then(() => { throw new Error('late boom'); });",
        "Promise.resolve().then(() => { Promise.reject('plain'); });",
        "Promise.resolve().then(() => { __kiana.promise('nope'); });",
        "Promise.reject('direct plain');",
        "queueMicrotask(() => { throw new Error('microtask boom'); });",
    ] {
        let harness = Harness::new();
        harness
            .engine
            .eval_script::<()>(source, "<drop-unpumped>")
            .expect("schedule the job");
        // 没有 harness.pump()：job 在 Drop 的清理泵里才执行。
        drop(harness.engine);
    }
}

#[test]
fn drop_after_pumping_rejections_is_clean() {
    // 对照组：先泵掉同样的 job，错误队列里已有 Persistent，Drop 必须先清理掉它们。
    let harness = Harness::new();
    harness.eval::<()>("Promise.reject(new Error('pumped boom'));");
    harness.pump();
    assert_eq!(harness.engine.error_count(), 1);
    drop(harness.engine);
}

#[test]
fn drop_with_queued_invoke_completions_is_clean() {
    // 完成队列里排队的 `HostCompletion::Invoke` 持有 Persistent；不 drain 直接 drop，
    // 接收端必须先于 runtime 释放，否则 JS_FreeRuntime 断言会 abort。
    let harness = Harness::new();
    let callback: Persistent<Function<'static>> = harness.engine.with_js(|ctx| {
        let function: Function<'_> = ctx.eval("() => 1").expect("function");
        Persistent::save(&ctx, function)
    });
    assert!(
        harness
            .engine
            .completion_sender()
            .invoke(callback, Vec::new())
    );
    drop(harness.engine);
}

#[test]
fn dropping_an_engine_with_a_settled_promise_is_clean() {
    let harness = Harness::new();
    harness
        .engine
        .register_host_kind("echo", |call: kiana_extensions::engine::HostCall<'_>| {
            call.completion_sender().resolve(call.id(), 1.0);
            Ok(())
        });
    harness.eval::<()>("__kiana.promise('echo').then(() => {});");
    harness.engine.try_drain_completions();
    harness.pump();
    drop(harness.engine);
}

#[test]
fn invalidate_records_the_stale_message_and_lets_in_flight_work_finish() {
    // TS 语义（loader.ts:195、runner.ts:726）：invalidate 只记录 stale 文案（幂等、
    // 首次生效）并退订事件总线；已发起的 kiana.exec/fetch/tool 调用继续跑并按真实结果
    // settle，只有之后新的 ctx/kiana 调用才抛 stale。
    let harness = Harness::new();
    let captured: std::rc::Rc<std::cell::Cell<Option<u64>>> = Default::default();
    let captured_in_handler = std::rc::Rc::clone(&captured);
    harness.engine.register_host_kind(
        "never",
        move |call: kiana_extensions::engine::HostCall<'_>| {
            captured_in_handler.set(Some(call.id()));
            Ok(())
        },
    );
    harness.eval::<()>(
        r#"globalThis.__settled = null;
           globalThis.__rejected = null;
           __kiana.promise('never').then(
             value => { __settled = value; },
             error => { __rejected = error.message; }
           );"#,
    );
    assert_eq!(harness.engine.pending_count(), 1);

    let in_flight = harness.engine.invalidate(STALE);
    assert_eq!(in_flight, 1);
    // 先到先得：第二次 invalidate 不覆盖文案（TS 的 `if (!this.staleMessage)`）。
    harness.engine.invalidate("a later message");
    assert_eq!(harness.engine.stale_message().as_deref(), Some(STALE));
    // 在飞行中的 promise 没有被 reject，也没有被移除。
    assert_eq!(harness.engine.pending_count(), 1);
    assert!(harness.eval::<bool>("__settled === null && __rejected === null"));

    // 宿主任务照常完成 → promise 正常 resolve。
    let id = captured.get().expect("host kind ran");
    harness
        .engine
        .completion_sender()
        .resolve(id, HostValue::String("done".to_owned()));
    assert_eq!(harness.engine.try_drain_completions(), 1);
    harness.pump();
    assert_eq!(harness.eval::<String>("__settled"), "done");
    assert!(harness.eval::<bool>("__rejected === null"));
    assert_eq!(harness.engine.pending_count(), 0);
}

#[test]
fn invalidate_keeps_handles_and_persistent_slots() {
    // TS 的 invalidate 不销毁 UI 组件/主题回调：旧 ctx 的后续调用由 api.js 的
    // assertActive() 抛 stale，句柄与持久槽只在整代销毁（Drop/reload）时清理。
    let harness = Harness::new();
    let handle = harness.engine.with_js(|ctx| {
        let object = Object::new(ctx.clone()).expect("object");
        harness.engine.store_handle(object)
    });
    harness.engine.with_js(|ctx| {
        let callback: Function<'_> = ctx.eval("(() => 1)").expect("function");
        harness
            .engine
            .put_persistent("theme.bg", Persistent::save(&ctx, callback));
    });
    assert_eq!(harness.engine.handle_count(), 1);
    assert_eq!(harness.engine.persistent_slot_count(), 1);

    harness.engine.invalidate(STALE);
    assert_eq!(harness.engine.handle_count(), 1);
    assert_eq!(harness.engine.persistent_slot_count(), 1);
    assert!(
        harness
            .engine
            .with_handle(handle, |_| Ok(()))
            .expect("handle")
            .is_some()
    );
}

#[test]
fn module_names_carry_generation_and_sequence() {
    let harness = Harness::new();
    let first = harness.engine.scoped_module_name("kiana:virtual/api");
    let second = harness.engine.scoped_module_name("kiana:virtual/api");
    assert_eq!(first, "kiana:virtual/api?v=0-1");
    assert_eq!(second, "kiana:virtual/api?v=0-2");
    assert_ne!(first, second, "moduleCache:false 等价（§3.4）");
}

#[test]
fn reload_destroys_the_generation_and_rebuilds_installed_engine() {
    let harness = Harness::new();
    assert!(harness.engine.is_installed());
    let engine = JsEngine::reload(harness.engine).expect("reload");
    assert_eq!(engine.generation(), 1);
    assert!(engine.is_installed());
    // prelude 在新代里重新安装。
    assert!(
        engine
            .eval_script::<bool>(
                "typeof __kiana === 'object' && typeof setTimeout === 'function'",
                "<reload>"
            )
            .expect("prelude after reload")
    );
    assert_eq!(
        engine.scoped_module_name("kiana:virtual/api"),
        "kiana:virtual/api?v=1-1"
    );
}

#[test]
fn reload_after_invalidate_keeps_old_generation_data_out() {
    let harness = Harness::new();
    let old_name = harness.engine.scoped_module_name("kiana:virtual/api");
    harness.engine.invalidate(STALE);
    assert_eq!(harness.engine.stale_message().as_deref(), Some(STALE));
    let engine = JsEngine::reload(harness.engine).expect("reload");
    assert_eq!(engine.generation(), 1);
    assert!(
        engine.stale_message().is_none(),
        "new generation has no stale flag"
    );
    assert_ne!(engine.scoped_module_name("kiana:virtual/api"), old_name);
}

#[test]
fn repeated_reloads_are_clean() {
    let mut engine = Harness::new().engine;
    for generation in 1..=10 {
        engine = JsEngine::reload(engine).expect("reload");
        assert_eq!(engine.generation(), generation);
    }
    assert!(engine.is_installed());
}

// ---------------------------------------------------------------------------
// 在飞行中的宿主任务与 JS 回调（§2.8 的硬约束回归）
// ---------------------------------------------------------------------------
//
// 宿主任务跨 `await` 持有 JS 回调时销毁引擎（drop/reload），以前的实现会触发
// `JS_FreeRuntime: Assertion 'list_empty(&rt->gc_obj_list)' failed` 并 abort 整个
// 进程：引擎清掉的只是自己持有的 `Persistent`，任务手里的那个还在 JS 堆里。
// 修复：发给宿主任务的 `HostCompletionSender` 锚住一份 runtime（`HostCallback`
// 保证先释放 JS 值再释放 runtime），引擎销毁不再 abort。

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use kiana_extensions::engine::HostCall;

/// 构造一个“捕获 payload 里的 JS 函数并交给 spawn_local 任务”的 host kind。
fn install_capture_kind<F>(engine: &JsEngine, body: F)
where
    F: for<'js> Fn(HostCall<'js>, rquickjs::Function<'js>) + 'static,
{
    engine.register_host_kind("capture", move |call: HostCall<'_>| {
        let function = call
            .payload()
            .clone()
            .into_function()
            .expect("payload is a JS function");
        body(call, function);
        Ok(())
    });
}

#[tokio::test(flavor = "current_thread")]
async fn drop_with_a_task_holding_a_captured_callback_is_clean() {
    run_local(async {
        let harness = Harness::new();
        let finished = Rc::new(Cell::new(false));
        let finished_in_task = Rc::clone(&finished);
        install_capture_kind(&harness.engine, move |call, function| {
            let callback = call.completion_sender().capture(function);
            let sender = call.completion_sender();
            let finished_in_task = Rc::clone(&finished_in_task);
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                // 引擎已销毁：发送失败，JS 值在 invoke 返回前已被释放。
                assert!(
                    !callback.invoke(vec![HostValue::Number(1.0)]),
                    "invoke after engine drop must fail"
                );
                assert!(sender.is_closed());
                drop(sender);
                drop(callback);
                finished_in_task.set(true);
            });
        });
        harness.eval::<()>("__kiana.promise('capture', () => 1);");
        assert_eq!(harness.engine.pending_count(), 1);

        // 关键断言：宿主任务还持有 JS 回调时销毁引擎，进程不得 abort。
        drop(harness.engine);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(finished.get(), "the host task must still finish cleanly");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn drop_with_a_task_holding_a_raw_persistent_and_sender_is_clean() {
    // 裸 `Persistent` + 锚住的发送端，任务按声明逆序（先回调、后发送端）释放：
    // 这是 `HostCompletionSender::capture` 之前各单元最自然的写法。
    run_local(async {
        let harness = Harness::new();
        let finished = Rc::new(Cell::new(false));
        let finished_in_task = Rc::clone(&finished);
        install_capture_kind(&harness.engine, move |call, function| {
            let ctx = call.ctx().clone();
            let callback: Persistent<Function<'static>> = Persistent::save(&ctx, function);
            let sender = call.completion_sender();
            let finished_in_task = Rc::clone(&finished_in_task);
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                // 发不出去（接收端已回收）；按逆序先释放 JS 值、再释放 runtime 锚。
                assert!(!sender.resolve(1, HostValue::Undefined));
                drop(callback);
                drop(sender);
                finished_in_task.set(true);
            });
        });
        harness.eval::<()>("__kiana.promise('capture', () => 1);");
        drop(harness.engine);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(finished.get());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn reload_with_a_task_holding_a_captured_callback_is_clean() {
    run_local(async {
        let harness = Harness::new();
        let finished = Rc::new(Cell::new(false));
        let finished_in_task = Rc::clone(&finished);
        install_capture_kind(&harness.engine, move |call, function| {
            let callback = call.completion_sender().capture(function);
            let finished_in_task = Rc::clone(&finished_in_task);
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                // 旧代已销毁：调用失败，但释放顺序安全。
                assert!(!callback.invoke(vec![HostValue::Undefined]));
                drop(callback);
                finished_in_task.set(true);
            });
        });
        harness.eval::<()>("__kiana.promise('capture', () => 1);");
        let engine = JsEngine::reload(harness.engine).expect("reload");
        assert_eq!(engine.generation(), 1);
        // 新代照常可用，旧代的任务随后安全结束。
        assert_eq!(
            engine
                .eval_script::<i32>("1 + 1", "<new-gen>")
                .expect("eval"),
            2
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(finished.get());
    })
    .await;
}
