//! `runtime/vm.ts` 的逐条断言（spec §7.8 的 VM 层等价物）。
//!
//! TS 侧没有 `vm.test.ts`：`vm.ts` 只经由 `deterministic.test.ts`（经 `CodemodeSandboxBase`）与
//! `sandbox.test.ts` 被间接覆盖。这里按 spec §7.8 的 16 条断言**直接驱动 `start_vm`**，把
//! VM 层的行为（时钟、随机、指纹、seq 缓冲、drain、中断、崩溃文案）锁在它自己的边界上，
//! 不经任何 launcher。
//!
//! 所有 `expected` 值都取自 TS `startVm` 的真实输出（`STARTED_AT = Date.UTC(2026, 0, 15, 10, 0, 0)`
//! = `1768471200000`，默认 `seed: 1`），不是推算的。
//!
//! 需要嵌入的 `quickjs.wasm`，所以整个文件在 feature `wasm` 关闭时为空。

#![cfg(feature = "wasm")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use kiana_codemode::runtime::protocol::{
    HostResult, HostToWorkerMessage, VmDeterministic, VmGlobalInfo, VmStartData, VmToolInfo,
    WorkerDoneOutcome, WorkerToHostMessage,
};
use kiana_codemode::runtime::vm::{
    DETERMINISTIC_VERSION, JOBS_PER_POLL, MAX_STACK_SIZE, QUICKJS_VERSION, QUICKJS_WASI_VERSION,
    StartVmError, StartVmOptions, VmSession, start_vm,
};
use kiana_codemode::wasm::load_quickjs_wasm;

/// spec §7.8 的 `STARTED_AT`：`Date.UTC(2026, 0, 15, 10, 0, 0)`。
const STARTED_AT: f64 = 1_768_471_200_000.0;

/// spec/agent-codemode.md §2.2.8 的 `PINNED_SEED_ZERO`（seed 0 的前两个 `Math.random()`）。
const PINNED_SEED_ZERO: [f64; 2] = [0.671_137_253_026_676_4, 0.725_846_145_283_366_7];

/// spec/agent-codemode.md §2.2.8 的 `PINNED_FIRST_RANDOM`（seed 1 的第一个 `Math.random()`）。
const PINNED_FIRST_RANDOM: f64 = 0.342_274_506_053_352_75;

// ---------------------------------------------------------------------------
// 测试脚手架：一个最小的 `startVm` 调用方
// ---------------------------------------------------------------------------

/// 从 JSON 值里取数字（`serde_json` 的 `Number` 区分整数与浮点，测试只关心数值）。
fn num(value: &serde_json::Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("expected a number, got {value}"))
}

/// 收集 `post` 的消息（TS 测试里的 `messages` 数组）。
type Sink = Arc<Mutex<Vec<WorkerToHostMessage>>>;

/// 一个跑起来的 VM 与它的消息日志。
struct Harness {
    session: VmSession,
    messages: Sink,
}

impl Harness {
    /// 到目前为止收到的全部消息。
    fn messages(&self) -> Vec<WorkerToHostMessage> {
        self.messages.lock().expect("messages").clone()
    }

    /// 已收到的消息类型，如 `["started", "call", "idle"]`。
    fn types(&self) -> Vec<&'static str> {
        self.messages()
            .iter()
            .map(WorkerToHostMessage::type_name)
            .collect()
    }

    /// 到目前为止发出的调用（`type: "call"`）。
    fn calls(&self) -> Vec<(u64, &'static str, String, Option<String>)> {
        self.messages()
            .into_iter()
            .filter_map(|message| match message {
                WorkerToHostMessage::Call {
                    id,
                    target,
                    name,
                    args,
                } => Some((
                    id,
                    match target {
                        kiana_codemode::types::CodemodeCallTarget::Tool => "tool",
                        kiana_codemode::types::CodemodeCallTarget::Global => "global",
                    },
                    name,
                    args,
                )),
                _ => None,
            })
            .collect()
    }

    /// 每一条 `idle` 的 `(seq, issued)`（TS 测试里的 `idles`）。
    fn idles(&self) -> Vec<(u64, u64)> {
        self.messages()
            .into_iter()
            .filter_map(|message| match message {
                WorkerToHostMessage::Idle { seq, issued } => Some((seq, issued)),
                _ => None,
            })
            .collect()
    }

    /// `started` 的指纹。
    fn fingerprint(&self) -> String {
        self.messages()
            .into_iter()
            .find_map(|message| match message {
                WorkerToHostMessage::Started { fingerprint } => Some(fingerprint),
                _ => None,
            })
            .expect("started")
    }

    /// `done` 的载荷。
    fn done(&self) -> WorkerDoneOutcome {
        self.messages()
            .into_iter()
            .find_map(|message| match message {
                WorkerToHostMessage::Done(outcome) => Some(outcome),
                _ => None,
            })
            .expect("done")
    }

    /// `done`、`ok: true` 的返回值（JSON 文本）。
    fn value(&self) -> String {
        match self.done() {
            WorkerDoneOutcome::Ok(done) => done.value.expect("value"),
            WorkerDoneOutcome::Error(done) => panic!("expected ok, got error {:?}", done.error),
        }
    }

    /// `done`、`ok: true` 的返回值，解析成 JSON。
    fn json_value(&self) -> serde_json::Value {
        serde_json::from_str(&self.value()).expect("json value")
    }

    /// `done`、`ok: false` 的错误文本。
    fn error(&self) -> String {
        match self.done() {
            WorkerDoneOutcome::Error(done) => done.error,
            WorkerDoneOutcome::Ok(done) => panic!("expected error, got ok {:?}", done.value),
        }
    }

    /// `crash` 消息的文本（按到达顺序）。
    fn crashes(&self) -> Vec<String> {
        self.messages()
            .into_iter()
            .filter_map(|message| match message {
                WorkerToHostMessage::Crash { message } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// 输出项（`type: "output"`）。
    fn outputs(&self) -> Vec<kiana_codemode::types::CodemodeOutputItem> {
        self.messages()
            .into_iter()
            .filter_map(|message| match message {
                WorkerToHostMessage::Output { item } => Some(item),
                _ => None,
            })
            .collect()
    }

    /// 投递一条成功结果。
    fn deliver_ok(&mut self, id: u64, payload: &str, seq: Option<u64>, now: Option<f64>) {
        self.session
            .deliver(HostToWorkerMessage::Result(HostResult {
                id,
                ok: true,
                payload: Some(payload.to_string()),
                seq,
                now,
                properties: None,
            }));
    }
}

/// `startVm` 的参数（只暴露测试用得到的部分）。
struct Options {
    code: String,
    tools: Vec<VmToolInfo>,
    globals: Vec<VmGlobalInfo>,
    memory_limit_bytes: Option<f64>,
    deterministic: Option<VmDeterministic>,
    max_stack_size: Option<usize>,
    interrupt_budget: Option<u32>,
    should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Options {
    /// 默认：两个工具（`echo`、`wait`）、无 globals、空 store、确定性 seed 1。
    fn new(code: &str) -> Self {
        Options {
            code: code.to_string(),
            tools: vec![
                VmToolInfo {
                    name: "echo".into(),
                    js_name: "echo".into(),
                    description: String::new(),
                },
                VmToolInfo {
                    name: "wait".into(),
                    js_name: "wait".into(),
                    description: String::new(),
                },
            ],
            globals: Vec::new(),
            memory_limit_bytes: None,
            deterministic: Some(VmDeterministic {
                started_at: STARTED_AT,
                seed: 1.0,
            }),
            max_stack_size: None,
            interrupt_budget: None,
            should_interrupt: Arc::new(|| false),
        }
    }

    fn seed(mut self, seed: f64) -> Self {
        self.deterministic = Some(VmDeterministic {
            started_at: STARTED_AT,
            seed,
        });
        self
    }

    fn started_at(mut self, started_at: f64) -> Self {
        self.deterministic = Some(VmDeterministic {
            started_at,
            seed: 1.0,
        });
        self
    }

    fn nondeterministic(mut self) -> Self {
        self.deterministic = None;
        self
    }

    fn heap(mut self, bytes: f64) -> Self {
        self.memory_limit_bytes = Some(bytes);
        self
    }

    fn budget(mut self, budget: u32) -> Self {
        self.interrupt_budget = Some(budget);
        self
    }

    fn stack(mut self, bytes: usize) -> Self {
        self.max_stack_size = Some(bytes);
        self
    }

    fn tools(mut self, tools: Vec<VmToolInfo>) -> Self {
        self.tools = tools;
        self
    }

    fn globals(mut self, globals: Vec<VmGlobalInfo>) -> Self {
        self.globals = globals;
        self
    }

    fn interrupt(mut self, should_interrupt: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.should_interrupt = should_interrupt;
        self
    }
}

/// 起一个 VM，返回会话与消息日志。
async fn start(options: Options) -> Harness {
    let wasm = load_quickjs_wasm(None)
        .await
        .expect("embedded quickjs.wasm");
    let messages: Sink = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&messages);
    let session = start_vm(StartVmOptions {
        data: VmStartData {
            code: options.code,
            tools: options.tools,
            globals: options.globals,
            memory_limit_bytes: options.memory_limit_bytes,
            store: Default::default(),
            deterministic: options.deterministic,
        },
        wasm,
        max_stack_size: options.max_stack_size,
        interrupt_budget: options.interrupt_budget,
        post: Arc::new(move |message| {
            sink.lock().expect("messages").push(message);
        }),
        should_interrupt: options.should_interrupt,
    })
    .await;
    match session {
        Ok(session) => Harness { session, messages },
        Err(error) => panic!("start_vm failed: {error}"),
    }
}

/// 起一个 VM；只关心失败原因时用。
async fn start_result(options: Options) -> Result<Harness, StartVmError> {
    let wasm = load_quickjs_wasm(None)
        .await
        .expect("embedded quickjs.wasm");
    let messages: Sink = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&messages);
    let session = start_vm(StartVmOptions {
        data: VmStartData {
            code: options.code,
            tools: options.tools,
            globals: options.globals,
            memory_limit_bytes: options.memory_limit_bytes,
            store: Default::default(),
            deterministic: options.deterministic,
        },
        wasm,
        max_stack_size: options.max_stack_size,
        interrupt_budget: options.interrupt_budget,
        post: Arc::new(move |message| {
            sink.lock().expect("messages").push(message);
        }),
        should_interrupt: options.should_interrupt,
    })
    .await;
    session.map(|session| Harness { session, messages })
}

// ---------------------------------------------------------------------------
// §7.8 第 1 条：相同 start/seed/结果 → 相同的时钟、随机与指纹
// ---------------------------------------------------------------------------

/// 时钟读虚拟时间，`Math.random()` 走 pinned 的引擎播种链，脚本运行前报指纹。
///
/// TS: `deterministic.test.ts` 第 1 个 `it` 与 `vm.ts` 的 `deterministicWasi` 注释。
#[tokio::test]
async fn clock_and_random_are_deterministic() {
    let mut harness = start(Options::new(
        "return [Date.now(), performance.now(), Math.random(), Math.random(), new Date(Date.now()).toISOString(), new Date(0).getHours(), new Date(0).getTimezoneOffset(), typeof WeakRef, typeof FinalizationRegistry]",
    ))
    .await;
    let value = harness.json_value();
    harness.session.dispose();

    assert_eq!(num(&value[0]), STARTED_AT, "Date.now() = startedAt");
    assert_eq!(num(&value[1]), 0.0, "performance.now() = 0");
    assert_eq!(num(&value[2]), PINNED_FIRST_RANDOM);
    assert_eq!(num(&value[3]), 0.462_054_608_139_009_8);
    assert_eq!(
        value[4],
        serde_json::json!("2026-01-15T10:00:00.000Z"),
        "UTC, not the host's zone"
    );
    assert_eq!(num(&value[5]), 0.0, "new Date(0).getHours()");
    assert_eq!(num(&value[6]), 0.0, "getTimezoneOffset()");
    assert_eq!(value[7], serde_json::json!("undefined"), "no WeakRef");
    assert_eq!(
        value[8],
        serde_json::json!("undefined"),
        "no FinalizationRegistry"
    );

    assert_eq!(
        harness.fingerprint(),
        format!(
            "quickjs-wasi {QUICKJS_WASI_VERSION}, quickjs {QUICKJS_VERSION}, prelude 8c6c9242, deterministic {DETERMINISTIC_VERSION}, stack {MAX_STACK_SIZE}, heap none, budget none"
        )
    );
    // `started` → `done` → 收尾 `idle`（seq 0）。
    assert_eq!(harness.types(), ["started", "done", "idle"]);
    assert_eq!(harness.idles(), [(0, 0)]);
}

/// seed 0 给出 `PINNED_SEED_ZERO`，seed 2 与 seed 1 不同（`seed + 1` 的原因）。
///
/// TS: spec §2.2.8「确定性随机数」的验证清单。
#[tokio::test]
async fn seed_zero_and_two_are_pinned() {
    let mut zero = start(Options::new("return [Math.random(), Math.random()]").seed(0.0)).await;
    let zero_value = zero.json_value();
    zero.session.dispose();
    assert_eq!(num(&zero_value[0]), PINNED_SEED_ZERO[0]);
    assert_eq!(num(&zero_value[1]), PINNED_SEED_ZERO[1]);
    assert_ne!(
        num(&zero_value[0]),
        PINNED_FIRST_RANDOM,
        "seed 0 must not repeat seed 1"
    );

    let mut two = start(Options::new("return [Math.random()]").seed(2.0)).await;
    let two_value = two.json_value();
    two.session.dispose();
    assert_eq!(num(&two_value[0]), 0.013_411_759_080_029_118);
}

/// `startedAt` 只移动时钟，不改变随机值。
///
/// TS: `deterministic.test.ts`「`startedAt + 5000` 不改变随机值」。
#[tokio::test]
async fn started_at_does_not_change_randomness() {
    let mut harness =
        start(Options::new("return [Math.random(), Date.now()]").started_at(STARTED_AT + 5000.0))
            .await;
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(num(&value[0]), PINNED_FIRST_RANDOM);
    assert_eq!(num(&value[1]), STARTED_AT + 5000.0);
}

// ---------------------------------------------------------------------------
// §7.8 第 2 条：UTC 且无 WeakRef（宿主 TZ 不影响确定性 VM）
// ---------------------------------------------------------------------------

/// 宿主时区对确定性 VM 不可见；非确定性 VM 用宿主时区。
///
/// TS: `deterministic.test.ts` 第 2 个 `it`（`TZ=Asia/Kolkata` 下
/// `[0, 0, "undefined", "undefined"]`，同一 code 在 `execute()` 里得到
/// `[5, -330, "function", "function"]`）。
///
/// 这里把宿主 TZ 设成 `Asia/Kolkata`（UTC+5:30）再跑：确定性 VM 必须是 `[0, 0]`。
#[tokio::test]
async fn deterministic_vm_ignores_host_timezone() {
    let previous = std::env::var_os("TZ");
    // SAFETY: 测试进程里只有本用例读写 TZ；`afterEach` 的等价物在末尾恢复。
    unsafe { std::env::set_var("TZ", "Asia/Kolkata") };

    let mut deterministic = start(Options::new(
        "return [new Date(0).getHours(), new Date(0).getTimezoneOffset(), typeof WeakRef, typeof FinalizationRegistry]",
    ))
    .await;
    let value = deterministic.json_value();
    deterministic.session.dispose();
    assert_eq!(num(&value[0]), 0.0, "UTC hour");
    assert_eq!(num(&value[1]), 0.0, "UTC offset");
    assert_eq!(value[2], serde_json::json!("undefined"));
    assert_eq!(value[3], serde_json::json!("undefined"));

    let mut host = start(
        Options::new(
            "return [new Date(0).getHours(), new Date(0).getTimezoneOffset(), typeof WeakRef]",
        )
        .nondeterministic(),
    )
    .await;
    let value = host.json_value();
    host.session.dispose();
    assert_eq!(num(&value[0]), 5.0, "host local hour");
    assert_eq!(num(&value[1]), -330.0, "host offset");
    assert_eq!(
        value[2],
        serde_json::json!("function"),
        "WeakRef exists outside deterministic mode"
    );

    match previous {
        Some(value) => unsafe { std::env::set_var("TZ", value) },
        None => unsafe { std::env::remove_var("TZ") },
    }
}

// ---------------------------------------------------------------------------
// §7.8 第 3 条：按 seq 应用，即使乱序 deliver
// ---------------------------------------------------------------------------

/// 早到的结果先缓冲，从 `nextSeq` 起连续应用；每应用一条报一次 `idle`。
///
/// TS: `deterministic.test.ts` 第 3 个 `it`——`deliver(2, …)` 先于 `deliver(1, …)`，结果为
/// `["b", "a"]`，该 `it` 断言的 `idles` 前两项是 `[[0, 2], [1, 2]]`。
///
/// 这里一直读到脚本结束，所以还会看到**第三条** `idle`（`[2, 2]`）：`deliver` 的
/// `post({type:"idle", seq: nextSeq, issued})` 在 `apply`（其中 drain 会发出 `done`）**之后**
/// 执行，所以最后一条 `idle` 排在 `done` 后面。那条 TS 测试在第二个 `deliver` 之后立刻取
/// `idles`，取到的就是前两条 `[0, 2]`、`[1, 2]`。
#[tokio::test]
async fn results_apply_in_seq_order() {
    let mut harness = start(Options::new(
        "const order = [];\nawait Promise.all([tools.wait(\"a\").then(() => order.push(\"a\")), tools.wait(\"b\").then(() => order.push(\"b\"))]);\nreturn order;",
    ))
    .await;
    let calls = harness.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].3.as_deref(), Some("\"a\""));
    assert_eq!(calls[1].3.as_deref(), Some("\"b\""));

    // b 是结果 1、a 是结果 2，但 a 先到。
    harness.deliver_ok(calls[0].0, "null", Some(2), Some(STARTED_AT + 2000.0));
    harness.deliver_ok(calls[1].0, "null", Some(1), Some(STARTED_AT + 1000.0));
    let value = harness.json_value();
    let idles = harness.idles();
    let done = harness.done();
    harness.session.dispose();

    assert_eq!(
        value,
        serde_json::json!(["b", "a"]),
        "seq order wins over arrival order"
    );
    assert_eq!(
        idles,
        [(0, 2), (1, 2), (2, 2)],
        "idle after the first run, after result 1, and after result 2"
    );
    match done {
        WorkerDoneOutcome::Ok(done) => {
            let during = done.during.expect("during");
            assert_eq!((during.seq, during.issued), (2, 2));
        }
        WorkerDoneOutcome::Error(done) => panic!("expected ok, got {}", done.error),
    }
}

// ---------------------------------------------------------------------------
// §7.8 第 4 条：指纹
// ---------------------------------------------------------------------------

/// 事件顺序、`issued` 计数与指纹的形状。
///
/// TS: `deterministic.test.ts` 第 4 个 `it`——`events` 前三项为 `["started", "call 1", "idle 0"]`，
/// `idles === [[0,1],[1,3],[2,3]]`，`calls` 为 `[[1,"tool","echo",1], …]`，指纹匹配
/// `/^quickjs-wasi \S+, quickjs \S+, prelude [0-9a-f]{8}, deterministic \d+, stack \d+, heap 67108864, budget \S+$/`。
#[tokio::test]
async fn fingerprint_and_issued_counts() {
    let mut harness = start(
        Options::new("await tools.echo(1); await Promise.all([tools.echo(2), tools.echo(3)]);")
            .heap(64.0 * 1024.0 * 1024.0),
    )
    .await;
    let fingerprint = harness.fingerprint();
    let types = harness.types();
    assert_eq!(fingerprint.matches(", ").count(), 6, "seven segments");
    assert!(fingerprint.starts_with("quickjs-wasi "));
    assert!(fingerprint.contains(", deterministic 1, "));
    assert!(fingerprint.ends_with(", heap 67108864, budget none"));

    // 前三项：started、第一条 call、脚本首次 drain 的 idle。
    assert_eq!(&types[..3], ["started", "call", "idle"]);

    let mut seq = 0u64;
    let mut answered = std::collections::HashSet::new();
    loop {
        let calls = harness.calls();
        let next = calls
            .iter()
            .find(|call| !answered.contains(&call.0))
            .cloned();
        let Some((id, _, _, args)) = next else {
            break;
        };
        answered.insert(id);
        seq += 1;
        let payload = args.expect("args");
        harness.deliver_ok(
            id,
            &payload,
            Some(seq),
            Some(STARTED_AT + 1000.0 * seq as f64),
        );
    }
    let idles = harness.idles();
    let calls = harness.calls();
    let done = harness.done();
    harness.session.dispose();

    assert_eq!(idles, [(0, 1), (1, 3), (2, 3), (3, 3)]);
    assert_eq!(
        calls
            .iter()
            .map(|(id, target, name, args)| (*id, *target, name.clone(), args.clone()))
            .collect::<Vec<_>>(),
        vec![
            (1, "tool", "echo".to_string(), Some("1".to_string())),
            (2, "tool", "echo".to_string(), Some("2".to_string())),
            (3, "tool", "echo".to_string(), Some("3".to_string())),
        ]
    );
    match done {
        WorkerDoneOutcome::Ok(done) => {
            let during = done.during.expect("during");
            assert_eq!((during.seq, during.issued), (3, 3));
        }
        WorkerDoneOutcome::Error(done) => panic!("expected ok, got {}", done.error),
    }
}

/// `memoryLimitBytes` 与 `interruptBudget` 进入指纹；不同配置的指纹不同。
///
/// TS: `deterministic.test.ts` 第 4 个 `it`——remote 为 `budget 50000`，worker 为 `budget none`，
/// `32 MiB` 的沙箱指纹不同于 `64 MiB`。
#[tokio::test]
async fn fingerprint_captures_heap_and_budget() {
    let mut with_budget = start(
        Options::new("return 1")
            .heap(64.0 * 1024.0 * 1024.0)
            .budget(50_000),
    )
    .await;
    let fingerprint = with_budget.fingerprint();
    with_budget.session.dispose();
    assert_eq!(
        fingerprint,
        format!(
            "quickjs-wasi {QUICKJS_WASI_VERSION}, quickjs {QUICKJS_VERSION}, prelude 8c6c9242, deterministic {DETERMINISTIC_VERSION}, stack {MAX_STACK_SIZE}, heap 67108864, budget 50000"
        )
    );

    let mut smaller = start(Options::new("return 1").heap(32.0 * 1024.0 * 1024.0)).await;
    let small_fingerprint = smaller.fingerprint();
    smaller.session.dispose();
    assert_ne!(fingerprint, small_fingerprint);
    assert!(small_fingerprint.ends_with(", heap 33554432, budget none"));
}

/// 非确定性模式**不报** `started`/`idle`（没有指纹要核对）。
///
/// TS: `vm.ts` 里两处 `post` 都在 `if (clock)` 之内。
#[tokio::test]
async fn nondeterministic_mode_reports_no_started_or_idle() {
    let mut harness = start(Options::new("return 1").nondeterministic()).await;
    let types = harness.types();
    harness.session.dispose();
    assert_eq!(types, ["done"]);
}

// ---------------------------------------------------------------------------
// §7.8 第 5 条：globals 与 tools 一起编号
// ---------------------------------------------------------------------------

/// `spread: true` 的全局函数把全部实参作为数组送出；编号与 tools 共用。
///
/// TS: `deterministic.test.ts` 第 5 个 `it`——`{id:1, target:"global", name:"search", args:["x",2]}`。
#[tokio::test]
async fn globals_share_the_call_numbering() {
    let mut harness = start(
        Options::new("const found = await search(\"x\", 2); return found;")
            .tools(Vec::new())
            .globals(vec![VmGlobalInfo {
                name: "search".into(),
                spread: true,
            }]),
    )
    .await;
    let call = harness.calls()[0].clone();
    harness.deliver_ok(call.0, "[\"found\"]", Some(1), Some(STARTED_AT + 1000.0));
    let value = harness.value();
    let idles = harness.idles();
    harness.session.dispose();

    assert_eq!(
        call,
        (
            1,
            "global",
            "search".to_string(),
            Some("[\"x\",2]".to_string())
        )
    );
    assert_eq!(value, "[\"found\"]");
    assert_eq!(idles, [(0, 1), (1, 1)]);
}

// ---------------------------------------------------------------------------
// §7.8 第 6 条：时钟单调
// ---------------------------------------------------------------------------

/// 结果里的 `now` 只会把虚拟时钟往前推，绝不倒退。
///
/// TS: `deterministic.test.ts` 第 6 个 `it`——`deliver(1, STARTED_AT - 2000, …)` 后结果为
/// `[STARTED_AT, STARTED_AT, 0]`。
#[tokio::test]
async fn clock_never_moves_backwards() {
    let mut harness = start(Options::new(
        "const before = Date.now(); await tools.wait(1); return [before, Date.now(), performance.now()];",
    ))
    .await;
    // 恢复的宿主时钟落后于启动这次运行的时钟：`now` 必须被忽略。
    let id = harness.calls()[0].0;
    harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT - 2000.0));
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(num(&value[0]), STARTED_AT, "before");
    assert_eq!(
        num(&value[1]),
        STARTED_AT,
        "a behind host clock never moves the VM backwards"
    );
    assert_eq!(num(&value[2]), 0.0, "performance.now()");
}

/// 结果里的 `now` 前进时，VM 时钟随之前进（单调不减）。
///
/// TS: `vm.ts` 的 `clock.now = Math.max(clock.now, next.now)`。
#[tokio::test]
async fn clock_follows_forward_now() {
    let mut harness = start(Options::new(
        "await tools.wait(1); return [Date.now(), performance.now()];",
    ))
    .await;
    let id = harness.calls()[0].0;
    harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT + 1000.0));
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(num(&value[0]), STARTED_AT + 1000.0);
    assert_eq!(num(&value[1]), 1000.0);
}

// ---------------------------------------------------------------------------
// §7.8 第 7 条：结束时报告正在处理的结果号
// ---------------------------------------------------------------------------

/// `done.during` 是脚本结束时正在处理的结果号与当时的 `issued`。
///
/// TS: `deterministic.test.ts` 第 7 个 `it`——`await echo(1); return await echo(2)` →
/// `{seq:2, issued:2}`；无调用 → `{seq:0, issued:0}`；`execute()` 结果**没有** `during`。
#[tokio::test]
async fn done_reports_the_result_in_progress() {
    let mut harness = start(Options::new(
        "await tools.echo(1); return await tools.echo(2)",
    ))
    .await;
    let first = harness.calls()[0].0;
    harness.deliver_ok(first, "null", Some(1), Some(STARTED_AT + 1000.0));
    let second = harness.calls()[1].0;
    harness.deliver_ok(second, "\"done\"", Some(2), Some(STARTED_AT + 2000.0));
    let during = match harness.done() {
        WorkerDoneOutcome::Ok(done) => done.during.expect("during"),
        WorkerDoneOutcome::Error(done) => panic!("expected ok, got {}", done.error),
    };
    harness.session.dispose();
    assert_eq!((during.seq, during.issued), (2, 2));

    let mut idle_harness = start(Options::new("return 1")).await;
    let during = match idle_harness.done() {
        WorkerDoneOutcome::Ok(done) => done.during.expect("during"),
        WorkerDoneOutcome::Error(_done) => panic!("expected ok"),
    };
    idle_harness.session.dispose();
    assert_eq!((during.seq, during.issued), (0, 0));
}

// ---------------------------------------------------------------------------
// §7.8 第 9 条：rejection 携带 properties
// ---------------------------------------------------------------------------

/// `ok: false` 的结果带上 `properties`（`name`/`message`/`stack` 被忽略）。
///
/// TS: `deterministic.test.ts` 第 9 个 `it`——`[true, "denied", "Error", {code:7}]`。
///
/// 「name/message/stack 被忽略」是 **prelude** 的 `settle` 做的，VM 只负责把 `properties`
/// 原样送进 guest；这里断言 VM 侧确实把它送到了。
#[tokio::test]
async fn rejection_carries_properties() {
    let mut harness = start(Options::new(
        "try { await tools.echo(1); } catch (e) { return [e instanceof Error, e.message, e.name, e.structuredOutput]; }",
    ))
    .await;
    let id = harness.calls()[0].0;
    harness
        .session
        .deliver(HostToWorkerMessage::Result(HostResult {
            id,
            ok: false,
            payload: Some("denied".into()),
            seq: Some(1),
            now: Some(STARTED_AT),
            properties: Some(
                "{\"structuredOutput\":{\"code\":7},\"name\":\"Ignored\",\"message\":\"ignored\"}"
                    .into(),
            ),
        }));
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(
        value,
        serde_json::json!([true, "denied", "Error", {"code": 7}])
    );
}

// ---------------------------------------------------------------------------
// §7.8 第 14 条：重复 seq / 缺失 seq → crash
// ---------------------------------------------------------------------------

/// 重复的 `seq` 转成 `crash`（文案逐字来自 `vm.ts`）。
///
/// TS: `deterministic.test.ts` 第 14 个 `it`——已用过的 seq 再 deliver 抛 `used once`。
#[tokio::test]
async fn duplicate_seq_crashes() {
    let mut harness = start(Options::new("await tools.wait(1); await tools.wait(2);")).await;
    let id = harness.calls()[0].0;
    harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT));
    harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT));
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(
        crashes,
        vec![format!(
            "Error: Result {id} has sequence number 1, expected 2 or later, once"
        )]
    );
}

/// 缺失的 `seq` 同样转成 `crash`，文案里是 `undefined`。
///
/// TS: `vm.ts` 的 `deliver`——`seq === undefined` 走同一个 `throw new Error(...)`。
#[tokio::test]
async fn missing_seq_crashes() {
    let mut harness = start(Options::new("await tools.wait(1);")).await;
    let id = harness.calls()[0].0;
    harness
        .session
        .deliver(HostToWorkerMessage::Result(HostResult {
            id,
            ok: true,
            payload: None,
            seq: None,
            now: None,
            properties: None,
        }));
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(
        crashes,
        vec![format!(
            "Error: Result {id} has sequence number undefined, expected 1 or later, once"
        )]
    );
}

// ---------------------------------------------------------------------------
// §7.8 第 12 条：脚本侧错误与 stall
// ---------------------------------------------------------------------------

/// 脚本抛 `Error`：`done` 带 `{name, message, stack}`，`stack` 是 `head + "\n" + 帧`。
///
/// TS: `sandbox.test.ts` 的错误形状 + `vm.ts` 的 `describeException`。
#[tokio::test]
async fn thrown_error_becomes_a_done_error() {
    let mut harness = start(Options::new("throw new Error(\"boom\")")).await;
    let error = harness.error();
    let types = harness.types();
    harness.session.dispose();

    let parsed: serde_json::Value = serde_json::from_str(&error).expect("json error");
    assert_eq!(parsed["name"], serde_json::json!("Error"));
    assert_eq!(parsed["message"], serde_json::json!("boom"));
    let stack = parsed["stack"].as_str().expect("stack");
    assert!(stack.starts_with("Error: boom\n"), "{stack}");
    assert!(stack.contains("codemode.js:1:"), "{stack}");
    assert!(
        !stack.contains("codemode-prelude.js"),
        "prelude frames are filtered by the prelude itself: {stack}"
    );
    assert_eq!(types, ["started", "done", "idle"]);
}

/// 脚本编译失败：直接 `done`（没有 `during`），脚本一次都没跑。
///
/// TS: `vm.ts` 的 `vm.evalCode(…, "codemode.js")` 分支——`post({type:"done", ok:false, error})`
/// 后 `return session`。
#[tokio::test]
async fn syntax_error_skips_the_run() {
    let mut harness = start(Options::new("function")).await;
    let error = harness.error();
    let types = harness.types();
    harness.session.dispose();

    let parsed: serde_json::Value = serde_json::from_str(&error).expect("json error");
    assert_eq!(parsed["name"], serde_json::json!("SyntaxError"));
    let stack = parsed["stack"].as_str().expect("stack");
    assert!(stack.starts_with("SyntaxError: "), "{stack}");
    assert!(stack.contains("codemode.js:2:1"), "{stack}");
    assert_eq!(types, ["started", "done"]);
    assert!(
        !error.contains("during"),
        "a compile failure reports no during: {error}"
    );
}

/// 等待永远不可能 settle 的 promise：`stalled()` 以 `Error` 结束脚本。
///
/// TS: `sandbox.test.ts` 的 stall 断言 + `prelude.js` 的 `stalled()`。
#[tokio::test]
async fn stalled_promise_ends_the_script() {
    let mut harness = start(Options::new("await new Promise(() => {}); return 1;")).await;
    let error = harness.error();
    let during = match harness.done() {
        WorkerDoneOutcome::Error(done) => done.during.expect("during"),
        WorkerDoneOutcome::Ok(_) => panic!("expected error"),
    };
    harness.session.dispose();
    let parsed: serde_json::Value = serde_json::from_str(&error).expect("json error");
    assert_eq!(parsed["name"], serde_json::json!("Error"));
    assert_eq!(
        parsed["message"],
        serde_json::json!(
            "The script is waiting on a promise that can never settle: no tool call is pending, and timers do not exist here."
        )
    );
    assert_eq!((during.seq, during.issued), (0, 0));
}

/// 队列里的 promise job 失败（微任务里抛出）：`crash` 文案是 `Error: Promise job failed`。
///
/// TS: `vm.ts` 的 `drain`——`if (exports.qjs_execute_pending_job() < 0) throw new Error("Promise job failed")`。
#[tokio::test]
async fn failing_promise_job_crashes() {
    let mut harness = start(Options::new(
        "queueMicrotask(() => { throw new Error(\"z\"); }); return 3;",
    ))
    .await;
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(crashes, vec!["Error: Promise job failed".to_string()]);
}

// ---------------------------------------------------------------------------
// §7.8 第 12 条（续）：中断
// ---------------------------------------------------------------------------

/// `shouldInterrupt` 恒为真：QuickJS 自己的轮询抛出不可 catch 的 `InternalError: interrupted`。
///
/// TS: `sandbox.test.ts` 的 abort 断言 → `crash` `InternalError: interrupted`。
#[tokio::test]
async fn interrupt_during_script_crashes() {
    let mut harness = start(
        Options::new("while (true) {}")
            .nondeterministic()
            .interrupt(Arc::new(|| true)),
    )
    .await;
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(crashes, vec!["InternalError: interrupted".to_string()]);
}

/// drain 期间的 `shouldInterrupt` 每 [`JOBS_PER_POLL`] 个 job 轮询一次。
///
/// TS: `vm.ts` 的 `drain`——`while (true) await null` 每次迭代是一个只跑几条指令的 job，
/// QuickJS 自己的按操作码计数会让它跑很久；`JOBS_PER_POLL` 的轮询是唯一及时的检查。
#[tokio::test]
async fn interrupt_during_drain_crashes() {
    let polls = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&polls);
    let mut harness = start(
        Options::new("while (true) await null;")
            .nondeterministic()
            .interrupt(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst) > 120
            })),
    )
    .await;
    let crashes = harness.crashes();
    let seen = polls.load(Ordering::SeqCst);
    harness.session.dispose();
    assert_eq!(crashes, vec!["Error: Execution interrupted".to_string()]);
    assert!(
        seen > JOBS_PER_POLL,
        "the drain loop polled shouldInterrupt ({seen} polls)"
    );
}

// ---------------------------------------------------------------------------
// §7.8 第 8 条的表亲：out of memory 仍是可 catch 的 InternalError
// ---------------------------------------------------------------------------

/// `memoryLimitBytes` 超限在脚本里是**可 catch** 的 `InternalError: out of memory`，不是 wasm trap。
///
/// TS: spec 附录 A.6 第 10 条与 `sandbox.test.ts` 的 remote OOM 断言。
#[tokio::test]
async fn out_of_memory_stays_catchable() {
    let mut harness = start(
        Options::new(
            "try { const a = []; for (;;) a.push(\"x\".repeat(100000)); } catch (e) { return [e instanceof Error, e.name, e.message]; }",
        )
        .nondeterministic()
        .heap(8.0 * 1024.0 * 1024.0),
    )
    .await;
    let value = harness.json_value();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert!(crashes.is_empty(), "no crash: {crashes:?}");
    assert_eq!(
        value,
        serde_json::json!([true, "InternalError", "out of memory"])
    );
}

/// 深递归仍是可 catch 的 `RangeError`，不是宿主栈溢出（spec 附录 A.6 第 1 条）。
///
/// TS: `sandbox.test.ts`「turns deep recursion into a catchable RangeError」
/// （`["RangeError", depth > 1000]`）。
#[tokio::test]
async fn deep_recursion_is_a_catchable_range_error() {
    let mut harness = start(
        Options::new(
            "let depth = 0;\nfunction dive() { depth++; dive(); }\ntry { dive(); } catch (error) { return [error.name, depth > 1000]; }",
        )
        .nondeterministic(),
    )
    .await;
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(value, serde_json::json!(["RangeError", true]));
}

/// `maxStackSize` 越小，递归越早被判超限（两档都仍可 catch）。
///
/// TS: `vm.ts` 的 `maxStackSize` 注释——守卫必须在原生栈耗尽前触发。
#[tokio::test]
async fn max_stack_size_is_respected() {
    let mut harness = start(
        Options::new(
            "let depth = 0;\nfunction dive() { depth++; dive(); }\ntry { dive(); } catch (error) { return [error.name, depth]; }",
        )
        .nondeterministic()
        .stack(256 * 1024),
    )
    .await;
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(value[0], serde_json::json!("RangeError"));
    let depth = value[1].as_f64().expect("depth");
    assert!(
        depth > 500.0,
        "inline 的 256 KiB 守卫约 1000 层，实测 {depth}"
    );
}

/// `maxStackSize` 超过 `MAX_STACK_SIZE` 时 `start_vm` 直接失败（quickjs-wasi 的选项校验）。
///
/// TS: `normalizeOptions` 抛 `RangeError: maxStackSize must be an integer between 0 and
/// 524288`；`worker.ts:37` 用 `` `${error.name}: ${error.message}` `` 把它转成 `crash`。
/// `StartVmError` 因此必须同时暴露 name 与 message（只有 message 的调用方拼不出前缀）。
#[tokio::test]
async fn max_stack_size_above_the_limit_is_rejected() {
    let result = start_result(Options::new("return 1").stack(MAX_STACK_SIZE + 1)).await;
    match result {
        Ok(_) => panic!("expected a StartVmError"),
        Err(error) => {
            assert_eq!(error.name(), "RangeError");
            assert_eq!(
                error.message(),
                format!("maxStackSize must be an integer between 0 and {MAX_STACK_SIZE}")
            );
            assert_eq!(
                format!("{}: {}", error.name(), error.message()),
                format!(
                    "RangeError: maxStackSize must be an integer between 0 and {MAX_STACK_SIZE}"
                ),
                "worker.ts 的 crash 文案"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 输出项与 dispose 语义
// ---------------------------------------------------------------------------

/// `text()` / `console.*` / `image()` 走同一个 `bridge("output", …)` 分支。
///
/// TS: `vm.ts` 的 `bridge` `"output"` 分支。
#[tokio::test]
async fn output_items_reach_the_host() {
    let mut harness = start(Options::new(
        "text(\"hi\"); console.log(\"c\", 1); image(\"data:image/png;base64,iVBORw0KGgo=\"); return 1;",
    ))
    .await;
    let outputs = harness.outputs();
    let value = harness.value();
    harness.session.dispose();

    assert_eq!(outputs.len(), 3);
    assert_eq!(
        serde_json::to_value(&outputs[0]).unwrap(),
        serde_json::json!({"type": "text", "text": "hi"})
    );
    assert_eq!(
        serde_json::to_value(&outputs[1]).unwrap(),
        serde_json::json!({"type": "text", "text": "c 1", "console": true})
    );
    assert_eq!(
        serde_json::to_value(&outputs[2]).unwrap(),
        serde_json::json!({"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"})
    );
    assert_eq!(value, "1");
}

/// `dispose` 之后的 `deliver` 静默忽略，且可以重复 `dispose`。
///
/// TS: `vm.ts` 的 `deliver`/`dispose`——`if (disposed) return;`。
#[tokio::test]
async fn deliver_after_dispose_is_ignored() {
    let mut harness = start(Options::new("await tools.wait(1);")).await;
    let id = harness.calls()[0].0;
    harness.session.dispose();
    let before = harness.messages().len();
    harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT));
    harness.session.dispose();
    assert_eq!(
        harness.messages().len(),
        before,
        "disposed sessions ignore deliver"
    );
}

/// `store()` 的写集合以 JSON 数组文本随 `done` 返回（prelude 的 `serializeWrites`）。
///
/// TS: `sandbox.test.ts` 的 store 断言——`[["k","{\"a\":1}"],["gone"]]`。
#[tokio::test]
async fn store_writes_are_reported() {
    let mut harness = start(Options::new(
        "store(\"k\", {a: 1}); store(\"gone\", undefined); return 1",
    ))
    .await;
    let writes = match harness.done() {
        WorkerDoneOutcome::Ok(done) => done.writes,
        WorkerDoneOutcome::Error(done) => panic!("expected ok, got {}", done.error),
    };
    harness.session.dispose();
    assert_eq!(writes, "[[\"k\",\"{\\\"a\\\":1}\"],[\"gone\"]]");
}

/// `exit()` 让脚本成功结束且**没有**返回值（TS 的 `value` 是 `undefined`）。
///
/// TS: `vm.ts` 的 `done` 分支——`b === undefined || b.isUndefined ? undefined : b.toString()`。
#[tokio::test]
async fn exit_reports_success_without_a_value() {
    let mut harness = start(Options::new("exit(42)")).await;
    let done = harness.done();
    harness.session.dispose();
    match done {
        WorkerDoneOutcome::Ok(done) => {
            assert_eq!(done.value, None);
            assert_eq!(done.writes, "[]");
        }
        WorkerDoneOutcome::Error(done) => panic!("expected ok, got {}", done.error),
    }
}

/// 结果里的 `properties` 不是合法 JSON：`settle` 抛出，VM 转 `crash`（且不消耗 seq）。
///
/// TS: `vm.ts` 的 `apply`——`withScope` 里的 `callFunction` 抛出后由 `deliver` 的 catch 转
/// `post(crashMessage(error))`；`SyntaxError` 的文案来自 QuickJS 的 `JSON.parse`。
#[tokio::test]
async fn malformed_properties_crash_the_session() {
    let mut harness = start(Options::new(
        "try { await tools.echo(1); } catch (e) { return e.message; }",
    ))
    .await;
    let id = harness.calls()[0].0;
    harness
        .session
        .deliver(HostToWorkerMessage::Result(HostResult {
            id,
            ok: false,
            payload: Some("denied".into()),
            seq: Some(1),
            now: Some(STARTED_AT),
            properties: Some("{bad".into()),
        }));
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(
        crashes,
        vec![
            "SyntaxError: Expected property name or '}' in JSON at position 1 (line 1 column 2)"
                .to_string()
        ]
    );
}

/// `now` 不是有限数时被忽略（TS 的 `Number.isFinite(next.now)` 判断）。
///
/// TS: `vm.ts` 的 `if (next.now !== undefined && Number.isFinite(next.now)) clock.now = …`。
/// 注意 `deliver` 本身不校验 `now`：`NaN` 在 TS 里只是被跳过，不是错误（「not a finite
/// number」的报错在 `execution.ts` 的 `deliver` 守卫里，不在 VM 层）。
#[tokio::test]
async fn non_finite_now_is_ignored() {
    let mut harness = start(Options::new(
        "await tools.wait(1); return [Date.now(), performance.now()];",
    ))
    .await;
    let id = harness.calls()[0].0;
    harness.deliver_ok(id, "null", Some(1), Some(f64::NAN));
    let value = harness.json_value();
    harness.session.dispose();
    assert_eq!(num(&value[0]), STARTED_AT, "NaN now leaves the clock alone");
    assert_eq!(num(&value[1]), 0.0);
}

/// 会话必须能跨线程移动：worker 后端把 VM 放在自己的线程里，launcher 持有 `VmSession`。
///
/// TS 侧是 `postMessage` 的 structured clone；Rust 侧的等价约束是 `Send`。`wasmtime::Store`
/// 在 `T: Send` 时是 `Send`，这条断言把它钉住。
#[test]
fn session_is_send() {
    fn assert_send<T: Send>(_: std::marker::PhantomData<T>) {}
    assert_send::<VmSession>(std::marker::PhantomData);
}

// ---------------------------------------------------------------------------
// 审查回归：VM 层的 6 条 finding（golden 值全部取自 TS `startVm` 的真实输出）
// ---------------------------------------------------------------------------

/// finding 1（critical）：wasm trap 必须变成 `crash`，不能被吞掉当成 `done`。
///
/// TS 在 40000 层 `JSON.stringify` 上稳定报 `RangeError: Maximum call stack size exceeded`
/// （20500 已经在边界上：同一份 TS 实现两次跑结果不同，回归取 40000）。修复前 `raw_call`
/// 把 trap 当成普通返回值，drain 继续跑并报了一个「成功」的 `done`。
#[tokio::test]
async fn wasm_trap_becomes_a_crash() {
    let mut harness = start(Options::new(
        "let v=1;for(let i=0;i<40000;i++)v=[v];try{return ['ok',JSON.stringify(v).length]}catch(e){return ['caught',e.name,e.message]}",
    ))
    .await;
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();

    assert_eq!(types, ["started", "crash"], "trap 之后不能再报 done");
    assert_eq!(
        crashes,
        vec!["RangeError: Maximum call stack size exceeded".to_string()]
    );
}

/// finding 1（critical）+ finding 4（major）：TS 能跑的深度不能被 wasm 栈上限误杀。
///
/// TS 实测：9000 层 → 18001、12000 层 → 24001（`done, ok`），远低于它 2 万层左右的崩溃
/// 边界。旧实现把 `max_wasm_stack` 留在 2 MiB，9000 层就 trap 成「crash」。
#[tokio::test]
async fn deep_json_stringify_is_accepted_below_the_ts_limit() {
    for (depth, expected) in [(9000usize, "18001"), (12000, "24001")] {
        let mut harness = start(Options::new(&format!(
            "let v=1;for(let i=0;i<{depth};i++)v=[v];return JSON.stringify(v).length"
        )))
        .await;
        let value = harness.value();
        let types = harness.types();
        harness.session.dispose();
        assert_eq!(value, expected, "depth {depth}");
        assert_eq!(types, ["started", "done", "idle"], "depth {depth}");
    }
}

/// 审查回归（major）：worker 后端的 wasm 栈预算与 TS 的 4 MB worker 线程逐点对齐。
///
/// TS worker 实测：嵌套字面量 2600 层 `done`、2800 层 crash；`JSON.stringify` 22000 层
/// `done`、23000 层 crash。旧配置（5 MiB）在 2600 / 21000 层就 trap，比 TS 早 7–9%；新预算
/// （`WASM_STACK_SIZE = 5 650 000`）落在同一组阈值上。测试取两侧各有约 5% 余量的点
/// （2600 成功 / 2900 crash，21000 成功 / 24000 crash）：既能抓住预算被改小的回归，也不会
/// 因为入口栈的几十字节偏移而抖动。
#[tokio::test]
async fn worker_wasm_stack_budget_matches_the_ts_worker_thread() {
    let nested = |depth: usize| format!("return {}1{}", "[".repeat(depth), "]".repeat(depth));
    let stringify = |depth: usize| {
        format!("let v=1;for(let i=0;i<{depth};i++)v=[v];return JSON.stringify(v).length")
    };

    // 成功的一侧：TS worker 在这些深度上 `done`。
    let mut harness = start(Options::new(&nested(2600))).await;
    let types = harness.types();
    harness.session.dispose();
    assert_eq!(types, ["started", "done", "idle"], "source nesting 2600");

    let mut harness = start(Options::new(&stringify(21000))).await;
    let value = harness.value();
    let types = harness.types();
    harness.session.dispose();
    assert_eq!(value, "42001", "stringify 21000");
    assert_eq!(types, ["started", "done", "idle"], "stringify 21000");

    // 失败的一侧：TS worker 在这里已经耗尽 4 MB 原生栈。
    let mut harness = start(Options::new(&nested(2900))).await;
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(types, ["started", "crash"], "source nesting 2900");
    assert_eq!(
        crashes,
        vec!["RangeError: Maximum call stack size exceeded".to_string()],
        "source nesting 2900"
    );

    let mut harness = start(Options::new(&stringify(24000))).await;
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(types, ["started", "crash"], "stringify 24000");
    assert_eq!(
        crashes,
        vec!["RangeError: Maximum call stack size exceeded".to_string()],
        "stringify 24000"
    );
}

/// finding 1（critical）：深递归成功时 `store()` 的写集合照常随 `done` 上报。
///
/// TS: `store('a','b')` 先写、再做 9000 层 `JSON.stringify` → `value: "18001"`、
/// `writes: [["a","\"b\""]]`。trap 被吞掉时这条路径会报出错误的 `done`。
#[tokio::test]
async fn deep_json_stringify_keeps_store_writes() {
    let mut harness = start(Options::new(
        "let v=1;for(let i=0;i<9000;i++)v=[v];store('a','b');return JSON.stringify(v).length",
    ))
    .await;
    let done = harness.done();
    harness.session.dispose();
    match done {
        WorkerDoneOutcome::Ok(done) => {
            assert_eq!(done.value.as_deref(), Some("18001"));
            assert_eq!(done.writes, "[[\"a\",\"\\\"b\\\"\"]]");
        }
        WorkerDoneOutcome::Error(done) => panic!("expected ok, got {}", done.error),
    }
}

/// finding 1（critical）：`eval` 与 promise job（drain）里的 trap 也必须变成 crash。
///
/// TS 实测：2400 层嵌套源码正常结束，4000 层源码是 `RangeError: Maximum call stack size
/// exceeded`；脚本内 4000 层 `eval` 同样 crash；微任务里 40000 层 `JSON.stringify` 也是
/// crash（不是 `stalled`，也不是 `done`）。
#[tokio::test]
async fn deep_parser_and_microtask_recursion_are_crashes() {
    let nested = |depth: usize| format!("return {}1{}", "[".repeat(depth), "]".repeat(depth));

    // 源码解析（host 的 evalCode → `Exports::eval`）：TS 能接受的 2400 层。
    let mut harness = start(Options::new(&nested(2400))).await;
    assert_eq!(
        harness.types(),
        ["started", "done", "idle"],
        "源码嵌套 2400"
    );
    harness.session.dispose();

    // 同一路径的 4000 层：trap → crash。
    let mut harness = start(Options::new(&nested(4000))).await;
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(types, ["started", "crash"], "源码嵌套 4000");
    assert_eq!(
        crashes,
        vec!["RangeError: Maximum call stack size exceeded".to_string()]
    );

    // 脚本内的 JS `eval`：4000 层同样 crash（QuickJS 的 C 递归）。
    let mut harness = start(Options::new(
        "let s='';for(let i=0;i<4000;i++)s+='[';s+='1';for(let i=0;i<4000;i++)s+=']';try{return ['ok',eval(s).length]}catch(e){return ['caught',e.name,e.message]}",
    ))
    .await;
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(types, ["started", "crash"], "script eval 4000");
    assert_eq!(
        crashes,
        vec!["RangeError: Maximum call stack size exceeded".to_string()]
    );

    // 微任务（drain 的 `executePendingJobs` 路径）：40000 层。
    let mut harness = start(Options::new(
        "let v=1;for(let i=0;i<40000;i++)v=[v];await null;try{return ['ok',JSON.stringify(v).length]}catch(e){return ['caught',e.name,e.message]}",
    ))
    .await;
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(types, ["started", "crash"], "微任务 40000");
    assert_eq!(
        crashes,
        vec!["RangeError: Maximum call stack size exceeded".to_string()]
    );
}

/// finding 2（critical）：`BigInt(Math.round(ms)) * 1_000_000n` 的精确回绕。
///
/// golden 值来自 TS `startVm`：`startedAt` 与结果的 `now` 取 `Math.max` 后喂给 WASI
/// `clock_time_get`，再由 `Date.now()` / `performance.now()` 读回。
/// - `startedAt = 1e33`：`Date.now()` 与 `performance.now()` 都是 0（乘积是 2^64 的倍数）；
/// - `startedAt = 1e19`：`Date.now()` 是 1590897978359（不是饱和到 u64::MAX）；
/// - `startedAt = -1e19`：`Date.now()` 用更大的 `now`（1768471200000），
///   `performance.now()` 是 3359369178103.4146（负差值的精确回绕）。
#[tokio::test]
async fn clock_nanoseconds_wrap_like_the_ts_bigint_expression() {
    let code = "const a = await tools.echo('x'); return [a, Date.now(), performance.now()];";
    for (started_at, date_now, performance_now) in [
        (1e33, 0.0, 0.0),
        (1e19, 1_590_897_978_359.0, 0.0),
        (-1e19, STARTED_AT, 3_359_369_178_103.414_6),
    ] {
        let mut harness = start(Options::new(code).started_at(started_at)).await;
        let id = harness.calls()[0].0;
        harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT));
        let value = harness.json_value();
        harness.session.dispose();
        assert_eq!(value[0], serde_json::Value::Null);
        assert_eq!(
            num(&value[1]),
            date_now,
            "startedAt {started_at}: Date.now()"
        );
        assert_eq!(
            num(&value[2]),
            performance_now,
            "startedAt {started_at}: performance.now()"
        );
    }
}

/// finding 5（minor）：非有限的 `startedAt` 让时钟读抛 `RangeError`，VM 报 crash。
///
/// TS: `BigInt(Math.round(NaN))` 抛 `RangeError: The number NaN cannot be converted to a
/// BigInt because it is not an integer`（`±Infinity` 同文案）。修复前宿主函数把错误压成
/// 空值，时钟读返回 0。
#[tokio::test]
async fn non_finite_started_at_crashes_with_the_ts_range_error() {
    for (started_at, text) in [
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
    ] {
        let mut harness = start(Options::new("return Date.now()").started_at(started_at)).await;
        let types = harness.types();
        let crashes = harness.crashes();
        harness.session.dispose();
        assert_eq!(types, ["started", "crash"], "startedAt {text}");
        assert_eq!(
            crashes,
            vec![format!(
                "RangeError: The number {text} cannot be converted to a BigInt because it is not an integer"
            )]
        );
    }
}

/// finding（minor）：`Math.max(NaN, finite)` 保持 NaN——`deliver` 不能把 NaN 时钟救回有限值。
///
/// TS: `vm.ts:218` 的 `Math.max(clock.now, next.now)` 与 `vm.ts:111` 的
/// `BigInt(Math.round(clock.now))`。`startedAt = NaN` 时，脚本先 `await`，让一条 `now`
/// 有限的投递经过 `Math.max`；TS 的 `Math.max(NaN, 1768471200000)` 仍是 `NaN`，随后的
/// 时钟读抛 `RangeError`。Rust 的 `f64::max(NaN, x)` 返回 `x`（修复前这里会返回 done）。
///
/// golden 来自 TS `startVm` 的实测：消息序列 `started`、`call`、`idle(0)`、`crash`。
#[tokio::test]
async fn nan_started_at_survives_a_finite_delivered_now() {
    let mut harness =
        start(Options::new("await tools.wait(1); return Date.now();").started_at(f64::NAN)).await;
    let id = harness.calls()[0].0;
    harness.deliver_ok(id, "null", Some(1), Some(STARTED_AT));
    let types = harness.types();
    let crashes = harness.crashes();
    harness.session.dispose();
    assert_eq!(types, ["started", "call", "idle", "crash"]);
    assert_eq!(
        crashes,
        vec![
            "RangeError: The number NaN cannot be converted to a BigInt because it is not an integer"
                .to_string()
        ]
    );
}

/// finding 6（minor）：`startVm` 的 try 块失败后 `deliver` 必须 crash 成 TS 的 `TypeError`。
///
/// TS: prelude 因堆上限失败 → 第一次 `crash`（1024 字节是 `InternalError: out of memory`，
/// 200000 字节是 `<null>: <null>`）；随后 `deliver` 读 `settle.ptr`：
/// `TypeError: Cannot read properties of undefined (reading 'ptr')`。
#[tokio::test]
async fn deliver_after_a_failed_startup_crashes_with_the_ts_type_error() {
    for (heap, startup_crash) in [
        (1024.0, "InternalError: out of memory"),
        (200_000.0, "<null>: <null>"),
    ] {
        let mut harness = start(Options::new("return 1").heap(heap)).await;
        assert_eq!(
            harness.crashes(),
            vec![startup_crash.to_string()],
            "heap {heap} 的启动失败"
        );
        harness.deliver_ok(1, "null", Some(1), Some(STARTED_AT));
        let crashes = harness.crashes();
        harness.session.dispose();
        assert_eq!(
            crashes,
            vec![
                startup_crash.to_string(),
                "TypeError: Cannot read properties of undefined (reading 'ptr')".to_string(),
            ],
            "heap {heap}: 启动失败后的 deliver"
        );
    }
}

/// finding 3（major）：WTF-8 的孤立代理项只产生**一个** U+FFFD，而不是三个。
///
/// TS 的 `text()` 原样送出 `a\ud800b`（JS 字符串可以带孤立代理项）；Rust 的 `String` 不能，
/// 这里的策略是每个非法码元一个 U+FFFD（`decode_wtf8`，ARCHITECTURE §8 第 31 条同类决定）。
/// 合法代理对（😀）必须原样保留。`JSON.stringify` 在 VM 内把代理项转义成 ASCII `\ud800`，
/// 所以 `done` 的返回值以 JSON 转义文本返回，不受 Rust `String` 无法表示孤立代理项的限制。
#[tokio::test]
async fn lone_surrogates_decode_to_one_replacement_character() {
    let mut harness = start(Options::new(
        "text('a'+String.fromCharCode(0xD800)+'b'); text(String.fromCharCode(0xD83D,0xDE00)); text('😀'); return JSON.stringify('a'+String.fromCharCode(0xD800)+'b')",
    ))
    .await;
    let outputs = harness.outputs();
    let value = harness.value();
    harness.session.dispose();
    let texts: Vec<serde_json::Value> = outputs
        .iter()
        .map(|item| serde_json::to_value(item).expect("output item"))
        .collect();
    assert_eq!(
        texts[0],
        serde_json::json!({"type": "text", "text": "a\u{FFFD}b"}),
        "孤立代理项 → 恰好一个 U+FFFD"
    );
    assert_eq!(
        texts[1],
        serde_json::json!({"type": "text", "text": "😀"}),
        "代理对原样保留"
    );
    assert_eq!(texts[2], serde_json::json!({"type": "text", "text": "😀"}));
    // prelude 先 `JSON.stringify` 结果，所以这里多一层引号/转义：TS 的 done.value 是 15 个码元
    // `"\"a\\ud800b\""`（孤立代理项已被 VM 内的 `JSON.stringify` 转义成 ASCII）。
    assert_eq!(
        value, "\"\\\"a\\\\ud800b\\\"\"",
        "VM 内的 JSON.stringify 仍把代理项转义成 \\ud800"
    );
}
