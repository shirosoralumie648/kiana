//! runtime/execution.ts：一次执行的宿主侧，VM 在哪里跑由 launcher 决定。
//!
//! TS 源：`packages/codemode/src/runtime/execution.ts`（573 行）。
//!
//! 宿主拥有脚本不得影响的一切：工具表、调用记录、输出、deadline、调用方的取消信号，以及 VM
//! 所报消息的解码。VM 本身跑在 [`VmLauncher`] 选择的地方：worker 线程（`CodemodeSandbox`）、
//! 调用线程（`InlineCodemodeSandbox`）或通过 RPC 到达的另一个 isolate（`RemoteCodemodeSandbox`）。
//! 两侧只交换 `protocol.rs` 里的消息，因此 launcher 之间只差传输与终止方式，行为相同。
//!
//! 入口是 [`CodemodeSandboxBase`]：`runtime::host`、`runtime::inline`、`runtime::remote` 的
//! sandbox 都持有它（只注入不同的 [`VmLauncher`]），每次成功开始执行的 `execute()`/`start()`
//! 在这里构造一个 `Execution`；sandbox 已关闭或 `start()` 的参数校验失败时在更早处直接返回
//! （`SandboxClosed` / `StartError`），不会构造 `Execution`。消息形状见 `protocol.rs`，
//! 脚本可见面见 `prelude_source.rs`。
//!
//! 一次执行的生命周期（逐条对应 TS 的构造函数与 `finish`）：
//! 1. 构造函数武装 deadline 与 abort 监听，然后向 launcher 要 channel。
//! 2. VM 的消息经 [`VmEvents::message`] 到达；工具调用在这里执行，结果经 [`VmChannel::post`]
//!    送回。launcher 返回 channel 之前产生的回复排队，channel 到达后按序冲洗。
//! 3. 第一个终态事件获胜：`done`、`crash`、launcher 失败、deadline 或 abort。`finish()` 取消
//!    在途工具调用、请 channel 停止，并在该请求完成后结算结果。worker 或远端 VM 可以更晚退出；
//!    之后的事件被忽略。
//!
//! # Rust 映射
//!
//! - `VmEvents`/`VmChannel` 是对象安全 trait，用 `Arc<dyn …>` 共享（TS 里是普通对象）。
//! - `VmLauncher = Arc<dyn Fn(VmStartData, Arc<dyn VmEvents>) -> BoxFuture<Result<Arc<dyn VmChannel>, String>>>`：
//!   TS 的 `VmChannel | Promise<VmChannel>` 与 rejection 合并成一个 future；拒绝值取 `errorMessage`
//!   文本（Rust 侧直接是 `String`）。
//! - `AbortSignal` → [`CancellationToken`]（ARCHITECTURE.md §4.2）。
//! - 「宿主事件循环在 VM 计算期间不跑」→ [`HostEventLoop`]：inline launcher 在 VM 执行期间
//!   占住闸门，deadline 与 abort 监听等它让出后才 `finish`（spec §A.5.2）。worker/remote
//!   launcher 不占闸门，行为不变。
//! - `performance.now()` → [`Instant`] 差值的毫秒数（f64，不取整）：起点在登记在途调用时
//!   取（`PendingCall::started_at`），用于调用记录与 `deliver`/`finish` 的记账。
//! - `Execution.promise` → [`tokio::sync::watch`]：Promise 可以被任意多个调用方 await，watch 的
//!   接收端克隆给出同一结果。
//! - 异步要求：工具调用与 `channel.stop()` 通过 `tokio::spawn` 驱动，deadline 用
//!   `tokio::time::sleep`，因此需要 Tokio 运行时（ARCHITECTURE.md §4.2）。调用方当前不在
//!   运行时上下文时，`runtime_handle` 会惰性建一个进程级 multi-thread runtime（2 个 worker
//!   线程，`static` 存活到进程结束）承接这些任务。
//!
//! # 共享状态与并发
//!
//! `Execution` 被多个任务共享：deadline、abort 监听、launcher、每个工具调用，以及 `deliver()`
//! 的调用方。需要一致性的复合状态都在 `std::sync::Mutex` 后面（`channel`、`shared`、
//! `output`、`calls`、`timer`、`abort_listener`），单值用原子量（`finished`、闸门的
//! `generation`）；临界区不 `.await`、不调用外部回调、也不向 channel 投递，这些动作都在
//! 释放锁之后进行，因此没有跨 `.await` 持有的锁。临界区并非只做字段读写：`deliver` 在持有
//! `shared` 时序列化 `properties`/`payload`（`kiana_json::js_object::stringify`），`finish`
//! 也在持有 `shared` 时获取 `calls` 锁。锁的嵌套只有一个方向：
//! `shared` → `calls`（`forward_call` 与 `finish` 在持有 `shared` 时更新调用记录），
//! `deliver` 则先释放 `shared` 再锁 `calls`；不存在反向嵌套。
//!
//! 终态只有一个：`finish()` 用 `AtomicBool::swap` 决胜，之后的消息、`deliver()` 与后续终态
//! 事件都被忽略。结果经 `tokio::sync::watch` 广播；`Execution` 自持一个接收端
//! （`_result_keepalive`），所以 `send` 不会因为暂时没有等待者而失败。后台任务与结果 future
//! 都持有 `Arc<Execution>`，因此宿主先丢掉 sandbox 也不会让在途执行失去发送方。
//!
//! # 已知偏差
//!
//! - `AbortSignal.reason` 需要一个附加载体：冻结的 `CodemodeExecuteOptions::signal` /
//!   `CodemodeRunOptions::signal` 只有 [`CancellationToken`]（`types.rs`，不由本单元拥有），
//!   无法表达 `signal.reason`。TS 是 `reason instanceof Error ? reason.message : "Execution
//!   aborted"`；在 Node/Bun 上 `AbortController.abort()`（无参，或 `undefined`）把 `reason`
//!   设成一个 `message` 为 `This operation was aborted` 的 DOMException，而 DOMException
//!   `instanceof Error` 为真（node 26 实测），因此**裸取消**的可观察文本是
//!   `This operation was aborted`。裸 [`CancellationToken`] 不带任何载荷，最接近的映射就是
//!   「调用方 `abort()` 且没给原因」这一条，所以默认用它；显式原因走附加的
//!   `reason: Option<String>` 参数（ARCHITECTURE.md §4.2 的 `CancellationToken` + 可选 reason）：
//!   [`CodemodeSandboxBase::execute_with_abort_reason`] /
//!   [`CodemodeSandboxBase::start_with_abort_reason`] 传入原因后就是那个 `reason.message`。
//!   TS 里「`reason` 不是 `Error`」的生产者（例如 `abort("why")`）在 Rust 侧要显式传
//!   `Some("Execution aborted".to_string())` 才能逐字复现（spec §3.4 的那一行）。
//!   冻结的 `execute`/`start` 走默认（等价于 Node 的裸 `abort()`）。
//! - `VmStartData.store` 的键序由 [`kiana_json::js_object::stringify`] 的 JS 枚举序决定（整数索引
//!   键在前），与 `Object.entries` 一致。
//! - JSON 无法表示 `undefined`：工具参数为无参时是 `Value::Null`、工具返回 `Value::Null` 时脚本
//!   看到 `null`（见 `types.rs` 的 `CodemodeToolExecute` 文档）。
//! - `deliver()` 的 `properties` 校验（`The properties of result ${seq} are not an object`）在
//!   Rust 侧不可达：`CodemodeOutcomeErr.properties` 是 `Option<IndexMap>`
//!   （`types.rs` 的冻结形状），`null`/数组/原始值无法表达。TS 的四条校验里这一条因此只剩类型
//!   保证，其余三条逐字保留。
//! - 消息解码与 TS 一样是**按字段**的：守卫只校验 `type`，其余字段在解构时原样读取（不整体
//!   走 `serde`），因此类型不对的字段落在与 TS 相同的分支上（例如 `{"type":"done","ok":true,
//!   "value":5}` 经 `JSON.parse(5)` 得到 `5`，缺 `writes` 落到 `store writes is not valid JSON`）。
//!   与 TS 的差异都只在畸形（真实 prelude 不会发出的）消息上：`{"type":"crash"}` 缺 `message`
//!   时 TS 的 `message` 是 `undefined`，冻结的 `CodemodeError.message` 是 `String`，这里用空串；
//!   `output` 消息里不是规范形状的 `item`（缺字段、类型不对、未知 `type`）在 TS 里原样进
//!   `output`，冻结的 `CodemodeOutputItem` 无法表示，这里跳过该项（TS 从不因为 output 消息
//!   失败，这里也不）；`during` 不是对象、或 `seq`/`issued` 不是数字时，TS 产出
//!   `{ during: {} }` 或原样透传（例如 `{seq:"2"}`），冻结的 `CodemodeDuring` 是两个 `u64`，
//!   这里按缺失/0 处理。`"during": null` 是特例：TS 在 `finish()` 内部抛 TypeError，而那时
//!   `finished` 已置位，于是结果 promise **永不结算**、`close()` 也永远挂住（node 26 实测）；
//!   这里把非对象的 `during` 当作缺失，执行正常结算——刻意不复制的一条 TS 崩溃路径。
//!   `call` 的 `target` 不是 `"tool"` 时，TS 的 `forwardCall` 把原值交给 `onCall`；冻结的
//!   `CodemodeHostCall.target` 只有 `"tool" | "global"`，这里记为 `global`（路由与调用记账
//!   保持同一路由与调用记账，只有这个畸形值本身无法透传）。
//! - `deliver()` 的 `seq` 是 `u64`（`HostResult.seq` 的冻结类型）：TS 接受任意正整数，超过
//!   `u64::MAX` 的 `seq` 在这里被钳到 `u64::MAX`（实际调用方按 1、2、… 递增，不可达）。
//! - `setTimeout` 越界时的告警（`TimeoutOverflowWarning` / `TimeoutNegativeWarning` /
//!   `TimeoutNaNWarning`）不复制：那是 Node 运行时经 `process.emitWarning` 打到 stderr 的
//!   运行时噪声，不是 kiana 的输出；kiana CLI 启动时把 `process.emitWarning` 设为 noop
//!   （spec/ca-cli.md），产品里本来也看不到。**延迟**与 Node 一致：判界用的是未截断的
//!   `after`（`!(after >= 1 && after <= 2^31-1)` → 1 ms，见 `node_timeout_delay`）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use kiana_json::js_num::is_js_integer;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runtime::protocol::{
    CallTarget, HostResult, HostToWorkerMessage, VmDeterministic, VmGlobalInfo, VmStartData,
    VmToolInfo, is_worker_to_host_message,
};
use crate::types::{
    BoxFuture, CodemodeCall, CodemodeCallStatus, CodemodeDeclaredGlobal, CodemodeDeclaredTool,
    CodemodeDuring, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutcome,
    CodemodeOutputItem, CodemodeResult, CodemodeRun, CodemodeRunOptions,
    CodemodeSandboxBaseOptions, CodemodeStoreWrites, CodemodeTool, CodemodeToolContext,
};

/// 默认总 deadline（毫秒）：300000。
///
/// [`CodemodeSandboxBase::new`] 在 `options.timeout_ms` 缺省时用它作为 sandbox 的默认值，
/// `execute()` 未另给 `timeout_ms` 时沿用该值；`start()` 的默认不是它，而是无 deadline
/// （`INFINITY`）。
///
/// TS: `execution.ts#DEFAULT_TIMEOUT_MS`。
pub const DEFAULT_TIMEOUT_MS: f64 = 300_000.0;

/// 单个全局名的标识符形态：`^[A-Za-z_$][A-Za-z0-9_$]*$`（JS 正则，ASCII 类）。
///
/// TS: `execution.ts#IDENTIFIER`。
fn is_identifier(part: &str) -> bool {
    let mut chars = part.chars();
    match chars.next() {
        None => false,
        Some(first) => {
            (first.is_ascii_alphabetic() || first == '_' || first == '$')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        }
    }
}

/// 脚本不得遮蔽的内建全局。
///
/// TS: `execution.ts#RESERVED_GLOBALS`。
const RESERVED_GLOBALS: [&str; 9] = [
    "tools",
    "ALL_TOOLS",
    "console",
    "text",
    "image",
    "exit",
    "globalThis",
    "store",
    "load",
];

fn is_reserved_global(name: &str) -> bool {
    RESERVED_GLOBALS.contains(&name)
}

/// 回调：launcher 用它报告 VM 做了什么。
///
/// TS: `execution.ts#VmEvents`。
pub trait VmEvents: Send + Sync {
    /// VM 的一条消息。这里做校验，所以 launcher 可以原样透传收到的任何值。
    ///
    /// 解码失败或宿主回调 panic 会以 `kind: "sandbox"` 结束本次执行，不会把错误抛回
    /// launcher；`done`/`crash` 之后到达的消息被忽略（`finished` 已置位）。
    fn message(&self, message: Value);
    /// VM 在桥接之外失败或停止，例如 worker 退出或远端错误。
    ///
    /// 错误原样进入结果（`kind` 不被改写）；若本次执行已有终态，这次调用被忽略。
    fn failure(&self, error: CodemodeError);
    /// 只有把 VM 跑在宿主线程上的 launcher（[`crate::runtime::inline`]）才取它：它用这个闸门
    /// 把 deadline 与 abort 事件推迟到 VM 让出宿主之后（TS 里调用线程被计算占住，宿主事件
    /// 循环不跑）。worker 与 remote 的 launcher 不取，宿主事件照常立即触发；默认 `None`。
    fn host_event_loop(&self) -> Option<Arc<HostEventLoop>> {
        None
    }
}

/// 「宿主事件循环」闸门：让跑在宿主线程上的 VM 把 deadline 与 abort 事件推迟到它让出宿主
/// 之后的那个事件循环轮次。
///
/// TS 的 inline 后端把 VM 跑在调用线程上：脚本计算期间宿主事件循环不跑，`timeoutMs` 定时器
/// 与 abort 监听都不会触发，它们要等 VM 让出线程（等待工具或结束）才运行（spec §A.5.2）。
/// Rust 的 VM 在专用线程上，因此 launcher 显式占住闸门：[`set_busy`](Self::set_busy) 在 VM
/// 开始执行前调用，[`set_idle`](Self::set_idle) 在 VM 让出、且这一轮产生的 VM 消息都交给
/// 宿主之后调用（TS 的微任务先于定时器运行）；deadline 与 abort 监听先用
/// [`wait_until_idle`](Self::wait_until_idle) 等待。worker 与 remote 的 launcher 从不占用
/// 闸门，它们的宿主事件照常立即触发。
///
/// `generation` 是唯一真相，`busy` 的 watch 只是唤醒信号：[`wait_until_idle`](Self::wait_until_idle)
/// 每轮先订阅再读 `generation`，因此不会在「读到忙」与「开始等待」之间错过一次
/// [`set_idle`](Self::set_idle)。
pub struct HostEventLoop {
    /// 当前占用者（0 = 空闲）；每次占用加一，让出时必须带上同一代令牌。
    generation: AtomicU64,
    busy: watch::Sender<u64>,
}

impl HostEventLoop {
    /// 新建一个空闲的闸门（每次执行一个）。
    fn new() -> Self {
        let (busy, _) = watch::channel(0);
        HostEventLoop {
            generation: AtomicU64::new(0),
            busy,
        }
    }

    /// VM 开始执行（宿主线程被它占住），返回这次占用的令牌。
    pub fn set_busy(&self) -> u64 {
        let token = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let _ = self.busy.send(token);
        token
    }

    /// VM 让出宿主。调用方必须先把这一轮 VM 消息交给宿主。
    ///
    /// 只有令牌仍是最新时才会真正放开：并发的下一段执行可能已经重新占住了闸门。
    pub fn set_idle(&self, token: u64) {
        if self
            .generation
            .compare_exchange(token, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = self.busy.send(0);
        }
    }

    /// 等到 VM 让出宿主；VM 从未开始或已经让出时立即返回。
    pub async fn wait_until_idle(&self) {
        loop {
            let mut busy = self.busy.subscribe();
            if self.generation.load(Ordering::SeqCst) == 0 {
                return;
            }
            if busy.changed().await.is_err() {
                return;
            }
        }
    }
}

/// 宿主对运行中 VM 的句柄。
///
/// TS: `execution.ts#VmChannel`。
pub trait VmChannel: Send + Sync {
    /// 向 VM 投递一条宿主消息。
    ///
    /// channel 装上之前（launcher 的 future 还没返回）产生的回复由 `Execution` 排队，装上后
    /// 按产生顺序冲洗。
    fn post(&self, message: HostToWorkerMessage);
    /// 请求 VM 关闭。只调用一次；可以在 worker 或远端 VM 真正退出之前完成。
    fn stop(&self) -> BoxFuture<'static, ()>;
}

/// 为一次执行启动 VM。返回的 future 失败时错误文本成为 `kind: "sandbox"` 的 message。
///
/// TS: `execution.ts#VmLauncher`。
pub type VmLauncher = Arc<
    dyn Fn(VmStartData, Arc<dyn VmEvents>) -> BoxFuture<'static, Result<Arc<dyn VmChannel>, String>>
        + Send
        + Sync,
>;

/// TS 的 `errorMessage(error)`：`Error` 取 `message`，否则 `String(error)`。
///
/// Rust 侧错误的 `Display` 就是 TS 的 `message`（ARCHITECTURE.md §4.1），所以这里直接取 `Display`。
/// `host.rs`/`inline.rs`/`remote.rs` 的 launcher 用它把失败文本拼进
/// `Failed to load QuickJS: …` 这类前缀（TS 的同一个导入）。
pub fn error_message(error: &(dyn std::error::Error + 'static)) -> String {
    error.to_string()
}

/// `store` 的序列化快照：`key -> JSON 文本`。
///
/// TS: `execution.ts#serializeStore`（`JSON.stringify(value)` 为 `undefined` 的条目跳过）。
/// Rust 的输入是 `serde_json::Value`，无法表示 JS 的 `undefined`/函数/`Symbol`，因此每个条目
/// 都能序列化、没有条目会被跳过（`NaN`/`Infinity` 同样不可表达）。
fn serialize_store(store: Option<&IndexMap<String, Value>>) -> IndexMap<String, String> {
    let mut serialized = IndexMap::new();
    if let Some(store) = store {
        for (key, value) in store {
            serialized.insert(key.clone(), kiana_json::js_object::stringify(value));
        }
    }
    serialized
}

/// 桥接载荷无法解码。
///
/// TS: `execution.ts#BridgeError`（脚本可能改写了内建，使 prelude 送出畸形数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct BridgeError(String);

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl BridgeError {
    fn new(message: impl Into<String>) -> Self {
        BridgeError(message.into())
    }
}

/// `JSON.parse(json)`，失败 → `BridgeError("<what> is not valid JSON")`。
///
/// TS: `execution.ts#parseBridgeJson`。
fn parse_bridge_json(json: &str, what: &str) -> Result<Value, BridgeError> {
    kiana_json::js_parse::parse(json)
        .map_err(|_| BridgeError::new(format!("{what} is not valid JSON")))
}

/// 消息里的一个字段是「字符串」还是 `undefined`（键缺失）。
///
/// TS 的守卫只校验 `type`；其余字段在解构时被读取，类型错了也不会在这里报错，而是带着
/// 原始值继续（例如 `{"type":"done","ok":true,"value":5}` 的 `value` 是数字 5，
/// `message.value === undefined` 为假 → `parseBridgeJson(5, "return value")` →
/// `JSON.parse(5)` 得到 `5`）。所以这里只在字段是**字符串**时取字符串，其余一律按
/// `undefined` 处理，把「保留原值」的语义交给调用方。
///
/// 返回 `None` 表示 TS 的 `undefined`（键缺失、或值不是字符串——后者在 TS 里会以别的
/// 方式失败，见各调用点）。
fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    match object.get(key) {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    }
}

/// JS 的数值字段 → `u64`：`1` 与 `1.0` 是同一个数（`serde_json` 的 `as_u64` 只认前者的
/// 表示），因此整数取整后接受；负数/非有限值返回 [`None`]；大于 `u64::MAX as f64` 的值返回
/// [`None`]，但恰好等于该值的输入（2^64）会经 Rust 浮点转整数的饱和语义得到 `u64::MAX`
/// （冻结的协议字段是 `u64`，见模块文档）。
fn js_u64(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_f64()?;
    if !number.is_finite() || number.fract() != 0.0 || number < 0.0 || number > u64::MAX as f64 {
        return None;
    }
    Some(number as u64)
}

/// `JSON.parse(value)` 的兜底：TS 在类型不是字符串时会让 `JSON.parse` 走 `String(value)`
/// 之前的 `ToString` 步骤。
///
/// TS 的 `JSON.parse(5)` → `5`；`JSON.parse(null)` → `null`；`JSON.parse(true)` → `true`；
/// `JSON.parse(undefined)` 抛 `SyntaxError: "undefined" is not valid JSON`（V8 的
/// `JSON.parse` 对非字符串先做 `ToString`）。
fn parse_bridge_value(value: Option<&Value>, what: &str) -> Result<Value, BridgeError> {
    match value {
        Some(Value::String(text)) => parse_bridge_json(text, what),
        // TS: `JSON.parse(undefined)` → `SyntaxError`，模板是 `"undefined" is not valid JSON`；
        // 桥接层统一成 `<what> is not valid JSON`。
        None => Err(BridgeError::new(format!("{what} is not valid JSON"))),
        // 非字符串：`ToString(value)` 后再 `JSON.parse`（数字/布尔/null 的文本都是合法 JSON）。
        Some(other) => parse_bridge_json(&kiana_json::js_string::js_string(Some(other)), what),
    }
}

/// 解码 `writes`：`[key, json]` 是写入，`[key]` 是删除。
///
/// TS: `execution.ts#parseStoreWrites`。参数是 `JSON.parse` 之后的值；调用方负责先按
/// [`parse_bridge_value`] 的规则解析（TS 的 `JSON.parse(5)` 会先 `ToString` 再解析）。
fn parse_store_writes_value(entries: &Value) -> Result<CodemodeStoreWrites, BridgeError> {
    let Value::Array(entries) = entries else {
        return Err(BridgeError::new("store writes are not an array"));
    };
    let mut writes = CodemodeStoreWrites::default();
    for entry in entries {
        let Value::Array(entry) = entry else {
            return Err(BridgeError::new("store writes contain a malformed entry"));
        };
        let malformed = || BridgeError::new("store writes contain a malformed entry");
        let Some(Value::String(key)) = entry.first() else {
            return Err(malformed());
        };
        let value = match entry.len() {
            1 => None,
            2 => match entry.get(1) {
                Some(Value::String(value)) => Some(value),
                _ => return Err(malformed()),
            },
            _ => return Err(malformed()),
        };
        match value {
            None => writes.delete.push(key.clone()),
            Some(value) => {
                let parsed = parse_bridge_json(
                    value,
                    &format!("store value for {}", kiana_json::js_string::js_quote(key)),
                )?;
                writes.set.insert(key.clone(), parsed);
            }
        }
    }
    Ok(writes)
}

/// `{ seq, issued }` 的 `during`（确定性模式）。
///
/// TS: `during === undefined ? {} : { during: { seq: during.seq, issued: during.issued } }`
/// （`execution.ts#finish`）。字段缺失时 `undefined`；冻结的 [`CodemodeDuring`] 是两个
/// `u64`，因此缺失的字段取 0（真实 prelude 永远给数字）。
///
/// 非对象（含 `null`）在这里当作缺失：TS 对 `null` 会抛 TypeError，而那个 throw 发生在
/// `finish()` 内部、`finished` 已置位之后，结果 promise 因此永不结算、`close()` 也挂住
/// （node 26 实测）——一个不可取的崩溃路径，见模块文档。
fn parse_during(value: Option<&Value>) -> Option<CodemodeDuring> {
    let Some(Value::Object(object)) = value else {
        return None;
    };
    Some(CodemodeDuring {
        seq: js_u64(object.get("seq")).unwrap_or(0),
        issued: js_u64(object.get("issued")).unwrap_or(0),
    })
}

/// 解码脚本错误 `{ name?, message, stack? }`（`JSON.parse` 之后的值）。
///
/// TS: `execution.ts#parseScriptError`。调用方先用 [`parse_bridge_value`] 解析。
fn parse_script_error_value(parsed: &Value) -> Result<CodemodeError, BridgeError> {
    let object = match parsed {
        Value::Object(object) => Some(object),
        // TS 的 `typeof parsed !== "object" || parsed === null`：数组能走到解构，但取不到字段，
        // 于是落到 "script error is malformed"。
        Value::Array(_) => None,
        _ => return Err(BridgeError::new("script error is not an object")),
    };
    let Some(object) = object else {
        return Err(BridgeError::new("script error is malformed"));
    };
    let message = match object.get("message") {
        Some(Value::String(message)) => message.clone(),
        _ => return Err(BridgeError::new("script error is malformed")),
    };
    let name = match object.get("name") {
        None => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => return Err(BridgeError::new("script error is malformed")),
    };
    let stack = match object.get("stack") {
        None => None,
        Some(Value::String(stack)) => Some(stack.clone()),
        Some(_) => return Err(BridgeError::new("script error is malformed")),
    };
    Ok(CodemodeError {
        kind: CodemodeErrorKind::Script,
        name,
        message,
        stack,
    })
}

/// 校验全局名：标识符或 `namespace.member`，不得遮蔽内建全局，也不得彼此冲突。
///
/// TS: `execution.ts#checkGlobals`。检查顺序：先逐项校验，再校验命名空间冲突。
///
/// 命名空间用 `IndexMap`（插入序）而不是 `HashSet`：TS 的 `Set` 按插入序迭代，第二个循环因此
/// 总是报**注册序里第一个**冲突的命名空间；`HashSet` 的迭代序随进程随机，同一个输入会在
/// `a`/`c` 之间摇摆。
fn check_globals(globals: &[CodemodeDeclaredGlobal]) -> Result<(), String> {
    let mut names: HashSet<String> = HashSet::new();
    let mut namespaces: IndexMap<String, ()> = IndexMap::new();
    for global in globals {
        let parts: Vec<&str> = global.name.split('.').collect();
        if parts.len() > 2
            || !parts.iter().all(|part| is_identifier(part))
            || is_reserved_global(parts[0])
        {
            return Err(format!("Invalid global name \"{}\"", global.name));
        }
        if names.contains(&global.name) {
            return Err(format!("Global \"{}\" is already registered", global.name));
        }
        if parts.len() == 2 {
            namespaces.insert(parts[0].to_string(), ());
        }
        names.insert(global.name.clone());
    }
    for name in namespaces.keys() {
        if names.contains(name) {
            return Err(format!(
                "Global \"{name}\" conflicts with the namespace \"{name}\""
            ));
        }
    }
    Ok(())
}

/// 一次调用的在途状态。
///
/// TS: `execution.ts#PendingCall`（`record` 是 `calls` 里的同一对象引用；Rust 用下标）。
struct PendingCall {
    /// `calls` 向量里的下标（仅 tool 调用有记录）。
    record: Option<usize>,
    started_at: Instant,
    /// 取消时通知工具（TS: `AbortController`）。
    controller: CancellationToken,
}

/// `deliver()` 已用过的结果号与在途调用。
#[derive(Default)]
struct Shared {
    /// `deliver()` 已经消耗的结果号（每次执行内唯一，从 1 起）。
    delivered: HashSet<u64>,
    /// 正在等待回答的调用：VM 的 `id` → 调用状态。
    pending: HashMap<u64, PendingCall>,
}

/// 宿主与 VM 之间的通道状态：channel 未就绪前回复排队。
#[derive(Default)]
struct ChannelState {
    /// launcher 返回的 VM 句柄；`None` 表示 channel 尚未安装——launcher 仍在启动，或启动
    /// 失败/执行已结束而从未安装。
    channel: Option<Arc<dyn VmChannel>>,
    /// channel 就绪前产生的宿主消息，按产生顺序保存。
    unsent: Vec<HostToWorkerMessage>,
}

/// 宿主回调抛出的错误（Rust 里是 panic，TS 里是 throw）。
///
/// 桥接载荷解码失败（[`BridgeError`]）与宿主自身失败分开，两者的最终文案不同。
enum HostFailure {
    /// 桥接载荷无法解码，最终文案是 `Sandbox bridge broken: …`。
    Bridge(BridgeError),
    /// 宿主回调 panic（或缺少必填回调），最终文案是 `Sandbox host failed: …`。
    Host(String),
}

impl From<BridgeError> for HostFailure {
    fn from(error: BridgeError) -> Self {
        HostFailure::Bridge(error)
    }
}

/// 从 `catch_unwind` 的载荷里取文本：`panic!("literal")` 的载荷是 `&str`，
/// `panic!("{x}")` 与 `std::panic::panic_any(String)` 是 `String`，其它类型回落到
/// `"an unknown error"`。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "an unknown error".to_string()
    }
}

/// 轮询一个 future，把**轮询期间的 panic** 折成 `Err(payload)`。
///
/// TS 的 `try { await tool.execute(...) } catch` 覆盖的是工具的整个生命周期；Rust 里「异步
/// 工具抛错」的惯用写法就是 panic，因此在 `.await` 上也要接住（只在创建 future 时
/// `catch_unwind` 不够：panic 发生在 `poll` 里，会直接杀掉宿主任务）。
///
/// `Pin<Box<F>>` 是 `Unpin`，所以投影不需要 unsafe；捕获到 panic 后立即返回 `Ready`，不会再
/// 轮询已经 panic 的 future。
struct CatchUnwind<F: std::future::Future>(std::pin::Pin<Box<F>>);

impl<F: std::future::Future> CatchUnwind<F> {
    fn new(future: F) -> Self {
        CatchUnwind(Box::pin(future))
    }
}

impl<F: std::future::Future> std::future::Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            this.0.as_mut().poll(context)
        })) {
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
            Err(payload) => std::task::Poll::Ready(Err(payload)),
        }
    }
}

/// 执行宿主回调并把 panic 折成 `Sandbox host failed:` 的文本（TS 的 `throw` 在 `receive` 的 catch 里）。
fn call_host<F: FnOnce()>(callback: F) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)).map_err(panic_message)
}

/// 宿主的运行时句柄；调用方不在 Tokio 运行时内时退回到进程级运行时（见模块文档）。
///
/// 兜底运行时是 `static` 的 multi-thread runtime（2 个 worker 线程、`enable_all`），第一次
/// 需要时创建、进程结束前不关闭，因此本模块 `spawn` 的任务不会因为调用方没有 runtime 而
/// 无处执行。
fn runtime_handle() -> tokio::runtime::Handle {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return handle;
    }
    static FALLBACK: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    FALLBACK
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("codemode: building the fallback Tokio runtime")
        })
        .handle()
        .clone()
}

/// `Execution.executors`：已注册工具与全局函数的查找表。
///
/// TS 里 `tools` 是执行开始时的快照、`globals` 是 sandbox 的活表（构造后新注册的全局函数
/// 能被在途执行看到）——两者语义不同，因此这里分别是不可变快照与共享的可变表。
pub(crate) struct Executors {
    pub(crate) tools: Arc<IndexMap<String, CodemodeTool>>,
    pub(crate) globals: Arc<Mutex<IndexMap<String, CodemodeTool>>>,
}

/// 宿主控制的 run 的回调（`start()` 路径）。
///
/// TS: `execution.ts#Control`。
#[derive(Clone)]
pub(crate) struct Control {
    /// VM 报告指纹时调用，参数是 `message.fingerprint`。
    pub(crate) on_started: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// VM 发起调用时调用；宿主已经在 `pending` 里登记，回答走 `CodemodeRun::deliver`。
    pub(crate) on_call: Option<Arc<dyn Fn(crate::types::CodemodeHostCall) + Send + Sync>>,
    /// VM 处理完结果 `seq` 并再次等待时调用，第二个参数是到那时为止发出的调用数。
    pub(crate) on_idle: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
}

/// [`Execution`] 的构造参数。
///
/// TS: `execution.ts#ExecutionOptions`。
pub(crate) struct ExecutionOptions {
    pub(crate) code: String,
    pub(crate) tools: Vec<CodemodeDeclaredTool>,
    pub(crate) globals: Vec<CodemodeDeclaredGlobal>,
    pub(crate) executors: Executors,
    pub(crate) timeout_ms: f64,
    pub(crate) signal: Option<CancellationToken>,
    /// `signal.reason` 的 `message`（TS: `reason instanceof Error ? reason.message : undefined`）。
    pub(crate) abort_reason: Option<String>,
    pub(crate) memory_limit_bytes: Option<f64>,
    /// `load()` 的初值快照：key → JSON 文本（`serialize_store` 的输出）。
    pub(crate) store: IndexMap<String, String>,
    /// `Some` = `start()` 的确定性 run；`None` = `execute()`。
    pub(crate) deterministic: Option<VmDeterministic>,
    /// `start()` 的宿主回调；`None` 表示 `execute()`（宿主自己执行工具）。
    pub(crate) control: Option<Control>,
    /// VM 启动器：由具体 sandbox 子类（host/inline/remote）注入。
    pub(crate) launch: VmLauncher,
}

/// 在独立 VM 里跑一次脚本。
///
/// TS: `execution.ts#Execution`。
pub(crate) struct Execution {
    /// 所有后台任务的 Tokio 句柄（deadline、abort 监听、launcher、工具调用、`stop`）。
    handle: tokio::runtime::Handle,
    /// 结果广播通道：`finish()` 写入一次，所有 `result_future` 读同一份。
    result_tx: watch::Sender<Option<CodemodeResult>>,
    /// 常驻接收端：保证 `result_tx.send` 永不因「暂无接收者」而丢弃结果（TS 的 Promise 与是否
    /// 有人 await 无关）。字段本身不被读取，存在即用途。
    _result_keepalive: watch::Receiver<Option<CodemodeResult>>,
    /// 终态标志：`finish()` 以 `swap` 决胜；置位后消息、`deliver` 与后续终态事件都被丢弃。
    finished: Arc<AtomicBool>,
    /// 工具/全局函数查找表（`execute` 路径使用；`start` 路径是空表，调用全走 `control`）。
    executors: Executors,
    /// `start` 路径的宿主回调；`None` 表示 `execute` 路径（宿主自己执行工具）。
    control: Option<Control>,
    /// `signal.reason` 是 `Error` 时的 `message`（TS: `Execution.onAbort`）。
    abort_reason: Option<String>,
    /// launcher 返回的 VM 句柄，以及 channel 就绪前的待发回复。
    channel: Mutex<ChannelState>,
    /// `deliver` 已用过的结果号与在途调用（id → 调用）。
    shared: Mutex<Shared>,
    /// 累积的输出项，`finish` 时克隆进结果。
    output: Mutex<Vec<CodemodeOutputItem>>,
    /// 按登记顺序累积的工具调用记录（全局调用不记账），`finish` 时克隆进结果。
    calls: Mutex<Vec<CodemodeCall>>,
    /// deadline 任务句柄；`finish`（或 `Drop`）时 abort。
    timer: Mutex<Option<JoinHandle<()>>>,
    /// abort 监听任务句柄；`finish`（或 `Drop`）时 abort，等价 TS 的 `{once:true}` 移除。
    abort_listener: Mutex<Option<JoinHandle<()>>>,
    /// 跑在宿主线程上的 VM（inline）占住宿主事件循环时，deadline 与 abort 等它让出。
    host_event_loop: Arc<HostEventLoop>,
}

impl Execution {
    /// 武装 deadline 与 abort 监听，然后请求 launcher 启动 VM。
    ///
    /// TS: `execution.ts#Execution.constructor`。
    fn start(options: ExecutionOptions) -> Arc<Execution> {
        let handle = runtime_handle();
        let (result_tx, _result_keepalive) = watch::channel(None);
        let execution = Arc::new(Execution {
            handle: handle.clone(),
            result_tx,
            _result_keepalive,
            finished: Arc::new(AtomicBool::new(false)),
            executors: options.executors,
            control: options.control,
            abort_reason: options.abort_reason,
            channel: Mutex::new(ChannelState::default()),
            shared: Mutex::new(Shared::default()),
            output: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            timer: Mutex::new(None),
            abort_listener: Mutex::new(None),
            host_event_loop: Arc::new(HostEventLoop::new()),
        });

        // 1. deadline：`Number.isFinite(timeoutMs)` 时才武装。
        if options.timeout_ms.is_finite() {
            let delay = node_timeout_delay(options.timeout_ms);
            let timeout_ms = options.timeout_ms;
            let this = Arc::clone(&execution);
            let host_event_loop = Arc::clone(&execution.host_event_loop);
            let task = handle.spawn(async move {
                tokio::time::sleep(delay).await;
                // TS 里定时器回调只在宿主事件循环跑的时候才执行；inline 的 VM 正占用宿主
                // 线程时它要等到 VM 让出（spec §A.5.2）。
                host_event_loop.wait_until_idle().await;
                this.finish(
                    Some(CodemodeError::new(
                        CodemodeErrorKind::Timeout,
                        format!(
                            "Execution timed out after {} ms",
                            kiana_json::js_num::format_js_number(timeout_ms)
                        ),
                    )),
                    None,
                    None,
                    None,
                );
            });
            *execution.timer.lock().expect("timer mutex") = Some(task);
        }

        // 2. abort：已取消则立即结束，否则挂一个监听（`{once:true}`，finish 时移除）。
        if let Some(signal) = &options.signal {
            if signal.is_cancelled() {
                execution.on_abort();
            } else {
                let this = Arc::clone(&execution);
                let signal = signal.clone();
                let host_event_loop = Arc::clone(&execution.host_event_loop);
                let task = handle.spawn(async move {
                    signal.cancelled().await;
                    // abort 事件同样要等宿主事件循环能跑（inline 的 VM 计算期间不行）。
                    host_event_loop.wait_until_idle().await;
                    this.on_abort();
                });
                *execution.abort_listener.lock().expect("abort mutex") = Some(task);
            }
        }
        if execution.finished.load(Ordering::SeqCst) {
            // 已经终态（信号在构造时就已取消 → `on_abort` → `finish`）：不再启动 VM。
            // TS 的构造函数在这里直接 return，launcher 根本不会被调用（tests/execution.rs 的
            // `an_already_cancelled_signal_finishes_before_launching` 锁定这一点）。
            return execution;
        }

        // 3. 启动 VM。
        let data = VmStartData {
            code: options.code,
            tools: options
                .tools
                .iter()
                .map(|tool| VmToolInfo {
                    name: tool.name.clone(),
                    js_name: tool.script_name(),
                    description: tool.description.clone().unwrap_or_default(),
                })
                .collect(),
            globals: options
                .globals
                .iter()
                .map(|global| VmGlobalInfo {
                    name: global.name.clone(),
                    spread: global.spread == Some(true),
                })
                .collect(),
            memory_limit_bytes: options.memory_limit_bytes,
            store: options.store,
            deterministic: options.deterministic,
        };
        let events: Arc<dyn VmEvents> = Arc::new(ExecutionEvents(Arc::clone(&execution)));
        let launch = options.launch;
        let this = Arc::clone(&execution);
        handle.spawn(async move {
            // TS: `new Promise(resolve => resolve(options.launch(data, events))).then(…, error =>
            // finish({kind:"sandbox", message: errorMessage(error)}))`——launcher 的同步 throw 与
            // future 的 reject 是同一条路径。Rust 的 launcher 只有 panic 能表达 throw，因此创建
            // 与轮询两处都接住，统一落到 `finish(...)`；否则任务直接消失，执行永远不结算。
            let started = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                launch(data, events)
            })) {
                Ok(future) => match CatchUnwind::new(future).await {
                    Ok(started) => started,
                    Err(payload) => Err(panic_message(payload)),
                },
                Err(payload) => Err(panic_message(payload)),
            };
            match started {
                Ok(channel) => {
                    if this.finished.load(Ordering::SeqCst) {
                        channel.stop().await;
                        return;
                    }
                    this.install_channel(channel);
                }
                Err(message) => this.finish(
                    Some(CodemodeError::new(CodemodeErrorKind::Sandbox, message)),
                    None,
                    None,
                    None,
                ),
            }
        });
        execution
    }

    /// 结果 future（多个调用方共享同一份结果）。
    ///
    /// 返回的 future 自己持有 `Arc<Execution>`（TS 的 Promise 由 `Execution` 对象上的闭包
    /// `resolveResult` 支撑，只要有人在等就已经存在）。因此宿主丢掉 `CodemodeSandbox` 时，
    /// 在途执行的 result future 仍能正常结算，不会看到「发送方先消失」。
    fn result_future(self: &Arc<Self>) -> BoxFuture<'static, CodemodeResult> {
        let mut receiver = self.result_tx.subscribe();
        let keepalive = Arc::clone(self);
        Box::pin(async move {
            // 保活：sender 由 `Execution` 持有，`keepalive` 让 `Execution` 至少活到这个 future
            // 被 drop 或结算。
            let _keepalive = keepalive;
            loop {
                if let Some(result) = receiver.borrow_and_update().clone() {
                    return result;
                }
                if receiver.changed().await.is_err() {
                    // sender 随 `_keepalive` 一起存续；只有 `Execution` 被回收才可能到这里，
                    // 而那时结果必然已经写过（`finish` 在同一个 `Execution` 上）。
                    return receiver.borrow().clone().unwrap_or_else(|| {
                        CodemodeResult::failure(
                            CodemodeError::new(
                                CodemodeErrorKind::Sandbox,
                                "the execution was dropped before it settled",
                            ),
                            Vec::new(),
                            Vec::new(),
                            None,
                        )
                    });
                }
            }
        })
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// `CodemodeRun.deliver()`。
    ///
    /// 校验顺序与 TS 逐条一致：已结束静默返回 → `now` 有限 → `seq` 是 1 起、只用一次的正整数 →
    /// 调用在等待 → `properties` 是对象 → 序列化（先于记录任何状态）。
    fn deliver(&self, seq: f64, now: f64, id: u64, outcome: CodemodeOutcome) -> Result<(), String> {
        if self.is_finished() {
            return Ok(());
        }
        // TS 在模板字面量里插值 `${seq}`：那是 `Number::toString`（`String(NaN)` = `"NaN"`、
        // `String(Infinity)` = `"Infinity"`），**不是** JSON 的 `JSON.stringify` 规则（后者把
        // 非有限值写成 `null`）。`to_radix_string(v, 10)` 就是 `Number::toString`。
        let seq_text = kiana_json::js_num::to_radix_string(seq, 10);
        if !now.is_finite() {
            return Err(format!(
                "The clock value of result {seq_text} is not a finite number"
            ));
        }
        if !is_js_integer(seq) || seq < 1.0 {
            return Err(format!(
                "Result number {seq_text} is not a positive integer used once"
            ));
        }
        // `HostResult.seq` 是 u64；JS 里超出 u64 的整数在本协议内无法表达（见模块文档）。
        let seq_number = if seq >= u64::MAX as f64 {
            u64::MAX
        } else {
            seq as u64
        };
        // 通过全部校验后才动共享状态：失败的 `deliver` 不消耗 seq（tests/execution.rs 的
        // `deliver_validates_the_result_number_and_the_waiting_call` 锁定）。
        let mut shared = self.shared.lock().expect("shared mutex");
        if shared.delivered.contains(&seq_number) {
            return Err(format!(
                "Result number {seq_text} is not a positive integer used once"
            ));
        }
        if !shared.pending.contains_key(&id) {
            return Err(format!("Call {id} is not waiting for a result"));
        }
        // `properties` 必须是对象：Rust 的 `CodemodeOutcomeErr.properties` 只能是对象（类型即保证），
        // 因此这里直接序列化。
        let properties = match &outcome {
            CodemodeOutcome::Ok(ok) => {
                // 成功结果没有 `properties`：TS 只把 properties 用在失败支。`Ok(ok)` 的绑定
                // 在这里未使用，`let _ = ok;` 只为消费它以避免 `unused_variables` 警告
                // （等价写法是 `CodemodeOutcome::Ok(_)`）。
                let _ = ok;
                None
            }
            CodemodeOutcome::Err(error) => error.properties.as_ref().map(|properties| {
                let object: serde_json::Map<String, Value> = properties
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                kiana_json::js_object::stringify(&Value::Object(object))
            }),
        };
        let payload = match &outcome {
            CodemodeOutcome::Ok(ok) => ok.value.as_ref().map(kiana_json::js_object::stringify),
            CodemodeOutcome::Err(error) => Some(error.message.clone()),
        };
        let ok = matches!(outcome, CodemodeOutcome::Ok(_));
        shared.delivered.insert(seq_number);
        let pending = shared
            .pending
            .remove(&id)
            .expect("pending call checked above");
        drop(shared);

        if let Some(index) = pending.record {
            let duration = Instant::now().saturating_duration_since(pending.started_at);
            let mut calls = self.calls.lock().expect("calls mutex");
            if let Some(record) = calls.get_mut(index) {
                record.status = if ok {
                    CodemodeCallStatus::Ok
                } else {
                    CodemodeCallStatus::Error
                };
                // TS: `performance.now() - pending.startedAt`（不取整）。
                record.duration_ms = duration.as_secs_f64() * 1000.0;
            }
        }
        self.post(HostToWorkerMessage::Result(HostResult {
            id,
            ok,
            payload,
            seq: Some(seq_number),
            now: Some(now),
            properties,
        }));
        Ok(())
    }

    /// `abort(message)`：以 `kind: "aborted"` 结束，返回结果 future。
    fn abort(self: &Arc<Self>, message: &str) -> BoxFuture<'static, CodemodeResult> {
        self.finish(
            Some(CodemodeError::new(CodemodeErrorKind::Aborted, message)),
            None,
            None,
            None,
        );
        self.result_future()
    }

    /// 调用方 abort 的监听回调。
    ///
    /// TS: `execution.ts#Execution.onAbort`：`signal.reason instanceof Error ? reason.message :
    /// "Execution aborted"`。`signal.reason` 在 Rust 侧由 [`ExecutionOptions::abort_reason`] 承载
    /// （冻结的 `signal` 只有 [`CancellationToken`]，见模块文档）：显式 reason 走
    /// `Some(reason)`；`None` 对应「调用方无参 `abort()`」——Node/Bun 把 `reason` 设成
    /// `message` 为 `This operation was aborted` 的 DOMException，且它 `instanceof Error`，
    /// 所以可观察文本就是 `This operation was aborted`。
    fn on_abort(self: &Arc<Self>) {
        let message = match &self.abort_reason {
            Some(reason) => reason.clone(),
            None => "This operation was aborted".to_string(),
        };
        self.finish(
            Some(CodemodeError::new(CodemodeErrorKind::Aborted, message)),
            None,
            None,
            None,
        );
    }

    /// 有 channel 直接投递，否则排队等 channel 到达。
    fn post(&self, message: HostToWorkerMessage) {
        let mut state = self.channel.lock().expect("channel mutex");
        match &state.channel {
            Some(channel) => {
                let channel = Arc::clone(channel);
                drop(state);
                channel.post(message);
            }
            None => state.unsent.push(message),
        }
    }

    /// launcher 返回 channel：已结束则直接请它停止，否则装上并把排队的回复冲洗出去。
    fn install_channel(self: &Arc<Self>, channel: Arc<dyn VmChannel>) {
        let mut state = self.channel.lock().expect("channel mutex");
        if self.is_finished() {
            // `finish()` 在 channel 装上之前就跑完了：它看到的 channel 是 `None`，因此这里负责
            // 请求停止（TS 在构造函数里用 `finished` 检查覆盖同一条路径）。
            drop(state);
            self.stop_channel(channel);
            return;
        }
        state.channel = Some(Arc::clone(&channel));
        let unsent = std::mem::take(&mut state.unsent);
        drop(state);
        for message in unsent {
            channel.post(message);
        }
    }

    /// 不阻塞地请求 channel 停止；`stop()` 里 panic 只会让任务以 `JoinError` 结束，结果照常结算
    /// （TS 的 `.catch(() => undefined)`）。
    fn stop_channel(&self, channel: Arc<dyn VmChannel>) {
        self.handle.spawn(async move {
            channel.stop().await;
        });
    }

    /// launcher 的一条消息：任何异常都变成 `sandbox` 错误，不会逃进 launcher 的传输层。
    ///
    /// TS: `execution.ts#Execution.receive`。
    fn receive(self: &Arc<Self>, message: Value) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_message(&message)
        }));
        let error = match outcome {
            Ok(Ok(())) => return,
            Ok(Err(HostFailure::Bridge(error))) => CodemodeError::new(
                CodemodeErrorKind::Sandbox,
                format!(
                    "Sandbox bridge broken: {error}. The script may have modified built-ins such as a prototype's toJSON."
                ),
            ),
            Ok(Err(HostFailure::Host(message))) => CodemodeError::new(
                CodemodeErrorKind::Sandbox,
                format!("Sandbox host failed: {message}"),
            ),
            Err(payload) => CodemodeError::new(
                CodemodeErrorKind::Sandbox,
                format!("Sandbox host failed: {}", panic_message(payload)),
            ),
        };
        self.finish(Some(error), None, None, None);
    }

    /// TS: `execution.ts#Execution.handleMessage`。
    fn handle_message(self: &Arc<Self>, message: &Value) -> Result<(), HostFailure> {
        if self.is_finished() {
            return Ok(());
        }
        if !is_worker_to_host_message(message) {
            return Err(BridgeError::new("unknown message from the worker").into());
        }
        // TS 的守卫只校验 `type`，其余字段在解构时按原样读取。这里因此按字段**结构化**解码，
        // 不整体走 serde：整体反序列化会把「类型不对」变成 serde 文本（`Sandbox host failed: …`），
        // 而 TS 要么安静接受（例如 `{"type":"output"}` 推入 `undefined`），要么把它交给桥接层
        // 的解析器（例如 `value: 5` 经 `JSON.parse(5)` 得到 `5`）。
        let object = match message {
            Value::Object(object) => object,
            // `is_worker_to_host_message` 要求对象，这里不可能到。
            _ => return Err(BridgeError::new("unknown message from the worker").into()),
        };
        let type_name = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match type_name {
            "output" => {
                // TS: `output.push(message.item)`——**不校验**、也不会抛错：任何值（含
                // `undefined`）都进 `output`。冻结的 `CodemodeOutputItem` 只能表示规范的两支
                // （`{type:"text",text}` / `{type:"image",data,mimeType}`），其余形状（缺
                // `item`、类型不对、未知 `type`）无法表达，这里跳过该项而不是让整次执行以
                // `Sandbox host failed: …` 结束——TS 永远不会因为一条 output 消息失败。真实
                // prelude 只产生规范形状。
                if let Some(item) = object.get("item")
                    && let Ok(item) = serde_json::from_value::<CodemodeOutputItem>(item.clone())
                {
                    self.output.lock().expect("output mutex").push(item);
                }
                Ok(())
            }
            "call" => {
                // TS 的 `const { id, name, target } = message` 是原样取值：`id` 不是数字时
                // `pending.has(undefined)` 为假，调用会照常登记（`pending` 用 `undefined` 作键），
                // 于是 `onCall` 收到 `id: undefined`。Rust 的冻结协议里 `id` 是 `u64`，这里只在
                // 它是非负整数（`1` 与 `1.0` 等价）时接受，否则当作 `0`（这条消息不会与任何
                // 真实调用相同；真实 prelude 永远发数字）。
                let id = js_u64(object.get("id")).unwrap_or(0);
                if self
                    .shared
                    .lock()
                    .expect("shared mutex")
                    .pending
                    .contains_key(&id)
                {
                    return Err(BridgeError::new(format!("duplicate call id {id}")).into());
                }
                // TS: `const isTool = message.target === "tool"`（execute 路径）/
                // `target === "tool" ? … : undefined`（`forwardCall` 的记录）。判定的是
                // **是否等于 `"tool"`**，所以缺失、`null`、任何别的字符串都落到 globals 一侧。
                let target = match object.get("target").and_then(Value::as_str) {
                    Some("tool") => CallTarget::Tool,
                    _ => CallTarget::Global,
                };
                let name = string_field(object, "name").unwrap_or_default();
                let args = object.get("args").cloned();
                match self.control.clone() {
                    Some(control) => self.forward_call(id, target, name, args, &control),
                    None => {
                        self.handle_call(id, target, name, args);
                        Ok(())
                    }
                }
            }
            "started" => {
                if let Some(on_started) = self
                    .control
                    .as_ref()
                    .and_then(|control| control.on_started.clone())
                {
                    // TS: `onStarted(message.fingerprint)`——指纹缺失时是 `undefined`。冻结的回调
                    // 收 `String`，用空串表示（真实 prelude 永远给字符串）。
                    let fingerprint = string_field(object, "fingerprint").unwrap_or_default();
                    call_host(|| on_started(fingerprint)).map_err(HostFailure::Host)?;
                }
                Ok(())
            }
            "idle" => {
                if let Some(on_idle) = self
                    .control
                    .as_ref()
                    .and_then(|control| control.on_idle.clone())
                {
                    // TS: 缺失字段是 `undefined`；`u64` 用 0 表示（真实 prelude 永远给数字）。
                    let seq = js_u64(object.get("seq")).unwrap_or(0);
                    let issued = js_u64(object.get("issued")).unwrap_or(0);
                    call_host(|| on_idle(seq, issued)).map_err(HostFailure::Host)?;
                }
                Ok(())
            }
            "done" => self.handle_done(object),
            "crash" => {
                // TS: `{ kind: "sandbox", message: message.message }`；缺失时 `message` 是
                // `undefined`（JSON 里无该键）。`CodemodeError.message` 是 `String`，用空串。
                let message = string_field(object, "message").unwrap_or_default();
                self.finish(
                    Some(CodemodeError::new(CodemodeErrorKind::Sandbox, message)),
                    None,
                    None,
                    None,
                );
                Ok(())
            }
            // `is_worker_to_host_message` 已经限定过 type。
            _ => Err(BridgeError::new("unknown message from the worker").into()),
        }
    }

    /// `done` 的两支：按 `message.ok` 判别（TS 用 `if (!message.ok)`，任何 falsy 值都走失败支）。
    fn handle_done(
        self: &Arc<Self>,
        object: &serde_json::Map<String, Value>,
    ) -> Result<(), HostFailure> {
        // 先解码、后 `finish()`（`finish` 一旦开始就不得再抛错）。
        let during = parse_during(object.get("during"));
        let ok = match object.get("ok") {
            Some(Value::Bool(ok)) => *ok,
            // TS 的 `if (!message.ok)` 是 JS 真值判断：缺失与 `null` 是 falsy；非零数、非空串、
            // 数组与对象都是 truthy（下面各分支一一对应）。
            Some(Value::Null) => false,
            Some(Value::Number(number)) => number.as_f64().is_some_and(|v| v != 0.0),
            Some(Value::String(text)) => !text.is_empty(),
            Some(Value::Array(_)) | Some(Value::Object(_)) => true,
            None => false,
        };
        if !ok {
            let error = parse_script_error_value(&parse_bridge_value(
                object.get("error"),
                "script error",
            )?)?;
            self.finish(Some(error), None, None, during);
            return Ok(());
        }
        // TS: `message.value === undefined ? undefined : parseBridgeJson(message.value, "return value")`。
        let value = match object.get("value") {
            None => None,
            Some(raw) => Some(parse_bridge_value(Some(raw), "return value")?),
        };
        // TS: `parseStoreWrites(message.writes)`；`writes` 缺失时 `parseBridgeJson(undefined, …)`
        // 也抛（`JSON.parse(undefined)` → SyntaxError），落到同一条 bridge 错误。
        let writes =
            parse_store_writes_value(&parse_bridge_value(object.get("writes"), "store writes")?)?;
        self.finish(None, value, Some(writes), during);
        Ok(())
    }

    /// 宿主控制的 run：记录调用并交给调用方，调用方用 `deliver()` 回答。
    fn forward_call(
        self: &Arc<Self>,
        id: u64,
        target: CallTarget,
        name: String,
        args: Option<Value>,
        control: &Control,
    ) -> Result<(), HostFailure> {
        // TS: `message.args === undefined ? undefined : parseBridgeJson(message.args, "call arguments")`。
        // 非字符串的 `args` 走 `JSON.parse` 的 `ToString`（`JSON.parse(5)` → `5`）。
        let args = match args {
            Some(args) => Some(parse_bridge_value(Some(&args), "call arguments")?),
            None => None,
        };
        let started_at = Instant::now();
        {
            let mut shared = self.shared.lock().expect("shared mutex");
            let record = match target {
                CallTarget::Tool => {
                    let mut calls = self.calls.lock().expect("calls mutex");
                    calls.push(CodemodeCall {
                        name: name.clone(),
                        status: CodemodeCallStatus::Cancelled,
                        duration_ms: 0.0,
                    });
                    Some(calls.len() - 1)
                }
                CallTarget::Global => None,
            };
            shared.pending.insert(
                id,
                PendingCall {
                    record,
                    started_at,
                    controller: CancellationToken::new(),
                },
            );
        }
        // 先登记 pending 再回调：调用方可以同步 `deliver()`。
        //
        // TS 的 `onCall` 是必填字段，`control.onCall(...)` 一旦不是函数就会抛
        // `TypeError: control.onCall is not a function`，被 `receive` 的 catch 折成
        // `Sandbox host failed: control.onCall is not a function`（调用记录已经登记，随后
        // 由 `finish` 补上 duration，状态保持 `cancelled`）。Rust 的冻结签名把它放宽成
        // `Option`（见 `types.rs`），`None` 必须复现同一个失败，而不是把调用悄悄搁置。
        let Some(on_call) = control.on_call.clone() else {
            return Err(HostFailure::Host(
                "control.onCall is not a function".to_string(),
            ));
        };
        call_host(|| {
            on_call(crate::types::CodemodeHostCall {
                id,
                target,
                name,
                args,
            })
        })
        .map_err(HostFailure::Host)
    }

    /// `execute` 路径的工具调用：查表、执行、回答。
    fn handle_call(
        self: &Arc<Self>,
        id: u64,
        target: CallTarget,
        name: String,
        args: Option<Value>,
    ) {
        // 阶段 1：登记。调用记录（仅 tool）与 pending 都在派生任务之前写好：`finish()` 依赖
        // `pending` 找到在途调用并取消它的 controller；任务结束时也靠 pending 是否还在判断
        // 执行是否已经终结。
        let is_tool = target == CallTarget::Tool;
        let controller = CancellationToken::new();
        let record = if is_tool {
            let mut calls = self.calls.lock().expect("calls mutex");
            calls.push(CodemodeCall {
                name: name.clone(),
                status: CodemodeCallStatus::Cancelled,
                duration_ms: 0.0,
            });
            Some(calls.len() - 1)
        } else {
            None
        };
        let started_at = Instant::now();
        self.shared.lock().expect("shared mutex").pending.insert(
            id,
            PendingCall {
                record,
                started_at,
                controller: controller.clone(),
            },
        );

        // 阶段 2：查表。工具是执行开始时拍的快照（`Executors::tools`），全局函数是 sandbox 的
        // 共享表；查不到也在任务里回答 `Unknown tool/global "<name>"`，而不是让调用悬着。
        let tool = match target {
            CallTarget::Tool => self.executors.tools.get(&name).cloned(),
            CallTarget::Global => self
                .executors
                .globals
                .lock()
                .expect("globals mutex")
                .get(&name)
                .cloned(),
        };
        // 阶段 3：在宿主任务里执行工具并回答。这个任务可能比执行本身活得久：收尾时若
        // `finish()` 已经把 pending 取走，就不再投递回答。
        let this = Arc::clone(self);
        self.handle.spawn(async move {
            let (reply, status) = match tool {
                None => (
                    HostToWorkerMessage::err(
                        id,
                        format!(
                            "Unknown {} \"{name}\"",
                            if is_tool { "tool" } else { "global" }
                        ),
                    ),
                    CodemodeCallStatus::Error,
                ),
                Some(tool) => {
                    // TS 在这里直接 `JSON.parse(message.args)`（畸形参数会变成工具错误，而不是
                    // 桥接错误），错误文本是 V8 的 `JSON.parse` 文本；非字符串的 `args` 同样先
                    // `ToString` 再解析（`JSON.parse(5)` → `5`）。
                    let parsed = match &args {
                        Some(Value::String(text)) => {
                            kiana_json::js_parse::parse(text).map_err(|error| error.message)
                        }
                        Some(other) => kiana_json::js_parse::parse(
                            &kiana_json::js_string::js_string(Some(other)),
                        )
                        .map_err(|error| error.message),
                        None => Ok(Value::Null),
                    };
                    match parsed {
                        Err(message) => (
                            HostToWorkerMessage::err(id, message),
                            CodemodeCallStatus::Error,
                        ),
                        Ok(args) => {
                            let execute = Arc::clone(&tool.execute);
                            let context = CodemodeToolContext {
                                signal: controller.clone(),
                            };
                            // TS 的 `try { value = await tool.execute(...) } catch` 覆盖工具的
                            // 整个生命周期：既包括创建（同步部分），也包括 `await` 期间。Rust 的
                            // 异步工具用 panic 表达「抛错」，所以两处都要接住——只接创建的话，
                            // 轮询中的 panic 会杀掉这个任务，调用永远等不到回答。
                            let result =
                                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    execute(args, context)
                                })) {
                                    Ok(future) => match CatchUnwind::new(future).await {
                                        Ok(result) => result,
                                        Err(payload) => Err(panic_message(payload)),
                                    },
                                    Err(payload) => Err(panic_message(payload)),
                                };
                            match result {
                                Ok(value) => (
                                    HostToWorkerMessage::ok(
                                        id,
                                        Some(kiana_json::js_object::stringify(&value)),
                                    ),
                                    CodemodeCallStatus::Ok,
                                ),
                                Err(message) => (
                                    HostToWorkerMessage::err(id, message),
                                    CodemodeCallStatus::Error,
                                ),
                            }
                        }
                    }
                }
            };
            // finish() 已取消：记录保持 "cancelled"，VM 也走了或正在走。
            let removed = this
                .shared
                .lock()
                .expect("shared mutex")
                .pending
                .remove(&id);
            let Some(pending) = removed else {
                return;
            };
            if let Some(index) = pending.record {
                let duration = Instant::now().saturating_duration_since(started_at);
                if let Some(record) = this.calls.lock().expect("calls mutex").get_mut(index) {
                    record.status = status;
                    record.duration_ms = duration.as_secs_f64() * 1000.0;
                }
            }
            this.post(reply);
        });
    }

    /// 结束一次执行：第一个终态事件获胜，之后的事件被忽略。
    ///
    /// TS: `execution.ts#Execution.finish`。
    fn finish(
        self: &Arc<Self>,
        error: Option<CodemodeError>,
        value: Option<Value>,
        writes: Option<CodemodeStoreWrites>,
        during: Option<CodemodeDuring>,
    ) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        // 决胜之后立刻停掉两个监听任务：它们各自持有 `Arc<Execution>`，且不应再产生终态事件。
        if let Some(task) = self.timer.lock().expect("timer mutex").take() {
            task.abort();
        }
        if let Some(task) = self.abort_listener.lock().expect("abort mutex").take() {
            task.abort();
        }

        // 排空在途调用：记录在登记时已初始化为 `Cancelled`，这里只补 `duration_ms`；控制器在
        // 释放 `shared` 锁之后才 cancel。
        let now = Instant::now();
        let cancelled: Vec<PendingCall> = {
            let mut shared = self.shared.lock().expect("shared mutex");
            let pending = std::mem::take(&mut shared.pending);
            pending
                .into_values()
                .inspect(|call| {
                    if let Some(index) = call.record {
                        let duration = now.saturating_duration_since(call.started_at);
                        if let Some(record) = self.calls.lock().expect("calls mutex").get_mut(index)
                        {
                            record.duration_ms = duration.as_secs_f64() * 1000.0;
                        }
                    }
                })
                .collect()
        };
        for call in &cancelled {
            call.controller.cancel();
        }
        // 执行已结束：丢弃尚未投递的回复；若 channel 已经装好，留到下面的任务里 `stop()`。
        let channel = {
            let mut state = self.channel.lock().expect("channel mutex");
            state.unsent.clear();
            state.channel.clone()
        };

        let output = self.output.lock().expect("output mutex").clone();
        let calls = self.calls.lock().expect("calls mutex").clone();
        let result = match error {
            Some(error) => CodemodeResult::failure(error, output, calls, during),
            None => {
                CodemodeResult::success(value, output, calls, writes.unwrap_or_default(), during)
            }
        };
        // 结算任务：先 `stop()` 再写结果（模块文档的生命周期第 3 步）。`close()` 会等每个执行
        // 的 result future，因此一个永不完成的 `stop()` 会拖住 `close()`——TS 的 Promise.all
        // 同样如此（tests/execution.rs 的 `close_aborts_every_execution_before_awaiting_any_of_them`）。
        let this = Arc::clone(self);
        let handle = self.handle.clone();
        handle.spawn(async move {
            if let Some(channel) = channel {
                // TS 的 `.catch(() => undefined)`：`stop()` 自己 panic 也不能让结果挂住。
                let _ = tokio::spawn(async move { channel.stop().await }).await;
            }
            // 接收端可能已全部丢弃（`close()` 之后没人 await 结果）；发送失败不影响状态。
            let _ = this.result_tx.send(Some(result));
        });
    }
}

impl Drop for Execution {
    // 与 `finish()` 相同的清理：确保还没被取走的 deadline / abort 监听任务不会在
    // `Execution` 回收之后继续存在。正常路径由 `finish()` 先取走句柄，这里是兜底。
    fn drop(&mut self) {
        if let Some(task) = self.timer.get_mut().expect("timer mutex").take() {
            task.abort();
        }
        if let Some(task) = self.abort_listener.get_mut().expect("abort mutex").take() {
            task.abort();
        }
    }
}

/// Node 的 `setTimeout` 延迟：`Timeout` 构造器只判断 `!(after >= 1 && after <= 2^31-1)`
/// （判的是**未截断**的值：NaN、< 1、> `TIMEOUT_MAX` 一律 1 ms），通过后原样保留，
/// 由 libuv 的 `uv_timer_start(uint64_t)` 截断成整数毫秒。
fn node_timeout_delay(ms: f64) -> Duration {
    const TIMEOUT_MAX: f64 = 2_147_483_647.0;
    if !(1.0..=TIMEOUT_MAX).contains(&ms) {
        Duration::from_millis(1)
    } else {
        Duration::from_millis(ms as u64)
    }
}

/// 把 [`Arc<Execution>`] 暴露成 launcher 的 [`VmEvents`]。
struct ExecutionEvents(Arc<Execution>);

impl VmEvents for ExecutionEvents {
    fn message(&self, message: Value) {
        self.0.receive(message);
    }

    fn failure(&self, error: CodemodeError) {
        self.0.finish(Some(error), None, None, None);
    }

    fn host_event_loop(&self) -> Option<Arc<HostEventLoop>> {
        Some(Arc::clone(&self.0.host_event_loop))
    }
}

/// sandbox 已关闭（TS: `new Error("Sandbox is closed")`）。
///
/// 零大小错误类型，只用作 `execute*` 的 `Err` 变体；文本由 `Display` 固定给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxClosed;

impl std::fmt::Display for SandboxClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sandbox is closed")
    }
}

impl std::error::Error for SandboxClosed {}

/// `start()` 的同步校验错误（TS 里是同步 `throw new Error(...)`）。
///
/// 公开字段 `.0` 是错误文本，`Display` 逐字输出它（与 TS 的 `Error.message` 相同）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError(pub String);

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StartError {}

impl StartError {
    fn new(message: impl Into<String>) -> Self {
        StartError(message.into())
    }
}

/// 工具表、默认值与在途执行；所有 sandbox 共享的行为。子类只通过 [`VmLauncher`] 选择 VM 在哪跑。
///
/// 脚本看到 `tools.<name>(args)`、`ALL_TOOLS`、输出辅助 `text`/`image`/`exit`，以及
/// `console.*`、`store`/`load` 与配置的全局函数；别的都没有（没有 timer、`fetch`、`process`、
/// `require`、模块）。每次 `execute()` 都有自己的 VM；sandbox 只持有工具表与默认值。
/// `close()` 取消在途执行。
///
/// 所有方法都只借用 `&self`：工具表与在途列表在 `std::sync::Mutex` 后面，因此实例可以放进
/// `Arc` 在多个任务间共享（`tests/execution.rs` 的
/// `close_never_leaves_a_concurrent_execution_running` 并发调用 `execute()` 与 `close()`）。
///
/// TS: `execution.ts#CodemodeSandboxBase`。
pub struct CodemodeSandboxBase {
    tools_by_name: Mutex<IndexMap<String, CodemodeTool>>,
    globals_by_name: Arc<Mutex<IndexMap<String, CodemodeTool>>>,
    timeout_ms: f64,
    memory_limit_bytes: Option<f64>,
    launch: VmLauncher,
    running: Mutex<Vec<Arc<Execution>>>,
    closed: AtomicBool,
}

impl CodemodeSandboxBase {
    /// 构造：注册工具、校验全局名、存入全局函数。
    ///
    /// TS: `execution.ts#CodemodeSandboxBase.constructor`。固定顺序为：先注册 tools，
    /// 再 `checkGlobals`，最后填 globals——因此非法 global 让构造函数失败（Rust 里是 `Err`）。
    /// `timeout_ms` 缺省时取 [`DEFAULT_TIMEOUT_MS`]；失败时不产生 sandbox，也不启动任何 VM。
    pub fn new(
        options: CodemodeSandboxBaseOptions,
        launch: VmLauncher,
    ) -> Result<Self, StartError> {
        let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let mut tools_by_name = IndexMap::new();
        for tool in options.tools.unwrap_or_default() {
            if tools_by_name.contains_key(&tool.name) {
                return Err(StartError::new(format!(
                    "Tool \"{}\" is already registered",
                    tool.name
                )));
            }
            tools_by_name.insert(tool.name.clone(), tool);
        }
        let globals = options.globals.unwrap_or_default();
        let declared: Vec<CodemodeDeclaredGlobal> = globals.iter().map(declared_global).collect();
        check_globals(&declared).map_err(StartError::new)?;
        let mut globals_by_name = IndexMap::new();
        for global in globals {
            globals_by_name.insert(global.name.clone(), global);
        }
        Ok(CodemodeSandboxBase {
            tools_by_name: Mutex::new(tools_by_name),
            globals_by_name: Arc::new(Mutex::new(globals_by_name)),
            timeout_ms,
            memory_limit_bytes: options.memory_limit_bytes,
            launch,
            running: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        })
    }

    /// 注册一个工具；重名 → `Tool "<name>" is already registered`。
    ///
    /// 只影响之后开始的 `execute()`：每次执行在调用 `execute()` 时克隆一份工具表快照。
    ///
    /// TS: `execution.ts#CodemodeSandboxBase.registerTool`。
    pub fn register_tool(&self, tool: CodemodeTool) -> Result<(), String> {
        let mut tools = self.tools_by_name.lock().expect("tools mutex");
        if tools.contains_key(&tool.name) {
            return Err(format!("Tool \"{}\" is already registered", tool.name));
        }
        tools.insert(tool.name.clone(), tool);
        Ok(())
    }

    /// 注销一个工具，返回它是否存在。
    ///
    /// 不影响已经在途的执行：它们持有自己的工具表快照。
    ///
    /// TS: `execution.ts#CodemodeSandboxBase.unregisterTool`。
    pub fn unregister_tool(&self, name: &str) -> bool {
        self.tools_by_name
            .lock()
            .expect("tools mutex")
            .shift_remove(name)
            .is_some()
    }

    /// TS 的 `get tools()`：注册顺序的快照。
    pub fn tools(&self) -> Vec<CodemodeTool> {
        self.tools_by_name
            .lock()
            .expect("tools mutex")
            .values()
            .cloned()
            .collect()
    }

    /// TS 的 `get globals()`：注册顺序的快照。
    pub fn globals(&self) -> Vec<CodemodeTool> {
        self.globals_by_name
            .lock()
            .expect("globals mutex")
            .values()
            .cloned()
            .collect()
    }

    /// `code` 是 async 函数体：`return` 与顶层 `await` 可用。
    ///
    /// 脚本失败不会 reject（返回 `{ ok: false }`）；只有 sandbox 已关闭时以
    /// `Sandbox is closed` 失败。脚本可以用 `store(key, value)` 与 `load(key)` 访问
    /// `options.store`。
    ///
    /// 本次执行的工具表是调用时的快照：之后的 `register_tool`/`unregister_tool` 不影响它。
    ///
    /// TS: `execution.ts#CodemodeSandboxBase.execute`。
    pub fn execute(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
    ) -> BoxFuture<'static, Result<CodemodeResult, SandboxClosed>> {
        self.execute_with_abort_reason(code, options, None)
    }

    /// [`execute`](Self::execute) 的带 `signal.reason` 版本（ARCHITECTURE.md §4.2 的
    /// `CancellationToken` + 可选 reason）。
    ///
    /// TS 的 `Execution.onAbort` 用 `signal.reason instanceof Error ? reason.message :
    /// "Execution aborted"`；`reason` 只能由调用方在取消时提供（`CancellationToken` 不带
    /// 载荷），因此这里显式接收。`reason` 为 [`None`] 对应无参 `abort()`：Node/Bun
    /// 把 `reason` 设成 `message` 为 `This operation was aborted` 的 DOMException，且
    /// `DOMException instanceof Error === true`，所以结果是 `This operation was aborted`。
    /// TS 里 `abort("why")` 这类「reason 不是 `Error`」的生产者要传
    /// `Some("Execution aborted".to_string())` 才能逐字复现 spec §3.4 的那一行。
    ///
    /// 冻结的 [`execute`](Self::execute) 等价于 `reason = None`。
    pub fn execute_with_abort_reason(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
        reason: Option<String>,
    ) -> BoxFuture<'static, Result<CodemodeResult, SandboxClosed>> {
        if self.closed.load(Ordering::SeqCst) {
            return Box::pin(async { Err(SandboxClosed) });
        }
        let tools = Arc::new(self.tools_by_name.lock().expect("tools mutex").clone());
        let globals = Arc::clone(&self.globals_by_name);
        let execution_options = ExecutionOptions {
            code: code.to_string(),
            tools: tools.values().map(declared_tool).collect(),
            globals: self
                .globals_by_name
                .lock()
                .expect("globals mutex")
                .values()
                .map(declared_global)
                .collect(),
            executors: Executors { tools, globals },
            timeout_ms: options.timeout_ms.unwrap_or(self.timeout_ms),
            signal: options.signal,
            abort_reason: reason,
            memory_limit_bytes: self.memory_limit_bytes,
            store: serialize_store(options.store.as_ref()),
            deterministic: None,
            control: None,
            launch: Arc::clone(&self.launch),
        };
        let Some(execution) = self.start_tracked(execution_options) else {
            return Box::pin(async { Err(SandboxClosed) });
        };
        let result = execution.result_future();
        Box::pin(async move { Ok(result.await) })
    }

    /// 确定性 run：调用方回答脚本的每一次调用。
    ///
    /// 相同的脚本、起点、seed、工具、全局函数与 store，按相同 `seq` 顺序给出相同结果，就在任何
    /// 同指纹的 VM 上产生相同的调用、参数与输出；宿主因此可以在重启后重放。VM 跑在 UTC，
    /// 时钟读取 `started_at`，之后读正在处理的结果的 `now`，随机数来自 `seed`，且不存在
    /// `WeakRef` 与 `FinalizationRegistry`。
    ///
    /// sandbox 自己的工具与全局函数不参与：run 声明自己的，所有调用都交给 `options.on_call`。
    /// 脚本失败不会 reject；用 `options.signal` 取消。`timeout_ms` 的默认是**无 deadline**
    /// （`INFINITY`），不是 sandbox 的默认 300000（`execute()` 才用后者）。
    ///
    /// 参数校验在启动 VM 之前完成（`started_at` 有限且非负、`seed` 是 32 位无符号整数、工具名
    /// 与脚本名唯一、全局名合法），失败返回 [`StartError`]，不会产生在途执行。
    ///
    /// TS: `execution.ts#CodemodeSandboxBase.start`。
    pub fn start(
        &self,
        code: &str,
        options: CodemodeRunOptions,
    ) -> Result<CodemodeRun, StartError> {
        self.start_with_abort_reason(code, options, None)
    }

    /// [`start`](Self::start) 的带 `signal.reason` 版本（见
    /// [`execute_with_abort_reason`](Self::execute_with_abort_reason)）。
    pub fn start_with_abort_reason(
        &self,
        code: &str,
        options: CodemodeRunOptions,
        reason: Option<String>,
    ) -> Result<CodemodeRun, StartError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(StartError::new("Sandbox is closed"));
        }
        if !options.started_at.is_finite() || options.started_at < 0.0 {
            return Err(StartError::new(
                "startedAt must be a non-negative number of milliseconds since the epoch",
            ));
        }
        if !is_js_integer(options.seed)
            || options.seed < 0.0
            || options.seed > 0xffff_ffffu32 as f64
        {
            return Err(StartError::new("seed must be a 32-bit unsigned integer"));
        }
        let tools = options.tools.unwrap_or_default();
        let mut names = HashSet::new();
        for tool in &tools {
            if !names.insert(tool.name.clone()) {
                return Err(StartError::new("Tool names must be unique"));
            }
        }
        let mut script_names = HashSet::new();
        for tool in &tools {
            if !script_names.insert(tool.script_name()) {
                return Err(StartError::new("Tool script names must be unique"));
            }
        }
        let globals = options.globals.unwrap_or_default();
        check_globals(&globals).map_err(StartError::new)?;
        let execution_options = ExecutionOptions {
            code: code.to_string(),
            tools,
            globals,
            executors: Executors {
                tools: Arc::new(IndexMap::new()),
                globals: Arc::new(Mutex::new(IndexMap::new())),
            },
            timeout_ms: options.timeout_ms.unwrap_or(f64::INFINITY),
            signal: options.signal,
            abort_reason: reason,
            memory_limit_bytes: self.memory_limit_bytes,
            store: serialize_store(options.store.as_ref()),
            deterministic: Some(VmDeterministic {
                started_at: options.started_at,
                seed: options.seed,
            }),
            control: Some(Control {
                on_started: options.on_started,
                on_call: options.on_call,
                on_idle: options.on_idle,
            }),
            launch: Arc::clone(&self.launch),
        };
        let Some(execution) = self.start_tracked(execution_options) else {
            return Err(StartError::new("Sandbox is closed"));
        };
        let finished = Arc::clone(&execution.finished);
        let result = execution.result_future();
        let deliver_execution = Arc::clone(&execution);
        Ok(CodemodeRun::new(
            result,
            finished,
            Arc::new(move |seq, now, id, outcome| deliver_execution.deliver(seq, now, id, outcome)),
        ))
    }

    /// 取消在途执行（它们以 `kind: "aborted"` 结束），并拒绝新的执行。
    ///
    /// 在途执行的结果消息是 `Sandbox closed`；关闭后 `execute()` 以 `SandboxClosed` 失败、
    /// `start()` 返回 `StartError("Sandbox is closed")`。本方法等所有结果 future 结算后才返回，
    /// 因此某个 `channel.stop()` 不完成会拖住它。
    ///
    /// TS: `execution.ts#CodemodeSandboxBase.close`。
    pub async fn close(&self) {
        let running: Vec<Arc<Execution>> = {
            // 置位与排空在同一把锁里：并发的 `execute()`/`start()` 因此有明确的先后——要么
            // 先登记（随后被这里取消），要么在 [`Self::start_tracked`] 的检查里看到已关闭。
            // TS 单线程里 `closed = true` 与 `[...running]` 本来就不可分割。
            let mut list = self.running.lock().expect("running mutex");
            self.closed.store(true, Ordering::SeqCst);
            std::mem::take(&mut *list)
        };
        // TS: `Promise.all([...running].map(e => e.abort("Sandbox closed")))`——`.map` 是**先**
        // 为每个执行调用 `abort()`（同步 finish，含请求 channel 停止）**再** `Promise.all` 等待。
        // 因此某一个 `channel.stop()` 若永不完成，其余执行仍已全部取消；这里同样先全部 abort，
        // 再逐个 await 结果 future。
        let results: Vec<BoxFuture<'static, CodemodeResult>> = running
            .into_iter()
            .map(|execution| execution.abort("Sandbox closed"))
            .collect();
        for result in results {
            result.await;
        }
    }

    /// 在 `running` 锁内检查关闭状态、启动并登记执行；返回 [`None`] 表示 sandbox 已关闭。
    ///
    /// TS 的 `execute()`/`start()` 是单线程的：`closed` 检查、构造 `Execution` 与
    /// `running.add(execution)` 同步完成，中间没有窗口，所以任何先于 `close()` 开始的执行都
    /// 会留在 `running` 里并被取消。Rust 侧若把检查与登记分成两步，并发的 `close()` 可以在
    /// 两步之间排空列表：这个执行既不进列表、也不会被取消，`execute()` 照样跑完，脚本甚至能
    /// 在 `close()` 返回后继续调用工具。这里共用 `running` 锁把「检查 → 启动 → 登记」串行化，
    /// [`Self::close`] 的「置位 → 排空」也在同一把锁里，两者因此有明确的先后。
    ///
    /// TS 的 Set 持有执行本身，直到它的 Promise settle 时才在 `.finally` 里删除；Rust 这边没有
    /// 「settle 时回调」的等价点，因此在这里顺手清掉已结束的项：引用数仍以「在途执行数 + 1」
    /// 为界，而 `close()` 总能取消在途执行。
    fn start_tracked(&self, options: ExecutionOptions) -> Option<Arc<Execution>> {
        let mut list = self.running.lock().expect("running mutex");
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let execution = Execution::start(options);
        list.retain(|execution| !execution.is_finished());
        list.push(Arc::clone(&execution));
        Some(execution)
    }
}

/// `CodemodeTool` → `CodemodeDeclaredTool`（`execute()` 路径的 `tools` 快照）。
fn declared_tool(tool: &CodemodeTool) -> CodemodeDeclaredTool {
    CodemodeDeclaredTool {
        name: tool.name.clone(),
        js_name: None,
        description: tool.description.clone(),
    }
}

/// `CodemodeTool` → `CodemodeDeclaredGlobal`。
fn declared_global(tool: &CodemodeTool) -> CodemodeDeclaredGlobal {
    CodemodeDeclaredGlobal {
        name: tool.name.clone(),
        spread: tool.spread,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::node_timeout_delay;

    /// Node 的 `Timeout` 用**未截断**的 `after` 判界：`2147483647.5` 超界 → 1 ms（若先 `trunc`
    /// 会误判为合法的 2147483647 ms）；合法的 `2147483647` 原样保留，小数毫秒由
    /// `uv_timer_start(uint64_t)` 截断。
    #[test]
    fn node_timeout_delay_matches_the_timeout_constructor() {
        assert_eq!(
            node_timeout_delay(2_147_483_647.5),
            Duration::from_millis(1)
        );
        assert_eq!(
            node_timeout_delay(2_147_483_647.0),
            Duration::from_millis(2_147_483_647)
        );
        assert_eq!(
            node_timeout_delay(300_000.0),
            Duration::from_millis(300_000)
        );
        assert_eq!(node_timeout_delay(100.5), Duration::from_millis(100));
        assert_eq!(node_timeout_delay(0.5), Duration::from_millis(1));
        assert_eq!(node_timeout_delay(0.0), Duration::from_millis(1));
        assert_eq!(node_timeout_delay(-0.5), Duration::from_millis(1));
        assert_eq!(node_timeout_delay(f64::NAN), Duration::from_millis(1));
        assert_eq!(node_timeout_delay(f64::INFINITY), Duration::from_millis(1));
    }
}
