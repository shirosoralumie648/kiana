//! 定时器与 AbortSignal（设计文档 §2.5、§2.6、§10.4 第 4 条、§10.5）。
//!
//! TS 对应物：Node 的 `setTimeout`/`setInterval`/`setImmediate`/`process.nextTick`、
//! 全局 `AbortController`/`AbortSignal`。关键回归：`setTimeout` **必须真的等 delay**
//! （`@anthropic-ai/sdk` 的 `fetchWithTimeout` 事故，§2.5）。

mod support;

use std::time::Duration;

use kiana_extensions::engine::{HostCall, HostValue, JsErrorKind};
use support::{Harness, TEST_TIMEOUT, run_local};

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_timeout_waits_and_is_not_a_microtask() {
    run_local(async {
        let harness = Harness::new();
        harness
            .eval::<()>("globalThis.__fired = false; setTimeout(() => { __fired = true; }, 10);");
        harness.pump();
        assert!(
            !harness.eval::<bool>("__fired"),
            "setTimeout must not run as a microtask (design §2.5)"
        );
        assert_eq!(harness.engine.timer_count(), 1);
        // 不驱动主循环、只看当前时刻：还没到期。
        assert_eq!(harness.engine.fire_due_timers(), 0);
        assert!(!harness.eval::<bool>("__fired"));
        assert!(harness.settle_all().await);
        assert!(harness.eval::<bool>("__fired"));
        assert_eq!(harness.engine.timer_count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timers_fire_in_deadline_order_and_clear_timeout_removes() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__log = [];
               const a = setTimeout(() => __log.push("a"), 5);
               const b = setTimeout(() => __log.push("b"), 10);
               const c = setTimeout(() => __log.push("c"), 15);
               clearTimeout(b);
               globalThis.__ids = [a, b, c];"#,
        );
        assert_eq!(
            harness.engine.timer_count(),
            2,
            "clearTimeout must remove the timer"
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__log)"),
            "[\"a\",\"c\"]"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn interval_repeats_until_cleared_inside_callback() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__ticks = 0;
               globalThis.__interval = setInterval(() => {
                 __ticks += 1;
                 if (__ticks === 3) clearInterval(__interval);
               }, 5);"#,
        );
        assert_eq!(harness.engine.timer_count(), 1);
        assert!(harness.settle_all().await);
        assert_eq!(harness.eval::<i32>("__ticks"), 3);
        assert_eq!(harness.engine.timer_count(), 0);
    })
    .await;
}

#[test]
fn set_immediate_and_next_tick_map_to_microtasks() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__order = [];
           setImmediate(() => __order.push("immediate"));
           queueMicrotask(() => __order.push("microtask"));
           Promise.resolve().then(() => __order.push("promise"));
           __kiana.nextTick(() => __order.push("nextTick"));"#,
    );
    assert_eq!(harness.eval::<i32>("__order.length"), 0);
    harness.pump();
    // 设计文档 §2.5 把 `setImmediate`/`process.nextTick` 映射到 `queueMicrotask`，所以
    // 顺序 == 入队顺序（setImmediate 在 Node 里是 check 阶段的宏任务，这里有意近似；
    // 扩展语料不依赖该差异）。
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__order)"),
        "[\"immediate\",\"microtask\",\"promise\",\"nextTick\"]"
    );
}

#[test]
fn clear_immediate_cancels_before_the_microtask_runs() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__fired = false;
           globalThis.__immediate = setImmediate(() => { __fired = true; });
           clearImmediate(__immediate);"#,
    );
    harness.pump();
    assert!(!harness.eval::<bool>("__fired"));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn delay_normalization_matches_node() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__order = [];
               setTimeout(() => __order.push("negative"), -5);
               setTimeout(() => __order.push("default"));
               setTimeout(() => __order.push("fraction"), 1.9);"#,
        );
        // Node 的 `getTimerDuration`：非 [1, TIMEOUT_MAX] 的 delay 一律 1 ms。
        assert_eq!(harness.engine.timer_count(), 3);
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__order)"),
            "[\"negative\",\"default\",\"fraction\"]"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// 定时器批次语义（每个回调之后泵微任务、看门狗按定时器计）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn microtasks_run_between_timer_callbacks() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__log = [];
               setTimeout(async () => { __log.push("a1"); await null; __log.push("a2"); }, 1);
               setTimeout(() => __log.push("b"), 1);
               setTimeout(() => __log.push("c"), 2);"#,
        );
        assert!(harness.settle_all().await);
        // Node 26: 回调 → 微任务 → 下一个定时器。
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__log)"),
            "[\"a1\",\"a2\",\"b\",\"c\"]"
        );
    })
    .await;
}

#[test]
fn slow_timeout_does_not_suppress_other_due_timers_and_is_recorded() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__log = [];
           setTimeout(() => {
             const started = Date.now();
             while (Date.now() - started < 150) {}
             __log.push("a");
           }, 1);
           setTimeout(() => __log.push("b"), 1);"#,
    );
    std::thread::sleep(Duration::from_millis(20));
    // 每个定时器一次独立事务：a 的看门狗中断只影响 a，b 仍然运行。
    assert_eq!(harness.engine.fire_due_timers(), 2);
    assert_eq!(harness.eval::<String>("JSON.stringify(__log)"), "[\"b\"]");
    assert_eq!(harness.engine.timer_count(), 0);
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1, "the overrun must be surfaced");
    assert_eq!(errors[0].kind, JsErrorKind::Timer);
    assert_eq!(errors[0].name, "InternalError");
    assert_eq!(errors[0].message, "interrupted");
}

#[test]
fn slow_interval_callback_is_recorded_and_the_interval_survives() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__log = [];
           globalThis.__ticks = 0;
           globalThis.__interval = setInterval(() => {
             __ticks += 1;
             __log.push("tick" + __ticks);
             if (__ticks === 1) {
               const started = Date.now();
               while (Date.now() - started < 150) {}
             }
           }, 5);"#,
    );
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(harness.engine.fire_due_timers(), 1);
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__log)"),
        "[\"tick1\"]"
    );
    // Node 不会因为一次慢回调杀死 interval；看门狗只中断这一拍。
    assert_eq!(harness.engine.timer_count(), 1, "interval must stay armed");
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, JsErrorKind::Timer);
    assert_eq!(errors[0].message, "interrupted");
    // 清理，避免后续驱动继续触发。
    harness.eval::<()>("clearInterval(__interval);");
    assert_eq!(harness.engine.timer_count(), 0);
}

#[test]
fn next_tick_callback_errors_are_recorded_like_set_immediate() {
    let harness = Harness::new();
    harness.eval::<()>("__kiana.nextTick(() => { throw new Error('nextTick boom'); });");
    harness.pump();
    let errors = harness.take_errors();
    assert_eq!(errors.len(), 1, "a throwing nextTick must not vanish");
    assert_eq!(errors[0].kind, JsErrorKind::Timer);
    assert_eq!(errors[0].message, "nextTick boom");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_timeout_returns_a_node_compatible_timeout_handle() {
    run_local(async {
        let harness = Harness::new();
        assert_timeout_handle_shape(&harness);
        assert!(!harness.eval::<bool>("__hasRef"));
        assert!(harness.settle_all().await);
        assert_eq!(harness.eval::<i32>("__fired"), 1);
        // refresh 重新激活已触发的定时器（Node 语义）。
        harness.eval::<()>("__t.refresh();");
        assert_eq!(harness.engine.timer_count(), 1);
        assert!(harness.settle_all().await);
        assert_eq!(harness.eval::<i32>("__fired"), 2);
        // clearTimeout 之后的 refresh 不得重新激活。
        harness.eval::<()>("clearTimeout(__t); __t.refresh();");
        assert_eq!(harness.engine.timer_count(), 0);
        assert!(harness.settle_all().await);
        assert_eq!(harness.eval::<i32>("__fired"), 2);
    })
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn refresh_inside_the_callback_reactivates_a_one_shot_timer() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__fired = 0;
               globalThis.__t = setTimeout(() => {
                 __fired += 1;
                 if (__fired === 1) __t.refresh();
               }, 5);"#,
        );
        assert!(harness.settle_all().await);
        // Node 26: refresh() 在回调里重新激活一次性定时器 → 共触发 2 次。
        assert_eq!(harness.eval::<i32>("__fired"), 2);
        assert_eq!(harness.engine.timer_count(), 0);
    })
    .await;
}

fn assert_timeout_handle_shape(harness: &Harness) {
    harness.eval::<()>(
        r#"globalThis.__fired = 0;
           globalThis.__t = setTimeout(() => { __fired += 1; }, 5);
           globalThis.__shape = [
             typeof __t,
             typeof __t.unref,
             typeof __t.ref,
             typeof __t.refresh,
             typeof __t.hasRef,
             typeof __t[Symbol.toPrimitive],
             Number(__t) > 0,
             String(__t) === String(Number(__t)),
           ];
           __t.unref();
           globalThis.__hasRef = __t.hasRef();
           __t.ref();"#,
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__shape)"),
        "[\"object\",\"function\",\"function\",\"function\",\"function\",\"function\",true,true]"
    );
}

// ---------------------------------------------------------------------------
// AbortController / AbortSignal
// ---------------------------------------------------------------------------

#[test]
fn abort_signal_aborts_listeners_and_stale_registrations() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__log = [];
           const controller = new AbortController();
           const signal = controller.signal;
           __log.push(signal.aborted);
           signal.addEventListener("abort", () => __log.push("sync:" + signal.reason.name));
           controller.abort();
           __log.push(signal.aborted);
           signal.addEventListener("abort", () => __log.push("late"));
           try { signal.throwIfAborted(); } catch (error) { __log.push("threw:" + error.name); }"#,
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__log)"),
        "[false,\"sync:AbortError\",true,\"threw:AbortError\"]"
    );
    // abort 之后注册的监听器必须在微任务里触发（§2.6 的挂起 bug 回归）。
    harness.pump();
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__log)"),
        "[false,\"sync:AbortError\",true,\"threw:AbortError\",\"late\"]"
    );
}

#[test]
fn abort_signal_any_combines_and_preserves_reason() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__any = null;
           globalThis.__early = null;
           const a = new AbortController();
           const b = new AbortController();
           const combined = AbortSignal.any([a.signal, b.signal]);
           combined.addEventListener("abort", () => { __any = combined.reason; });
           b.abort("stop");
           const pre = AbortSignal.abort("early");
           const again = AbortSignal.any([pre]);
           __early = [again.aborted, again.reason];"#,
    );
    assert_eq!(harness.eval::<String>("__any"), "stop");
    assert!(harness.eval::<bool>("__early[0] === true && __early[1] === 'early'"));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn abort_signal_timeout_aborts_with_timeout_error() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__timeout = null;
               const signal = AbortSignal.timeout(5);
               signal.addEventListener("abort", () => {
                 __timeout = [signal.reason.name, signal.reason.code, signal.reason.message];
               });"#,
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__timeout)"),
            "[\"TimeoutError\",23,\"The operation was aborted due to timeout\"]"
        );
    })
    .await;
}

#[test]
fn abort_signal_timeout_validates_delay_like_node() {
    let harness = Harness::new();
    let error = harness.expect_error("AbortSignal.timeout(-1);");
    assert_eq!(error.name, "RangeError");
    assert_eq!(
        error.message,
        "The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received -1"
    );
    let error = harness.expect_error("AbortSignal.timeout(1.5);");
    assert_eq!(error.name, "RangeError");
    assert_eq!(
        error.message,
        "The value of \"delay\" is out of range. It must be an integer. Received 1.5"
    );
    let error = harness.expect_error("AbortSignal.timeout(NaN);");
    assert_eq!(
        error.message,
        "The value of \"delay\" is out of range. It must be an integer. Received NaN"
    );
    let error = harness.expect_error("AbortSignal.timeout('5');");
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        error.message,
        "The \"delay\" argument must be of type number. Received type string ('5')"
    );
    let error = harness.expect_error("AbortSignal.timeout(null);");
    assert_eq!(
        error.message,
        "The \"delay\" argument must be of type number. Received null"
    );
    let error = harness.expect_error("AbortSignal.timeout(4294967296);");
    assert_eq!(
        error.message,
        "The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 4294967296"
    );
    // 上限本身合法；> 2^31-1 由 setTimeout 归一化成 1 ms（与 Node 相同）。
    assert!(harness.eval::<bool>("AbortSignal.timeout(4294967295).aborted === false"));
}

#[test]
fn abort_signal_dedupes_identical_listeners() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__count = 0;
           const controller = new AbortController();
           const listener = () => { __count += 1; };
           controller.signal.addEventListener("abort", listener);
           controller.signal.addEventListener("abort", listener);
           controller.abort();"#,
    );
    assert_eq!(harness.eval::<i32>("__count"), 1);
}

#[test]
fn abort_reason_is_a_dom_exception_with_the_node_code() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__reason = {};
           const controller = new AbortController();
           controller.abort();
           const reason = controller.signal.reason;
           __reason.constructor = reason.constructor.name;
           __reason.name = reason.name;
           __reason.code = reason.code;
           __reason.isError = reason instanceof Error;
           __reason.isDomException = reason instanceof DOMException;
           __reason.text = String(reason);
           __reason.tag = Object.prototype.toString.call(reason);
           __reason.keys = Object.keys(reason);"#,
    );
    assert_eq!(
        harness.eval::<String>("__reason.constructor"),
        "DOMException"
    );
    assert_eq!(harness.eval::<String>("__reason.name"), "AbortError");
    assert_eq!(harness.eval::<i32>("__reason.code"), 20);
    assert!(harness.eval::<bool>("__reason.isError"));
    assert!(harness.eval::<bool>("__reason.isDomException"));
    assert_eq!(
        harness.eval::<String>("__reason.text"),
        "AbortError: This operation was aborted"
    );
    assert_eq!(
        harness.eval::<String>("__reason.tag"),
        "[object DOMException]"
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__reason.keys)"),
        "[]"
    );
    // 显式 reason 原样保留；未知 DOMException 名的 code 是 0。
    harness.eval::<()>(
        r#"const explicit = new AbortController();
           explicit.abort("why");
           globalThis.__explicit = explicit.signal.reason;
           globalThis.__unknownCode = new DOMException("m", "NoSuchError").code;"#,
    );
    assert_eq!(harness.eval::<String>("__explicit"), "why");
    assert_eq!(harness.eval::<i32>("__unknownCode"), 0);
}

// ---------------------------------------------------------------------------
// 设计文档 §10.5 的组合验收
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn seven_timers_and_promise_chain_finish_within_31ms_virtual() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__events = [];
               globalThis.__values = null;
               const delays = [1, 2, 3, 4, 5, 6, 7];
               Promise.all(delays.map((delay, index) => new Promise(resolve => {
                 setTimeout(() => { __events.push(index); resolve(index); }, delay);
               }))).then(values => { __values = values; });"#,
        );
        assert_eq!(harness.engine.timer_count(), 7);
        let started = tokio::time::Instant::now();
        assert!(harness.settle_all().await);
        let elapsed = started.elapsed();
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            "[0,1,2,3,4,5,6]"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__values)"),
            "[0,1,2,3,4,5,6]"
        );
        assert_eq!(harness.engine.timer_count(), 0);
        // paused 时钟下总耗时 == 最大 delay + 一次空闲探测，远小于设计文档的 31 ms 目标。
        assert!(
            elapsed < Duration::from_millis(31),
            "virtual elapsed {elapsed:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn render_during_a_43ms_host_await() {
    run_local(async {
        let harness = Harness::new();
        harness
            .engine
            .register_host_kind("sleep", |call: HostCall<'_>| {
                let milliseconds: f64 = call.payload_json()?;
                let id = call.id();
                let sender = call.completion_sender();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(Duration::from_millis(milliseconds as u64)).await;
                    sender.resolve(id, HostValue::String("done".to_owned()));
                });
                Ok(())
            });
        harness.eval::<()>(
            r#"globalThis.__frames = 0;
               globalThis.__done = null;
               __kiana.promise("sleep", 43).then(value => { __done = value; });"#,
        );
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_millis(21)).await;
            harness.eval::<()>("__frames++");
            assert!(
                harness.eval::<bool>("__done === null"),
                "host promise must still be pending while rendering"
            );
        }
        assert!(
            harness
                .drive_until(TEST_TIMEOUT, |h| h.eval::<bool>("__done !== null"))
                .await
        );
        assert_eq!(harness.eval::<i32>("__frames"), 2);
        assert_eq!(harness.eval::<String>("__done"), "done");
    })
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timer_callback_error_is_recorded_not_thrown_into_the_loop() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>("setTimeout(() => { throw new Error('timer boom'); }, 1);");
        assert!(harness.settle_all().await);
        let errors = harness.take_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, JsErrorKind::Timer);
        assert_eq!(errors[0].message, "timer boom");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Node 26 的定时器/AbortSignal 表面（§2.5、§2.6）
// ---------------------------------------------------------------------------

#[test]
fn timer_callback_validation_matches_node_error_text_and_code() {
    let harness = Harness::new();
    for (source, received) in [
        ("setTimeout('x', 1);", "type string ('x')"),
        ("setTimeout(3, 1);", "type number (3)"),
        ("setTimeout(NaN, 1);", "type number (NaN)"),
        ("setTimeout(null, 1);", "null"),
        ("setTimeout(undefined, 1);", "undefined"),
        ("setInterval({}, 1);", "an instance of Object"),
        ("setInterval([], 1);", "an instance of Array"),
        ("setTimeout(true, 1);", "type boolean (true)"),
        ("setImmediate(1);", "type number (1)"),
        ("__kiana.nextTick(2);", "type number (2)"),
        ("queueMicrotask(3);", "type number (3)"),
    ] {
        let error = harness.expect_error(source);
        assert_eq!(error.name, "TypeError", "{source}");
        assert_eq!(
            error.message,
            format!("The \"callback\" argument must be of type function. Received {received}"),
            "{source}"
        );
        let code: String = harness.eval(&format!(
            "(function () {{ try {{ {source} }} catch (error) {{ return String(error.code); }} }})()"
        ));
        assert_eq!(code, "ERR_INVALID_ARG_TYPE", "{source}");
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn set_immediate_returns_a_node_compatible_handle() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__fired = 0;
               globalThis.__i = setImmediate(function (value) {
                 __fired += 1;
                 globalThis.__immediateThis = [this === __i, this.constructor.name, value];
               }, 7);
               globalThis.__shape = [
                 typeof __i,
                 __i.constructor.name,
                 typeof __i.ref,
                 typeof __i.unref,
                 typeof __i.hasRef,
                 __i.hasRef(),
                 Number.isNaN(Number(__i)),
               ];
               __i.unref();
               globalThis.__unrefHasRef = __i.hasRef();
               __i.ref();"#,
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__shape)"),
            "[\"object\",\"Immediate\",\"function\",\"function\",\"function\",true,true]"
        );
        assert!(!harness.eval::<bool>("__unrefHasRef"));
        assert!(harness.settle_all().await);
        assert_eq!(harness.eval::<i32>("__fired"), 1);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__immediateThis)"),
            "[true,\"Immediate\",7]"
        );
        // clearImmediate 接受句柄；已取消的句柄不再执行。
        harness.eval::<()>(
            "globalThis.__i2 = setImmediate(() => { __fired += 10; }); clearImmediate(__i2);",
        );
        harness.pump();
        assert_eq!(harness.eval::<i32>("__fired"), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_callbacks_bind_this_to_the_handle() {
    run_local(async {
        let harness = Harness::new();
        harness.eval::<()>(
            r#"globalThis.__timeoutLog = null;
               globalThis.__intervalLog = null;
               globalThis.__t = setTimeout(function (a, b) {
                 __timeoutLog = [this === __t, this.constructor.name, a, b, this._idleTimeout];
               }, 3, "x", "y");
               globalThis.__iv = setInterval(function () {
                 __intervalLog = [this === __iv, this.constructor.name];
                 clearInterval(__iv);
               }, 3);"#,
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__timeoutLog)"),
            "[true,\"Timeout\",\"x\",\"y\",3]"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__intervalLog)"),
            "[true,\"Timeout\"]"
        );
    })
    .await;
}

#[test]
fn fractional_timer_delays_are_kept_on_the_handle() {
    let harness = Harness::new();
    // Node 保留 [1, TIMEOUT_MAX] 内的分数（`_idleTimeout === 1.9`），不 trunc。
    let delay = harness.eval::<f64>(
        "(function () { const t = setTimeout(() => {}, 1.9); const delay = t._idleTimeout; clearTimeout(t); return delay; })()",
    );
    assert_eq!(delay, 1.9);
}

#[test]
fn abort_signal_onabort_matches_event_target() {
    let harness = Harness::new();
    harness.eval::<()>(
        r#"globalThis.__log = [];
           const c = new AbortController();
           c.signal.onabort = () => __log.push("first");
           c.signal.onabort = function (event) {
             __log.push("second:" + event.type + ":" + (this === c.signal));
           };
           c.abort();
           globalThis.__onabortType = typeof c.signal.onabort;
           const c2 = new AbortController();
           c2.signal.onabort = () => __log.push("never");
           c2.signal.onabort = null;
           c2.abort();
           globalThis.__afterNull = c2.signal.onabort;
           // abort 之后赋值：Node 在微任务里投递。
           const c3 = new AbortController();
           c3.abort();
           c3.signal.onabort = () => __log.push("late");
           queueMicrotask(() => __log.push("micro"));"#,
    );
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__log)"),
        "[\"second:abort:true\"]"
    );
    assert_eq!(harness.eval::<String>("__onabortType"), "function");
    assert!(harness.eval::<bool>("__afterNull === null"));
    harness.pump();
    assert_eq!(
        harness.eval::<String>("JSON.stringify(__log)"),
        "[\"second:abort:true\",\"late\",\"micro\"]"
    );
}

#[test]
fn abort_signal_constructor_and_any_validate_like_node() {
    let harness = Harness::new();
    // `new AbortSignal()` 非法（含子类调用 super）。
    for source in [
        "new AbortSignal();",
        "new (class extends AbortSignal { constructor() { super(); } })();",
    ] {
        let error = harness.expect_error(source);
        assert_eq!(error.name, "TypeError", "{source}");
        assert_eq!(error.message, "Illegal constructor", "{source}");
    }
    assert_eq!(
        harness.eval::<String>(
            "(function () { try { new AbortSignal(); } catch (error) { return String(error.code); } })()"
        ),
        "ERR_ILLEGAL_CONSTRUCTOR"
    );

    for (source, message) in [
        (
            "AbortSignal.any();",
            "signals cannot be converted to sequence.",
        ),
        (
            "AbortSignal.any(undefined);",
            "signals cannot be converted to sequence.",
        ),
        (
            "AbortSignal.any(null);",
            "signals cannot be converted to sequence.",
        ),
        (
            "AbortSignal.any(3);",
            "signals cannot be converted to sequence.",
        ),
        (
            "AbortSignal.any({});",
            "signals cannot be converted to sequence.",
        ),
        (
            "AbortSignal.any('abc');",
            "signals cannot be converted to sequence.",
        ),
        (
            "AbortSignal.any([null]);",
            "signals[0] is not of type AbortSignal.",
        ),
        (
            "AbortSignal.any([undefined]);",
            "signals[0] is not of type AbortSignal.",
        ),
        (
            "AbortSignal.any([{}]);",
            "signals[0] is not of type AbortSignal.",
        ),
        (
            "AbortSignal.any([new AbortController().signal, 5]);",
            "signals[1] is not of type AbortSignal.",
        ),
    ] {
        let error = harness.expect_error(source);
        assert_eq!(error.name, "TypeError", "{source}");
        assert_eq!(error.message, message, "{source}");
    }
    assert_eq!(
        harness.eval::<String>(
            "(function () { try { AbortSignal.any([null]); } catch (error) { return String(error.code); } })()"
        ),
        "ERR_INVALID_ARG_TYPE"
    );
    assert!(harness.eval::<bool>("AbortSignal.any([]).aborted === false"));
}

#[test]
fn abort_signal_and_controller_are_non_enumerable_globals() {
    let harness = Harness::new();
    for global in ["AbortController", "AbortSignal", "DOMException"] {
        assert!(
            !harness.eval::<bool>(&format!("Object.keys(globalThis).includes('{global}')")),
            "{global} must be non-enumerable like Node"
        );
        assert!(
            harness.eval::<bool>(&format!(
                "Object.getOwnPropertyDescriptor(globalThis, '{global}').enumerable === false"
            )),
            "{global} descriptor must be enumerable:false"
        );
    }
    // Node 的定时器全局是可枚举的（与 Abort* 相反）。
    for global in ["setTimeout", "setInterval", "queueMicrotask"] {
        assert!(
            harness.eval::<bool>(&format!("Object.keys(globalThis).includes('{global}')")),
            "{global} must be enumerable like Node"
        );
    }
}

#[test]
fn abort_timeout_validation_carries_node_error_codes() {
    let harness = Harness::new();
    for (source, expected) in [
        ("AbortSignal.timeout(-1);", "ERR_OUT_OF_RANGE"),
        ("AbortSignal.timeout(1.5);", "ERR_OUT_OF_RANGE"),
        ("AbortSignal.timeout('5');", "ERR_INVALID_ARG_TYPE"),
    ] {
        let code: String = harness.eval(&format!(
            "(function () {{ try {{ {source} }} catch (error) {{ return String(error.code); }} }})()"
        ));
        assert_eq!(code, expected, "{source}");
    }
}

#[test]
fn node26_timer_and_abort_surface_matches_the_golden_fixture() {
    // `fixtures/engine/node26_surface.js` 是同一段源码：Node 26 用
    // `vm.runInThisContext` 跑出的结果存进 `node26_surface.node26.json`；引擎跑同一段
    // 必须逐字一致，否则可观测行为会与 Node 26 偏差（全局枚举性、参数校验文案+codes、AbortSignal 表面、句柄形状）。
    let harness = Harness::new();
    let source = include_str!("fixtures/engine/node26_surface.js");
    let text = harness.eval::<String>(source);
    let actual: serde_json::Value = serde_json::from_str(&text).expect("engine output JSON");
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/engine/node26_surface.node26.json"))
            .expect("golden JSON");
    assert_eq!(actual, expected);
}
