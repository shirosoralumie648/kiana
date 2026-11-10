//! 引擎基础：prelude 安装、唯一入口 `with_js`、限制/看门狗、错误物化、泄漏。
//!
//! 设计文档 §2.0–§2.7、§10.5；`ARCHITECTURE.md` §5 的 `engine/mod.rs` / `engine/limits.rs`。
//! TS 对应物：无（Node/jiti 由 V8 提供这些能力）。

mod support;

use std::time::{Duration, Instant};

use kiana_extensions::engine::{
    self, DEFAULT_MAX_STACK_SIZE, DEFAULT_MEMORY_LIMIT, DEFAULT_WATCHDOG, EngineConfig,
    FACTORY_DEADLINE, HANDLER_DEADLINE, HostCompletion, HostValue, JsEngine, JsErrorKind,
};
use rquickjs::{Ctx, Function, Object, Persistent};
use support::{Harness, catch_error};

/// 引擎升级的【逐字】错误文案基线（`tests/fixtures/engine/quickjs_messages.json`）。
fn engine_messages() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/engine/quickjs_messages.json"))
        .expect("quickjs_messages.json")
}

fn expected_message(section: &str) -> (String, String) {
    let fixture = engine_messages();
    (
        fixture[section]["name"].as_str().expect("name").to_owned(),
        fixture[section]["message"]
            .as_str()
            .expect("message")
            .to_owned(),
    )
}

// ---------------------------------------------------------------------------
// prelude 安装与全局面
// ---------------------------------------------------------------------------

#[test]
fn prelude_installs_protocol_timers_and_abort() {
    let harness = Harness::new();

    assert!(harness.eval::<bool>(
        "typeof __kiana === 'object' && typeof __kiana.promise === 'function' && typeof __kiana.settle === 'function'"
    ));
    assert!(harness.eval::<bool>("typeof __kiana.hostStart === 'function'"));
    assert!(harness.eval::<bool>(
        "typeof setTimeout === 'function' && typeof clearTimeout === 'function' && typeof setInterval === 'function' && typeof clearInterval === 'function'"
    ));
    assert!(harness.eval::<bool>(
        "typeof setImmediate === 'function' && typeof clearImmediate === 'function'"
    ));
    assert!(harness.eval::<bool>(
        "typeof AbortController === 'function' && typeof AbortSignal === 'function'"
    ));
    assert!(harness.eval::<bool>("typeof queueMicrotask === 'function'"));

    // 内部全局不可枚举（设计文档 §9.3）。
    assert!(harness.eval::<bool>(
        "Object.getOwnPropertyDescriptor(globalThis, '__kiana').enumerable === false"
    ));
    assert!(harness.eval::<bool>(
        "Object.getOwnPropertyDescriptor(globalThis, '__kiana_host').enumerable === false"
    ));
    assert!(
        !harness
            .engine
            .global_keys()
            .iter()
            .any(|key| key == "__kiana")
    );
    assert!(harness.engine.is_installed());

    // prelude 源是编译期 include_str! 进来的。
    assert!(engine::prelude_core_source().len() > 4_000);
}

#[test]
fn install_is_idempotent() {
    let harness = Harness::new();
    engine::install(&harness.engine).expect("second install");
    assert_eq!(harness.eval::<i32>("__kiana.seq === 0 ? 1 : 0"), 1);
}

#[test]
fn register_global_installs_values() {
    let harness = Harness::new();
    harness
        .engine
        .register_global("__answer", |ctx| {
            let object = Object::new(ctx.clone())?;
            object.set("value", 42)?;
            Ok(object.into_value())
        })
        .expect("register_global");
    assert_eq!(harness.eval::<i32>("__answer.value"), 42);
}

#[test]
fn module_source_registry_keeps_sources() {
    let harness = Harness::new();
    assert!(harness.engine.module_source("kiana:virtual/test").is_none());
    harness
        .engine
        .register_module_source("kiana:virtual/test", "export const value = 1;");
    assert_eq!(
        harness.engine.module_source("kiana:virtual/test"),
        Some("export const value = 1;")
    );
    let sources = harness.engine.module_sources();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].0, "kiana:virtual/test");
    assert_eq!(
        harness.engine.remove_module_source("kiana:virtual/test"),
        Some("export const value = 1;")
    );
    assert!(harness.engine.module_source("kiana:virtual/test").is_none());
}

#[test]
fn engine_exposes_runtime_and_a_loader_hook() {
    use rquickjs::Module;
    use rquickjs::loader::{ImportAttributes, Loader, Resolver};
    use rquickjs::module::Declared;

    // 回归（设计文档 §1.2 / §3.1 / §9.2 第 5 步）：`Runtime::set_loader` 是
    // rquickjs 0.14 唯一的模块解析钩子，`Ctx` 没有 setter，所以引擎必须把 runtime
    // 暴露给 loader 单元。
    #[derive(Default)]
    struct TestResolver;
    impl Resolver for TestResolver {
        fn resolve<'js>(
            &mut self,
            _ctx: &Ctx<'js>,
            _base: &str,
            name: &str,
            _attributes: Option<ImportAttributes<'js>>,
        ) -> rquickjs::Result<String> {
            Ok(name.to_owned())
        }
    }

    struct TestLoader;
    impl Loader for TestLoader {
        fn load<'js>(
            &mut self,
            ctx: &Ctx<'js>,
            name: &str,
            _attributes: Option<ImportAttributes<'js>>,
        ) -> rquickjs::Result<Module<'js, Declared>> {
            Module::declare(
                ctx.clone(),
                name,
                "export const value = 42; export default 'ok';",
            )
        }
    }

    let harness = Harness::new();
    // 只读 accessor 可用（与 set_loader 指向同一个 runtime）。
    let _runtime = harness.engine.runtime();
    harness.engine.set_loader(TestResolver, TestLoader);

    let (value, default): (i32, String) = harness
        .engine
        .with_js(|ctx| -> rquickjs::Result<(i32, String)> {
            let promise: rquickjs::Promise<'_> = ctx.eval("import('test:loader')")?;
            loop {
                if let Some(result) = promise.result::<Object<'_>>() {
                    let namespace = result?;
                    return Ok((namespace.get("value")?, namespace.get("default")?));
                }
                if !ctx.execute_pending_job() {
                    return Err(rquickjs::Exception::throw_message(
                        &ctx,
                        "dynamic import did not settle",
                    ));
                }
            }
        })
        .expect("dynamic import through the installed loader");
    assert_eq!(value, 42);
    assert_eq!(default, "ok");
}

// ---------------------------------------------------------------------------
// with_js / 重入
// ---------------------------------------------------------------------------

#[test]
fn nested_with_js_panics_with_refcell_borrow() {
    let harness = Harness::new();
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        harness.engine.with_js(|_ctx| harness.engine.with_js(|_| 1));
    }))
    .expect_err("nested with_js must panic");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| text.to_string()))
        .unwrap_or_default();
    assert!(
        message.contains("RefCell already borrowed"),
        "unexpected panic message: {message}"
    );
    // 展开后引擎仍然可用（RefCell 守卫在 unwind 时释放）。
    assert_eq!(harness.eval::<i32>("1 + 1"), 2);
}

#[test]
fn host_function_can_eval_and_pump_inside_a_transaction() {
    let harness = Harness::new();
    harness
        .engine
        .register_global("__nested", |ctx| {
            let function = Function::new(
                ctx.clone(),
                |ctx: Ctx<'_>, n: i64| -> rquickjs::Result<i64> {
                    let extra: i64 = ctx.eval(format!("({n} * 2)"))?;
                    let mut pumped = 0;
                    while ctx.execute_pending_job() {
                        pumped += 1;
                    }
                    Ok(extra + pumped as i64)
                },
            )?;
            Ok(function.into_value())
        })
        .expect("register_global");
    assert_eq!(harness.eval::<i64>("__nested(21)"), 42);
}

#[test]
fn js_to_host_to_js_reentrancy_depth_four() {
    let harness = Harness::new();
    harness
        .engine
        .register_global("__echo", |ctx| {
            // 宿主函数 → 调回 JS → 再进宿主函数，深度 4。
            let function = Function::new(
                ctx.clone(),
                |callback: Function<'_>, n: i64| -> rquickjs::Result<i64> {
                    let value: i64 = callback.call((n,))?;
                    Ok(value + 1)
                },
            )?;
            Ok(function.into_value())
        })
        .expect("register_global");
    let value: i64 =
        harness.eval("(function step(n) { if (n >= 4) return 0; return __echo(step, n + 1); })(0)");
    assert_eq!(value, 4);
}

// ---------------------------------------------------------------------------
// 限制 / 看门狗
// ---------------------------------------------------------------------------

#[test]
fn limits_have_the_documented_defaults() {
    assert_eq!(DEFAULT_MEMORY_LIMIT, 512 * 1024 * 1024);
    assert_eq!(DEFAULT_MAX_STACK_SIZE, 4 * 1024 * 1024);
    assert_eq!(DEFAULT_WATCHDOG, Duration::from_millis(100));
    assert_eq!(HANDLER_DEADLINE, Duration::from_secs(30));
    assert_eq!(FACTORY_DEADLINE, Duration::from_secs(10));
}

#[test]
fn watchdog_interrupts_infinite_loop_within_100ms() {
    let harness = Harness::new();
    let started = Instant::now();
    let error = harness.expect_error("for (;;) {}");
    let elapsed = started.elapsed();
    let (name, message) = expected_message("interrupt");
    assert_eq!(error.name, name);
    assert_eq!(error.message, message);
    // QuickJS 的 interrupt handler 在每个指令边界触发；实测约 100.3 ms（§2.7）。
    assert!(
        elapsed >= Duration::from_millis(50),
        "fired too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "fired too late: {elapsed:?}"
    );
    // 中断后引擎仍然可用。
    assert_eq!(harness.eval::<i32>("40 + 2"), 42);
}

#[test]
fn explicit_deadline_overrides_the_watchdog() {
    let harness = Harness::new();
    let started = Instant::now();
    let error = harness
        .engine
        .with_deadline(Duration::from_millis(250), || {
            harness.engine.with_js(|ctx| {
                ctx.eval::<(), _>("for (;;) {}")
                    .err()
                    .map(|_| catch_error(&ctx))
                    .expect("interrupt")
            })
        });
    let elapsed = started.elapsed();
    assert_eq!(error.message, "interrupted");
    assert!(
        elapsed >= Duration::from_millis(200),
        "explicit deadline must override the 100 ms watchdog: {elapsed:?}"
    );
}

#[test]
fn external_interrupt_flag_stops_the_next_transaction() {
    let harness = Harness::new();
    harness.engine.interrupt();
    assert!(harness.engine.is_interrupted());
    // QuickJS 只在指令边界（循环/调用）检查中断；小表达式可能跑完而不触发。
    let error = harness.expect_error("for (;;) {}");
    assert_eq!(error.message, "interrupted");
    harness.engine.clear_interrupt();
    assert!(!harness.engine.is_interrupted());
    assert_eq!(harness.eval::<i32>("1 + 1"), 2);
}

#[test]
fn out_of_memory_is_catchable_inside_js() {
    let mut config = EngineConfig::new();
    config.memory_limit = 8 * 1024 * 1024;
    let harness = Harness::with_config(config);
    // 一次巨大的分配请求：在 8 MiB 上限下立即失败，且此时堆里还有空间让 QuickJS
    // 分配 InternalError 对象（增量分配逼近上限时 QuickJS 会退回 `throw null`，
    // 见 quickjs.c 的 JS_ThrowError2）。
    harness.eval::<()>(
        r#"globalThis.__oom = "";
           try {
             new Array(50_000_000).fill(1);
           } catch (error) {
             __oom = String(error);
           }"#,
    );
    let text: String = harness.eval("__oom");
    let (name, message) = expected_message("out_of_memory");
    assert_eq!(text, format!("{name}: {message}"));
}

#[test]
fn out_of_memory_is_reported_when_uncaught() {
    let mut config = EngineConfig::new();
    config.memory_limit = 8 * 1024 * 1024;
    let harness = Harness::with_config(config);
    let error = harness.expect_error("new Array(50_000_000).fill(1);");
    let (name, message) = expected_message("out_of_memory");
    assert_eq!(error.name, name);
    assert_eq!(error.message, message);
}

#[test]
fn stack_overflow_reports_maximum_call_stack() {
    // QuickJS 的 4 MiB JS 栈需要在 >= 4 MiB 的原生线程栈上才能由引擎自己检测；
    // libtest 的测试线程默认栈较小，这里显式给 16 MiB（生产里主线程通常 8 MiB）。
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let harness = Harness::new();
            let error = harness.expect_error("(function recurse(n) { return recurse(n + 1); })(0)");
            assert_eq!(error.name, "RangeError");
            assert_eq!(error.message, "Maximum call stack size exceeded");
        })
        .expect("spawn stack test")
        .join()
        .expect("stack overflow test thread");
}

// ---------------------------------------------------------------------------
// 顶层 await / job 泵
// ---------------------------------------------------------------------------

#[test]
fn top_level_await_settles_in_two_job_rounds() {
    let harness = Harness::new();
    let rounds = harness.engine.with_js(|ctx| {
        let promise = ctx
            .eval_promise(
                "const a = await Promise.resolve(1);
                 const b = await Promise.resolve(2);
                 globalThis.__tla = a + b;",
            )
            .expect("eval_promise");
        let mut rounds = 0;
        while promise.state() == rquickjs::promise::PromiseState::Pending {
            if !ctx.execute_pending_job() {
                break;
            }
            rounds += 1;
        }
        assert_eq!(promise.state(), rquickjs::promise::PromiseState::Resolved);
        rounds
    });
    assert_eq!(rounds, 2);
    assert_eq!(harness.eval::<i32>("__tla"), 3);
}

#[test]
fn pump_jobs_swallows_throwing_jobs() {
    let harness = Harness::new();
    harness.eval::<()>("Promise.resolve().then(() => { throw new Error('boom'); });");
    // job 抛出的错误被 rquickjs 吞掉（§2.4），必须由 rejection tracker 兜底。
    assert!(harness.pump() >= 1);
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, JsErrorKind::UnhandledRejection);
    assert_eq!(errors[0].message, "boom");
}

// ---------------------------------------------------------------------------
// 错误队列
// ---------------------------------------------------------------------------

#[test]
fn unhandled_rejection_is_recorded_and_drained() {
    let harness = Harness::new();
    harness.eval::<()>("Promise.reject(new Error('rejected'));");
    harness.pump();
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, JsErrorKind::UnhandledRejection);
    assert_eq!(errors[0].name, "Error");
    assert_eq!(errors[0].message, "rejected");
    // V8 的 `error.stack` 首行是 `Name: message`（ca-extensions §4.9 的
    // `stack.split("\n").slice(1)` 依赖它）；QuickJS 的原始 stack 只有帧，
    // 引擎必须补齐首行。
    let stack = errors[0].stack.as_deref().expect("stack");
    assert!(
        stack.starts_with("Error: rejected\n"),
        "stack must start with the V8-style header: {stack:?}"
    );
    assert!(harness.take_errors().is_empty());
    assert_eq!(harness.engine.error_count(), 0);
}

#[test]
fn stack_header_uses_the_custom_error_name() {
    let harness = Harness::new();
    harness.eval::<()>(
        "const error = new Error('bad'); error.name = 'CustomError'; Promise.reject(error);",
    );
    harness.pump();
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    let stack = errors[0].stack.as_deref().expect("stack");
    assert!(
        stack.starts_with("CustomError: bad\n"),
        "unexpected stack: {stack:?}"
    );
    assert!(
        stack
            .lines()
            .nth(1)
            .is_some_and(|line| line.trim_start().starts_with("at ")),
        "the first real frame must follow the header: {stack:?}"
    );
}

#[test]
fn handled_rejection_is_not_reported() {
    let harness = Harness::new();
    harness.eval::<()>("const promise = Promise.reject(new Error('ok')); promise.catch(() => {});");
    harness.pump();
    assert!(
        harness.take_errors().is_empty(),
        "handled rejections must be removed"
    );
}

#[test]
fn non_error_rejection_uses_string_conversion() {
    let harness = Harness::new();
    harness.eval::<()>("Promise.reject('plain');");
    harness.pump();
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].name, "Error");
    assert_eq!(errors[0].message, "plain");
}

#[test]
fn record_error_reports_timer_kind() {
    let harness = Harness::new();
    harness.eval::<()>("__kiana.recordError('timer', new Error('timer failed'));");
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, JsErrorKind::Timer);
    assert_eq!(errors[0].message, "timer failed");
}

// ---------------------------------------------------------------------------
// 句柄 / 持久槽 / GC
// ---------------------------------------------------------------------------

#[test]
fn handles_round_trip_and_drop() {
    let harness = Harness::new();
    let handle = harness.engine.with_js(|ctx| {
        let object = Object::new(ctx.clone()).expect("object");
        object.set("value", 7).expect("set");
        harness.engine.store_handle(object)
    });
    assert_eq!(harness.engine.handle_count(), 1);
    let value = harness
        .engine
        .with_handle(handle, |object| object.get::<_, i32>("value"))
        .expect("with_handle")
        .expect("handle exists");
    assert_eq!(value, 7);
    assert!(harness.engine.drop_handle(handle));
    assert!(!harness.engine.drop_handle(handle));
    assert_eq!(harness.engine.handle_count(), 0);
    assert!(
        harness
            .engine
            .with_handle(handle, |_| Ok(()))
            .expect("missing handle is not an error")
            .is_none()
    );
}

#[test]
fn persistent_slots_round_trip() {
    let harness = Harness::new();
    let persistent: Persistent<Function<'static>> = harness.engine.with_js(|ctx| {
        let function: Function<'_> = ctx.eval("(() => 41 + 1)").expect("function");
        Persistent::save(&ctx, function)
    });
    harness.engine.put_persistent("callback", persistent);
    assert_eq!(harness.engine.persistent_slot_count(), 1);
    let persistent = harness
        .engine
        .take_persistent::<Function<'static>>("callback")
        .expect("slot");
    let value = harness.engine.with_js(|ctx| {
        let function = persistent.restore(&ctx).expect("restore");
        function.call::<_, i32>(()).expect("call")
    });
    assert_eq!(value, 42);
    assert_eq!(harness.engine.persistent_slot_count(), 0);
}

#[test]
fn thousand_render_cycles_keep_persistents_stable() {
    let harness = Harness::new();
    harness.eval::<()>("globalThis.__renders = 0;");
    let baseline = harness.engine.live_persistent_count();
    for _ in 0..1000 {
        let handle = harness.engine.with_js(|ctx| {
            let object = Object::new(ctx.clone()).expect("object");
            harness.engine.store_handle(object)
        });
        assert!(harness.engine.drop_handle(handle));
        harness.eval::<()>("__renders++");
    }
    harness.engine.run_gc();
    assert_eq!(harness.eval::<i32>("__renders"), 1000);
    assert_eq!(harness.engine.live_persistent_count(), baseline);
}

#[test]
fn run_gc_if_needed_runs_every_hundred_ticks() {
    let harness = Harness::new();
    let mut runs = 0;
    for _ in 0..250 {
        if harness.engine.run_gc_if_needed() {
            runs += 1;
        }
    }
    assert_eq!(runs, 2);
}

// ---------------------------------------------------------------------------
// 完成队列的发送端形状（promise 细节见 engine_promise.rs）
// ---------------------------------------------------------------------------

#[test]
fn completion_sender_accepts_raw_completions() {
    let harness = Harness::new();
    let sender = harness.engine.completion_sender();
    assert!(sender.send(HostCompletion::Settle {
        id: 1,
        ok: true,
        value: HostValue::String("x".to_owned()),
    }));
    assert_eq!(harness.engine.try_drain_completions(), 1);
}

#[test]
fn engine_config_next_generation_bumps_only_generation() {
    let config = EngineConfig::new();
    let next = config.next_generation();
    assert_eq!(next.generation, config.generation + 1);
    assert_eq!(next.memory_limit, config.memory_limit);
    assert_eq!(next.max_stack_size, config.max_stack_size);
    assert_eq!(next.watchdog, config.watchdog);
}

#[test]
fn engine_generation_is_visible() {
    let harness = Harness::new();
    assert_eq!(harness.engine.generation(), 0);
    let engine = JsEngine::with_config(EngineConfig::new().next_generation()).expect("engine");
    assert_eq!(engine.generation(), 1);
}

// ---------------------------------------------------------------------------
// 求值失败不污染上下文（设计文档 §2.2 的 `with_js` 契约）
// ---------------------------------------------------------------------------

#[test]
fn failed_eval_script_leaves_no_pending_exception() {
    let harness = Harness::new();
    for source in [
        "throw new Error('boom');",
        "throw 'plain';",
        "missingFunction();",
        "null.x;",
    ] {
        let error = harness
            .engine
            .eval_script::<()>(source, "<dirty>")
            .expect_err("eval must fail");
        assert!(
            matches!(error, rquickjs::Error::Exception),
            "expected Exception for {source}, got {error:?}"
        );
        assert!(
            !harness.engine.with_js(|ctx| ctx.has_exception()),
            "pending exception survived eval_script({source})"
        );
        // 后续求值照常工作；丢弃的异常不进错误队列（它不是未处理拒绝）。
        assert_eq!(harness.eval::<i32>("40 + 2"), 42);
        assert_eq!(harness.engine.error_count(), 0);
    }
    // 对照：eval_script_error 仍然拿得到逐字错误文本，并且同样清掉挂起异常。
    let error = harness.expect_error("throw new Error('boom');");
    assert_eq!(error.name, "Error");
    assert_eq!(error.message, "boom");
    assert!(!harness.engine.with_js(|ctx| ctx.has_exception()));
}

// ---------------------------------------------------------------------------
// queueMicrotask 错误不再被 execute_pending_job 吞掉（设计文档 §2.4）
// ---------------------------------------------------------------------------

#[test]
fn throwing_queue_microtask_is_recorded_in_the_error_queue() {
    let harness = Harness::new();
    harness.eval::<()>("queueMicrotask(() => { throw new Error('micro boom'); });");
    assert_eq!(harness.engine.pump_jobs(), 1);
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1, "a throwing queueMicrotask must not vanish");
    assert_eq!(errors[0].name, "Error");
    assert_eq!(errors[0].message, "micro boom");

    // Node 的 ERR_INVALID_ARG_TYPE 校验（setImmediate/nextTick 复用同一段）。
    let error = harness.expect_error("queueMicrotask(3);");
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "The \"callback\" argument must be of type function. Received type number (3)"
    );
    assert_eq!(
        harness.eval::<String>(
            "(function () { try { queueMicrotask(3); } catch (error) { return String(error.code); } })()"
        ),
        "ERR_INVALID_ARG_TYPE"
    );
    // 正常微任务仍按顺序执行（包装不改变 job 顺序）。
    harness.eval::<()>(
        "globalThis.__order = []; queueMicrotask(() => __order.push(1)); Promise.resolve().then(() => __order.push(2));",
    );
    harness.pump();
    assert_eq!(harness.eval::<String>("JSON.stringify(__order)"), "[1,2]");
}

// ---------------------------------------------------------------------------
// deadline 只作用于单次事务（设计文档 §2.7）
// ---------------------------------------------------------------------------

#[test]
fn per_transaction_deadline_does_not_leak_into_later_transactions() {
    let harness = Harness::new();
    let started = Instant::now();
    let error = harness
        .engine
        .with_js_deadline(Duration::from_millis(250), |ctx| {
            ctx.eval::<(), _>("for (;;) {}")
                .err()
                .map(|_| catch_error(&ctx))
                .expect("interrupt")
        });
    assert_eq!(error.message, "interrupted");
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "with_js_deadline must widen the cap for that transaction: {:?}",
        started.elapsed()
    );
    // 事务结束后 deadline 立刻恢复（不残留到主循环的其它事务）：
    // 这是“泄漏”的直接状态断言；主循环里其它 with_js 因此重新拿到 100 ms 看门狗
    // （默认看门狗本身由 watchdog_interrupts_infinite_loop_within_100ms 覆盖）。
    assert!(!harness.engine.has_deadline());
    assert_eq!(harness.eval::<i32>("40 + 2"), 42);
    assert!(!harness.engine.has_deadline());

    // 对照：显式 guard 仍然按引擎级语义生效，直到 drop。
    {
        let _guard = harness.engine.deadline_guard(Duration::from_millis(250));
        assert!(harness.engine.has_deadline());
    }
    assert!(!harness.engine.has_deadline());
}

#[test]
fn an_expired_ambient_deadline_does_not_interrupt_unrelated_transactions() {
    // 跨 `.await` 持有 DeadlineGuard 的误用：预算在 await 期间到期后，主循环里的
    // 无关事务（渲染等）不会被第一条指令打断，而是拿到自己的 100 ms 看门狗。
    let harness = Harness::new();
    harness.engine.set_deadline(Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(20));
    assert!(harness.engine.deadline_expired());
    let started = Instant::now();
    let error = harness.expect_error("for (;;) {}");
    let elapsed = started.elapsed();
    assert_eq!(error.message, "interrupted");
    assert!(
        elapsed >= Duration::from_millis(50),
        "a stale deadline interrupted an unrelated transaction immediately: {elapsed:?}"
    );
    harness.engine.clear_deadline();
    assert!(!harness.engine.has_deadline());
    assert_eq!(harness.eval::<i32>("1 + 1"), 2);
}
