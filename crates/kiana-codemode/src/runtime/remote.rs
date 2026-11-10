//! runtime/remote.ts：VM 跑在另一个 isolate 里，经 RPC 边界（例如 Cloudflare Durable Object
//! stub）到达，而工具仍在调用方运行。
//!
//! TS 源：`packages/codemode/src/runtime/remote.ts`（277 行）。
//!
//! 问题：在调用线程上的 VM 在脚本计算期间会阻塞那个线程。在一个服务 agent 会话的 Durable
//! Object 里，CPU 密集的脚本会卡住该会话的每个其它请求（包括用户的 abort）达数秒。Cloudflare
//! 实测（2026 年 10 月）：计算 10 秒的脚本在会话对象或 Dynamic Worker 里运行会使会话的其它
//! 请求延迟 10 秒，而在单独部署的 Worker 的 Durable Object 里只延迟约 50 ms。
//!
//! 解法：调用方保留宿主角色（[`RemoteCodemodeSandbox`]：工具、调用记录、deadline、abort），
//! 远端只跑 VM（[`serve_codemode_remote`]）。
//!
//! 协议，一次执行一个 RPC 调用：
//!
//! ```text
//! client                                     remote
//! remote.execute(request, exchange) ───────▶ 起 VM（线程内，带中断预算）
//!                                  ◀─────── exchange([...outputs, call])   每个工具调用一次
//! 运行工具，用结果回答 ─────────────────────▶ VM 继续
//!                                  ◀─────── exchange([...outputs, done])   终态，回答被忽略
//! execute() 完成 ◀─────────────────────────  VM 被 dispose
//! ```
//!
//! `exchange` 是客户端传入的函数；在 Workers RPC 上它变成一个回调进客户端的 stub。输出在远端
//! 缓冲，随下一次 call 或终态消息一起发，所以打印循环不会变成每行一次 RPC。每次 `exchange`
//! 最多带一个 call，因此并发工具调用就是并发 RPC。同一个 stub 上的调用按序投递，从而保证
//! call/result 之前的输出顺序。
//!
//! 终止：客户端无法中断一个正在计算的远端 VM。客户端先结束时（deadline、abort、`close()`），
//! 它对每个在途与之后到达的 `exchange` 回答 `{ type: "stop" }`，远端在那一刻停下。不调用工具
//! 地计算的脚本会跑到中断预算用尽；远端把它报成 `failure` 消息，客户端把它转成本地 VM 相同的
//! `kind: "timeout"` 错误（spec §A.5.1：客户端**不等**远端退出）。
//!
//! 失败：如果 RPC 调用被拒（远端 isolate 崩溃、撞上 CPU 上限或被重新部署）或没有终态消息就
//! 返回，执行以 `kind: "sandbox"` 失败。
//!
//! # Rust 映射
//!
//! - `CodemodeRemoteMessage` = [`WorkerToHostMessage`] | `{ type: "failure", error }`；线形状与
//!   TS 相同（手写的 `Serialize`/`Deserialize`，见各自的文档）。
//! - `CodemodeRemoteReply` = `HostToWorkerMessage` | `{ type: "stop" }` | `null`。
//! - `CodemodeRemoteExchange` = `Arc<dyn Fn(Vec<CodemodeRemoteMessage>) -> BoxFuture<CodemodeRemoteReply>>`；
//!   TS 的 Promise 拒绝在 Rust 侧是 panic，`serve` 与 launcher 都在 await 处接住。
//! - TS 的 `execute(request, exchange): Promise<void>` 拒绝值是 `Error`，取 `errorMessage`
//!   文本，因此 Rust 侧是 `BoxFuture<Result<(), String>>`。
//!
//! # 已知偏差
//!
//! - TS 的 `CodemodeRemoteMessage` 运行时可以是**任意**值（畸形消息会原样交给宿主，由桥接层
//!   报 `Sandbox bridge broken: unknown message from the worker`）。Rust 的枚举只能表示两个
//!   规范形状：真实 VM 只产生规范消息，而 `serve_codemode_remote` 的消息来自本 crate 的 VM，
//!   因此这条路径不可达；对外的畸形消息输入在类型层被拒绝。
//! - TS 的 `interruptBudget` 是任意 number；Rust 用 `u32`（与 `vm.rs` 的冻结 API 一致）。
//!
//! # Rust 侧的两半
//!
//! 本文件同时实现 RPC 的两端，二者只通过上面的消息/回答/交换函数交互：
//!
//! - **远端半边** [`serve_codemode_remote`]：用 [`inline_launcher`] 在本进程跑一次执行的
//!   VM，把 [`VmEvents`] 回调收进 `ServeState`（输出缓冲、分发队列、channel、完成信号），
//!   再按批调用 `exchange`。
//! - **客户端半边** `remote_launcher` 与 [`RemoteCodemodeSandbox`]：实现 [`VmLauncher`]，
//!   把 [`VmStartData`] 包成 [`CodemodeRemoteRequest`] 交给 [`CodemodeRemote::execute`]，
//!   并用 `exchange` 把远端消息转给本地 [`VmEvents`]；收到 `call` 时挂起一个 oneshot，
//!   等宿主经 `RemoteChannel` 回答。
//!
//! # 顺序与并发
//!
//! - `serve` 的分发器按 FIFO **串行启动** exchange：上一批的 future 至少被轮询过一次才启动
//!   下一批，但不等它结算，所以不同批的 future 可以同时在途。回答在各自的 future 结算时
//!   按 `id` 交给 VM，`ServeState` 不要求它们按批顺序到达。
//! - 终态批次与普通批次共用同一条队列，因此 `done`/`crash`/`failure` 不会越过排在它之前
//!   的输出。
//! - 客户端在投递消息**之前**把 `id → oneshot` 登记进 `waiting`，宿主同步回答也不会丢；
//!   没有 waiter 的回答（重复或已停止）被忽略。
//! - 共享状态用 `std::sync::Mutex`（只覆盖取/放，不跨 `await`）与原子；`end()` 用
//!   `ended.swap(true)` 保证幂等。`serve_codemode_remote` 与 `remote_launcher` 内部都会
//!   `tokio::spawn`，必须在 Tokio 运行时内调用。
//! - `serve` 复用 [`inline_launcher`]，消息只在 VM 让出后成批到达 `ServeState`，所以脚本
//!   同步计算期间不会发出 exchange（`tests/portable.rs` 的
//!   `defers_remote_exchanges_until_the_script_yields` 锁定）。
//!
//! # 阅读顺序
//!
//! [`serve_codemode_remote`] 及其 `ServeState` 的 `end`/`reply`/`on_message`（远端）→
//! `remote_launcher`（客户端 exchange 与失败映射）→ `RemoteChannel` →
//! [`RemoteCodemodeSandbox`]（薄包装）。
//!
//! # 注意事项
//!
//! - `remote` 工厂不是构造 sandbox 时调用的：每次 `execute()`/`start()` 走一次 launcher
//!   才调用工厂与 `execute`。工厂 panic 被接住，执行以
//!   `Failed to reach the remote sandbox: …` 失败。
//! - `exchange` future 的 panic 按 Promise 拒绝语义处理：远端把它当作 `end(undefined)`
//!   （丢弃缓冲并停 VM）；客户端把 `execute` future 的 panic 当作
//!   `Remote sandbox failed: …`。
//! - `serve_codemode_remote` 永不返回 `Err`：远端失败只以
//!   [`CodemodeRemoteMessage::Failure`] 交付。客户端 `execute` 返回 `Ok(())` 时若还没收到
//!   终态，执行按 `Remote sandbox returned before the script settled` 失败。
//! - 本文件的类型经 `index.rs` 与 `portable.rs` 再导出，是冻结公开面的一部分
//!   （`tests/api_surface.rs` 锁定它们存在）；改名或改签名属于 API 变更。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::runtime::execution::{CodemodeSandboxBase, StartError, VmChannel, VmEvents, VmLauncher};
use crate::runtime::inline::{DEFAULT_INTERRUPT_BUDGET, inline_launcher};
use crate::runtime::protocol::{HostToWorkerMessage, VmStartData, WorkerToHostMessage};
use crate::types::{
    BoxFuture, CodemodeError, CodemodeErrorKind, CodemodeExecuteOptions, CodemodeOutputItem,
    CodemodeResult, CodemodeRun, CodemodeRunOptions, CodemodeSandboxBaseOptions, CodemodeTool,
};
use crate::wasm::CodemodeWasmModule;

/// 远端在不等工具调用的情况下冲刷输出缓冲的项数阈值。
///
/// TS: `remote.ts#FLUSH_ITEMS`（模块私有常量）。
const FLUSH_ITEMS: usize = 1_000;

/// 远端在不等工具调用的情况下冲刷输出缓冲的字符（UTF-16 码元）阈值。
///
/// TS: `remote.ts#FLUSH_CHARS`（`1 << 20`）。
const FLUSH_CHARS: usize = 1 << 20;

/// `CodemodeRemoteRequest`：客户端让远端跑一个脚本。
///
/// 线形状是 `{ data, interruptBudget? }`（`#[serde(rename_all = "camelCase")]`，预算缺省
/// 时键省略）。
///
/// `data` 必须自包含（源码、工具描述、globals、store、内存上限、确定性参数）：远端拿不到
/// 调用方的工具表或全局函数，脚本里对工具的调用只会以 `call` 消息回到调用方。
///
/// TS: `remote.ts#CodemodeRemoteRequest`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodemodeRemoteRequest {
    /// 运行一个脚本所需的全部数据。
    pub data: VmStartData,
    /// 请求的中断预算。远端可以调低它，见 [`CodemodeServeOptions`]。
    ///
    /// `None` 表示用远端的默认（上限）；远端拿到后按 `min(请求值, 上限)` 取预算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt_budget: Option<u32>,
}

/// 远端发给客户端的消息：VM 的消息，或 VM 桥接之外的失败。
///
/// 手工实现 `Serialize`/`Deserialize`：两支的判别与形状不同（`Worker` 委托给
/// [`WorkerToHostMessage`]；`Failure` 是另一支对象）。线格式行为：
///
/// - 序列化：`Worker` 直接委托给 [`WorkerToHostMessage`]（对象里已有 `type`）；`Failure` 写
///   `{"type":"failure","error":…}`。
/// - 反序列化：只有顶层对象且 `type == "failure"` 走 failure 一支；其余整体按
///   [`WorkerToHostMessage`] 解析，未知 `type` 是错误而不是原样保留。
///
/// TS: `remote.ts#CodemodeRemoteMessage`（`WorkerToHostMessage | { type: "failure"; error }`）。
#[derive(Debug, Clone, PartialEq)]
pub enum CodemodeRemoteMessage {
    /// 一条 VM → 宿主消息。
    Worker(WorkerToHostMessage),
    /// `{ type: "failure", error }`：远端 VM 在桥接之外失败（例如中断预算耗尽）。
    Failure {
        /// 失败原因。
        error: CodemodeError,
    },
}

impl Serialize for CodemodeRemoteMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            CodemodeRemoteMessage::Worker(message) => message.serialize(serializer),
            CodemodeRemoteMessage::Failure { error } => {
                // TS 的对象字面量是 `{ type: "failure", error }`。
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "failure")?;
                map.serialize_entry("error", error)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for CodemodeRemoteMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        // TS 用 `typeof message === "object" && message !== null && message.type === "failure"`
        // 判别 failure 一支；其余交给 `WorkerToHostMessage`。
        if value.get("type").and_then(Value::as_str) == Some("failure") {
            let error = value.get("error").cloned().unwrap_or(Value::Null);
            serde_json::from_value::<CodemodeError>(error)
                .map(|error| CodemodeRemoteMessage::Failure { error })
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value::<WorkerToHostMessage>(value)
                .map(CodemodeRemoteMessage::Worker)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// 客户端对一次 exchange 的回答：exchange 携带的调用的结果、`null`（没带调用），或
/// `stop`（客户端不再想要这次执行）。
///
/// 线形状：`Result` → `{"type":"result",…}`；`Stop` → `{"type":"stop"}`；`Null` → JSON
/// `null`。反序列化只接受这三者，其他值（数组、字符串、未知 `type` 的对象）报错。
///
/// TS: `remote.ts#CodemodeRemoteReply`（`HostToWorkerMessage | { type: "stop" } | null`）。
#[derive(Debug, Clone, PartialEq)]
pub enum CodemodeRemoteReply {
    /// 宿主对某次调用的回答（`{ type: "result", … }`）。
    Result(HostToWorkerMessage),
    /// `{ type: "stop" }`：结束执行。
    Stop,
    /// `null`：仅确认，没有调用要回答。
    Null,
}

impl Serialize for CodemodeRemoteReply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            CodemodeRemoteReply::Result(message) => message.serialize(serializer),
            CodemodeRemoteReply::Stop => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("type", "stop")?;
                map.end()
            }
            CodemodeRemoteReply::Null => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for CodemodeRemoteReply {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match &value {
            Value::Null => Ok(CodemodeRemoteReply::Null),
            Value::Object(object) => match object.get("type").and_then(Value::as_str) {
                Some("stop") => Ok(CodemodeRemoteReply::Stop),
                Some("result") => serde_json::from_value::<HostToWorkerMessage>(value)
                    .map(CodemodeRemoteReply::Result)
                    .map_err(serde::de::Error::custom),
                _ => Err(serde::de::Error::custom(
                    "an exchange reply must be a result message, { type: \"stop\" }, or null",
                )),
            },
            _ => Err(serde::de::Error::custom(
                "an exchange reply must be a result message, { type: \"stop\" }, or null",
            )),
        }
    }
}

/// `CodemodeRemoteExchange`：客户端把远端的一批消息交给宿主并等回答。
///
/// 契约：
///
/// - 会被反复调用（通常每次一批消息）；远端只有在上一批的 future 至少被轮询过一次之后
///   才启动下一批，但不等它结算，因此不同批的 future 可以同时在途。
/// - 每批最多携带一个 `call`；没有调用时实现应返回 [`CodemodeRemoteReply::Null`]，
///   有调用时返回与它的 `id` 对应的 [`CodemodeRemoteReply::Result`]。
/// - future 的 panic 按 Promise 拒绝语义处理；`serve_codemode_remote` 的分发器用
///   `CatchUnwind` 接住，按 `end(undefined)`（丢弃缓冲并停 VM）处理。
///
/// TS: `remote.ts#CodemodeRemoteExchange`（`(messages) => Promise<CodemodeRemoteReply>`）。
pub type CodemodeRemoteExchange = Arc<
    dyn Fn(Vec<CodemodeRemoteMessage>) -> BoxFuture<'static, CodemodeRemoteReply> + Send + Sync,
>;

/// 远端端，通常是 `CodemodeSandboxDurableObject` 的 Durable Object stub；任何带这个方法的东西
/// 都行，例如测试里的进程内对象。
///
/// TS: `remote.ts#CodemodeRemote`。
pub trait CodemodeRemote: Send + Sync {
    /// 跑一次执行；`exchange` 把 VM 的消息交给宿主并带回回答。
    ///
    /// 实现约定里有两处会改变调用方看到的错误：
    ///
    /// - 返回 `Ok(())` 之前应已通过 `exchange` 交付终态（`done`/`crash`）或 `failure`；
    ///   否则客户端报 `Remote sandbox returned before the script settled`（`kind:
    ///   "sandbox"`）。
    /// - `Err(message)` 即 TS 里 Promise 的拒绝值（取 `errorMessage` 文本），执行以
    ///   `Remote sandbox failed: <message>` 结束；future 的 panic 走同一条路径。
    ///
    /// 客户端已经 `stop`（正常结算、deadline、abort 或 `close()`）之后，上述错误不再上报：
    /// 本次执行已在本地结算。
    fn execute(
        &self,
        request: CodemodeRemoteRequest,
        exchange: CodemodeRemoteExchange,
    ) -> BoxFuture<'static, Result<(), String>>;
}

/// [`RemoteCodemodeSandbox`] 的选项。
///
/// TS: `remote.ts#RemoteCodemodeSandboxOptions`（`extends CodemodeSandboxBaseOptions`）。
#[derive(Clone)]
pub struct RemoteCodemodeSandboxOptions {
    /// 基类选项（工具、全局函数、默认 deadline、内存上限）。
    pub base: CodemodeSandboxBaseOptions,
    /// 返回跑一次执行的远端的工厂，每次执行调用一次（正常启动 VM 的 `execute()` 与
    /// `start()` 都会走 launcher，因此都会调用它并启动一个远端 VM；构造 sandbox 时不调用）。
    /// Durable Object 就在这里挑选实例，例如 `env.CODEMODE_SANDBOX.getByName(sessionId)`。
    /// 抛错（Rust 里是 panic）→ `Failed to reach the remote sandbox: …`。
    pub remote: Arc<dyn Fn() -> Arc<dyn CodemodeRemote> + Send + Sync>,
    /// 向远端请求的中断预算。默认用远端的默认值。
    ///
    /// 这只是请求值：远端按自己的 `max_interrupt_budget` 取较小者（见
    /// [`CodemodeServeOptions`]）。
    pub interrupt_budget: Option<u32>,
}

impl std::fmt::Debug for RemoteCodemodeSandboxOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCodemodeSandboxOptions")
            .field("base", &self.base)
            .field("remote", &"<factory>")
            .field("interrupt_budget", &self.interrupt_budget)
            .finish()
    }
}

/// [`serve_codemode_remote`] 的选项。
///
/// 两个 `max_*` 都是**服务端策略**：既是请求值的上限，也是请求缺省时的默认值。注意
/// `max_interrupt_budget: None` 表示用 [`DEFAULT_INTERRUPT_BUDGET`]，不是无限预算。
///
/// TS: `remote.ts#CodemodeServeOptions`。
#[derive(Clone)]
pub struct CodemodeServeOptions {
    /// 编译好的 `quickjs-wasi/quickjs.wasm`。
    pub wasm: CodemodeWasmModule,
    /// 请求中断预算的上限，以及请求没给预算时的预算。
    ///
    /// `None` → 用 [`DEFAULT_INTERRUPT_BUDGET`]；请求给了更小的值则取更小的值。
    pub max_interrupt_budget: Option<u32>,
    /// 请求 VM 内存的上限，以及请求没给上限时的限制。
    ///
    /// `None` 表示不额外设上限：请求有值就用请求值；两者都缺省时
    /// `data.memory_limit_bytes` 保持 `None`，`start_vm` 不调用 `qjs_set_memory_limit`；
    /// 内存只受 wasm32 4 GiB 地址空间限制（spec 的 `memoryLimitBytes` 默认值说明）。
    pub max_memory_limit_bytes: Option<f64>,
}

impl std::fmt::Debug for CodemodeServeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodemodeServeOptions")
            .field("wasm", &"<CodemodeWasmModule>")
            .field("max_interrupt_budget", &self.max_interrupt_budget)
            .field("max_memory_limit_bytes", &self.max_memory_limit_bytes)
            .finish()
    }
}

/// `Math.min(a, b)`：任一为 NaN 即 NaN（`f64::min` 会返回另一个值，与 JS 不同）。
///
/// 选择规则是 `a < b ? a : b`，因此不保留 `-0` 的符号（`a = -0.0`、`b = 0.0` 时得到
/// `0.0`）。本函数只用于合并 `memoryLimitBytes`。
fn js_math_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// UTF-16 码元数（JS 的 `string.length`）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// VM 的终态消息：脚本结束（`done`，成功或失败）或 VM 崩溃（`crash`）。`started` 与
/// `idle` 只是过程消息。
fn is_terminal(message: &WorkerToHostMessage) -> bool {
    matches!(
        message,
        WorkerToHostMessage::Done(_) | WorkerToHostMessage::Crash { .. }
    )
}

/// 远端侧 [`RemoteCodemodeSandbox`]：用 [`inline_launcher`] 跑一次执行的 VM，并把它报告的
/// 一切经 `exchange` 转发。VM 停下且终态消息交付之后（或客户端要求停下时）完成。永不拒绝
/// （Rust 侧因此返回 `()`）。
///
/// 与 TS 的差别：Rust 的 VM 跑在 `vm.rs` 的专用 OS 线程上（TS 的 inline VM 跑在调用
/// 线程上）；`serve` 使用 [`inline_launcher`]，因此沿用其按中断预算终止脚本、按 VM 让出
/// 分批转发消息的行为（spec §2.2.12）。`ServeEvents` 不实现 [`VmEvents::host_event_loop`]，
/// 所以没有宿主事件循环闸门。
///
/// # 参数
///
/// - `request`：脚本数据与请求预算；`data.memory_limit_bytes` 与
///   `options.max_memory_limit_bytes` 按 [`CodemodeServeOptions`] 的规则合并。
/// - `exchange`：每批消息调用一次；分发器保证上一批的 future 已被轮询后才启动下一批。
/// - `options`：wasm 模块与服务端策略上限。
///
/// # 返回
///
/// 终态批次（若存在）的 exchange 结算、VM 的 channel（若已建立）停止之后返回；客户端
/// `stop` 时不为当前缓冲发终态 exchange，直接停 channel；已入队的批次仍会照常 exchange
/// （其回答会被 `reply` 丢弃，见 `end` 的说明）。内部会 `tokio::spawn`，必须在 Tokio
/// 运行时内调用。
///
/// TS: `remote.ts#serveCodemodeRemote`。
pub async fn serve_codemode_remote(
    request: CodemodeRemoteRequest,
    exchange: CodemodeRemoteExchange,
    options: CodemodeServeOptions,
) {
    let CodemodeRemoteRequest {
        data,
        interrupt_budget,
    } = request;
    // 预算：请求缺省 → 用上限；请求给了值 → 取它与上限的较小者（TS 的 `Math.min`）。
    let max_budget = options
        .max_interrupt_budget
        .unwrap_or(DEFAULT_INTERRUPT_BUDGET);
    let budget = interrupt_budget.map_or(max_budget, |budget| budget.min(max_budget));
    // 内存：请求值缺省 → 用上限；上限缺省 → 用请求值；两者皆有 → 取较小者（spec §A.5.5）。
    let memory_limit_bytes = match (data.memory_limit_bytes, options.max_memory_limit_bytes) {
        (None, max_memory) => max_memory,
        (Some(requested), None) => Some(requested),
        (Some(requested), Some(max_memory)) => Some(js_math_min(requested, max_memory)),
    };
    let data = VmStartData {
        memory_limit_bytes,
        ..data
    };

    let (resolved_tx, resolved_rx) = oneshot::channel::<()>();
    let (dispatch_tx, mut dispatch_rx) = tokio::sync::mpsc::unbounded_channel::<DispatchItem>();
    let state = Arc::new(ServeState {
        exchange,
        dispatch: dispatch_tx,
        channel: Mutex::new(None),
        ended: AtomicBool::new(false),
        buffered: Mutex::new(Vec::new()),
        buffered_chars: AtomicUsize::new(0),
        unsent: Mutex::new(Vec::new()),
        resolved: Mutex::new(Some(resolved_tx)),
    });

    // exchange 的分发器：按 FIFO 逐批启动 exchange，并且**只有**当上一批的 exchange future
    // 被轮询过一次（也就是消息已经交给宿主）之后才启动下一批。TS 里 `exchange(batch)` 的同步
    // 前缀就是这个时刻，同一 stub 上的调用按序到达；Rust 的 future 在被轮询之前什么都不做，
    // 所以需要这个握手，否则终态批次可能超车先前的输出批次（宿主结算后就会丢掉迟到的输出）。
    //
    // 分发器只持弱引用：`ServeState` 释放（serve 结束）后队列里剩余批次不再启动；已在途
    // 批次的内层任务各持一个强引用，会跑完并安全地把回答交给可能已经 `ended` 的 state。
    {
        let weak = Arc::downgrade(&state);
        tokio::spawn(async move {
            while let Some(item) = dispatch_rx.recv().await {
                let Some(state) = weak.upgrade() else {
                    break;
                };
                let DispatchItem {
                    messages,
                    delivered,
                } = item;
                let future = (state.exchange)(messages);
                let (polled_tx, polled_rx) = oneshot::channel::<()>();
                let task_state = Arc::clone(&state);
                tokio::spawn(async move {
                    let mut future = CatchUnwind::new(future);
                    let mut polled_tx = Some(polled_tx);
                    let mut first = true;
                    let outcome = std::future::poll_fn(move |context| {
                        let poll = std::pin::Pin::new(&mut future).poll(context);
                        if first {
                            first = false;
                            if let Some(polled_tx) = polled_tx.take() {
                                let _ = polled_tx.send(());
                            }
                        }
                        poll
                    })
                    .await;
                    // `end()` 的 `delivered`：exchange future 结算（TS 里 Promise settle）。
                    if let Some(delivered) = delivered {
                        let _ = delivered.send(());
                    }
                    match outcome {
                        Ok(reply) => task_state.reply(reply),
                        // exchange 拒绝 → `end(undefined)`。
                        Err(_) => task_state.end(None),
                    }
                });
                let _ = polled_rx.await;
            }
        });
    }

    let events: Arc<dyn VmEvents> = Arc::new(ServeEvents {
        state: Arc::clone(&state),
    });
    let launch = inline_launcher(options.wasm, budget);
    match CatchUnwind::new(launch(data, events)).await {
        Ok(Ok(channel)) => {
            *state.channel.lock().expect("channel mutex") = Some(Arc::clone(&channel));
            if state.ended.load(Ordering::SeqCst) {
                // `end()` 已经跑过（或正在跑）：它可能看到的 channel 还是 `None`，所以这里补上
                // 停止（TS：`void started.stop()`，两处都停是幂等的）。
                let _ = CatchUnwind::new(channel.stop()).await;
            } else {
                // 在 channel 到位之前产生的回答，按序补发。
                let unsent = std::mem::take(&mut *state.unsent.lock().expect("unsent mutex"));
                for message in unsent {
                    channel.post(message);
                }
            }
        }
        // 拒绝（launcher 的 `Err` 与 panic 都按 Promise 拒绝语义处理）：`end({type:"failure",
        // error:{kind:"sandbox", message: errorMessage(error)}})`。
        Ok(Err(message)) => state.end(Some(CodemodeRemoteMessage::Failure {
            error: CodemodeError::new(CodemodeErrorKind::Sandbox, message),
        })),
        Err(payload) => state.end(Some(CodemodeRemoteMessage::Failure {
            error: CodemodeError::new(CodemodeErrorKind::Sandbox, panic_message(payload)),
        })),
    }

    // 等 `end()` 的完成信号：它在终态批次结算（或客户端 stop）并停掉 channel 之后发出。
    // `ServeState` 在本函数里一直存活，正常路径不会走到 RecvError；忽略它以保持“永不拒绝”。
    let _ = resolved_rx.await;
}

/// 一批要经 `exchange` 发出的消息（下发队列的一项）。
struct DispatchItem {
    messages: Vec<CodemodeRemoteMessage>,
    /// 结算信号：该批的 exchange future 结束后触发（`end()` 等它再停 channel）。
    delivered: Option<oneshot::Sender<()>>,
}

/// `serveCodemodeRemote` 的共享状态（TS 里是 Promise 执行器里的闭包变量）。
struct ServeState {
    exchange: CodemodeRemoteExchange,
    /// 分批下发的队列（见 `serve_codemode_remote` 里的分发器）。
    dispatch: tokio::sync::mpsc::UnboundedSender<DispatchItem>,
    /// launcher 返回的 channel；`end()` 在回调执行时读取它（TS 的 `channel?.stop()`）。
    channel: Mutex<Option<Arc<dyn VmChannel>>>,
    /// `end()` 是否已开始（幂等门闩）：置位后 `on_message` 与 `reply` 丢弃后续输入。
    ended: AtomicBool,
    /// 尚未经 exchange 发出的输出与 `started`（分批规则见 `on_message`）。
    buffered: Mutex<Vec<CodemodeRemoteMessage>>,
    /// 缓冲里输出项的字符串长度之和（Text 的 `text` 或 Image 的 `data`，UTF-16 码元；
    /// 冲刷阈值见 [`FLUSH_CHARS`]）。
    buffered_chars: AtomicUsize,
    /// channel 到位之前产生的回答。
    unsent: Mutex<Vec<HostToWorkerMessage>>,
    /// serve 的完成通知（`end()` 最终 resolve 的那个 Promise）。
    resolved: Mutex<Option<oneshot::Sender<()>>>,
}

impl ServeState {
    /// `takeBuffer()`。
    fn take_buffer(&self) -> Vec<CodemodeRemoteMessage> {
        let batch = std::mem::take(&mut *self.buffered.lock().expect("buffered mutex"));
        self.buffered_chars.store(0, Ordering::SeqCst);
        batch
    }

    /// `end(terminal?)`：幂等。先发最后一批（含 terminal），等 exchange resolve 后
    /// `channel.stop()`，再 resolve。
    ///
    /// `terminal` 为 [`None`]（客户端要求停下）时缓冲被丢弃、不发 exchange——TS 的
    /// `terminal ? exchange([...batch, terminal]).catch(…) : Promise.resolve()`。
    ///
    /// “不发 exchange”只指本次调用：已经排在分发队列里的批次不会被取消，分发器仍会为它们
    /// 调用 exchange（它不检查 `ended`），只是它们结算后的回答会被 `reply` 丢弃。`end` 之后
    /// 新开始的 `on_message` 会在入口被 `ended` 拦住；已通过入口检查的并发 `on_message` 仍可能
    /// 入队一批（其回答会被 `reply` 丢弃）。
    fn end(self: &Arc<Self>, terminal: Option<CodemodeRemoteMessage>) {
        if self.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        let batch = self.take_buffer();
        let delivered_rx = match terminal {
            Some(terminal) => {
                let mut messages = batch;
                messages.push(terminal);
                let (delivered_tx, delivered_rx) = oneshot::channel::<()>();
                // 走同一条 FIFO 队列：终态批次一定排在它之前的输出/调用批次之后。
                let _ = self.dispatch.send(DispatchItem {
                    messages,
                    delivered: Some(delivered_tx),
                });
                Some(delivered_rx)
            }
            None => {
                // `takeBuffer()` 的副作用也要有（批次的丢弃是可见的语义：不再发送）。
                drop(batch);
                None
            }
        };
        let state = Arc::clone(self);
        tokio::spawn(async move {
            if let Some(delivered) = delivered_rx {
                // TS: `.catch(() => null)`——exchange 拒绝也继续走停止/完成。
                let _ = delivered.await;
            }
            // `channel` 在回调执行时才读取（TS 的 `.then(() => channel?.stop())`）。
            let channel = state.channel.lock().expect("channel mutex").clone();
            if let Some(channel) = channel {
                // `.catch(() => undefined)`
                let _ = CatchUnwind::new(channel.stop()).await;
            }
            let resolved = state.resolved.lock().expect("resolved mutex").take();
            if let Some(resolved) = resolved {
                let _ = resolved.send(());
            }
        });
    }

    /// `reply(message)`：`ended || message === null` → 忽略；`stop` → 结束；否则交给 VM
    /// （channel 未到位时排队）。
    fn reply(self: &Arc<Self>, message: CodemodeRemoteReply) {
        if self.ended.load(Ordering::SeqCst) {
            return;
        }
        match message {
            CodemodeRemoteReply::Null => {}
            CodemodeRemoteReply::Stop => self.end(None),
            CodemodeRemoteReply::Result(message) => {
                let channel = self.channel.lock().expect("channel mutex").clone();
                match channel {
                    Some(channel) => channel.post(message),
                    None => self.unsent.lock().expect("unsent mutex").push(message),
                }
            }
        }
    }

    /// 把一批消息交给 exchange（入 FIFO 队列；分发器负责真正调用与处理结果）。
    fn dispatch(&self, messages: Vec<CodemodeRemoteMessage>) {
        let _ = self.dispatch.send(DispatchItem {
            messages,
            delivered: None,
        });
    }

    /// `onMessage(message)`：输出入缓冲（缓冲项数或输出字符数达阈值时冲刷），`started` 只入
    /// 缓冲，`call`/`idle` 立即冲刷，其余（`done`/`crash`）作为终态结束。
    fn on_message(self: &Arc<Self>, message: Value) {
        if self.ended.load(Ordering::SeqCst) {
            return;
        }
        // 真实 VM 只产生规范消息（见模块文档的已知偏差）；解析失败的消息没有类型可承载。
        let Ok(message) = serde_json::from_value::<WorkerToHostMessage>(message) else {
            return;
        };
        match message {
            WorkerToHostMessage::Output { item } => {
                // `bufferedChars += item.type === "text" ? item.text.length : item.data.length`
                // ——JS 的 `string.length` 是 UTF-16 码元数。
                let chars = match &item {
                    CodemodeOutputItem::Text { text, .. } => utf16_len(text),
                    CodemodeOutputItem::Image { data, .. } => utf16_len(data),
                };
                let count = {
                    let mut buffered = self.buffered.lock().expect("buffered mutex");
                    buffered.push(CodemodeRemoteMessage::Worker(WorkerToHostMessage::Output {
                        item,
                    }));
                    buffered.len()
                };
                let buffered_chars = self
                    .buffered_chars
                    .fetch_add(chars, Ordering::SeqCst)
                    .wrapping_add(chars);
                if count >= FLUSH_ITEMS || buffered_chars >= FLUSH_CHARS {
                    self.dispatch(self.take_buffer());
                }
            }
            // `started` 只入缓冲：宿主重放脚本时会等到它，以确认脚本没有进一步调用。
            message @ WorkerToHostMessage::Started { .. } => {
                self.buffered
                    .lock()
                    .expect("buffered mutex")
                    .push(CodemodeRemoteMessage::Worker(message));
            }
            // 立即发送：重放脚本的宿主等它来确认脚本没有再发起调用。
            message @ (WorkerToHostMessage::Call { .. } | WorkerToHostMessage::Idle { .. }) => {
                let mut batch = self.take_buffer();
                batch.push(CodemodeRemoteMessage::Worker(message));
                self.dispatch(batch);
            }
            // `done`/`crash` 是终态：随最后一次冲刷一起发出。
            message @ (WorkerToHostMessage::Done(_) | WorkerToHostMessage::Crash { .. }) => {
                self.end(Some(CodemodeRemoteMessage::Worker(message)));
            }
        }
    }
}

/// 把 serve 的 [`VmEvents`] 接到 [`ServeState`] 上。
struct ServeEvents {
    state: Arc<ServeState>,
}

impl VmEvents for ServeEvents {
    fn message(&self, message: Value) {
        self.state.on_message(message);
    }

    fn failure(&self, error: CodemodeError) {
        self.state
            .end(Some(CodemodeRemoteMessage::Failure { error }));
    }
}

/// 客户端侧 launcher：宿主在本进程，VM 在远端。
///
/// 流程：
///
/// 1. 调 `remote` 工厂（panic 被 `catch_unwind` 接住，作为 launcher 的 `Err` 文本
///    `Failed to reach the remote sandbox: …`）；
/// 2. 在后台任务里调 [`CodemodeRemote::execute`]，把下方构造的 `exchange` 交给它；
/// 3. `exchange` 收到 `call` 时先登记 `id → oneshot`，再把消息转给宿主；future 在宿主经
///    `RemoteChannel::post` 回答该 `id` 时完成，没有调用时立即返回 `Null`；
/// 4. `execute` 结算后按 `settled`/`stopped` 决定是否报告 `kind: "sandbox"` 失败。
///
/// `settled` 在收到 `done`/`crash`/`failure` 时置位，用来区分“远端正常返回”与“没交付终态
/// 就返回”；`stopped` 由 channel 的 `stop` 置位（`Execution` 在正常结算、deadline、abort
/// 或 `close()` 时都会调它），用来丢弃本地已经结算之后才到达的远端错误。
///
/// TS: `remote.ts#remoteLauncher`。
fn remote_launcher(
    remote: Arc<dyn Fn() -> Arc<dyn CodemodeRemote> + Send + Sync>,
    interrupt_budget: Option<u32>,
) -> VmLauncher {
    Arc::new(move |data: VmStartData, events: Arc<dyn VmEvents>| {
        let remote = Arc::clone(&remote);
        Box::pin(async move {
            // `stopped`：宿主侧的 channel 已经要求停止；`settled`：远端已交付终态或 failure。
            let stopped = Arc::new(AtomicBool::new(false));
            let settled = Arc::new(AtomicBool::new(false));
            let waiting: Arc<Mutex<HashMap<u64, oneshot::Sender<CodemodeRemoteReply>>>> =
                Arc::new(Mutex::new(HashMap::new()));

            let exchange: CodemodeRemoteExchange = {
                let events = Arc::clone(&events);
                let stopped = Arc::clone(&stopped);
                let settled = Arc::clone(&settled);
                let waiting = Arc::clone(&waiting);
                Arc::new(move |messages: Vec<CodemodeRemoteMessage>| {
                    let events = Arc::clone(&events);
                    let stopped = Arc::clone(&stopped);
                    let settled = Arc::clone(&settled);
                    let waiting = Arc::clone(&waiting);
                    Box::pin(async move {
                        if stopped.load(Ordering::SeqCst) {
                            return CodemodeRemoteReply::Stop;
                        }
                        // 先登记 waiter：宿主可能同步回答一个调用（例如工具在返回 promise
                        // 之前就抛错）。
                        let call = messages.iter().find_map(|message| match message {
                            CodemodeRemoteMessage::Worker(WorkerToHostMessage::Call {
                                id, ..
                            }) => Some(*id),
                            _ => None,
                        });
                        let receiver = call.map(|id| {
                            let (sender, receiver) = oneshot::channel();
                            waiting.lock().expect("waiting mutex").insert(id, sender);
                            receiver
                        });
                        for message in &messages {
                            match message {
                                CodemodeRemoteMessage::Failure { error } => {
                                    settled.store(true, Ordering::SeqCst);
                                    events.failure(error.clone());
                                }
                                CodemodeRemoteMessage::Worker(message) => {
                                    // 畸形（真实 VM 不会发出的）消息同样交给宿主，由它按
                                    // 坏桥接失败；这里只按类型判断终态。
                                    if is_terminal(message) {
                                        settled.store(true, Ordering::SeqCst);
                                    }
                                    events.message(
                                        serde_json::to_value(message)
                                            .expect("WorkerToHostMessage serializes"),
                                    );
                                }
                            }
                        }
                        match receiver {
                            // waiter 消失（channel 被丢弃）时按 `stop` 处理。
                            Some(receiver) => receiver.await.unwrap_or(CodemodeRemoteReply::Stop),
                            None => CodemodeRemoteReply::Null,
                        }
                    })
                })
            };

            // `remote()` 抛错 → `Failed to reach the remote sandbox: …`。
            let target = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (remote)()))
            {
                Ok(target) => target,
                Err(payload) => {
                    return Err(format!(
                        "Failed to reach the remote sandbox: {}",
                        panic_message(payload)
                    ));
                }
            };
            let request = CodemodeRemoteRequest {
                data,
                interrupt_budget,
            };

            {
                let events = Arc::clone(&events);
                let stopped = Arc::clone(&stopped);
                let settled = Arc::clone(&settled);
                // `Promise.resolve().then(() => target.execute(request, exchange)).then(…)`
                tokio::spawn(async move {
                    let execute = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        target.execute(request, exchange)
                    }));
                    match execute {
                        Ok(future) => match CatchUnwind::new(future).await {
                            Ok(Ok(())) => {
                                if !settled.load(Ordering::SeqCst)
                                    && !stopped.load(Ordering::SeqCst)
                                {
                                    events.failure(CodemodeError::new(
                                        CodemodeErrorKind::Sandbox,
                                        "Remote sandbox returned before the script settled",
                                    ));
                                }
                            }
                            Ok(Err(message)) => {
                                if !stopped.load(Ordering::SeqCst) {
                                    events.failure(CodemodeError::new(
                                        CodemodeErrorKind::Sandbox,
                                        format!("Remote sandbox failed: {message}"),
                                    ));
                                }
                            }
                            Err(payload) => {
                                if !stopped.load(Ordering::SeqCst) {
                                    events.failure(CodemodeError::new(
                                        CodemodeErrorKind::Sandbox,
                                        format!(
                                            "Remote sandbox failed: {}",
                                            panic_message(payload)
                                        ),
                                    ));
                                }
                            }
                        },
                        // 同步 panic 等同 TS 的同步 throw → Promise 拒绝。
                        Err(payload) => {
                            if !stopped.load(Ordering::SeqCst) {
                                events.failure(CodemodeError::new(
                                    CodemodeErrorKind::Sandbox,
                                    format!("Remote sandbox failed: {}", panic_message(payload)),
                                ));
                            }
                        }
                    }
                });
            }

            Ok(Arc::new(RemoteChannel { stopped, waiting }) as Arc<dyn VmChannel>)
        })
    })
}

/// 客户端对远端 VM 的 channel：`post` 唤醒等待该调用的 exchange，`stop` 用 `stop` 回答全部
/// waiter。
///
/// `post` 只处理 `waiting` 里存在的 `id`：重复回答或 `stop` 之后到达的回答被忽略。
/// `stop` 可重复调用：置位 `stopped` 并排空当前 waiter 表（每个 waiter 收到一次 `Stop`）。
struct RemoteChannel {
    stopped: Arc<AtomicBool>,
    waiting: Arc<Mutex<HashMap<u64, oneshot::Sender<CodemodeRemoteReply>>>>,
}

impl VmChannel for RemoteChannel {
    fn post(&self, message: HostToWorkerMessage) {
        let id = message.id();
        let sender = self.waiting.lock().expect("waiting mutex").remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(CodemodeRemoteReply::Result(message));
        }
    }

    fn stop(&self) -> BoxFuture<'static, ()> {
        self.stopped.store(true, Ordering::SeqCst);
        let waiters: Vec<oneshot::Sender<CodemodeRemoteReply>> = self
            .waiting
            .lock()
            .expect("waiting mutex")
            .drain()
            .map(|(_, sender)| sender)
            .collect();
        for sender in waiters {
            let _ = sender.send(CodemodeRemoteReply::Stop);
        }
        Box::pin(async {})
    }
}

/// 每次执行都在远端（通常是部署了 `CodemodeSandboxDurableObject` 的 Cloudflare Durable
/// Object）上跑 VM，而工具在这里运行。API 与结果同 `CodemodeSandbox`，因此可以替换它。区别：
/// 不调用工具地计算的脚本受远端中断预算而不是 `timeoutMs` 限制；deadline 或 abort 之后远端 VM
/// 会继续计算，直到它下一次调用工具或用尽预算（spec §A.5.1）。
///
/// 本结构只替换 launcher：工具表、全局函数、deadline、abort 与结果结算都在本进程的
/// [`CodemodeSandboxBase`] 里。远端收到的执行数据只有 [`VmStartData`] 与可选的
/// `interruptBudget`（外加把调用与输出送回宿主的 `exchange` 回调）。
///
/// TS: `remote.ts#RemoteCodemodeSandbox extends CodemodeSandboxBase`。
pub struct RemoteCodemodeSandbox {
    base: Arc<CodemodeSandboxBase>,
}

impl RemoteCodemodeSandbox {
    /// 构造：创建远端 launcher 并交给 [`CodemodeSandboxBase`]；此处**不**调用
    /// `options.remote` 工厂，也不启动 VM（每次 `execute()`/`start()` 才启动）。
    ///
    /// TS 的构造函数会因非法全局名抛错，因此 Rust 侧返回 `Result`。Rust 侧的
    /// [`CodemodeSandboxBase::new`] 还会拒绝重复的工具名（同样返回 [`StartError`]）。
    pub fn new(options: RemoteCodemodeSandboxOptions) -> Result<Self, StartError> {
        let launch = remote_launcher(Arc::clone(&options.remote), options.interrupt_budget);
        let base = CodemodeSandboxBase::new(options.base, launch)?;
        Ok(RemoteCodemodeSandbox {
            base: Arc::new(base),
        })
    }

    /// 基类（工具表、`execute`/`start`/`close`）。
    pub fn base(&self) -> &Arc<CodemodeSandboxBase> {
        &self.base
    }

    /// TS: `CodemodeSandboxBase.execute`。
    ///
    /// 脚本失败不会让 future 失败：结果在 [`CodemodeResult`] 的 `Err` 一支里；只有 sandbox
    /// 已关闭时才返回 `Err(SandboxClosed)`。正常启动 VM 的每次调用都会调用一次 `remote`
    /// 工厂；sandbox 已关闭或 `options.signal` 在启动前已取消时不会启动远端。
    pub fn execute(
        &self,
        code: &str,
        options: CodemodeExecuteOptions,
    ) -> BoxFuture<'static, Result<CodemodeResult, crate::runtime::execution::SandboxClosed>> {
        self.base.execute(code, options)
    }

    /// TS: `CodemodeSandboxBase.start`。
    ///
    /// 确定性 run：每次调用启动一次执行（`options.signal` 在启动前已取消时直接以 aborted
    /// 结算，不启动 VM）。参数校验失败（`started_at`/`seed` 非法、工具名或脚本名重复、
    /// 全局名非法、sandbox 已关闭）同步返回 [`StartError`]，不启动 VM。
    pub fn start(
        &self,
        code: &str,
        options: CodemodeRunOptions,
    ) -> Result<CodemodeRun, StartError> {
        self.base.start(code, options)
    }

    /// TS: `CodemodeSandboxBase.close`。
    ///
    /// 取消在途执行（它们以 `kind: "aborted"` 结束）并拒绝之后的新执行；等所有结果 future
    /// 结算后才返回。
    pub async fn close(&self) {
        self.base.close().await;
    }

    /// TS: `CodemodeSandboxBase.registerTool`。
    ///
    /// 重名返回 `Err("Tool \"<name>\" is already registered")`；只影响之后开始的执行
    /// （在途执行持有自己的工具表快照）。
    pub fn register_tool(&self, tool: CodemodeTool) -> Result<(), String> {
        self.base.register_tool(tool)
    }

    /// TS: `CodemodeSandboxBase.unregisterTool`。
    ///
    /// 返回该名字此前是否存在；在途执行不受影响。
    pub fn unregister_tool(&self, name: &str) -> bool {
        self.base.unregister_tool(name)
    }

    /// TS: `get tools()`。
    ///
    /// 注册顺序的快照。
    pub fn tools(&self) -> Vec<CodemodeTool> {
        self.base.tools()
    }

    /// TS: `get globals()`。
    ///
    /// 注册顺序的快照。
    pub fn globals(&self) -> Vec<CodemodeTool> {
        self.base.globals()
    }
}

impl std::fmt::Debug for RemoteCodemodeSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteCodemodeSandbox")
    }
}

// ---------------------------------------------------------------------------
// panic → 文本 / future 的 panic 捕获
// ---------------------------------------------------------------------------

/// panic 载荷 → 文本（与 `execution.rs` 的同名辅助一致：TS 的 `errorMessage` 取 `Error.message`）。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "an unknown error".to_string()
    }
}

/// 轮询一个 future，把**轮询期间的 panic** 折成 `Err(payload)`（TS 里 `Promise` 的拒绝）。
///
/// 与 `execution.rs` 的同名适配器同义；两个模块各持一份以避免改动共享文件（`execution.rs`
/// 的实现是私有的）。`Pin<Box<F>>` 是 `Unpin`，投影不需要 unsafe。
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
