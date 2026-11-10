//! `packages/codemode/test/deterministic.test.ts` 的移植（spec §7.8）。
//!
//! `start()` 用一个由调用方回答调用的确定性 run 跑脚本，使 durable 宿主可以在重启后重放。
//! 每个测试在 worker 后端（Node）与 remote 后端（Cloudflare）上各跑一次——后者对每条消息做
//! `structuredClone`，并把第一个 `seq === 1` 的回答延迟 30 ms 以打乱到达顺序。
//!
//! 需要嵌入的 `quickjs.wasm`，因此整个文件在 feature `wasm` 关闭时为空。

#![cfg(feature = "wasm")]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use kiana_codemode::runtime::execution::CodemodeSandboxBase;
use kiana_codemode::runtime::host::CodemodeSandbox;
use kiana_codemode::runtime::protocol::HostToWorkerMessage;
use kiana_codemode::runtime::remote::{
    CodemodeRemote, CodemodeRemoteExchange, CodemodeRemoteReply, CodemodeRemoteRequest,
    CodemodeServeOptions, RemoteCodemodeSandbox, RemoteCodemodeSandboxOptions,
    serve_codemode_remote,
};
use kiana_codemode::types::{
    BoxFuture, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeErrorKind, CodemodeHostCall,
    CodemodeOutcome, CodemodeOutcomeErr, CodemodeOutcomeOk, CodemodeResult, CodemodeResultOk,
    CodemodeRun, CodemodeRunOptions, CodemodeSandboxBaseOptions, CodemodeSandboxOptions,
};
use kiana_codemode::wasm::{CodemodeWasmModule, load_quickjs_wasm};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// 远端的中断预算（TS: `const BUDGET = 50_000`）。
const BUDGET: u32 = 50_000;

/// `Date.UTC(2026, 0, 15, 10, 0, 0)`。
const STARTED_AT: f64 = 1_768_471_200_000.0;

/// seed 0 的前两个 `Math.random()`（TS: `PINNED_SEED_ZERO`）。
const PINNED_SEED_ZERO: [f64; 2] = [0.671_137_253_026_676_4, 0.725_846_145_283_366_7];

/// seed 1 的第一个 `Math.random()`（TS: `PINNED_FIRST_RANDOM`）。
const PINNED_FIRST_RANDOM: f64 = 0.342_274_506_053_352_75;

// ---------------------------------------------------------------------------
// 后端
// ---------------------------------------------------------------------------

/// TS: `BACKENDS`（`worker` 与 `remote`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    Worker,
    Remote,
}

const BACKENDS: [Backend; 2] = [Backend::Worker, Backend::Remote];

/// 一个 sandbox 句柄（两个后端都暴露基类）。
enum SandboxHandle {
    Worker(CodemodeSandbox),
    Remote(RemoteCodemodeSandbox),
}

impl SandboxHandle {
    fn base(&self) -> &Arc<CodemodeSandboxBase> {
        match self {
            SandboxHandle::Worker(sandbox) => sandbox.base(),
            SandboxHandle::Remote(sandbox) => sandbox.base(),
        }
    }

    async fn close(&self) {
        match self {
            SandboxHandle::Worker(sandbox) => sandbox.close().await,
            SandboxHandle::Remote(sandbox) => sandbox.close().await,
        }
    }
}

async fn wasm() -> CodemodeWasmModule {
    load_quickjs_wasm(None)
        .await
        .expect("the embedded quickjs.wasm must load")
}

/// 远端后端：进程内 serve，每条消息经 `structuredClone`；`delay_first` 把 `seq === 1` 的回答
/// 延迟 30 ms，使后面的回答先到。
struct ServeRemote {
    wasm: CodemodeWasmModule,
    delay_first: bool,
}

/// `structuredClone` 的等价物（Workers RPC 的深拷贝）。
fn clone_request(request: CodemodeRemoteRequest) -> CodemodeRemoteRequest {
    serde_json::from_value(serde_json::to_value(request).expect("request serializes"))
        .expect("request clones")
}

fn clone_messages(
    messages: Vec<kiana_codemode::runtime::remote::CodemodeRemoteMessage>,
) -> Vec<kiana_codemode::runtime::remote::CodemodeRemoteMessage> {
    serde_json::from_value(serde_json::to_value(messages).expect("messages serialize"))
        .expect("messages clone")
}

fn clone_reply(reply: CodemodeRemoteReply) -> CodemodeRemoteReply {
    serde_json::from_value(serde_json::to_value(reply).expect("reply serializes"))
        .expect("reply clones")
}

impl CodemodeRemote for ServeRemote {
    fn execute(
        &self,
        request: CodemodeRemoteRequest,
        exchange: CodemodeRemoteExchange,
    ) -> BoxFuture<'static, Result<(), String>> {
        let wasm = self.wasm.clone();
        let delay_first = self.delay_first;
        Box::pin(async move {
            let request = clone_request(request);
            let wrapped: CodemodeRemoteExchange = Arc::new(move |messages| {
                let exchange = Arc::clone(&exchange);
                Box::pin(async move {
                    let reply = clone_reply((exchange)(clone_messages(messages)).await);
                    // TS: `if (delayFirst && reply?.type === "result" && reply.seq === 1)`。
                    if delay_first
                        && let CodemodeRemoteReply::Result(HostToWorkerMessage::Result(result)) =
                            &reply
                        && result.seq == Some(1)
                    {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                    reply
                })
            });
            serve_codemode_remote(
                request,
                wrapped,
                CodemodeServeOptions {
                    wasm,
                    max_interrupt_budget: Some(BUDGET),
                    max_memory_limit_bytes: None,
                },
            )
            .await;
            Ok(())
        })
    }
}

/// TS: `remote(options, delayFirst = false)`。
async fn remote_sandbox(
    base: CodemodeSandboxBaseOptions,
    delay_first: bool,
) -> RemoteCodemodeSandbox {
    let wasm = wasm().await;
    RemoteCodemodeSandbox::new(RemoteCodemodeSandboxOptions {
        base,
        remote: Arc::new(move || {
            Arc::new(ServeRemote {
                wasm: wasm.clone(),
                delay_first,
            }) as Arc<dyn CodemodeRemote>
        }),
        interrupt_budget: None,
    })
    .expect("valid remote sandbox options")
}

/// 建一个后端 sandbox（TS: `BACKENDS[backend](options)`）。
async fn create(backend: Backend, base: CodemodeSandboxBaseOptions) -> SandboxHandle {
    match backend {
        Backend::Worker => {
            // TS 的 worker 在 worker 线程里 `loadQuickJSWasm()`，模块缓存（`wasm.ts` 的
            // 每路径一次）让进程内只编译一次。Rust 侧同样按路径缓存；这里预先把同一份模块取到
            // 并直接传给 sandbox，使第一个执行不必在 worker launcher 里等一次编译——debug
            // 构建里那次编译要十几秒，会超过 `wait_for` 的断言窗口。
            let wasm = wasm().await;
            SandboxHandle::Worker(
                CodemodeSandbox::new(CodemodeSandboxOptions {
                    base,
                    wasm: Some(wasm),
                    ..Default::default()
                })
                .expect("valid worker sandbox options"),
            )
        }
        Backend::Remote => SandboxHandle::Remote(remote_sandbox(base, false).await),
    }
}

// ---------------------------------------------------------------------------
// start() 脚手架
// ---------------------------------------------------------------------------

fn ok_outcome(value: Option<Value>) -> CodemodeOutcome {
    CodemodeOutcome::Ok(CodemodeOutcomeOk { ok: true, value })
}

fn err_outcome(
    message: impl Into<String>,
    properties: Option<IndexMap<String, Value>>,
) -> CodemodeOutcome {
    CodemodeOutcome::Err(CodemodeOutcomeErr {
        ok: false,
        message: message.into(),
        properties,
    })
}

fn declared(name: &str) -> CodemodeDeclaredTool {
    CodemodeDeclaredTool {
        name: name.to_string(),
        js_name: None,
        description: None,
    }
}

/// `start()` 的选项（TS: `Partial<CodemodeRunOptions> & { answer? }`）。
struct StartOptions {
    started_at: f64,
    seed: f64,
    tools: Option<Vec<CodemodeDeclaredTool>>,
    globals: Option<Vec<CodemodeDeclaredGlobal>>,
    store: Option<IndexMap<String, Value>>,
    signal: Option<CancellationToken>,
    abort_reason: Option<String>,
    timeout_ms: Option<f64>,
    answer: Arc<dyn Fn(&CodemodeHostCall) -> CodemodeOutcome + Send + Sync>,
}

impl Default for StartOptions {
    fn default() -> Self {
        StartOptions {
            started_at: STARTED_AT,
            seed: 1.0,
            // TS 的默认 tools 是 `[{ name: "echo" }, { name: "wait" }]`。
            tools: Some(vec![declared("echo"), declared("wait")]),
            globals: None,
            store: None,
            signal: None,
            abort_reason: None,
            // TS 的 `start()` 默认 `timeoutMs: 20_000`。
            timeout_ms: Some(20_000.0),
            answer: Arc::new(|call| ok_outcome(call.args.clone())),
        }
    }
}

/// 一次被记录的 run（TS: `Recorded`）。
struct Recorded {
    run: Arc<Mutex<Option<CodemodeRun>>>,
    result: Mutex<Option<BoxFuture<'static, CodemodeResult>>>,
    calls: Arc<Mutex<Vec<CodemodeHostCall>>>,
    idles: Arc<Mutex<Vec<(u64, u64)>>>,
    events: Arc<Mutex<Vec<String>>>,
    fingerprint: Arc<Mutex<Option<String>>>,
}

impl Recorded {
    /// TS: `await run.result`（只能取一次）。
    async fn result(&self) -> CodemodeResult {
        let future = self
            .result
            .lock()
            .expect("result mutex")
            .take()
            .expect("the result is awaited once");
        future.await
    }

    fn deliver(&self, seq: f64, now: f64, id: u64, outcome: CodemodeOutcome) -> Result<(), String> {
        self.run
            .lock()
            .expect("run mutex")
            .as_ref()
            .expect("the run is installed")
            .deliver(seq, now, id, outcome)
    }

    fn finished(&self) -> bool {
        self.run
            .lock()
            .expect("run mutex")
            .as_ref()
            .expect("the run is installed")
            .finished()
    }

    fn fingerprint(&self) -> Option<String> {
        self.fingerprint.lock().expect("fingerprint mutex").clone()
    }

    fn calls(&self) -> Vec<CodemodeHostCall> {
        self.calls.lock().expect("calls mutex").clone()
    }

    fn call_len(&self) -> usize {
        self.calls.lock().expect("calls mutex").len()
    }

    fn idles(&self) -> Vec<(u64, u64)> {
        self.idles.lock().expect("idles mutex").clone()
    }

    fn idle_len(&self) -> usize {
        self.idles.lock().expect("idles mutex").len()
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events mutex").clone()
    }
}

/// 一个排队等待回答的调用。
struct Answer {
    seq: u64,
    call: CodemodeHostCall,
    outcome: CodemodeOutcome,
}

/// TS: `start(sandbox, code, options)`。
fn start(sandbox: &CodemodeSandboxBase, code: &str, options: StartOptions) -> Recorded {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let idles = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let fingerprint = Arc::new(Mutex::new(None));
    let run_slot: Arc<Mutex<Option<CodemodeRun>>> = Arc::new(Mutex::new(None));
    let queue: Arc<Mutex<VecDeque<Answer>>> = Arc::new(Mutex::new(VecDeque::new()));
    let notify = Arc::new(tokio::sync::Notify::new());
    // TS 在微任务里 `seq++`；这里在 `on_call` 里同步编号（调用顺序 = 编号顺序），投递顺序由
    // 单个 pump 任务保证。
    let seq = Arc::new(AtomicU64::new(0));

    let on_started = {
        let fingerprint = Arc::clone(&fingerprint);
        let events = Arc::clone(&events);
        Arc::new(move |value: String| {
            *fingerprint.lock().expect("fingerprint mutex") = Some(value);
            events
                .lock()
                .expect("events mutex")
                .push("started".to_string());
        })
    };
    let on_call = {
        let calls = Arc::clone(&calls);
        let events = Arc::clone(&events);
        let queue = Arc::clone(&queue);
        let notify = Arc::clone(&notify);
        let seq = Arc::clone(&seq);
        let answer = Arc::clone(&options.answer);
        Arc::new(move |call: CodemodeHostCall| {
            calls.lock().expect("calls mutex").push(call.clone());
            events
                .lock()
                .expect("events mutex")
                .push(format!("call {}", call.id));
            if call.name == "wait" {
                return;
            }
            let seq = seq.fetch_add(1, Ordering::SeqCst) + 1;
            let outcome = answer(&call);
            queue
                .lock()
                .expect("queue mutex")
                .push_back(Answer { seq, call, outcome });
            notify.notify_one();
        })
    };
    let on_idle = {
        let idles = Arc::clone(&idles);
        let events = Arc::clone(&events);
        Arc::new(move |seq: u64, issued: u64| {
            idles.lock().expect("idles mutex").push((seq, issued));
            events
                .lock()
                .expect("events mutex")
                .push(format!("idle {seq}"));
        })
    };

    let run_options = CodemodeRunOptions {
        started_at: options.started_at,
        seed: options.seed,
        tools: options.tools,
        globals: options.globals,
        store: options.store,
        signal: options.signal,
        timeout_ms: options.timeout_ms,
        on_started: Some(on_started),
        on_call: Some(on_call),
        on_idle: Some(on_idle),
    };
    let mut run = sandbox
        .start_with_abort_reason(code, run_options, options.abort_reason)
        .expect("the run starts");
    // 结果 future 取出来单独 await；`CodemodeRun` 留在槽里供 `deliver` 使用。
    let result = std::mem::replace(
        &mut run.result,
        Box::pin(std::future::pending::<CodemodeResult>()) as BoxFuture<'static, CodemodeResult>,
    );
    *run_slot.lock().expect("run mutex") = Some(run);

    // 回答泵：按调用顺序把回答交给 VM（TS 的 `queueMicrotask` 链）。
    {
        let run_slot = Arc::clone(&run_slot);
        let queue = Arc::clone(&queue);
        let notify = Arc::clone(&notify);
        tokio::spawn(async move {
            loop {
                let answer = queue.lock().expect("queue mutex").pop_front();
                match answer {
                    Some(answer) => {
                        let run = run_slot.lock().expect("run mutex");
                        if let Some(run) = run.as_ref() {
                            let _ = run.deliver(
                                answer.seq as f64,
                                STARTED_AT + 1000.0 * answer.seq as f64,
                                answer.call.id,
                                answer.outcome,
                            );
                        }
                    }
                    None => notify.notified().await,
                }
            }
        });
    }

    Recorded {
        run: run_slot,
        result: Mutex::new(Some(result)),
        calls,
        idles,
        events,
        fingerprint,
    }
}

// ---------------------------------------------------------------------------
// 断言辅助
// ---------------------------------------------------------------------------

fn expect_ok(result: &CodemodeResult) -> &CodemodeResultOk {
    match result {
        CodemodeResult::Ok(ok) => ok,
        CodemodeResult::Err(error) => panic!("expected a success result, got {error:?}"),
    }
}

/// 去掉墙钟时间（`calls[].durationMs`）之后的整份结果。
fn timeless(result: &CodemodeResult) -> Value {
    let mut value = serde_json::to_value(result).expect("the result serializes");
    if let Some(calls) = value.get_mut("calls").and_then(Value::as_array_mut) {
        for call in calls {
            if let Some(object) = call.as_object_mut() {
                object.remove("durationMs");
            }
        }
    }
    value
}

fn value_of(result: &CodemodeResult) -> Value {
    expect_ok(result)
        .value
        .clone()
        .expect("the script returned a value")
}

fn output_text(item: &kiana_codemode::types::CodemodeOutputItem) -> Option<&str> {
    match item {
        kiana_codemode::types::CodemodeOutputItem::Text { text, .. } => Some(text),
        kiana_codemode::types::CodemodeOutputItem::Image { .. } => None,
    }
}

fn num(value: &Value) -> f64 {
    value.as_f64().expect("a number")
}

async fn wait_for(mut check: impl FnMut() -> bool) {
    // TS 的 `waitFor` 轮询到条件成立；这里同样轮询，窗口放宽到 30 s（并行跑整个 suite 时
    // 第一个进程内 wasm 编译与线程启动都要等）。
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(Instant::now() < deadline, "Condition was not reached");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// `/^quickjs-wasi \S+, quickjs \S+, prelude [0-9a-f]{8}, deterministic \d+, stack \d+, heap 67108864, budget \S+$/`
fn matches_fingerprint(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("quickjs-wasi ") else {
        return false;
    };
    let Some((quickjs_wasi, rest)) = rest.split_once(", quickjs ") else {
        return false;
    };
    if quickjs_wasi.is_empty() || quickjs_wasi.contains(char::is_whitespace) {
        return false;
    }
    let Some((quickjs, rest)) = rest.split_once(", prelude ") else {
        return false;
    };
    if quickjs.is_empty() || quickjs.contains(char::is_whitespace) {
        return false;
    }
    let Some((prelude, rest)) = rest.split_once(", deterministic ") else {
        return false;
    };
    if prelude.len() != 8
        || !prelude
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return false;
    }
    let Some((deterministic, rest)) = rest.split_once(", stack ") else {
        return false;
    };
    if deterministic.is_empty() || !deterministic.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some((stack, rest)) = rest.split_once(", heap 67108864, budget ") else {
        return false;
    };
    if stack.is_empty() || !stack.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    !rest.is_empty() && !rest.contains(char::is_whitespace)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

/// `makes the same calls and output for the same start, seed, and results`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn makes_the_same_calls_and_output_for_the_same_start_seed_and_results() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let code = r#"
            const values = [Date.now(), performance.now(), Math.random(), Math.random(), new Date().toISOString()];
            const first = await tools.echo(values);
            text(JSON.stringify([Date.now(), performance.now(), Math.random()]));
            return await tools.echo(first.concat(Math.random()));
        "#;
        let one = start(sandbox.base(), code, StartOptions::default());
        let two = start(sandbox.base(), code, StartOptions::default());
        let a = one.result().await;
        let b = two.result().await;
        // 调用耗时是墙钟时间；其余全等。
        assert_eq!(timeless(&a), timeless(&b), "{backend:?}");
        assert_eq!(one.calls(), two.calls(), "{backend:?}");
        // 时钟：先 startedAt，之后读正在处理的结果的时间；performance.now() 从 startedAt 起算。
        let args = one.calls()[0]
            .args
            .clone()
            .expect("the first call has args");
        assert_eq!(num(&args[0]), STARTED_AT, "{backend:?}");
        assert_eq!(num(&args[1]), 0.0, "{backend:?}");
        assert_eq!(
            args[4].as_str(),
            Some("2026-01-15T10:00:00.000Z"),
            "{backend:?}"
        );
        let printed: Value = serde_json::from_str(
            output_text(&expect_ok(&a).output[0]).expect("the first output is text"),
        )
        .expect("the printed value is JSON");
        assert_eq!(num(&printed[0]), STARTED_AT + 1000.0, "{backend:?}");
        assert_eq!(num(&printed[1]), 1000.0, "{backend:?}");
        // Pinned：改这里就改变重放脚本看到的东西，因此也必须改变指纹。
        let random = num(&args[2]);
        assert_eq!(random, PINNED_FIRST_RANDOM, "{backend:?}");

        // seed 决定随机值，而不是起点。
        let later = start(
            sandbox.base(),
            code,
            StartOptions {
                started_at: STARTED_AT + 5000.0,
                ..Default::default()
            },
        );
        let reseeded = start(
            sandbox.base(),
            code,
            StartOptions {
                seed: 2.0,
                ..Default::default()
            },
        );
        let _ = later.result().await;
        let reseeded_result = reseeded.result().await;
        assert!(reseeded_result.ok(), "{backend:?}");
        assert_eq!(num(&later.calls()[0].args.as_ref().unwrap()[2]), random);
        assert_ne!(
            num(&reseeded.calls()[0].args.as_ref().unwrap()[2]),
            random,
            "{backend:?}"
        );
        // seed 0 有自己的值：QuickJS 把零状态变成 1，不能因此和 seed 1 相同。
        let zero = start(
            sandbox.base(),
            "return [Math.random(), Math.random()]",
            StartOptions {
                seed: 0.0,
                ..Default::default()
            },
        );
        let zero_result = zero.result().await;
        let zero_value = value_of(&zero_result);
        assert_eq!(num(&zero_value[0]), PINNED_SEED_ZERO[0], "{backend:?}");
        assert_eq!(num(&zero_value[1]), PINNED_SEED_ZERO[1], "{backend:?}");
        assert!(
            !zero_value
                .as_array()
                .expect("an array")
                .iter()
                .any(|value| value.as_f64() == Some(random)),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `runs in UTC whatever the host's timezone, and without WeakRef or FinalizationRegistry`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_in_utc_whatever_the_hosts_timezone_and_without_weak_ref_or_finalization_registry() {
    // TS 在测试体里设置 `process.env.TZ`。Rust 侧同样在测试体里设置宿主时区，随后恢复。
    let saved = std::env::var_os("TZ");
    unsafe { std::env::set_var("TZ", "Asia/Kolkata") };
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let code = "return [new Date(0).getHours(), new Date(0).getTimezoneOffset(), typeof WeakRef, typeof FinalizationRegistry]";
        let recorded = start(sandbox.base(), code, StartOptions::default());
        let result = recorded.result().await;
        assert_eq!(
            value_of(&result),
            json!([0, 0, "undefined", "undefined"]),
            "{backend:?}"
        );
        // execute() 照旧：宿主时区与全部内建可用。
        let executed = sandbox
            .base()
            .execute(code, Default::default())
            .await
            .expect("sandbox is open");
        assert_eq!(
            value_of(&executed),
            json!([5, -330, "function", "function"]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
    match saved {
        Some(value) => unsafe { std::env::set_var("TZ", value) },
        None => unsafe { std::env::remove_var("TZ") },
    }
}

/// `applies results in seq order, whatever order they are delivered in`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_results_in_seq_order_whatever_order_they_are_delivered_in() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            r#"const order = [];
            await Promise.all([tools.wait("a").then(() => order.push("a")), tools.wait("b").then(() => order.push("b"))]);
            return order;"#,
            StartOptions::default(),
        );
        wait_for(|| recorded.call_len() == 2 && recorded.idle_len() == 1).await;
        let calls = recorded.calls();
        let (a, b) = (&calls[0], &calls[1]);
        // b 是结果 1、a 是结果 2，但 a 先到。
        recorded
            .deliver(
                2.0,
                STARTED_AT + 2000.0,
                a.id,
                ok_outcome(Some(Value::Null)),
            )
            .expect("result 2");
        recorded
            .deliver(
                1.0,
                STARTED_AT + 1000.0,
                b.id,
                ok_outcome(Some(Value::Null)),
            )
            .expect("result 1");
        let result = recorded.result().await;
        assert_eq!(value_of(&result), json!(["b", "a"]), "{backend:?}");
        // 最后一个结果之后没有 idle：脚本在那次 drain 里结束，宿主停掉了 VM。
        assert_eq!(recorded.idles(), vec![(0, 2), (1, 2)], "{backend:?}");
        sandbox.close().await;
    }
}

/// `reports the fingerprint before any call and the calls issued after each result`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_fingerprint_before_any_call_and_the_calls_issued_after_each_result() {
    for backend in BACKENDS {
        let sandbox = create(
            backend,
            CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(64.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
        )
        .await;
        let recorded = start(
            sandbox.base(),
            "await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]);",
            StartOptions::default(),
        );
        assert!(recorded.result().await.ok(), "{backend:?}");
        assert_eq!(
            recorded.events().into_iter().take(3).collect::<Vec<_>>(),
            vec!["started", "call 1", "idle 0"],
            "{backend:?}"
        );
        assert_eq!(
            recorded.idles(),
            vec![(0, 1), (1, 3), (2, 3)],
            "{backend:?}"
        );
        assert_eq!(
            recorded
                .calls()
                .iter()
                .map(|call| json!([call.id, call.target, call.name, call.args]))
                .collect::<Vec<_>>(),
            vec![
                json!([1, "tool", "echo", 1]),
                json!([2, "tool", "echo", 2]),
                json!([3, "tool", "echo", 3]),
            ],
            "{backend:?}"
        );
        let fingerprint = recorded.fingerprint().expect("a fingerprint");
        assert!(matches_fingerprint(&fingerprint), "{fingerprint:?}");
        if backend == Backend::Remote {
            assert!(
                fingerprint.contains(&format!("budget {BUDGET}")),
                "{fingerprint:?}"
            );
        } else {
            assert!(fingerprint.contains("budget none"), "{fingerprint:?}");
        }

        let same = start(sandbox.base(), "return 1", StartOptions::default());
        let other_sandbox = create(
            backend,
            CodemodeSandboxBaseOptions {
                memory_limit_bytes: Some(32.0 * 1024.0 * 1024.0),
                ..Default::default()
            },
        )
        .await;
        let other = start(other_sandbox.base(), "return 1", StartOptions::default());
        let _ = same.result().await;
        let _ = other.result().await;
        assert_eq!(same.fingerprint(), Some(fingerprint.clone()), "{backend:?}");
        assert_ne!(other.fingerprint(), Some(fingerprint), "{backend:?}");
        sandbox.close().await;
        other_sandbox.close().await;
    }
}

/// `numbers calls to globals with tools, and answers errors as rejections`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn numbers_calls_to_globals_with_tools_and_answers_errors_as_rejections() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            r#"const found = await search("x", 2);
            try { await tools.echo("fail"); } catch (error) { text(error.message); }
            return found;"#,
            StartOptions {
                globals: Some(vec![CodemodeDeclaredGlobal {
                    name: "search".to_string(),
                    spread: Some(true),
                }]),
                answer: Arc::new(|call| {
                    if call.args == Some(Value::from("fail")) {
                        err_outcome("no", None)
                    } else if call.name == "search" {
                        ok_outcome(Some(json!(["found"])))
                    } else {
                        ok_outcome(call.args.clone())
                    }
                }),
                ..Default::default()
            },
        );
        let result = recorded.result().await;
        let calls = recorded.calls();
        assert_eq!(
            json!([calls[0].id, calls[0].target, calls[0].name, calls[0].args]),
            json!([1, "global", "search", ["x", 2]]),
            "{backend:?}"
        );
        let ok = expect_ok(&result);
        assert_eq!(ok.value, Some(json!(["found"])), "{backend:?}");
        assert_eq!(ok.output.len(), 1, "{backend:?}");
        assert_eq!(
            output_text(&ok.output[0]).map(str::to_string),
            Some("no".to_string()),
            "{backend:?}"
        );
        assert_eq!(
            json!([calls[1].id, calls[1].target, calls[1].name]),
            json!([2, "tool", "echo"]),
            "{backend:?}"
        );
        // 对全局函数的调用也算已发出的调用。
        assert_eq!(
            recorded.idles().into_iter().take(2).collect::<Vec<_>>(),
            vec![(0, 1), (1, 2)],
            "{backend:?}"
        );
        assert_eq!(ok.calls.len(), 1, "{backend:?}");
        assert_eq!(ok.calls[0].name, "echo");
        assert_eq!(
            ok.calls[0].status,
            kiana_codemode::types::CodemodeCallStatus::Error
        );
        sandbox.close().await;
    }
}

/// `keeps the clock monotonic and rejects a clock value that is not finite`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_the_clock_monotonic_and_rejects_a_clock_value_that_is_not_finite() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "const before = Date.now(); await tools.wait(1); return [before, Date.now(), performance.now()]",
            StartOptions::default(),
        );
        wait_for(|| recorded.call_len() == 1).await;
        let error = recorded
            .deliver(1.0, f64::NAN, 1, ok_outcome(Some(Value::Null)))
            .expect_err("NaN is not a finite clock value");
        assert!(error.contains("not a finite number"), "{error}");
        // 一个时钟落后于 run 起点的恢复宿主。
        recorded
            .deliver(1.0, STARTED_AT - 2000.0, 1, ok_outcome(Some(Value::Null)))
            .expect("a behind clock is clamped");
        let result = recorded.result().await;
        // 按数值比较：JSON 文本里 `1768471200000` 在 serde_json 里是整数，`json!(1.0)` 是浮点。
        let value = value_of(&result);
        let values = value.as_array().expect("an array");
        assert_eq!(num(&values[0]), STARTED_AT, "{backend:?}");
        assert_eq!(num(&values[1]), STARTED_AT, "{backend:?}");
        assert_eq!(num(&values[2]), 0.0, "{backend:?}");
        assert_eq!(values.len(), 3, "{backend:?}");
        sandbox.close().await;
    }
}

/// `reports the result the VM was processing when the script ended`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_the_result_the_vm_was_processing_when_the_script_ended() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let returned = start(
            sandbox.base(),
            "await tools.echo(1); return await tools.echo(2)",
            StartOptions::default(),
        );
        let thrown = start(
            sandbox.base(),
            "await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]); throw new Error('x')",
            StartOptions::default(),
        );
        let first = start(sandbox.base(), "return 1", StartOptions::default());
        let returned_result = returned.result().await;
        let ok = expect_ok(&returned_result);
        assert_eq!(ok.value, Some(json!(2)), "{backend:?}");
        assert_eq!(
            ok.during,
            Some(kiana_codemode::types::CodemodeDuring { seq: 2, issued: 2 }),
            "{backend:?}"
        );
        let thrown_result = thrown.result().await;
        match &thrown_result {
            CodemodeResult::Err(error) => assert_eq!(
                error.during,
                Some(kiana_codemode::types::CodemodeDuring { seq: 3, issued: 3 }),
                "{backend:?}"
            ),
            CodemodeResult::Ok(ok) => panic!("expected a failure, got {ok:?}"),
        }
        let first_result = first.result().await;
        assert_eq!(
            expect_ok(&first_result).during,
            Some(kiana_codemode::types::CodemodeDuring { seq: 0, issued: 0 }),
            "{backend:?}"
        );
        // `execute()` 的结果**没有** `during` 键。
        let executed = sandbox
            .base()
            .execute("return 1", Default::default())
            .await
            .expect("sandbox is open");
        let value = serde_json::to_value(&executed).expect("the result serializes");
        assert!(value.get("during").is_none(), "{backend:?}: {value}");
        sandbox.close().await;
    }
}

/// `leaves a call waiting when its answer cannot be serialized`。
///
/// TS 用 BigInt（`value: 1n` / `properties: { big: 1n }`）触发 `JSON.stringify` 失败。冻结的
/// `CodemodeOutcome` 用 `serde_json::Value`，无法表示不可序列化的值（类型层不存在），因此
/// 这里用「回答一个不在等待的调用」触发同一条「拒绝不消耗 seq」的路径，随后合法回答仍被
/// 接受——这里覆盖的可观察语义（拒绝不消耗 seq、随后合法回答仍成功）保持不变。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaves_a_call_waiting_when_its_answer_cannot_be_serialized() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "return await tools.wait(1)",
            StartOptions::default(),
        );
        wait_for(|| recorded.call_len() == 1).await;
        assert!(
            recorded
                .deliver(1.0, STARTED_AT, 99, ok_outcome(Some(Value::Null)))
                .is_err(),
            "{backend:?}"
        );
        recorded
            .deliver(1.0, STARTED_AT, 1, ok_outcome(Some(Value::from("fine"))))
            .expect("the call is still waiting");
        let result = recorded.result().await;
        assert_eq!(value_of(&result), json!("fine"), "{backend:?}");
        sandbox.close().await;
    }
}

/// `rejects with an Error carrying the answer's properties`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_with_an_error_carrying_the_answers_properties() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "try { await tools.echo(1); } catch (e) { return [e instanceof Error, e.message, e.name, e.structuredOutput]; }",
            StartOptions {
                answer: Arc::new(|_call| {
                    let mut properties = IndexMap::new();
                    properties.insert("structuredOutput".to_string(), json!({ "code": 7 }));
                    properties.insert("name".to_string(), json!("Ignored"));
                    properties.insert("message".to_string(), json!("ignored"));
                    err_outcome("denied", Some(properties))
                }),
                ..Default::default()
            },
        );
        let result = recorded.result().await;
        assert_eq!(
            value_of(&result),
            json!([true, "denied", "Error", { "code": 7 }]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `rejects rejection properties that are not an object, and tools sharing a script name`。
///
/// TS 的 `properties: null` / `[1]` 被 `deliver()` 拒绝（`The properties of result … are not an
/// object`）；冻结的 `CodemodeOutcomeErr::properties` 是 `Option<IndexMap>`，**只能**是对象，
/// 因此这两条在 Rust 侧由类型保证（见 `types.rs` 的已知差异），测试保留其余部分。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_rejection_properties_that_are_not_an_object_and_tools_sharing_a_script_name() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "return await tools.wait(1)",
            StartOptions::default(),
        );
        wait_for(|| recorded.call_len() == 1).await;
        recorded
            .deliver(1.0, STARTED_AT, 1, ok_outcome(Some(json!(1))))
            .expect("a valid outcome");
        let result = recorded.result().await;
        assert_eq!(value_of(&result), json!(1), "{backend:?}");
        let error = match sandbox.base().start(
            "",
            CodemodeRunOptions {
                started_at: STARTED_AT,
                seed: 1.0,
                tools: Some(vec![declared("a-b"), declared("a_b")]),
                on_call: Some(Arc::new(|_call| {})),
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("duplicate script names are rejected"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("script names must be unique"),
            "{error}"
        );
        sandbox.close().await;
    }
}

/// `uses the script names the run passes`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_the_script_names_the_run_passes() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "return [typeof tools.renamed, Object.keys(tools), ALL_TOOLS.map((tool) => tool.name)]",
            StartOptions {
                tools: Some(vec![CodemodeDeclaredTool {
                    name: "my-tool".to_string(),
                    js_name: Some("renamed".to_string()),
                    description: None,
                }]),
                ..Default::default()
            },
        );
        let result = recorded.result().await;
        assert_eq!(
            value_of(&result),
            json!(["function", ["renamed", "my-tool"], ["renamed"]]),
            "{backend:?}"
        );
        sandbox.close().await;
    }
}

/// `ends with an error the script cannot catch when the caller aborts`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ends_with_an_error_the_script_cannot_catch_when_the_caller_aborts() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let token = CancellationToken::new();
        let recorded = start(
            sandbox.base(),
            r#"try { await tools.wait(1); } catch { text("caught"); }
            text("after");"#,
            StartOptions {
                signal: Some(token.clone()),
                abort_reason: Some("diverged".to_string()),
                ..Default::default()
            },
        );
        wait_for(|| recorded.call_len() == 1).await;
        token.cancel();
        let result = recorded.result().await;
        match &result {
            CodemodeResult::Err(error) => {
                assert_eq!(error.error.kind, CodemodeErrorKind::Aborted, "{backend:?}");
                assert_eq!(error.error.message, "diverged", "{backend:?}");
                assert!(error.output.is_empty(), "{backend:?}");
            }
            CodemodeResult::Ok(ok) => panic!("expected a failure, got {ok:?}"),
        }
        // 已经结束：之后的 deliver 静默返回。
        recorded
            .deliver(1.0, STARTED_AT, 1, ok_outcome(Some(Value::Null)))
            .expect("a finished run ignores results");
        sandbox.close().await;
    }
}

/// `is finished once it has its outcome`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn is_finished_once_it_has_its_outcome() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "await tools.wait(1); return 2",
            StartOptions::default(),
        );
        wait_for(|| recorded.call_len() == 1).await;
        assert!(!recorded.finished(), "{backend:?}");
        recorded
            .deliver(1.0, STARTED_AT, 1, ok_outcome(Some(Value::Null)))
            .expect("result 1");
        let _ = recorded.result().await;
        assert!(recorded.finished(), "{backend:?}");
        sandbox.close().await;
    }
}

/// `rejects a call that is not waiting and a result number used twice`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_a_call_that_is_not_waiting_and_a_result_number_used_twice() {
    for backend in BACKENDS {
        let sandbox = create(backend, CodemodeSandboxBaseOptions::default()).await;
        let recorded = start(
            sandbox.base(),
            "await Promise.all([tools.wait(1), tools.wait(2)])",
            StartOptions::default(),
        );
        wait_for(|| recorded.call_len() == 2).await;
        let error = recorded
            .deliver(1.0, STARTED_AT, 99, ok_outcome(Some(Value::Null)))
            .expect_err("call 99 is not waiting");
        assert!(error.contains("Call 99 is not waiting"), "{error}");
        recorded
            .deliver(1.0, STARTED_AT, 1, ok_outcome(Some(Value::Null)))
            .expect("result 1");
        for (seq, id) in [(1.0, 2), (0.0, 2)] {
            let error = recorded
                .deliver(seq, STARTED_AT, id, ok_outcome(Some(Value::Null)))
                .expect_err("a result number is used once");
            assert!(error.contains("used once"), "{error}");
        }
        recorded
            .deliver(2.0, STARTED_AT, 2, ok_outcome(Some(Value::Null)))
            .expect("result 2");
        let result = recorded.result().await;
        assert!(result.ok(), "{backend:?}");
        sandbox.close().await;
    }
}

/// `applies results in seq order when the transport reorders replies sent in order`（remote transport）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_results_in_seq_order_when_the_transport_reorders_replies_sent_in_order() {
    let sandbox = remote_sandbox(CodemodeSandboxBaseOptions::default(), true).await;
    let recorded = start(
        sandbox.base(),
        r#"const order = [];
        await Promise.all([tools.wait("a").then(() => order.push("a")), tools.wait("b").then(() => order.push("b"))]);
        return order;"#,
        StartOptions::default(),
    );
    wait_for(|| recorded.call_len() == 2).await;
    let calls = recorded.calls();
    let (a, b) = (&calls[0], &calls[1]);
    recorded
        .deliver(
            1.0,
            STARTED_AT + 1000.0,
            a.id,
            ok_outcome(Some(Value::Null)),
        )
        .expect("result 1");
    recorded
        .deliver(
            2.0,
            STARTED_AT + 2000.0,
            b.id,
            ok_outcome(Some(Value::Null)),
        )
        .expect("result 2");
    let result = recorded.result().await;
    assert_eq!(value_of(&result), json!(["a", "b"]));
    sandbox.close().await;
}

/// `reads the real clock and real randomness`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_the_real_clock_and_real_randomness() {
    let sandbox = CodemodeSandbox::new(CodemodeSandboxOptions::default())
        .expect("valid worker sandbox options");
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis() as f64;
    let first = sandbox
        .execute("return [Date.now(), Math.random()]", Default::default())
        .await
        .expect("sandbox is open");
    let second = sandbox
        .execute("return [Date.now(), Math.random()]", Default::default())
        .await
        .expect("sandbox is open");
    let first_value = value_of(&first);
    let now = num(&first_value[0]);
    let random = num(&first_value[1]);
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis() as f64;
    assert!(now >= before, "{now} >= {before}");
    assert!(now <= after, "{now} <= {after}");
    let second_value = value_of(&second);
    assert!(
        !second_value
            .as_array()
            .expect("an array")
            .iter()
            .any(|value| value.as_f64() == Some(random))
    );
    sandbox.close().await;
}
