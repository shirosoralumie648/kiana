//! 移植 `packages/coding-agent/test/codemode-worker-admission.test.ts`（5 条声明）。
//!
//! TS 测试 spy `Worker.prototype.terminate`/`once` 来断言“槽位只在 worker 真正退出时释放”。
//! Rust 的 worker 是 `std::thread`（terminate = 设置中断标志 + detach），没有可 spy 的
//! 对象；本文件用可观察的进程级计数
//! [`active_codemode_workers`](kiana_builtin_extensions::codemode::execute::active_codemode_workers)
//! 与真实执行结果断言同一语义：占满后到达顺序等待、等待受 `timeout_ms` 限制、释放后容量恢复。
//!
//! 两条断言在进程外不可观察，因此放在 `execute.rs` 的单元测试里（同一份生产闭包）：
//! - “结算不归还槽位、只有 worker 退出才 release”：
//!   `a_settled_script_keeps_its_slot_until_the_worker_exits`。实测 kiana-codemode 的 worker
//!   线程总在结果送达调用方之前就退出，外部计数无法制造 TS 用 mock terminate 拖住的窗口。
//! - FIFO 顺序：`admits_waiters_in_arrival_order` 用“先到者自旋、后到者立刻完成”区分
//!   FIFO 与任意/LIFO 唤醒。
//!
//! `std::env::set_var` 在 Rust 2024 是 `unsafe`，且测试并行运行；所有用例用进程级互斥锁
//! 串行化，并在结束时恢复环境变量。

mod codemode_support;

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_builtin_extensions::codemode::execute::{active_codemode_workers, execute_codemode};
use kiana_builtin_extensions::codemode::tool::{CodemodeToolOptions, create_codemode_tool};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_guard() -> MutexGuard<'static, ()> {
    let guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    clear_max_workers();
    guard
}

fn clear_max_workers() {
    // SAFETY: 所有读写 `KIANA_CODEMODE_MAX_WORKERS` 的测试都持有 `ENV_LOCK`。
    unsafe { std::env::remove_var("KIANA_CODEMODE_MAX_WORKERS") };
}

struct SetMaxWorkers;

impl SetMaxWorkers {
    fn new(value: &str) -> Self {
        // SAFETY: 调用方持有 `ENV_LOCK`，测试串行执行。
        unsafe { std::env::set_var("KIANA_CODEMODE_MAX_WORKERS", value) };
        SetMaxWorkers
    }
}

impl Drop for SetMaxWorkers {
    fn drop(&mut self) {
        clear_max_workers();
    }
}

fn run(
    code: &str,
    signal: Option<CancellationToken>,
) -> JoinHandle<Result<AgentToolResult<JsonValue>, kiana_agent_core::AgentToolError>> {
    let code = code.to_string();
    tokio::spawn(async move {
        execute_codemode(
            "worker-admission".to_string(),
            json!({"code": code}),
            signal,
            None,
            None,
            CodemodeToolOptions::default(),
        )
        .await
    })
}

async fn await_run(
    handle: JoinHandle<Result<AgentToolResult<JsonValue>, kiana_agent_core::AgentToolError>>,
) -> AgentToolResult<JsonValue> {
    tokio::time::timeout(Duration::from_secs(60), handle)
        .await
        .expect("script did not settle in 60s")
        .expect("script task panicked")
        .expect("execute_codemode returned an error")
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "condition was not met in 60s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_workers(expected: usize) {
    wait_until(|| active_codemode_workers() == expected).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 串行化环境变量与进程级 worker 计数，测试必须持锁等待。
async fn bounds_surviving_workers_after_timeout_abort_success() {
    let _guard = env_guard();
    for how in ["timeout", "abort", "success"] {
        let signal = CancellationToken::new();
        let code = match how {
            "timeout" => "// @options: {\"timeout_ms\": 200}\nwhile (true) {}",
            "abort" => "while (true) {}",
            _ => "return 1",
        };
        let handle = run(code, Some(signal.clone()));
        if how == "abort" {
            tokio::time::sleep(Duration::from_millis(200)).await;
            signal.cancel();
        }
        let result = await_run(handle).await;
        if how == "success" {
            assert_eq!(result.is_error, None);
        } else {
            assert_eq!(result.is_error, Some(true));
            assert!(
                codemode_support::has_text_containing(
                    &result,
                    if how == "timeout" {
                        "Script timed out:"
                    } else {
                        "Script aborted:"
                    },
                ),
                "{:?}",
                codemode_support::text_blocks(&result)
            );
        }
        // 取消/超时/成功三条路径都不得泄漏槽位（槽位在 worker 真正退出后释放）。
        wait_for_workers(0).await;
    }

    // 满载时脚本按到达顺序等待：先到的留在队列，后到且 timeout 更短的先失败。
    let _max = SetMaxWorkers::new("1");
    let holder_signal = CancellationToken::new();
    let holder = run("while (true) {}", Some(holder_signal.clone()));
    wait_for_workers(1).await;
    let waiting = run("return 3", None);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished(), "第二脚本不应在槽位释放前开跑");
    let timed_out = await_run(run("// @options: {\"timeout_ms\": 50}\nreturn 1", None)).await;
    assert_eq!(timed_out.is_error, Some(true));
    assert!(
        codemode_support::has_text_containing(
            &timed_out,
            "Execution timed out after 50 ms while waiting for a free codemode worker"
        ),
        "{:?}",
        codemode_support::text_blocks(&timed_out)
    );

    // 释放真正退出的 worker，而不是它的结果，恰好放行等待者。
    holder_signal.cancel();
    let _ = await_run(holder).await;
    let admitted = await_run(waiting).await;
    assert_eq!(admitted.is_error, None);
    assert!(codemode_support::has_text_containing(&admitted, "3"));
    wait_for_workers(0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn starts_no_worker_for_a_launch_that_resumes_after_its_result_timed_out() {
    let _guard = env_guard();
    // TS 用 `vi.spyOn(loadQuickJSWasm)` 让 wasm 延迟解析；Rust 的 wasm 编译在冻结的
    // `kiana-codemode` 后端里进行，无法注入。这里断言可观察结果：超时的启动不会留下常驻
    // worker，随后的脚本仍能正常执行。
    let results = futures::future::join_all(
        (0..3).map(|_| run("// @options: {\"timeout_ms\": 20}\nwhile (true) {}", None)),
    )
    .await;
    for result in results {
        let result = result.expect("task panicked").expect("execute error");
        assert_eq!(result.is_error, Some(true));
    }
    wait_for_workers(0).await;
    let ok = await_run(run("return 1", None)).await;
    assert_eq!(ok.is_error, None);
    wait_for_workers(0).await;
}

/// TS 用 mock 的 `Worker.terminate` 把 worker 的退出拖住，从而在每个释放周期断言
/// “恰好两个 worker、其余仍 pending”。Rust 的 worker 生命周期在 kiana-codemode 里，无法
/// mock；这里用不限时的自旋脚本占住槽位，再用各自的 token 逐个释放，达到同样的可观察效果：
/// 默认上限 2、按到达顺序、每次只放行一个。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn queues_a_concurrent_burst_behind_two_workers() {
    let _guard = env_guard();
    let tokens: Vec<CancellationToken> = (0..6).map(|_| CancellationToken::new()).collect();
    let mut handles: Vec<Option<JoinHandle<_>>> = tokens
        .iter()
        .map(|token| Some(run("while (true) {}", Some(token.clone()))))
        .collect();

    // 默认上限 2：前两个脚本占满槽位，其余四个排队。
    wait_for_workers(2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(active_codemode_workers(), 2);
    assert!(handles.iter().flatten().all(|handle| !handle.is_finished()));

    for released in 0..4 {
        tokens[released].cancel();
        let result = await_run(handles[released].take().expect("pending handle")).await;
        assert_eq!(result.is_error, Some(true));
        // 槽位只在 worker 真正退出后释放，随后恰好放行一个等待者。
        wait_for_workers(2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(active_codemode_workers(), 2);
        // 每次释放只放行一个：其余排队脚本仍未结算。
        let pending = handles
            .iter()
            .flatten()
            .filter(|handle| !handle.is_finished())
            .count();
        assert_eq!(pending, 5 - released);
    }

    tokens[4].cancel();
    tokens[5].cancel();
    for handle in handles.into_iter().flatten() {
        let result = await_run(handle).await;
        assert_eq!(result.is_error, Some(true));
    }
    wait_for_workers(0).await;
}

/// #11184：多个等待者时，槽位按**到达顺序**发放，而不是任意唤醒（TS 的 FIFO 队列断言；
/// 用 `Semaphore` 会退化成任意顺序）。
///
/// 观察方式：先到的是不限时的自旋脚本，后到的是会立刻“return 2”的脚本。上限 1 且占位者
/// 释放后，只有先到者能拿到槽位——后到者必须一直排队（`is_finished()` 为 false）；若准入
/// 变成 LIFO，后到者会先跑完。取消先到者后槽位转给后到者。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn admits_waiters_in_arrival_order() {
    let _guard = env_guard();
    let _max = SetMaxWorkers::new("1");
    let holder_signal = CancellationToken::new();
    let holder = run("while (true) {}", Some(holder_signal.clone()));
    wait_for_workers(1).await;

    let first_signal = CancellationToken::new();
    let first = run("while (true) {}", Some(first_signal.clone()));
    tokio::time::sleep(Duration::from_millis(150)).await;
    let second = run("return 2", None);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!first.is_finished(), "the first waiter must be queued");
    assert!(!second.is_finished(), "the second waiter must be queued");
    assert_eq!(active_codemode_workers(), 1);

    holder_signal.cancel();
    let _ = await_run(holder).await;

    // 释放的槽位归先到者：它开始自旋，后到者仍在排队。
    wait_for_workers(1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !second.is_finished(),
        "the second script must wait behind the first arrival"
    );
    assert_eq!(active_codemode_workers(), 1);

    // 先到者退出后才轮到后到者。
    first_signal.cancel();
    let first_result = await_run(first).await;
    assert_eq!(first_result.is_error, Some(true));
    let second_result = await_run(second).await;
    assert_eq!(second_result.is_error, None);
    assert!(codemode_support::has_text_containing(&second_result, "2"));
    wait_for_workers(0).await;
}

/// 被丢弃的 future（宿主取消，而非 token 取消）必须撤销排队；否则之后一次 admit 会把
/// 槽位记在一个没人接收的等待者上，永久泄漏。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn releases_a_queued_reservation_when_the_future_is_dropped() {
    let _guard = env_guard();
    let _max = SetMaxWorkers::new("1");
    let holder_signal = CancellationToken::new();
    let holder = run("while (true) {}", Some(holder_signal.clone()));
    wait_for_workers(1).await;

    let queued = run("return 1", None);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!queued.is_finished());
    queued.abort();
    match queued.await {
        Ok(_) => panic!("the queued task must be cancelled"),
        Err(error) => assert!(error.is_cancelled()),
    }

    holder_signal.cancel();
    let _ = await_run(holder).await;
    // 泄漏的槽位会让计数停在 1；被丢弃的 future 不得把它带进队列。
    wait_for_workers(0).await;

    // 泄漏的槽位还会让这个脚本等满 60 秒的准入上限；它必须立刻可运行。
    let ok = await_run(run("return 1", None)).await;
    assert_eq!(ok.is_error, None);
    wait_for_workers(0).await;
}

/// 准入阶段被 abort 时的文案：TS 取 `signal.reason.message`，Node 的裸 `abort()` 给的是
/// DOMException 的 "This operation was aborted"（不是 "Execution aborted" 兜底分支）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn reports_the_node_abort_reason_while_waiting_for_a_worker() {
    let _guard = env_guard();
    let _max = SetMaxWorkers::new("1");
    let holder_signal = CancellationToken::new();
    let holder = run("while (true) {}", Some(holder_signal.clone()));
    wait_for_workers(1).await;

    let queued_signal = CancellationToken::new();
    let queued = run("return 1", Some(queued_signal.clone()));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!queued.is_finished());
    queued_signal.cancel();
    let result = await_run(queued).await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        codemode_support::has_text_containing(
            &result,
            "Script aborted: This operation was aborted\n\nThe script did not start."
        ),
        "{:?}",
        codemode_support::text_blocks(&result)
    );
    assert_eq!(active_codemode_workers(), 1);

    holder_signal.cancel();
    let _ = await_run(holder).await;
    wait_for_workers(0).await;
}

/// `createCodemodeTool()` 没有会话（TS `ctx === undefined`）：即使 `models: true`，脚本也
/// 看不到 `models`。`wrap_tool_definition` 注入的占位上下文必须还原成 None。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn create_codemode_tool_without_a_session_does_not_declare_models() {
    let _guard = env_guard();
    let tool = create_codemode_tool(
        &[],
        CodemodeToolOptions {
            models: Some(true),
            ..CodemodeToolOptions::default()
        },
    );
    let result = tool
        .execute
        .execute(
            "direct",
            json!({"code": "return [typeof models, ALL_TOOLS.length];"}),
            None,
            None,
        )
        .await
        .expect("execute");
    assert_eq!(result.is_error, None);
    assert!(
        codemode_support::has_text_containing(&result, "[\"undefined\",0]"),
        "{:?}",
        codemode_support::text_blocks(&result)
    );
    wait_for_workers(0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn reads_the_limit_from_kiana_codemode_max_workers() {
    let _guard = env_guard();
    let _max = SetMaxWorkers::new("3");
    let tokens: Vec<CancellationToken> = (0..4).map(|_| CancellationToken::new()).collect();
    let handles: Vec<_> = tokens
        .iter()
        .map(|token| run("while (true) {}", Some(token.clone())))
        .collect();
    wait_for_workers(3).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(active_codemode_workers(), 3);
    for token in &tokens {
        token.cancel();
    }
    for handle in handles {
        let _ = await_run(handle).await;
    }
    wait_for_workers(0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 同上。
async fn ignores_an_invalid_kiana_codemode_max_workers() {
    let _guard = env_guard();
    let _max = SetMaxWorkers::new("0");
    let tokens: Vec<CancellationToken> = (0..3).map(|_| CancellationToken::new()).collect();
    let handles: Vec<_> = tokens
        .iter()
        .map(|token| run("while (true) {}", Some(token.clone())))
        .collect();
    wait_for_workers(2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(active_codemode_workers(), 2);
    for token in &tokens {
        token.cancel();
    }
    for handle in handles {
        let _ = await_run(handle).await;
    }
    wait_for_workers(0).await;
}

/// 额外覆盖（TS suite 的对应断言在 kiana-session 的 agent-session-codemode 测试里）：无会话上下文
/// 的脚本也走完整的输出整形流水线。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)] // 串行化环境变量与进程级 worker 计数。
async fn formats_script_output_and_failures() {
    let _guard = env_guard();
    let result = await_run(run(
        "text(\"one\"); console.log(\"log\"); return \"two\";",
        None,
    ))
    .await;
    assert_eq!(result.is_error, None);
    let blocks = codemode_support::text_blocks(&result);
    assert_eq!(blocks.len(), 2);
    assert!(
        blocks[0].starts_with("Script completed\nWall time ")
            && blocks[0].ends_with(" seconds\nOutput:\n"),
        "{}",
        blocks[0]
    );
    assert_eq!(
        blocks[1],
        "==> text 1/2 <==\none\n==> text 2/2 <==\ntwo\n<console_output>\nlog\n</console_output>"
    );

    let result = await_run(run("return 42", None)).await;
    assert_eq!(result.is_error, None);
    let blocks = codemode_support::text_blocks(&result);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1], "42");

    let result = await_run(run("throw new Error(\"boom\")", None)).await;
    assert_eq!(result.is_error, Some(true));
    let blocks = codemode_support::text_blocks(&result);
    assert!(blocks[0].starts_with("Script failed\nWall time "));
    assert!(blocks[1].starts_with("Script error:\n"));
    assert!(blocks[1].contains("boom"), "{}", blocks[1]);

    wait_for_workers(0).await;
}
