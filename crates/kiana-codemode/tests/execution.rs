//! `runtime/execution.ts` 的移植测试（假 launcher，不需要 VM）。
//!
//! TS 侧这些断言分散在 `sandbox.test.ts` 的 `worker host` 组（坏桥接 7 例、`--watch` 过滤、
//! 缺失 worker 文件）与 `deterministic.test.ts`（`deliver()` 的四类校验、`during`、`calls`
//! 记录）。这里用一个记录消息的假 [`VmLauncher`] 直接驱动宿主侧，因此可以在默认 feature
//! （无 wasm）下运行，并逐字锁定 spec §3.4 的错误文本。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_codemode::runtime::execution::{
    CodemodeSandboxBase, SandboxClosed, VmChannel, VmEvents, VmLauncher,
};
use kiana_codemode::runtime::protocol::{CallTarget, HostResult, HostToWorkerMessage, VmStartData};
use kiana_codemode::types::{CodemodeOutcomeErr, CodemodeOutcomeOk};
use kiana_codemode::{
    CodemodeCallStatus, CodemodeDeclaredGlobal, CodemodeDeclaredTool, CodemodeExecuteOptions,
    CodemodeOutcome, CodemodeResult, CodemodeRunOptions, CodemodeSandboxBaseOptions,
    CodemodeStoreWrites, CodemodeTool, CodemodeToolContext,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// 假 launcher：记录宿主发出的消息，暴露 events 让测试注入 VM 消息。
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeChannel {
    sent: Mutex<Vec<HostToWorkerMessage>>,
    stops: AtomicUsize,
}

impl FakeChannel {
    fn sent(&self) -> Vec<HostToWorkerMessage> {
        self.sent.lock().unwrap().clone()
    }

    fn results(&self) -> Vec<HostResult> {
        self.sent()
            .into_iter()
            .map(|message| match message {
                HostToWorkerMessage::Result(result) => result,
            })
            .collect()
    }
}

impl VmChannel for FakeChannel {
    fn post(&self, message: HostToWorkerMessage) {
        self.sent.lock().unwrap().push(message);
    }

    fn stop(&self) -> kiana_codemode::types::BoxFuture<'static, ()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}

struct FakeLauncher {
    channel: Arc<FakeChannel>,
    events: Mutex<Option<Arc<dyn VmEvents>>>,
    data: Mutex<Option<VmStartData>>,
    fail: Option<String>,
}

impl FakeLauncher {
    fn new() -> Arc<FakeLauncher> {
        Arc::new(FakeLauncher {
            channel: Arc::new(FakeChannel::default()),
            events: Mutex::new(None),
            data: Mutex::new(None),
            fail: None,
        })
    }

    fn failing(message: &str) -> Arc<FakeLauncher> {
        Arc::new(FakeLauncher {
            channel: Arc::new(FakeChannel::default()),
            events: Mutex::new(None),
            data: Mutex::new(None),
            fail: Some(message.to_string()),
        })
    }

    fn launcher(self: &Arc<Self>) -> VmLauncher {
        let this = Arc::clone(self);
        Arc::new(move |data: VmStartData, events: Arc<dyn VmEvents>| {
            let this = Arc::clone(&this);
            Box::pin(async move {
                *this.data.lock().unwrap() = Some(data);
                *this.events.lock().unwrap() = Some(events);
                match &this.fail {
                    Some(message) => Err(message.clone()),
                    None => Ok(Arc::clone(&this.channel) as Arc<dyn VmChannel>),
                }
            })
        })
    }

    async fn events(&self) -> Arc<dyn VmEvents> {
        for _ in 0..10_000 {
            let events = self.events.lock().unwrap().clone();
            if let Some(events) = events {
                // 让 launcher 的任务继续跑完（安装 channel，冲洗排队回复）。
                tokio::task::yield_now().await;
                return events;
            }
            tokio::task::yield_now().await;
        }
        panic!("the launcher was never called");
    }
}

fn base_with(
    launcher: &Arc<FakeLauncher>,
    options: CodemodeSandboxBaseOptions,
) -> Arc<CodemodeSandboxBase> {
    Arc::new(CodemodeSandboxBase::new(options, launcher.launcher()).expect("valid options"))
}

fn echo_tool(name: &str) -> CodemodeTool {
    CodemodeTool::new(
        name,
        Arc::new(|args: Value, _context: CodemodeToolContext| Box::pin(async move { Ok(args) })),
    )
}

fn record_tool(name: &str, seen: Arc<Mutex<Vec<Value>>>) -> CodemodeTool {
    CodemodeTool::new(
        name,
        Arc::new(move |args: Value, _context: CodemodeToolContext| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                seen.lock().unwrap().push(args.clone());
                Ok(args)
            })
        }),
    )
}

fn defaults() -> CodemodeSandboxBaseOptions {
    CodemodeSandboxBaseOptions {
        timeout_ms: Some(f64::INFINITY),
        ..Default::default()
    }
}

fn execute_options() -> CodemodeExecuteOptions {
    CodemodeExecuteOptions::default()
}

fn run_options() -> CodemodeRunOptions {
    CodemodeRunOptions {
        started_at: 1_700_000_000_000.0,
        seed: 1.0,
        timeout_ms: Some(f64::INFINITY),
        // TS 的 `onCall` 是必填字段；没有它时任何 call 都会抛
        // `control.onCall is not a function`（回归见 `start_without_on_call_fails_the_host`）。
        on_call: Some(Arc::new(|_call: kiana_codemode::CodemodeHostCall| {})),
        ..Default::default()
    }
}

/// 直接从未来的结果里取 `Err`。
/// 等到 channel 收到 `stop()`（`finish` 在独立任务里请求停止）。
async fn wait_for_stops(channel: &FakeChannel, expected: usize) {
    for _ in 0..1000 {
        if channel.stops.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!(
        "expected {expected} stop() calls, saw {}",
        channel.stops.load(Ordering::SeqCst)
    );
}

fn error_of(result: &CodemodeResult) -> &kiana_codemode::CodemodeError {
    match result {
        CodemodeResult::Err(error) => &error.error,
        CodemodeResult::Ok(_) => panic!("expected a failed result, got {result:?}"),
    }
}

// ---------------------------------------------------------------------------
// 桥接解码（spec §7.7 的 `reports a broken bridge as a sandbox error` 7 例）
// ---------------------------------------------------------------------------

const BRIDGE_SUFFIX: &str =
    ". The script may have modified built-ins such as a prototype's toJSON.";

async fn bridge_failure(payload: Value) -> String {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(payload);
    let result = result.await.expect("not closed");
    assert!(!result.ok());
    error_of(&result).message.clone()
}

#[tokio::test]
async fn store_writes_must_be_an_array() {
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":true,"value":"1","writes":"null"})).await,
        format!("Sandbox bridge broken: store writes are not an array{BRIDGE_SUFFIX}")
    );
}

#[tokio::test]
async fn store_write_entries_must_be_well_formed() {
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":true,"value":"1","writes":"[1]"})).await,
        format!("Sandbox bridge broken: store writes contain a malformed entry{BRIDGE_SUFFIX}")
    );
    // 长度 3、键非字符串、值非字符串（长度为 2 时）都是同一条消息。
    for writes in [r#"[[1,"2"]]"#, r#"[["k","v",3]]"#, r#"[["k",1]]"#, "[[]]"] {
        assert_eq!(
            bridge_failure(json!({"type":"done","ok":true,"value":"1","writes":writes})).await,
            format!("Sandbox bridge broken: store writes contain a malformed entry{BRIDGE_SUFFIX}"),
            "writes = {writes}"
        );
    }
}

#[tokio::test]
async fn store_values_must_be_json() {
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":true,"value":"1","writes":"[[\"k\", \"{\"]]"}))
            .await,
        format!("Sandbox bridge broken: store value for \"k\" is not valid JSON{BRIDGE_SUFFIX}")
    );
    // `JSON.stringify(key)` 的引号与转义。
    assert_eq!(
        bridge_failure(
            json!({"type":"done","ok":true,"value":"1","writes":"[[\"a\\\"b\", \"{\"]]"})
        )
        .await,
        format!(
            "Sandbox bridge broken: store value for \"a\\\"b\" is not valid JSON{BRIDGE_SUFFIX}"
        )
    );
}

#[tokio::test]
async fn return_value_must_be_json() {
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":true,"value":"{","writes":"[]"})).await,
        format!("Sandbox bridge broken: return value is not valid JSON{BRIDGE_SUFFIX}")
    );
}

#[tokio::test]
async fn script_errors_are_decoded() {
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":false,"error":"5"})).await,
        format!("Sandbox bridge broken: script error is not an object{BRIDGE_SUFFIX}")
    );
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":false,"error":"{}"})).await,
        format!("Sandbox bridge broken: script error is malformed{BRIDGE_SUFFIX}")
    );
    // `message` 必须是字符串；`name`/`stack` 若存在也必须是字符串。
    for error in [
        r#"{"message":5}"#,
        r#"{"message":"m","name":1}"#,
        r#"{"message":"m","stack":[]}"#,
        r#"[]"#,
    ] {
        assert_eq!(
            bridge_failure(json!({"type":"done","ok":false,"error":error})).await,
            format!("Sandbox bridge broken: script error is malformed{BRIDGE_SUFFIX}"),
            "error = {error}"
        );
    }
}

#[tokio::test]
async fn unknown_messages_are_bridge_errors() {
    assert_eq!(
        bridge_failure(json!({"type":"nonsense"})).await,
        format!("Sandbox bridge broken: unknown message from the worker{BRIDGE_SUFFIX}")
    );
    // 非对象（字符串/数字/null）同样落到 `unknown message`。
    for payload in [json!("done"), json!(7), json!(null), json!([])] {
        assert_eq!(
            bridge_failure(payload.clone()).await,
            format!("Sandbox bridge broken: unknown message from the worker{BRIDGE_SUFFIX}"),
            "payload = {payload}"
        );
    }
}

#[tokio::test]
async fn duplicate_call_ids_are_bridge_errors() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"null"}));
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"null"}));
    let result = result.await.expect("not closed");
    assert_eq!(
        error_of(&result).message,
        format!("Sandbox bridge broken: duplicate call id 1{BRIDGE_SUFFIX}")
    );
}

#[tokio::test]
async fn a_bridge_error_keeps_the_output_produced_so_far() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"output","item":{"type":"text","text":"partial"}}));
    events.message(json!({"type":"nonsense"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Err(error) = &result else {
        panic!("expected a failure");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(
        error.output,
        vec![kiana_codemode::CodemodeOutputItem::text("partial")]
    );
}

// ---------------------------------------------------------------------------
// 正常路径：done / output / call / crash / launcher 失败
// ---------------------------------------------------------------------------

#[tokio::test]
async fn successful_done_decodes_value_and_store_writes() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(
        json!({"type":"done","ok":true,"value":"{\"a\":1}","writes":"[[\"k\",\"2\"],[\"gone\"]]"}),
    );
    let result = result.await.expect("not closed");
    assert_eq!(
        result,
        CodemodeResult::success(
            Some(json!({"a": 1})),
            vec![],
            vec![],
            CodemodeStoreWrites {
                set: indexmap::IndexMap::from([("k".to_string(), json!(2))]),
                delete: vec!["gone".to_string()],
            },
            None,
        )
    );
    // `finish` 请求了 channel 停止。
    wait_for_stops(&launcher.channel, 1).await;
}

#[tokio::test]
async fn done_without_a_value_is_undefined() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(ok.store_writes, CodemodeStoreWrites::default());
}

#[tokio::test]
async fn script_failures_carry_name_message_and_stack() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("boom", execute_options());
    let events = launcher.events().await;
    events.message(json!({
        "type": "done",
        "ok": false,
        "error": "{\"name\":\"TypeError\",\"message\":\"boom 1\",\"stack\":\"TypeError: boom 1\\n    at codemode.js:2:31\"}",
        "during": {"seq": 2, "issued": 3}
    }));
    let result = result.await.expect("not closed");
    let CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.error.kind, kiana_codemode::CodemodeErrorKind::Script);
    assert_eq!(error.error.name.as_deref(), Some("TypeError"));
    assert_eq!(error.error.message, "boom 1");
    assert_eq!(
        error.error.stack.as_deref(),
        Some("TypeError: boom 1\n    at codemode.js:2:31")
    );
    assert_eq!(
        error.during,
        Some(kiana_codemode::CodemodeDuring { seq: 2, issued: 3 })
    );
}

#[tokio::test]
async fn output_messages_are_collected_in_order() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"output","item":{"type":"text","text":"a"}}));
    events.message(json!({"type":"output","item":{"type":"text","text":"b","console":true}}));
    events.message(
        json!({"type":"output","item":{"type":"image","data":"AA==","mimeType":"image/png"}}),
    );
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(
        ok.output,
        vec![
            kiana_codemode::CodemodeOutputItem::text("a"),
            kiana_codemode::CodemodeOutputItem::console("b"),
            kiana_codemode::CodemodeOutputItem::image("AA==", "image/png"),
        ]
    );
}

#[tokio::test]
async fn tool_calls_run_on_the_host_and_answer_the_vm() {
    let launcher = FakeLauncher::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![record_tool("echo", Arc::clone(&seen))]),
            ..defaults()
        },
    );
    let result = base.execute("await tools.echo({a:1})", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":4,"target":"tool","name":"echo","args":"{\"a\":1}"}));
    // 工具在宿主任务里跑：等它把回答投递回来。
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(*seen.lock().unwrap(), vec![json!({"a": 1})]);
    assert_eq!(ok.calls.len(), 1);
    assert_eq!(ok.calls[0].name, "echo");
    assert_eq!(ok.calls[0].status, CodemodeCallStatus::Ok);
    let replies = launcher.channel.results();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].id, 4);
    assert!(replies[0].ok);
    assert_eq!(replies[0].payload.as_deref(), Some("{\"a\":1}"));
    // 非确定性模式不带 seq/now/properties。
    assert_eq!(replies[0].seq, None);
    assert_eq!(replies[0].now, None);
    assert_eq!(replies[0].properties, None);
}

#[tokio::test]
async fn unknown_tools_become_tool_errors() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("await tools.nope()", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"nope","args":"null"}));
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let replies = launcher.channel.results();
    assert_eq!(replies.len(), 1);
    assert!(!replies[0].ok);
    assert_eq!(replies[0].payload.as_deref(), Some("Unknown tool \"nope\""));
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(ok.calls[0].status, CodemodeCallStatus::Error);
}

#[tokio::test]
async fn unknown_globals_become_global_errors() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("attach()", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":2,"target":"global","name":"attach","args":"null"}));
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let replies = launcher.channel.results();
    assert_eq!(
        replies[0].payload.as_deref(),
        Some("Unknown global \"attach\"")
    );
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    // 全局调用不记账。
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert!(ok.calls.is_empty());
}

/// TS: `const isTool = message.target === "tool"`（`handleCall`）。判定的是**相等**，缺失、
/// `null`、任何其它字符串都落到 globals 一侧，`tools.get(name)` 根本不会被查。node 26 实测：
/// 同时注册工具与同名全局时，`target:"weird"` 走全局（工具不被调用）。
#[tokio::test]
async fn call_targets_other_than_tool_route_to_globals() {
    let launcher = FakeLauncher::new();
    let tool_seen = Arc::new(Mutex::new(Vec::new()));
    let global_seen = Arc::new(Mutex::new(Vec::new()));
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![record_tool("echo", Arc::clone(&tool_seen))]),
            globals: Some(vec![record_tool("echo", Arc::clone(&global_seen))]),
            ..defaults()
        },
    );
    let result = base.execute("echo(1)", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"weird","name":"echo","args":"1"}));
    events.message(json!({"type":"call","id":2,"name":"echo","args":"2"}));
    events.message(json!({"type":"call","id":3,"target":null,"name":"echo","args":"3"}));
    events.message(json!({"type":"call","id":4,"target":"global","name":"echo","args":"4"}));
    for _ in 0..1000 {
        if launcher.channel.results().len() == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let replies = launcher.channel.results();
    assert_eq!(replies.len(), 4, "replies: {replies:?}");
    let by_id: std::collections::BTreeMap<u64, (bool, Option<String>)> = replies
        .iter()
        .map(|reply| (reply.id, (reply.ok, reply.payload.clone())))
        .collect();
    for id in 1..=4u64 {
        let (ok, payload) = by_id.get(&id).expect("reply for every call");
        assert!(*ok, "call {id} must succeed via the global: {payload:?}");
        assert_eq!(payload.as_deref(), Some(id.to_string().as_str()));
    }
    assert_eq!(
        *global_seen.lock().unwrap(),
        vec![json!(1), json!(2), json!(3), json!(4)],
        "all four calls must reach the global"
    );
    let tool_calls = tool_seen.lock().unwrap().clone();
    assert!(
        tool_calls.is_empty(),
        "the tool must not run: {tool_calls:?}"
    );
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    // 只有 `target:"tool"` 才记账；全局调用不记账。
    assert!(ok.calls.is_empty());
}

#[tokio::test]
async fn malformed_call_arguments_become_tool_errors_not_bridge_errors() {
    let launcher = FakeLauncher::new();
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![echo_tool("echo")]),
            ..defaults()
        },
    );
    let result = base.execute("await tools.echo(", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"{"}));
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let replies = launcher.channel.results();
    assert!(!replies[0].ok);
    // V8 的错误文本（`kiana-json` 的 `JSON.parse` 面）。
    assert!(replies[0].payload.as_deref().unwrap().contains("JSON"));
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    result.await.expect("not closed");
}

#[tokio::test]
async fn crashes_are_sandbox_errors() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"crash","message":"InternalError: out of memory"}));
    let result = result.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.message, "InternalError: out of memory");
}

#[tokio::test]
async fn launcher_failures_become_sandbox_errors_with_their_message() {
    let launcher = FakeLauncher::failing("worker capacity exhausted");
    let base = base_with(&launcher, defaults());
    let result = base
        .execute("return 1", execute_options())
        .await
        .expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.message, "worker capacity exhausted");
}

#[tokio::test]
async fn a_launcher_failure_carries_the_output_so_far() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"output","item":{"type":"text","text":"partial"}}));
    // launcher 已经返回成功；这里只验证失败路径也保留输出（用 crash 触发终态）。
    events.message(json!({"type":"crash","message":"boom"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Err(error) = &result else {
        panic!("expected failure");
    };
    assert_eq!(error.output.len(), 1);
}

/// launcher 的 future 在 `release()` 之前不结算，用来制造「执行先结束、channel 后装好」的时序。
struct DeferredLauncher {
    channel: Arc<FakeChannel>,
    events: Mutex<Option<Arc<dyn VmEvents>>>,
    release: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    wait: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl DeferredLauncher {
    fn new() -> Arc<DeferredLauncher> {
        let (release, wait) = tokio::sync::oneshot::channel();
        Arc::new(DeferredLauncher {
            channel: Arc::new(FakeChannel::default()),
            events: Mutex::new(None),
            release: Mutex::new(Some(release)),
            wait: Mutex::new(Some(wait)),
        })
    }

    fn launcher(self: &Arc<Self>) -> VmLauncher {
        let this = Arc::clone(self);
        Arc::new(move |_data: VmStartData, events: Arc<dyn VmEvents>| {
            let this = Arc::clone(&this);
            Box::pin(async move {
                *this.events.lock().unwrap() = Some(events);
                let wait = this.wait.lock().unwrap().take().expect("launched once");
                let _ = wait.await;
                Ok(Arc::clone(&this.channel) as Arc<dyn VmChannel>)
            })
        })
    }

    fn release(&self) {
        self.release
            .lock()
            .unwrap()
            .take()
            .expect("release once")
            .send(())
            .expect("launcher is waiting");
    }

    async fn events(&self) -> Arc<dyn VmEvents> {
        for _ in 0..10_000 {
            let events = self.events.lock().unwrap().clone();
            if let Some(events) = events {
                return events;
            }
            tokio::task::yield_now().await;
        }
        panic!("the launcher was never called");
    }
}

/// TS 的 `launch(data, events).then(channel => { if (finished) channel.stop(); ... })`：执行在
/// launcher 返回之前就结束后，晚到的 channel 只会收到一次 `stop()`，`finish()` 已清空
/// `unsent`，因此不会发出任何消息。
#[tokio::test]
async fn a_channel_installed_after_the_execution_finished_is_stopped_not_used() {
    let launcher = DeferredLauncher::new();
    let base =
        Arc::new(CodemodeSandboxBase::new(defaults(), launcher.launcher()).expect("valid options"));
    let result = base.execute("", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"value":"1","writes":"[]"}));
    let result = result.await.expect("not closed");
    assert!(
        matches!(result, CodemodeResult::Ok(_)),
        "result: {result:?}"
    );
    launcher.release();
    wait_for_stops(&launcher.channel, 1).await;
    assert!(
        launcher.channel.sent().is_empty(),
        "no reply may be posted to a channel that arrived after finish: {:?}",
        launcher.channel.sent()
    );
    assert_eq!(launcher.channel.stops.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// abort / timeout / close
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aborting_the_signal_ends_the_execution() {
    let launcher = FakeLauncher::new();
    let hang = CodemodeTool::new(
        "hang",
        Arc::new(|_args: Value, context: CodemodeToolContext| {
            Box::pin(async move {
                context.signal.cancelled().await;
                Err("cancelled".to_string())
            })
        }),
    );
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![hang]),
            ..defaults()
        },
    );
    let token = CancellationToken::new();
    let result = base.execute(
        "text('partial'); await tools.hang()",
        CodemodeExecuteOptions {
            signal: Some(token.clone()),
            ..execute_options()
        },
    );
    let events = launcher.events().await;
    events.message(json!({"type":"output","item":{"type":"text","text":"partial"}}));
    events.message(json!({"type":"call","id":1,"target":"tool","name":"hang","args":"null"}));
    token.cancel();
    let result = result.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Aborted);
    // TS: `controller.abort()`（无参）在 Node/Bun 上把 `reason` 设成 `message` 为
    // `This operation was aborted` 的 DOMException，而 `DOMException instanceof Error` 为真，
    // 所以 `onAbort` 取到的是这条 message（node 26 实测）。
    assert_eq!(error.message, "This operation was aborted");
    let CodemodeResult::Err(error) = &result else {
        panic!();
    };
    assert_eq!(
        error.output,
        vec![kiana_codemode::CodemodeOutputItem::text("partial")]
    );
    assert_eq!(error.calls.len(), 1);
    assert_eq!(error.calls[0].status, CodemodeCallStatus::Cancelled);
    wait_for_stops(&launcher.channel, 1).await;
}

/// TS: `signal.reason instanceof Error ? reason.message : "Execution aborted"`
/// （`sandbox.test.ts` 的 `controller.abort(new Error("user cancelled"))` 断言
/// `message: "user cancelled"`）。Rust 的冻结 `signal` 只有 `CancellationToken`，不带 reason，
/// 因此由调用方在取消时显式提供（[`CodemodeSandboxBase::execute_with_abort_reason`]）。
#[tokio::test]
async fn an_abort_reason_becomes_the_error_message() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let token = CancellationToken::new();
    let result = base.execute_with_abort_reason(
        "await tools.hang()",
        CodemodeExecuteOptions {
            signal: Some(token.clone()),
            ..execute_options()
        },
        Some("user cancelled".to_string()),
    );
    let _events = launcher.events().await;
    token.cancel();
    let result = result.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Aborted);
    assert_eq!(error.message, "user cancelled");
}

/// `deterministic.test.ts`：`controller.abort(new Error("diverged"))` →
/// `{ ok: false, error: { kind: "aborted", message: "diverged" }, output: [] }`，且此后的
/// `deliver()` 静默返回。
#[tokio::test]
async fn a_run_abort_reason_becomes_the_error_message() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let token = CancellationToken::new();
    let mut run = base
        .start_with_abort_reason(
            "await tools.hang()",
            CodemodeRunOptions {
                signal: Some(token.clone()),
                ..run_options()
            },
            Some("diverged".to_string()),
        )
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"hang","args":null}));
    token.cancel();
    let result = (&mut run.result).await;
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Aborted);
    assert_eq!(error.message, "diverged");
    let CodemodeResult::Err(failure) = &result else {
        panic!("expected failure");
    };
    assert!(failure.output.is_empty());
    // 已结束：`deliver()` 静默返回。
    run.deliver(
        1.0,
        1_700_000_000_000.0,
        1,
        CodemodeOutcome::Ok(CodemodeOutcomeOk {
            ok: true,
            value: Some(Value::Null),
        }),
    )
    .expect("finished runs ignore deliver");
}

/// TS: `CodemodeRunOptions.onCall` 是必填字段；运行时缺了它，`forwardCall` 里的
/// `control.onCall({...})` 抛 `TypeError: control.onCall is not a function`，`receive` 折成
/// `Sandbox host failed: control.onCall is not a function`。pending 已经登记，`finish()` 补上
/// duration、状态保持 `cancelled`（node 26 实测）。
#[tokio::test]
async fn start_without_on_call_fails_the_host() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let mut options = run_options();
    options.on_call = None;
    options.tools = Some(vec![CodemodeDeclaredTool {
        name: "echo".into(),
        js_name: None,
        description: None,
    }]);
    let run = base.start("await tools.echo(1)", options).expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"1"}));
    let result = run.result.await;
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(
        error.message,
        "Sandbox host failed: control.onCall is not a function"
    );
    let CodemodeResult::Err(failure) = &result else {
        panic!("expected failure");
    };
    assert!(failure.output.is_empty());
    assert_eq!(failure.calls.len(), 1);
    assert_eq!(failure.calls[0].name, "echo");
    assert_eq!(failure.calls[0].status, CodemodeCallStatus::Cancelled);
    assert!(failure.calls[0].duration_ms >= 0.0);
    wait_for_stops(&launcher.channel, 1).await;
}

#[tokio::test]
async fn an_already_cancelled_signal_finishes_before_launching() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let token = CancellationToken::new();
    token.cancel();
    let result = base
        .execute(
            "return 1",
            CodemodeExecuteOptions {
                signal: Some(token),
                ..execute_options()
            },
        )
        .await
        .expect("not closed");
    assert_eq!(
        error_of(&result).kind,
        kiana_codemode::CodemodeErrorKind::Aborted
    );
    // 已经结束 → launcher 仍然启动（TS 在构造函数里先 finish 再 return，根本不会调用 launch）。
    assert!(launcher.data.lock().unwrap().is_none());
}

#[tokio::test]
async fn the_deadline_reports_the_configured_timeout() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute(
        "while (true) {}",
        CodemodeExecuteOptions {
            timeout_ms: Some(30.0),
            ..execute_options()
        },
    );
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"hang","args":"null"}));
    let result = result.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Timeout);
    assert_eq!(error.message, "Execution timed out after 30 ms");
}

#[tokio::test]
async fn infinite_timeout_disables_the_deadline() {
    let launcher = FakeLauncher::new();
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            timeout_ms: Some(f64::INFINITY),
            ..Default::default()
        },
    );
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"value":"1","writes":"[]"}));
    let result = result.await.expect("not closed");
    assert!(result.ok());
}

#[tokio::test]
async fn the_default_timeout_is_five_minutes() {
    let launcher = FakeLauncher::new();
    // 不设 timeoutMs → 默认 300000（这里只验证它能跑完，不真等 5 分钟）。
    let base = base_with(&launcher, CodemodeSandboxBaseOptions::default());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"value":"1","writes":"[]"}));
    assert!(result.await.expect("not closed").ok());
}

#[tokio::test]
async fn close_aborts_running_executions_with_sandbox_closed() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("", execute_options());
    let _events = launcher.events().await;
    base.close().await;
    let result = result.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Aborted);
    assert_eq!(error.message, "Sandbox closed");
}

#[tokio::test]
async fn execute_rejects_after_close() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    base.close().await;
    let error: SandboxClosed = base
        .execute("return 1", execute_options())
        .await
        .expect_err("closed");
    assert_eq!(error.to_string(), "Sandbox is closed");
}

/// TS: `Promise.all([...running].map((execution) => execution.abort("Sandbox closed")))`。
/// `.map` 是**先**为每个执行调用 `abort()`（同步 finish，并请求 channel 停止），
/// `Promise.all` 之后才等待：因此某个 channel 的 `stop()` 永不完成时，其余执行仍已全部被取消。
/// 若实现退化成「逐个 abort 再 await」，第一个挂住就会让后面的执行永远不被取消（reviewer 指出
/// 的正是这一点）。
#[tokio::test]
async fn close_aborts_every_execution_before_awaiting_any_of_them() {
    struct Channel {
        stops: AtomicUsize,
        hang: bool,
    }

    impl VmChannel for Channel {
        fn post(&self, _message: HostToWorkerMessage) {}
        fn stop(&self) -> kiana_codemode::types::BoxFuture<'static, ()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if self.hang {
                Box::pin(std::future::pending())
            } else {
                Box::pin(async {})
            }
        }
    }

    // 每次 launch 给一个独立 channel；第一个的 `stop()` 永不完成。
    let channels: Arc<Mutex<Vec<Arc<Channel>>>> = Arc::new(Mutex::new(Vec::new()));
    let launcher_channels = Arc::clone(&channels);
    let launcher: VmLauncher = Arc::new(move |_data: VmStartData, _events: Arc<dyn VmEvents>| {
        let channel = Arc::new(Channel {
            stops: AtomicUsize::new(0),
            hang: launcher_channels.lock().unwrap().is_empty(),
        });
        launcher_channels.lock().unwrap().push(Arc::clone(&channel));
        Box::pin(async move { Ok(channel as Arc<dyn VmChannel>) })
    });
    let base = Arc::new(CodemodeSandboxBase::new(defaults(), launcher).expect("valid options"));
    let first = base.execute("return 1", execute_options());
    let second = base.execute("return 2", execute_options());
    for _ in 0..10_000 {
        if channels.lock().unwrap().len() == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (first_channel, second_channel) = {
        let channels = channels.lock().unwrap();
        assert_eq!(channels.len(), 2, "both executions must reach the launcher");
        (Arc::clone(&channels[0]), Arc::clone(&channels[1]))
    };
    assert!(
        first_channel.hang,
        "the first channel must be the hanging one"
    );

    let closer = tokio::spawn({
        let base = Arc::clone(&base);
        async move { base.close().await }
    });
    for _ in 0..10_000 {
        if second_channel.stops.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        second_channel.stops.load(Ordering::SeqCst),
        1,
        "close() must abort every execution before awaiting any of them"
    );
    let result = second.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Aborted);
    assert_eq!(error.message, "Sandbox closed");
    // `close()` 本身仍然挂在第一个 channel 的 `stop()` 上（Promise.all 的语义）。
    assert!(!closer.is_finished());
    assert_eq!(first_channel.stops.load(Ordering::SeqCst), 1);
    closer.abort();
    drop(first);
}

/// 关闭与启动的竞态：TS 的 `execute()`/`start()` 在单线程里同步完成 `closed` 检查、
/// 构造 `Execution` 与 `running.add()`，因此任何先于 `close()` 开始的执行都必然被取消，
/// 且 `close()` 返回后不会有执行处于存活状态。Rust 侧若把「检查 `closed`」与「登记到
/// `running`」分成两步，并发的 `execute()` 可能两头落空：既没被拒绝，也不在 `close()`
/// 排空的列表里，于是继续运行（还能继续调用工具）。
///
/// 这里按 reviewer 的复现方式把执行任务与关闭任务各自 `tokio::spawn`：多线程 runtime 会把
/// 它们放到不同 worker 上，让「检查 → 登记」与 `close()` 真正并行。`close()` 返回后，若执行
/// 仍未结束，就等 launcher 拿到的 events 补一条 `done`——只有逃过 `close()` 的执行才会因此
/// 变成 `ok: true`，被取消的执行早已在 `finish()` 里忽略后续消息。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_never_leaves_a_concurrent_execution_running() {
    struct NoopChannel;
    impl VmChannel for NoopChannel {
        fn post(&self, _message: HostToWorkerMessage) {}
        fn stop(&self) -> kiana_codemode::types::BoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }

    let iterations = 2000;

    for iteration in 0..iterations {
        let events: Arc<Mutex<Vec<Arc<dyn VmEvents>>>> = Arc::new(Mutex::new(Vec::new()));
        let launcher: VmLauncher = {
            let events = Arc::clone(&events);
            Arc::new(move |_data: VmStartData, vm_events: Arc<dyn VmEvents>| {
                events.lock().unwrap().push(vm_events);
                Box::pin(async { Ok(Arc::new(NoopChannel) as Arc<dyn VmChannel>) })
            })
        };
        let base = Arc::new(CodemodeSandboxBase::new(defaults(), launcher).expect("valid options"));
        let execution = tokio::spawn({
            let base = Arc::clone(&base);
            async move { base.execute("return 1", execute_options()).await }
        });
        let closer = tokio::spawn({
            let base = Arc::clone(&base);
            async move { base.close().await }
        });
        closer.await.expect("close task");

        // 被 `close()` 取消的执行已经结束；仍在跑的只可能是逃出 `running` 列表的执行。
        for _ in 0..10_000 {
            if execution.is_finished() {
                break;
            }
            let vm_events = events.lock().unwrap().first().cloned();
            if let Some(vm_events) = vm_events {
                vm_events.message(json!({"type":"done","ok":true,"writes":"[]"}));
                break;
            }
            tokio::task::yield_now().await;
        }

        match execution.await.expect("execute task") {
            Err(SandboxClosed) => {}
            Ok(result) => {
                assert!(
                    !result.ok(),
                    "iteration {iteration}: an execution survived close(): {result:?}"
                );
                let error = error_of(&result);
                assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Aborted);
                assert_eq!(error.message, "Sandbox closed");
            }
        }
    }
}

#[tokio::test]
async fn start_throws_after_close() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    base.close().await;
    let error = base.start("return 1", run_options()).err().expect("closed");
    assert_eq!(error.to_string(), "Sandbox is closed");
}

// ---------------------------------------------------------------------------
// 工具表与全局名
// ---------------------------------------------------------------------------

#[test]
fn register_and_unregister_tools() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    base.register_tool(echo_tool("echo")).expect("first");
    assert_eq!(
        base.register_tool(echo_tool("echo")).unwrap_err(),
        "Tool \"echo\" is already registered"
    );
    assert_eq!(base.tools().len(), 1);
    assert!(base.unregister_tool("echo"));
    assert!(!base.unregister_tool("echo"));
    assert!(base.tools().is_empty());
}

#[test]
fn duplicate_tools_in_the_options_are_rejected() {
    let launcher = FakeLauncher::new();
    let error = CodemodeSandboxBase::new(
        CodemodeSandboxBaseOptions {
            tools: Some(vec![echo_tool("echo"), echo_tool("echo")]),
            ..Default::default()
        },
        launcher.launcher(),
    )
    .err()
    .expect("duplicate");
    assert_eq!(error.to_string(), "Tool \"echo\" is already registered");
}

#[test]
fn global_names_are_validated_before_the_sandbox_exists() {
    let launcher = FakeLauncher::new();
    let global = |name: &str| {
        CodemodeTool::new(
            name,
            Arc::new(|_: Value, _: CodemodeToolContext| Box::pin(async { Ok(Value::Null) })),
        )
    };
    for name in [
        "a.b.c",
        "a.",
        ".a",
        "tools.x",
        "store.x",
        "a.not-valid",
        "not-valid",
        "",
        "9x",
    ] {
        let error = CodemodeSandboxBase::new(
            CodemodeSandboxBaseOptions {
                globals: Some(vec![global(name)]),
                ..Default::default()
            },
            launcher.launcher(),
        )
        .err()
        .unwrap_or_else(|| panic!("{name} must be rejected"));
        assert_eq!(error.to_string(), format!("Invalid global name \"{name}\""));
    }
    for name in [
        "tools",
        "ALL_TOOLS",
        "console",
        "text",
        "image",
        "exit",
        "globalThis",
        "store",
        "load",
    ] {
        let error = CodemodeSandboxBase::new(
            CodemodeSandboxBaseOptions {
                globals: Some(vec![global(name)]),
                ..Default::default()
            },
            launcher.launcher(),
        )
        .err()
        .unwrap_or_else(|| panic!("{name} must be reserved"));
        assert_eq!(error.to_string(), format!("Invalid global name \"{name}\""));
    }
}

#[test]
fn global_names_must_be_unique_and_may_not_shadow_a_namespace() {
    let launcher = FakeLauncher::new();
    let global = |name: &str| {
        CodemodeTool::new(
            name,
            Arc::new(|_: Value, _: CodemodeToolContext| Box::pin(async { Ok(Value::Null) })),
        )
    };
    let error = CodemodeSandboxBase::new(
        CodemodeSandboxBaseOptions {
            globals: Some(vec![global("attach"), global("attach")]),
            ..Default::default()
        },
        launcher.launcher(),
    )
    .err()
    .expect("duplicate");
    assert_eq!(error.to_string(), "Global \"attach\" is already registered");

    let error = CodemodeSandboxBase::new(
        CodemodeSandboxBaseOptions {
            globals: Some(vec![global("models"), global("models.list")]),
            ..Default::default()
        },
        launcher.launcher(),
    )
    .err()
    .expect("conflict");
    assert_eq!(
        error.to_string(),
        "Global \"models\" conflicts with the namespace \"models\""
    );
}

/// TS 的 `checkGlobals` 用一个 `Set` 收集命名空间并按插入序迭代，因此多个冲突时报的是
/// **注册序里第一个**冲突的命名空间。`HashSet` 的迭代序随进程随机，同一个输入会在
/// `a`/`c` 之间摇摆（reviewer 实测：TS 恒为 `a`）。
#[test]
fn the_first_conflicting_namespace_is_reported_deterministically() {
    let launcher = FakeLauncher::new();
    let global = |name: &str| {
        CodemodeTool::new(
            name,
            Arc::new(|_: Value, _: CodemodeToolContext| Box::pin(async { Ok(Value::Null) })),
        )
    };
    let options = |globals: Vec<CodemodeTool>| CodemodeSandboxBaseOptions {
        globals: Some(globals),
        ..Default::default()
    };
    let error = CodemodeSandboxBase::new(
        options(vec![global("a"), global("a.b"), global("c"), global("c.d")]),
        launcher.launcher(),
    )
    .err()
    .expect("conflict");
    assert_eq!(
        error.to_string(),
        "Global \"a\" conflicts with the namespace \"a\""
    );
    let error = CodemodeSandboxBase::new(
        options(vec![global("c"), global("c.d"), global("a"), global("a.b")]),
        launcher.launcher(),
    )
    .err()
    .expect("conflict");
    assert_eq!(
        error.to_string(),
        "Global \"c\" conflicts with the namespace \"c\""
    );
    // 注册序里第一个**命名空间**才决定报哪个名字：`z` 与 `y` 是命名空间，`a` 是冲突的全名。
    let error = CodemodeSandboxBase::new(
        options(vec![global("z.t"), global("a"), global("y.u"), global("y")]),
        launcher.launcher(),
    )
    .err()
    .expect("conflict");
    assert_eq!(
        error.to_string(),
        "Global \"y\" conflicts with the namespace \"y\""
    );
}

// ---------------------------------------------------------------------------
// start()：参数校验与确定性交付
// ---------------------------------------------------------------------------

#[test]
fn start_validates_started_at() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    for started_at in [-1.0, f64::NAN, f64::NEG_INFINITY] {
        let error = base
            .start(
                "return 1",
                CodemodeRunOptions {
                    started_at,
                    ..run_options()
                },
            )
            .err()
            .unwrap_or_else(|| panic!("startedAt = {started_at} must be rejected"));
        assert_eq!(
            error.to_string(),
            "startedAt must be a non-negative number of milliseconds since the epoch"
        );
    }
}

#[test]
fn start_validates_the_seed() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    for seed in [-1.0, 1.5, 4_294_967_296.0, f64::NAN, f64::INFINITY] {
        let error = base
            .start(
                "return 1",
                CodemodeRunOptions {
                    seed,
                    ..run_options()
                },
            )
            .err()
            .unwrap_or_else(|| panic!("seed = {seed} must be rejected"));
        assert_eq!(error.to_string(), "seed must be a 32-bit unsigned integer");
    }
}

#[test]
fn start_validates_tool_names_and_script_names() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let error = base
        .start(
            "return 1",
            CodemodeRunOptions {
                tools: Some(vec![
                    CodemodeDeclaredTool {
                        name: "a".into(),
                        js_name: None,
                        description: None,
                    },
                    CodemodeDeclaredTool {
                        name: "a".into(),
                        js_name: Some("b".into()),
                        description: None,
                    },
                ]),
                ..run_options()
            },
        )
        .err()
        .expect("duplicate names");
    assert_eq!(error.to_string(), "Tool names must be unique");

    let error = base
        .start(
            "return 1",
            CodemodeRunOptions {
                tools: Some(vec![
                    CodemodeDeclaredTool {
                        name: "a-b".into(),
                        js_name: None,
                        description: None,
                    },
                    CodemodeDeclaredTool {
                        name: "a_b".into(),
                        js_name: None,
                        description: None,
                    },
                ]),
                ..run_options()
            },
        )
        .err()
        .expect("duplicate script names");
    assert_eq!(error.to_string(), "Tool script names must be unique");
}

#[test]
fn start_validates_global_names() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let error = base
        .start(
            "return 1",
            CodemodeRunOptions {
                globals: Some(vec![CodemodeDeclaredGlobal {
                    name: "store".into(),
                    spread: None,
                }]),
                ..run_options()
            },
        )
        .err()
        .expect("reserved");
    assert_eq!(error.to_string(), "Invalid global name \"store\"");
}

#[tokio::test]
async fn deliver_rejects_a_non_finite_clock() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let run = base
        .start("await tools.echo(1)", run_options())
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"1"}));
    // 让 forwardCall 的 pending 登记落地。
    tokio::task::yield_now().await;
    let error = run
        .deliver(
            1.0,
            f64::NAN,
            1,
            CodemodeOutcome::Ok(CodemodeOutcomeOk {
                ok: true,
                value: Some(json!(1)),
            }),
        )
        .unwrap_err();
    assert_eq!(error, "The clock value of result 1 is not a finite number");
}

#[tokio::test]
async fn deliver_validates_the_result_number_and_the_waiting_call() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let run = base
        .start("await tools.echo(1)", run_options())
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"1"}));
    // 等 forwardCall 记录 pending。
    tokio::task::yield_now().await;
    let ok = || {
        CodemodeOutcome::Ok(CodemodeOutcomeOk {
            ok: true,
            value: Some(json!(1)),
        })
    };
    assert_eq!(
        run.deliver(0.0, 0.0, 1, ok()).unwrap_err(),
        "Result number 0 is not a positive integer used once"
    );
    assert_eq!(
        run.deliver(1.5, 0.0, 1, ok()).unwrap_err(),
        "Result number 1.5 is not a positive integer used once"
    );
    assert_eq!(
        run.deliver(1.0, 0.0, 99, ok()).unwrap_err(),
        "Call 99 is not waiting for a result"
    );
    // 校验失败的 deliver 不消耗 seq：同一 seq 仍然可用。
    run.deliver(1.0, 0.0, 1, ok()).expect("first result");
    assert_eq!(
        run.deliver(1.0, 0.0, 1, ok()).unwrap_err(),
        "Result number 1 is not a positive integer used once"
    );
}

/// TS 用模板字面量插值 `${seq}`（`Number::toString`），不是 `JSON.stringify` 的规则。
/// 非有限值的文本因此是 `NaN`/`Infinity`，不是 `null`。
#[tokio::test]
async fn deliver_formats_the_result_number_with_number_to_string() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let run = base
        .start("await tools.echo(1)", run_options())
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"1"}));
    tokio::task::yield_now().await;
    let ok = || {
        CodemodeOutcome::Ok(CodemodeOutcomeOk {
            ok: true,
            value: Some(json!(1)),
        })
    };
    // 错误文案是稳定契约，由黄金用例锁定（源自 `deterministic.test.ts` 的 `deliver(1, NaN, …)`）。
    assert_eq!(
        run.deliver(f64::NAN, 0.0, 1, ok()).unwrap_err(),
        "Result number NaN is not a positive integer used once"
    );
    assert_eq!(
        run.deliver(f64::INFINITY, 0.0, 1, ok()).unwrap_err(),
        "Result number Infinity is not a positive integer used once"
    );
    assert_eq!(
        run.deliver(f64::NEG_INFINITY, 0.0, 1, ok()).unwrap_err(),
        "Result number -Infinity is not a positive integer used once"
    );
    assert_eq!(
        run.deliver(1.0, f64::NAN, 1, ok()).unwrap_err(),
        "The clock value of result 1 is not a finite number"
    );
    // `seq` 本身非有限时，错误的时钟文案也用同一个文本。
    assert_eq!(
        run.deliver(f64::NAN, f64::INFINITY, 1, ok()).unwrap_err(),
        "The clock value of result NaN is not a finite number"
    );
    // 有限值仍按 JS 的 `Number::toString`：`1e21` 写作 `1e+21`，`-0` 写作 `0`。
    assert_eq!(
        run.deliver(1e21, 0.0, 99, ok()).unwrap_err(),
        "Call 99 is not waiting for a result"
    );
    assert_eq!(
        run.deliver(-0.0, 0.0, 1, ok()).unwrap_err(),
        "Result number 0 is not a positive integer used once"
    );
}

#[tokio::test]
async fn deliver_posts_a_result_message_with_seq_now_and_properties() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let run = base
        .start("await tools.echo(1)", run_options())
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":7,"target":"tool","name":"echo","args":"1"}));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let mut properties = indexmap::IndexMap::new();
    properties.insert("structuredOutput".to_string(), json!({"code": 7}));
    run.deliver(
        1.0,
        1_700_000_001_000.0,
        7,
        CodemodeOutcome::Err(CodemodeOutcomeErr {
            ok: false,
            message: "denied".into(),
            properties: Some(properties),
        }),
    )
    .expect("delivered");
    let replies = launcher.channel.results();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].id, 7);
    assert!(!replies[0].ok);
    assert_eq!(replies[0].payload.as_deref(), Some("denied"));
    assert_eq!(replies[0].seq, Some(1));
    assert_eq!(replies[0].now, Some(1_700_000_001_000.0));
    assert_eq!(
        replies[0].properties.as_deref(),
        Some("{\"structuredOutput\":{\"code\":7}}")
    );
    // 消息的线格式（JSON）是稳定契约，改动需同步锁定测试。
    let wire = serde_json::to_string(&HostToWorkerMessage::Result(replies[0].clone())).unwrap();
    assert_eq!(
        wire,
        r#"{"type":"result","id":7,"ok":false,"payload":"denied","seq":1,"now":1700000001000,"properties":"{\"structuredOutput\":{\"code\":7}}"}"#
    );
}

#[tokio::test]
async fn deliver_records_the_call_and_is_silent_once_finished() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let run = base
        .start("await tools.echo(1)", run_options())
        .expect("run");
    assert!(!run.finished());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"1"}));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    run.deliver(
        1.0,
        0.0,
        1,
        CodemodeOutcome::Ok(CodemodeOutcomeOk {
            ok: true,
            value: Some(json!("x")),
        }),
    )
    .expect("delivered");
    events.message(json!({"type":"done","ok":true,"value":"2","writes":"[]"}));
    // `finished` 在结果决定时置位（早于 `result` settle）。
    assert!(run.finished());
    // 终态之后的 deliver 静默返回。
    run.deliver(
        2.0,
        0.0,
        1,
        CodemodeOutcome::Ok(CodemodeOutcomeOk {
            ok: true,
            value: None,
        }),
    )
    .expect("silent after finish");
    let result = run.result.await;
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(ok.value, Some(json!(2)));
    assert_eq!(ok.calls.len(), 1);
    assert_eq!(ok.calls[0].status, CodemodeCallStatus::Ok);
    assert!(ok.calls[0].duration_ms >= 0.0);
    assert_eq!(ok.during, None);
}

#[tokio::test]
async fn start_runs_announce_the_vm_and_its_idle_points() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let started_seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let idle_seen: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls: Arc<Mutex<Vec<kiana_codemode::CodemodeHostCall>>> = Arc::new(Mutex::new(Vec::new()));
    let (started_sink, idle_sink, call_sink) = (
        Arc::clone(&started_seen),
        Arc::clone(&idle_seen),
        Arc::clone(&calls),
    );
    let run = base
        .start(
            "await tools.echo(1)",
            CodemodeRunOptions {
                on_started: Some(Arc::new(move |fingerprint: String| {
                    started_sink.lock().unwrap().push(fingerprint);
                })),
                on_idle: Some(Arc::new(move |seq: u64, issued: u64| {
                    idle_sink.lock().unwrap().push((seq, issued));
                })),
                on_call: Some(Arc::new(move |call: kiana_codemode::CodemodeHostCall| {
                    call_sink.lock().unwrap().push(call);
                })),
                ..run_options()
            },
        )
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"started","fingerprint":"fp"}));
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"[1]"}));
    events.message(json!({"type":"idle","seq":0,"issued":1}));
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*started_seen.lock().unwrap(), vec!["fp".to_string()]);
    assert_eq!(*idle_seen.lock().unwrap(), vec![(0, 1)]);
    assert_eq!(
        calls.lock().unwrap()[0],
        kiana_codemode::CodemodeHostCall {
            id: 1,
            target: CallTarget::Tool,
            name: "echo".into(),
            args: Some(json!([1])),
        }
    );
    // 确定性 run 的 VmStartData 带 `deterministic` 与 jsName 映射。
    let data = launcher.data.lock().unwrap().clone().expect("start data");
    assert_eq!(
        data.deterministic,
        Some(kiana_codemode::runtime::protocol::VmDeterministic {
            started_at: 1_700_000_000_000.0,
            seed: 1.0,
        })
    );
    events.message(
        json!({"type":"done","ok":true,"value":"1","writes":"[]","during":{"seq":1,"issued":1}}),
    );
    let result = run.result.await;
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(
        ok.during,
        Some(kiana_codemode::CodemodeDuring { seq: 1, issued: 1 })
    );
}

#[tokio::test]
async fn vm_start_data_carries_js_names_descriptions_and_the_store_snapshot() {
    let launcher = FakeLauncher::new();
    let mut store = indexmap::IndexMap::new();
    store.insert("k".to_string(), json!({"a": 1}));
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![{
                let mut tool = echo_tool("my-tool");
                tool.description = Some("does things".into());
                tool
            }]),
            globals: Some(vec![{
                let mut global = echo_tool("models.list");
                global.spread = Some(true);
                global
            }]),
            ..defaults()
        },
    );
    let result = base.execute(
        "return 1",
        CodemodeExecuteOptions {
            store: Some(store),
            ..execute_options()
        },
    );
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    result.await.expect("not closed");
    let data = launcher.data.lock().unwrap().clone().expect("start data");
    assert_eq!(data.tools.len(), 1);
    assert_eq!(data.tools[0].name, "my-tool");
    assert_eq!(data.tools[0].js_name, "my_tool");
    assert_eq!(data.tools[0].description, "does things");
    assert_eq!(data.globals.len(), 1);
    assert_eq!(data.globals[0].name, "models.list");
    assert!(data.globals[0].spread);
    assert_eq!(data.store.get("k").map(String::as_str), Some("{\"a\":1}"));
    assert_eq!(data.deterministic, None);
}

#[tokio::test]
async fn replies_produced_before_the_channel_exists_are_queued() {
    // launcher 挂起：先让工具回答排队，再放行 channel，验证 flush 顺序。
    struct PendingLauncher {
        channel: Arc<FakeChannel>,
        release: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        events: Mutex<Option<Arc<dyn VmEvents>>>,
    }
    let pending = Arc::new(PendingLauncher {
        channel: Arc::new(FakeChannel::default()),
        release: Mutex::new(None),
        events: Mutex::new(None),
    });
    let launcher = {
        let pending = Arc::clone(&pending);
        Arc::new(move |_data: VmStartData, events: Arc<dyn VmEvents>| {
            let pending = Arc::clone(&pending);
            let (tx, rx) = tokio::sync::oneshot::channel();
            *pending.release.lock().unwrap() = Some(tx);
            *pending.events.lock().unwrap() = Some(events);
            Box::pin(async move {
                let _ = rx.await;
                Ok(Arc::clone(&pending.channel) as Arc<dyn VmChannel>)
            })
                as kiana_codemode::types::BoxFuture<'static, Result<Arc<dyn VmChannel>, String>>
        }) as VmLauncher
    };
    let base = Arc::new(
        CodemodeSandboxBase::new(
            CodemodeSandboxBaseOptions {
                tools: Some(vec![echo_tool("echo")]),
                ..defaults()
            },
            launcher,
        )
        .expect("valid"),
    );
    let result = base.execute("await tools.echo(1)", execute_options());
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let events = pending.events.lock().unwrap().clone().expect("events");
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":"1"}));
    for _ in 0..1000 {
        tokio::time::sleep(Duration::from_millis(1)).await;
        if pending.channel.results().is_empty() {
            continue;
        }
        break;
    }
    // channel 还没装上：回答必须还在排队。
    assert!(pending.channel.results().is_empty());
    pending
        .release
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .send(())
        .unwrap();
    for _ in 0..1000 {
        if !pending.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    // channel 装上后，排队的回答被冲洗出去（顺序不变）。
    assert_eq!(pending.channel.results().len(), 1);
    assert_eq!(pending.channel.results()[0].id, 1);
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    assert!(result.ok());
}

// ---------------------------------------------------------------------------
// 宿主回调与 launcher 的 panic（TS 的 throw）
// ---------------------------------------------------------------------------

/// TS: `try { value = await tool.execute(args, …) } catch { reply = { ok:false, payload:
/// errorMessage(error) } }` 覆盖工具的整个生命周期。Rust 的异步工具用 panic 表达「抛错」，
/// **轮询期间的 panic** 也必须变成该调用的错误结果，而不是杀掉宿主任务。
#[tokio::test]
async fn a_tool_that_panics_while_polling_becomes_an_error_result() {
    let launcher = FakeLauncher::new();
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![CodemodeTool::new(
                "boom",
                Arc::new(|_args: Value, _context: CodemodeToolContext| {
                    Box::pin(async {
                        // 让 panic 发生在 `poll` 里而不是创建 future 时。
                        tokio::task::yield_now().await;
                        panic!("tool exploded");
                    })
                }),
            )]),
            ..defaults()
        },
    );
    let result = base.execute("await tools.boom()", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"boom","args":"null"}));
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let replies = launcher.channel.results();
    assert_eq!(replies.len(), 1, "the panicking call must be answered");
    assert_eq!(replies[0].id, 1);
    assert!(!replies[0].ok);
    assert_eq!(replies[0].payload.as_deref(), Some("tool exploded"));
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(ok.calls[0].status, CodemodeCallStatus::Error);
}

/// 创建 future 时 panic 也要变成错误结果（TS 的 `execute` 同步 throw）。
#[tokio::test]
async fn a_tool_that_panics_while_being_created_becomes_an_error_result() {
    let launcher = FakeLauncher::new();
    let unreachable_tool = CodemodeTool::new(
        "boom",
        Arc::new(|_args: Value, _context: CodemodeToolContext| {
            panic!("sync boom");
        }),
    );
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![unreachable_tool]),
            ..defaults()
        },
    );
    let result = base.execute("await tools.boom()", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":2,"target":"tool","name":"boom","args":"null"}));
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let replies = launcher.channel.results();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].payload.as_deref(), Some("sync boom"));
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    result.await.expect("not closed");
}

/// TS 的 `new Promise(resolve => resolve(options.launch(...))).then(…, error => finish({kind:
/// "sandbox", message: errorMessage(error)}))`：launcher 抛错（Rust 里是 panic）必须让执行以
/// `kind: "sandbox"` 结束，而不是让结果 future 永远挂住。
#[tokio::test]
async fn a_panicking_launcher_finishes_with_a_sandbox_error() {
    let launcher: VmLauncher = Arc::new(|_data: VmStartData, _events: Arc<dyn VmEvents>| {
        Box::pin(async { panic!("launcher exploded") })
    });
    let base = Arc::new(CodemodeSandboxBase::new(defaults(), launcher).expect("valid"));
    let result = base
        .execute("return 1", execute_options())
        .await
        .expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.message, "launcher exploded");
}

/// 创建 launcher future 时 panic 同样处理。
#[tokio::test]
async fn a_launcher_that_panics_while_being_created_finishes_with_a_sandbox_error() {
    let launcher: VmLauncher = Arc::new(
        |_data: VmStartData,
         _events: Arc<dyn VmEvents>|
         -> kiana_codemode::types::BoxFuture<'static, Result<Arc<dyn VmChannel>, String>> {
            panic!("sync launcher boom")
        },
    );
    let base = Arc::new(CodemodeSandboxBase::new(defaults(), launcher).expect("valid"));
    let result = base
        .execute("return 1", execute_options())
        .await
        .expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.message, "sync launcher boom");
}

/// TS 的 `Execution` 是被在途闭包（`resolveResult`）持有的 JS 对象，因此丢掉 sandbox 后
/// 在途的 `execute()` 仍能正常结算。Rust 侧的结果 future 必须自己持有 `Arc<Execution>`。
#[tokio::test]
async fn dropping_the_sandbox_does_not_break_an_in_flight_execution() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"output","item":{"type":"text","text":"partial"}}));
    // 判题式地丢掉 sandbox，之后 VM 才报终态。
    drop(base);
    events.message(json!({"type":"done","ok":true,"value":"1","writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(ok.value, Some(json!(1)));
    assert_eq!(
        ok.output,
        vec![kiana_codemode::CodemodeOutputItem::text("partial")]
    );
}

// ---------------------------------------------------------------------------
// 结构性解码（TS 的守卫只校验 `type`）
// ---------------------------------------------------------------------------

/// TS 的 `isWorkerToHostMessage` 只校验 `type`；其余字段在解构时被原样读取。reviewer 用假
/// launcher 实测（node 26）：
/// - `{"type":"done","ok":true,"value":5}` → `JSON.parse(5)` → `value: 5`（不是 host failure）；
/// - `{"type":"crash"}` → `{kind:"sandbox"}`，`message` 是 `undefined`；
/// - `{"type":"started"}` / `{"type":"idle"}` → 回调收到 `undefined`（Rust 侧取空串/0）；
/// - `{"type":"output"}` → 输出推入 `undefined`（冻结的输出类型无法表示，这里跳过该项，且
///   不因此失败——见 `output_items_the_frozen_type_cannot_represent_are_skipped`）；
/// - 缺 `writes` → `store writes is not valid JSON`（`JSON.parse(undefined)`）。
///
/// 这些都不是 spec §7.7 的 7 个坏桥接用例，但文本是稳定契约，改动需同步测试。
#[tokio::test]
async fn structurally_valid_messages_are_decoded_like_typescript() {
    // value 是数字：`JSON.parse(5)` → 5。
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"value":5,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(ok.value, Some(json!(5)));

    // value 是布尔：`JSON.parse(true)` → true。
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"value":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    assert_eq!(
        result,
        CodemodeResult::success(
            Some(json!(true)),
            vec![],
            vec![],
            CodemodeStoreWrites::default(),
            None,
        )
    );

    // crash 没有 message → 空消息（TS 的 `message` 是 `undefined`）。
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"crash"}));
    let result = result.await.expect("not closed");
    let error = error_of(&result);
    assert_eq!(error.kind, kiana_codemode::CodemodeErrorKind::Sandbox);
    assert_eq!(error.message, "");

    // 缺 writes → 桥接错误（`JSON.parse(undefined)`）。
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":true,"value":"1"})).await,
        format!("Sandbox bridge broken: store writes is not valid JSON{BRIDGE_SUFFIX}")
    );
    // writes 是数字 → `JSON.parse(5)` → 5 → "store writes are not an array"。
    assert_eq!(
        bridge_failure(json!({"type":"done","ok":true,"value":"1","writes":5})).await,
        format!("Sandbox bridge broken: store writes are not an array{BRIDGE_SUFFIX}")
    );
    // ok 缺失 → TS 的 `if (!message.ok)` 走失败支：`error` 缺失 → `JSON.parse(undefined)`。
    assert_eq!(
        bridge_failure(json!({"type":"done","value":"1","writes":"[]"})).await,
        format!("Sandbox bridge broken: script error is not valid JSON{BRIDGE_SUFFIX}")
    );
    // `1.0` 与 `1` 在 JS 里是同一个数：`during.seq` 因此是 1（不是 0）。
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":true,"value":"1","writes":"[]",
        "during":{"seq":1.0,"issued":2.0}}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success");
    };
    assert_eq!(
        ok.during,
        Some(kiana_codemode::CodemodeDuring { seq: 1, issued: 2 })
    );

    // ok 是字符串 `"yes"` → truthy → 成功支。
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"done","ok":"yes","value":"1","writes":"[]"}));
    let result = result.await.expect("not closed");
    assert!(result.ok(), "truthy ok must take the success branch");
}

/// TS: `output.push(message.item)`——**不校验**、也不抛错。缺 `item`、类型不对、未知 `type`
/// 的输出项在冻结的 `CodemodeOutputItem` 里无法表示，因此被跳过；执行照常结算，后续输出
/// （与最终结果）不受影响（TS 会把这些值原样放进 `output`，但绝不会因此失败）。
#[tokio::test]
async fn output_items_the_frozen_type_cannot_represent_are_skipped() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let result = base.execute("return 1", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"output"}));
    events.message(json!({"type":"output","item":"just a string"}));
    events.message(json!({"type":"output","item":{"type":"text"}}));
    events.message(json!({"type":"output","item":{"type":"nonsense"}}));
    events.message(json!({"type":"output","item":null}));
    events.message(json!({"type":"output","item":{"type":"text","text":"kept"}}));
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    let result = result.await.expect("not closed");
    let CodemodeResult::Ok(ok) = &result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(
        ok.output,
        vec![kiana_codemode::CodemodeOutputItem::text("kept")]
    );
}

/// TS: `control?.onStarted?.(message.fingerprint)` / `control?.onIdle?.(message.seq, message.issued)`
/// 在没有字段时传 `undefined`。冻结的回调参数是 `String`/`u64`，用空串/0 表示。
#[tokio::test]
async fn started_and_idle_without_fields_report_empty_values() {
    let launcher = FakeLauncher::new();
    let base = base_with(&launcher, defaults());
    let started = Arc::new(Mutex::new(Vec::<String>::new()));
    let idles = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let started_sink = Arc::clone(&started);
    let idles_sink = Arc::clone(&idles);
    let run = base
        .start(
            "return 1",
            CodemodeRunOptions {
                on_started: Some(Arc::new(move |fingerprint| {
                    started_sink.lock().unwrap().push(fingerprint);
                })),
                on_idle: Some(Arc::new(move |seq, issued| {
                    idles_sink.lock().unwrap().push((seq, issued));
                })),
                ..run_options()
            },
        )
        .expect("run");
    let events = launcher.events().await;
    events.message(json!({"type":"started"}));
    events.message(json!({"type":"idle"}));
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    run.result.await;
    assert_eq!(*started.lock().unwrap(), vec![String::new()]);
    assert_eq!(*idles.lock().unwrap(), vec![(0, 0)]);
}

/// TS: `args === undefined ? undefined : JSON.parse(args)`；非字符串的 `args` 先 `ToString`
/// （`JSON.parse(5)` → `5`，`JSON.parse(null)` → `null`）。
#[tokio::test]
async fn call_arguments_of_any_json_type_are_parsed_like_typescript() {
    let launcher = FakeLauncher::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let base = base_with(
        &launcher,
        CodemodeSandboxBaseOptions {
            tools: Some(vec![record_tool("echo", Arc::clone(&seen))]),
            ..defaults()
        },
    );
    let result = base.execute("await tools.echo()", execute_options());
    let events = launcher.events().await;
    events.message(json!({"type":"call","id":1,"target":"tool","name":"echo","args":5}));
    for _ in 0..1000 {
        if !launcher.channel.results().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(*seen.lock().unwrap(), vec![json!(5)]);
    events.message(json!({"type":"done","ok":true,"writes":"[]"}));
    result.await.expect("not closed");
}
