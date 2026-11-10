//! `client.ts`：`McpClient` 状态机、pending 表、超时、取消与分页。
//!
//! TS 源：`packages/mcp/src/client.ts`。规格：`spec/telemetry-mcp.md` §13（§13.1–§13.8）、§27.3、
//! §30.2/§30.4/§30.5/§30.8/§30.25/§30.27/§30.30。
//!
//! # 结构
//!
//! [`McpClient`] 是 [`Arc`] 包着的句柄：客户端注册到**传输**上的三个回调只持有
//! `Weak<ClientInner>`（传输由 `ClientState` 强引用，回调再用强引用会形成环）；
//! 用户注册的请求 handler 与通知监听器以 `Arc` 句柄存在状态表里，捕获什么由调用方决定。
//! 所有可变状态在内部的 `ClientState` 里由一把短临界区的 `std::sync::Mutex` 保护
//! （§4.2：纯内存短临界区）。锁**从不**跨 `await` 持有。
//!
//! # 一次连接的生命周期
//!
//! 1. [`McpClient::new`]：状态为 `Idle`，预置 `ping` handler；配置了 [`Roots`] 时再预置
//!    `roots/list`。
//! 2. [`McpClient::connect`]：`Idle` → `Connecting`，保存传输并挂上三个传输监听器；握手依次
//!    是 `transport.start()` → `initialize` 请求 → 校验并记录 `serverInfo`/
//!    `serverCapabilities`/`instructions` → `transport.set_protocol_version` →
//!    `notifications/initialized` → `Connected`。任一步失败都会 `close()`，最终停在 `Closed`。
//! 3. 出站请求：`request()`/`ping()`/`call_tool()`/`read_resource()`/`list_*()` 在**调用时**
//!    同步完成 `require_transport`、预取消检查、分配 id、登记 pending 与定时器；真正的等待
//!    在独立 task 里（`start_request` → `PendingDriver::run`）。调用方 drop 返回的 future
//!    不会撤销已经开始的请求。
//! 4. 入站消息：传输回调 → `handle_message`，按 **response → request → notification** 归类。
//!    响应结算 pending；服务端请求先同步查 handler、登记 `incoming`，再在 task 里等待并把
//!    结果或错误写回；通知先处理内置的 `notifications/progress` 与
//!    `notifications/cancelled`，再按注册顺序派发。
//! 5. 关闭：[`McpClient::close`] 或传输 `on_close` → `mark_closed`：状态置 `Closed`，所有
//!    pending 以 `McpConnectionClosedError` 拒绝，所有 incoming 被取消，close 监听器按快照
//!    调用一次。`Closed` 是终态，不能重连。
//!
//! # 并发与锁
//!
//! 只有一把客户端锁（`ClientInner.state` 的 `std::sync::Mutex`）。**同时获取**时顺序固定为
//! **客户端锁 → 独立子锁**（`ListenerSet`、`IncomingEntry.reason`），代码里没有反向再取客户端
//! 锁的情况（`handle_cancelled` 是客户端锁 → `reason` 的嵌套例子）。注意这两个子锁并不由
//! 客户端锁保护，也会在不持客户端锁时被单独获取：`handle_notification` 在释放客户端锁后
//! 直接锁 `ListenerSet`，`RequestContext::reason` 与 `mark_closed` 也直接锁 `reason`。
//! 锁不跨 `await`。监听器、progress 回调与请求 handler 都在**释放客户端锁之后**
//! 调用（先把需要的 `Arc`/条目克隆出来再出锁），回调执行期间不占这把锁。锁中毒一律用
//! `PoisonError::into_inner` 继续：某次加锁期间 panic 后，后续调用不会因 `PoisonError`
//! 失败。
//!
//! # 调用方与测试
//!
//! 生产代码里构造并使用 `McpClient` 的地方是 `kiana-builtin-extensions/src/mcp/runtime.rs`
//! （MCP 服务器连接管理：配置 roots/timeout、注册通知与 close 监听器、`connect`、
//! `list_tools`、`call_tool`、`close`）。行为契约由 `crates/kiana-mcp/tests/client.rs` 的
//! 集成测试锁定（`packages/mcp/test/client.test.ts` 的 13 个 `it` 逐字移植，另有评审回归
//! 用例）。
//!
//! # 阅读顺序
//!
//! [`McpClientOptions`]/[`McpRequestOptions`] → [`McpClient::connect`]（`initialize`）→
//! `start_request`/`PendingDriver`（出站）→ `handle_message`（入站）→ `mark_closed`（关闭）。
//!
//! # 与 TS 的映射
//!
//! - `Promise` → `async fn`；`AbortSignal` → [`CancellationToken`]；`AbortSignal.reason` 没有等价物，
//!   因此 [`McpRequestOptions::signal_reason`] 显式携带 `signal.reason` 的 JSON 值（见 §13.5 的
//!   逐项实测表）：
//!   `None`（裸 abort，DOMException）→ `"AbortError: This operation was aborted"`，
//!   `Some(null)` → `"Aborted"`，其余 → `String(value)`。
//! - `setTimeout` 定时器 → 每个请求一个 `tokio::sync::watch` 截止时刻 +
//!   [`tokio::time::sleep_until`]；进度通知重置截止时刻。**复刻**了 Node 对
//!   `delay > 2^31-1` 的钳位（§13.7：`timeoutMs = 1e10` 会立即超时）。
//! - `pending: Map<JsonRpcId, PendingRequest>` / `progressRequests` / `incoming` 三张表的键是
//!   [`JsonRpcId`]，其 `Eq`/`Hash` 已是 JS SameValueZero（§30.2）。
//! - `emitError` **不**捕获 listener 异常（§30.4）：Rust 的 error listener 返回 `Result`，
//!   错误沿调用链向上传播（传输的 message 派发点上表现为 `emit_message` 返回 `Err`）。
//! - `handleMessage` 的归类顺序是 **response → request → notification**（§13.7），与
//!   `parse_json_rpc_message` 的 request 优先不同；这里用
//!   [`classify_json_rpc_message`]。
//!
//! # 已知偏差（均为类型化 Rust 的固有差异）
//!
//! - 请求的**同步前缀**（`requireTransport`、预取消检查、分配 id、注册 pending、`armTimeout`）
//!   在调用 `request()`/`ping()`/`call_tool()`/… 的那一刻同步完成，与 TS 的
//!   `requestInternal` 在返回 Promise 之前完成这些步骤一致。`McpTransport::send` 本体是
//!   `async fn`，函数体在 driver task 的第一次 poll 才运行，因此"发出请求后立刻查询服务端收到
//!   的消息"仍需让出至少一次；但 id 占用与取消通知的行为与 TS 相同。
//! - `McpError::JsonRpc.code` 是 `i64`（冻结契约），非整数的 JSON-RPC error code 会被截断
//!   （TS 保留 f64）。
//! - `McpError::Timeout.timeout_ms` 是 `u64`（冻结契约），小数超时值在错误消息里被截断
//!   （TS 打印原值，如 `after 5.5ms`）。**调度**已按 Node 的规则复刻：`0 < ms < 1` 与
//!   `ms > 2^31-1` 都按 1 ms，其余值 `MathTrunc` 到整毫秒（`setTimeout(fn, 5.9)` ≡ 5 ms）。
//! - `Roots` 的 `readonly Root[] | (() => ...)` 用 [`Roots`] 枚举表达：`List` 每次调用克隆一份
//!   全新数组（等价于 `[...roots]`），`Provider` 每次 `await` 提供者。[`Roots::fallible_provider`]
//!   对应"提供者 throw"（TS 的回 `-32603` 路径）。
//! - 请求 ID 是 `u64`（TS 的 `nextRequestId++` 到 2^53 才丢精度，实际不可达）。
//! - `McpClientOptions.capabilities` 的"显式 `null`"无法表达：TS 用
//!   `capabilities.roots === undefined` 判断是否补 `{}`，因此配置输入 `{roots: null}` 会原样发送
//!   `"roots": null`；[`ClientCapabilities`]（冻结契约）把 `null` 与缺省都读成 `None`，这里只能
//!   当作缺省处理。同理 `call_tool` 的 `args` 是 `Option<Map>`，表达不了 TS 的
//!   `callTool(name, null)` → `"arguments": null`（`None` 等价于 `undefined`，省略该键）。
//! - 出站 JSON 的**键序**对调用方构造的对象同样按结构体声明序：`initialize` 的
//!   `capabilities`（TS 里是 `{...options.capabilities}`，键序由调用方的对象字面量决定）与
//!   `roots/list` 的每个 `Root`（TS 原样发送数组里的对象）。能复刻的只有 TS 语义里的**追加**：
//!   自动补的 `capabilities.roots` 排在末尾。`extra`（未知键）同样排在已知键之后；需要逐字
//!   保留键序的调用方可以把全部键放进 `capabilities.extra`（客户端只发送它，不做类型化读取），
//!   这样 `extra` 的插入序就是线上键序。`Root` 没有等价的兜底（冻结契约）。
//! - `list_resources`/`list_resource_templates`/`list_*_page`/`read_resource`/`call_tool` 返回
//!   **类型化**结构体（`Resource`/`CallToolResult`/…，§4.9 冻结）：键序是结构体声明序而不是
//!   TS `{...item, name}` 的"服务端原序 + 追加"，且 `Resource.extra` 等未知键排在已知键之后。
//!   显式 `null` 的已知可选字段（含 `serverInfo`/`capabilities`/`Tool`/`contents[]`/
//!   `content[]` 及 `annotations`/`execution` 的**嵌套**层）都会被补进 `extra` 保留下来，
//!   但它们同样排在已知键之后。要做到逐字等价需要把这些方法的返回类型改成原始 `Value`（契约变更）。
//!
//! - `handleRequest` 的"同步前奏"（查 handler、`incoming.set`）在客户端消息派发里同步完成，
//!   之后才 spawn 等待部分；TS 的实现同样在第一个 `await` 之前同步完成这些步骤。
//! - 定时器复刻了 Node 的钳位与截断（见上）：`timeoutMs > 2^31-1` 与 `0 < timeoutMs < 1`
//!   都按 1 ms 调度，其余值截断到整毫秒。`Infinity`/`NaN`/`<= 0` 不设定时器（永不超时）。
//! - `notifications/cancelled` 的发送是后台任务（TS 的 `void transport.send(...)`）；顺序上
//!   它排在当前请求的 reject 之后（先 `entry.reject`，再 `void send`）。发送失败
//!   经 `emitError` 上报（§13.7）；与 TS 一样，这次 `emitError` 发生在后台任务里，因此即使
//!   error listener 抛错也不会传回调用方。
//! - 请求的等待循环（`receiver`/定时器/取消/send）在**调用时**就放进独立 task 运行，对应 TS
//!   的"Promise executor 独立于 await 方"。因此调用方 drop 掉 `request()` 的 future 之后，
//!   超时仍然触发、`notifications/cancelled` 仍然发送、pending 条目仍然被清理（TS 语义）；
//!   即使在调用后立刻 drop（从未 poll）也一样。
//! - `with_progress_token` 对非对象 `params` 按 JS 对象展开处理（字符串/数组按索引展开）；
//!   字符串以 UTF-16 code unit 索引，孤立代理项在 JSON 里只能写成 U+FFFD（Rust 字符串是
//!   UTF-8，无法表达单个未配对的高/低代理项）。
//! - `handleRequest` 的后台 task 里，`emitError` 的 listener 异常无处传播（TS 里是未处理的
//!   Promise rejection），这里直接丢弃。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use futures_core::future::BoxFuture;
use serde_json::{Map, Value};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::auth_provider::McpTransportError;
use crate::jsonrpc::{
    JsonRpcId, JsonRpcMessage, JsonRpcResponse, McpError, classify_json_rpc_message,
    is_json_rpc_id, parse_json_rpc_message_typed, value_to_json_rpc_id,
};
use crate::protocol_types::{
    CallToolResult, ClientCapabilities, ContentAnnotations, ContentBlock, GetPromptResult,
    Implementation, InitializeResult, LATEST_PROTOCOL_VERSION, ListChangedCapability,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, Prompt,
    ReadResourceResult, Resource, ResourceContents, ResourceTemplate, Root, ServerCapabilities,
    SupportedProtocolVersion, Tool, is_supported_protocol_version,
};
use crate::transports::{McpTransport, McpTransportSendOptions, Subscription};

/// `DEFAULT_REQUEST_TIMEOUT_MS = 30_000`（包内常量，不从 crate 根再导出，§30.1）。
pub const DEFAULT_REQUEST_TIMEOUT_MS: f64 = 30_000.0;

/// `MAX_LIST_PAGES = 1_000`（包内常量，不从 crate 根再导出）。
pub const MAX_LIST_PAGES: usize = 1_000;

/// Node `setTimeout` 的最大延迟（`2^31 - 1` ms）；更大的值被钳到 1 ms（§13.7）。
const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// `ClientState`（`"idle" | "connecting" | "connected" | "closed"`）。
///
/// 状态转换与 [`McpClient::connect`]/[`McpClient::close`] 的实现一一对应：
///
/// - `Idle → Connecting`：`connect` 在锁内完成检查与切换；其他状态调用 `connect` 会直接
///   报错（`Closed` 是终态）；
/// - `Connecting → Connected`：`initialize` 握手全部完成之后；
/// - `Idle`/`Connecting`/`Connected` → `Closed`：`connect` 失败、显式 `close()`、或传输触发
///   `on_close`；`Closed` 没有出边。
///
/// `Connecting` 期间只有握手内部的请求/通知被允许（`allow_connecting`），公开的请求/通知
/// 会被 `McpConnectionClosedError` 拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionState {
    /// `idle`：尚未 `connect`。
    Idle,
    /// `connecting`：`connect()` 进行中（只有 initialize/initialized 被允许）。
    Connecting,
    /// `connected`：初始化完成。
    Connected,
    /// `closed`：`close()` 或传输关闭之后；不可重连（§30.8）。
    Closed,
}

impl ConnectionState {
    /// 线上/消息里的逐字状态名（`requireTransport` 的错误消息使用）。
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionState::Idle => "idle",
            ConnectionState::Connecting => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Closed => "closed",
        }
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `roots` 的取值：固定数组或异步提供者（TS：`readonly Root[] | (() => readonly Root[] | Promise<...>)`）。
#[derive(Clone)]
pub enum Roots {
    /// 固定的 root 列表；每次 `roots/list` 都返回一份新的数组（`[...roots]`）。
    List(Vec<Root>),
    /// 提供者；每次 `roots/list` 都调用并 `await` 它的返回值。
    Provider(Arc<dyn Fn() -> BoxFuture<'static, Vec<Root>> + Send + Sync>),
    /// 可能失败的提供者：`Err` 对应 TS 提供者 throw，客户端回
    /// `{code:-32603, message:<错误消息>}`（`McpError` 则原样带上 code/data）。
    FallibleProvider(
        Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<Root>, McpTransportError>> + Send + Sync>,
    ),
}

impl Roots {
    /// 固定列表（`roots: [...]`）。
    pub fn list(roots: Vec<Root>) -> Self {
        Roots::List(roots)
    }

    /// 异步提供者（`roots: () => ...`）。不可能失败；会失败的场景用
    /// [`Self::fallible_provider`]。
    pub fn provider<F>(provider: F) -> Self
    where
        F: Fn() -> BoxFuture<'static, Vec<Root>> + Send + Sync + 'static,
    {
        Roots::Provider(Arc::new(provider))
    }

    /// 可能失败的异步提供者（TS 里提供者可以 throw）。
    pub fn fallible_provider<F>(provider: F) -> Self
    where
        F: Fn() -> BoxFuture<'static, Result<Vec<Root>, McpTransportError>> + Send + Sync + 'static,
    {
        Roots::FallibleProvider(Arc::new(provider))
    }
}

impl From<Vec<Root>> for Roots {
    fn from(roots: Vec<Root>) -> Self {
        Roots::List(roots)
    }
}

impl std::fmt::Debug for Roots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Roots::List(roots) => f.debug_tuple("Roots::List").field(roots).finish(),
            Roots::Provider(_) => f.write_str("Roots::Provider(..)"),
            Roots::FallibleProvider(_) => f.write_str("Roots::FallibleProvider(..)"),
        }
    }
}

/// `McpClientOptions`（`extends Implementation`：`name`、`version` 必需，`title` 可选）。
///
/// TS 的构造函数做 `Object.freeze({ ...options })`；这里在 [`McpClient::new`] 里保存一份克隆。
///
/// `name`/`version` 不做校验或补默认值：用 `McpClientOptions::default()`（或空串）会把这些
/// 值原样写进 `clientInfo` 发给服务器。`request_timeout_ms` 为 `None` 时回落到
/// [`DEFAULT_REQUEST_TIMEOUT_MS`]。
#[derive(Debug, Clone, Default)]
pub struct McpClientOptions {
    /// `clientInfo.name`。
    pub name: String,
    /// `clientInfo.version`。
    pub version: String,
    /// `clientInfo.title`（缺省不写该键）。
    pub title: Option<String>,
    /// `capabilities`（被浅拷贝；`roots` 在配置了 [`roots`](Self::roots) 且未显式给出时补 `{}`）。
    pub capabilities: Option<ClientCapabilities>,
    /// `protocolVersion`（缺省 [`LATEST_PROTOCOL_VERSION`]）。
    pub protocol_version: Option<SupportedProtocolVersion>,
    /// 请求的默认超时毫秒（缺省 [`DEFAULT_REQUEST_TIMEOUT_MS`]）。
    pub request_timeout_ms: Option<f64>,
    /// 客户端提供的 roots（同时启用 `roots/list` handler 与 `capabilities.roots`）。
    pub roots: Option<Roots>,
}

impl McpClientOptions {
    /// `{ name, version }`。
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        McpClientOptions {
            name: name.into(),
            version: version.into(),
            ..McpClientOptions::default()
        }
    }

    /// 设置 `title`。
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 设置 `capabilities`。
    pub fn with_capabilities(mut self, capabilities: ClientCapabilities) -> Self {
        self.capabilities = Some(capabilities);
        self
    }

    /// 设置 `protocolVersion`。
    pub fn with_protocol_version(mut self, version: SupportedProtocolVersion) -> Self {
        self.protocol_version = Some(version);
        self
    }

    /// 设置 `requestTimeoutMs`。
    pub fn with_request_timeout_ms(mut self, timeout_ms: f64) -> Self {
        self.request_timeout_ms = Some(timeout_ms);
        self
    }

    /// 设置 `roots`。
    pub fn with_roots(mut self, roots: Roots) -> Self {
        self.roots = Some(roots);
        self
    }
}

/// 进度回调（`onProgress?: (p: ProgressNotification) => void`）。
///
/// 参数是 `notifications/progress` 的**原始 `params`**（TS 的
/// `entry.onProgress?.(params as unknown as ProgressNotification)`）：TS 只做类型断言，
/// 不做任何规范化，因此未知键、非数字的 `total`/`message` 以及原始键序都会原样到达回调。
/// 需要类型化视图时用 [`crate::protocol_types::ProgressNotification::from_value`]。
///
/// TS 的异常会被 `handleProgress` 捕获后交给 `emitError`；Rust 用 `Result` 表达"抛了"。
pub type ProgressCallback = Arc<dyn Fn(&Value) -> Result<(), McpTransportError> + Send + Sync>;

/// `McpRequestOptions`。
///
/// 每次请求的可选参数。所有字段都是 `Option`，但只有 `timeout_ms` 存在客户端级回落：
/// 未给出时依次取 [`McpClientOptions::request_timeout_ms`] 与
/// [`DEFAULT_REQUEST_TIMEOUT_MS`]。`signal`/`signal_reason`/`on_progress` 没有客户端级
/// 默认值，未给出时保持 `None`。
///
/// - `signal`：取消令牌；已取消的令牌会让请求在**分配 id 之前**失败（不消耗 id）。
/// - `signal_reason`：参与 `notifications/cancelled` 的 `reason` 字符串（`None` 与
///   `Some(null)` 含义不同，见字段文档）。
/// - `timeout_ms`：单次请求的截止时间；收到的进度通知会把它重置为“现在 + 同一个值”。
/// - `on_progress`：进度回调，`Arc` 共享；分页请求的每一页复用同一份选项。
///
/// `Clone` 是浅拷贝：`CancellationToken` 与回调 `Arc` 都与原值共享同一个底层对象。
#[derive(Clone, Default)]
pub struct McpRequestOptions {
    /// `signal`：请求取消令牌。
    pub signal: Option<CancellationToken>,
    /// `signal.reason`（TS 的 `AbortSignal.reason`）。
    ///
    /// `None` 表示"没有 reason"——对应用户代码里的裸 `abort()`：V8 会填入一个 `DOMException`，
    /// 取消通知里的字符串是 `"AbortError: This operation was aborted"`（§13.5）。
    /// `Some(Value::Null)` → `"Aborted"`；其余值按 `String(value)` 书写（`""` 会让通知省略
    /// `reason` 键，但保留 `requestId`）。
    pub signal_reason: Option<Value>,
    /// `timeoutMs`（缺省取客户端选项，再缺省 `30_000`）。
    pub timeout_ms: Option<f64>,
    /// `onProgress`。
    pub on_progress: Option<ProgressCallback>,
}

impl McpRequestOptions {
    /// 空选项（`{}`）。
    pub fn new() -> Self {
        McpRequestOptions::default()
    }

    /// 设置 `signal`（附 `reason` 时用 [`Self::with_signal_reason`]）。
    pub fn with_signal(mut self, signal: CancellationToken) -> Self {
        self.signal = Some(signal);
        self
    }

    /// 设置 `signal` 与 `signal.reason`。
    pub fn with_signal_reason(mut self, signal: CancellationToken, reason: Value) -> Self {
        self.signal = Some(signal);
        self.signal_reason = Some(reason);
        self
    }

    /// 设置 `timeoutMs`。
    pub fn with_timeout_ms(mut self, timeout_ms: f64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// 设置 `onProgress`。
    pub fn with_on_progress(mut self, on_progress: ProgressCallback) -> Self {
        self.on_progress = Some(on_progress);
        self
    }
}

impl std::fmt::Debug for McpRequestOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRequestOptions")
            .field("signal", &self.signal)
            .field("signal_reason", &self.signal_reason)
            .field("timeout_ms", &self.timeout_ms)
            .field("on_progress", &self.on_progress.as_ref().map(|_| ".."))
            .finish()
    }
}

/// 服务端 → 客户端的请求处理上下文（TS：`{ signal: AbortSignal }`）。
///
/// handler 用它感知取消：`signal` 在服务端发来 `notifications/cancelled` 或客户端关闭时被
/// 取消，`reason` 是那次取消的原因（JSON 值）。客户端只翻转令牌，不会强制中止 handler；
/// 长任务需要自己监听 [`RequestContext::signal`] 退出。
#[derive(Clone)]
pub struct RequestContext {
    signal: CancellationToken,
    reason: Arc<Mutex<Option<Value>>>,
}

impl RequestContext {
    /// `context.signal`。
    pub fn signal(&self) -> &CancellationToken {
        &self.signal
    }

    /// `context.signal.reason`（`notifications/cancelled` 携带的值；未取消为 `None`）。
    ///
    /// 客户端关闭导致的取消用 `{"name": <错误名>, "message": <错误消息>}` 表示。
    /// 服务端发来的 `notifications/cancelled` 没带 `reason` 时，值是
    /// `{"name":"AbortError","message":"This operation was aborted"}`；显式 `reason: null`
    /// 则原样是 `null`。只有第一次取消会写入，后续取消不改变它。
    pub fn reason(&self) -> Option<Value> {
        self.reason
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl std::fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestContext")
            .field("cancelled", &self.signal.is_cancelled())
            .field("reason", &self.reason())
            .finish()
    }
}

/// `RequestHandler = (params, context) => unknown | Promise<unknown>`。
///
/// `Ok(value)` 作为 JSON-RPC 响应的 `result`（`null` 会被换成 `{}`）。`Err(error)` 变成错误
/// 响应：包在 [`McpError::JsonRpc`] 里的错误原样保留 code/message/data，其他错误用 `-32603`
/// 加 `error.message()`。
pub type RequestHandler = Arc<
    dyn Fn(Option<&Value>, RequestContext) -> BoxFuture<'static, Result<Value, McpTransportError>>
        + Send
        + Sync,
>;

/// `NotificationListener = (params) => void`（`Err` 表示 listener 抛出）。
///
/// 返回 `Err` 时先上报给 error listener；只要 error listener 不再抛错，其余 listener 仍会
/// 被依次调用。
pub type NotificationListener =
    Arc<dyn Fn(Option<&Value>) -> Result<(), McpTransportError> + Send + Sync>;

/// `ErrorListener = (error) => void`（`Err` 表示 listener 抛出，会中断后续 listener，§30.4）。
pub type ErrorListener =
    Arc<dyn Fn(&McpTransportError) -> Result<(), McpTransportError> + Send + Sync>;

/// `CloseListener = () => void`（`Err` 表示 listener 抛出，交给 `emitError`）。
///
/// 若 error listener 也把该错误抛出来，剩余的 close 监听器被跳过、错误从 `mark_closed`
/// 返回（`close()` 会在 `transport.close()` 之前中止）。
pub type CloseListener = Arc<dyn Fn() -> Result<(), McpTransportError> + Send + Sync>;

/// 注册句柄（TS 的 `() => void` disposer）。
///
/// 句柄持有 `Arc<ClientInner>`（强引用）：只要句柄还在，客户端内部状态（含传输）就不会被
/// 释放，即使最后一个 [`McpClient`] 已经 drop。`unsubscribe()` 只注销注册，不释放这个
/// 引用；需要释放时还要 drop 句柄本身。
#[derive(Clone)]
pub struct McpSubscription {
    kind: SubscriptionKind,
    inner: Arc<ClientInner>,
}

#[derive(Clone)]
enum SubscriptionKind {
    /// `setRequestHandler`：只有当前 handler 仍是同一个 `Arc` 时才删除。
    Handler {
        method: String,
        handler: RequestHandler,
    },
    /// `onNotification`：disposer 闭包捕获了该 method 当时的 `Set`。
    ///
    /// TS 的 disposer 是 `listeners.delete(listener); if (listeners.size === 0)
    /// this.notificationListeners.delete(method)`：集合为空时删除的是**当前**的 method 映射，
    /// 即使它已经被重建成了另一个 `Set`（陈旧 disposer 会误删新注册的监听器）。
    Notification {
        method: String,
        listener: NotificationListener,
        /// 被闭包捕获的那个 `Set`（即使已从映射摘除，删除仍作用于它）。
        set: Arc<Mutex<ListenerSet<NotificationListener>>>,
    },
    /// `onError`：`this.errorListeners.delete(listener)`（**按函数身份**删除）。
    Error(ErrorListener),
    /// `onClose`：同上。
    Close(CloseListener),
}

impl McpSubscription {
    /// `disposer()`。幂等。
    pub fn unsubscribe(&self) {
        let mut state = self.inner.lock();
        match &self.kind {
            SubscriptionKind::Handler { method, handler } => {
                if let Some(index) = state
                    .request_handlers
                    .iter()
                    .position(|entry| &entry.method == method)
                    && Arc::ptr_eq(&state.request_handlers[index].handler, handler)
                {
                    state.request_handlers.remove(index);
                }
            }
            SubscriptionKind::Notification {
                method,
                listener,
                set,
            } => {
                // `listeners.delete(listener)` 只作用于闭包捕获的那个 Set——即使它已经被
                // 从 method 映射里摘除（摘除后它不会再被派发）。
                let empty = {
                    let mut listeners = set.lock().unwrap_or_else(PoisonError::into_inner);
                    listeners.remove(|entry| Arc::ptr_eq(entry, listener));
                    listeners.is_empty()
                };
                // `if (listeners.size === 0) this.notificationListeners.delete(method)`：
                // 被捕获的集合为空时，删除**当前** method 的映射——如果映射已被重建成了新的
                // `Set`，这里会把新注册的监听器一起清掉（TS 的陈旧 disposer 行为，§30.3 实测）。
                if empty {
                    state
                        .notification_listeners
                        .retain(|group| group.method != *method);
                }
            }
            SubscriptionKind::Error(listener) => {
                state
                    .error_listeners
                    .remove(|entry| Arc::ptr_eq(entry, listener));
            }
            SubscriptionKind::Close(listener) => {
                state
                    .close_listeners
                    .retain(|entry| !Arc::ptr_eq(entry, listener));
            }
        }
    }

    /// 该注册是否已失效（对应 TS 里"disposer 已被调用过"）。
    pub fn is_unsubscribed(&self) -> bool {
        let state = self.inner.lock();
        match &self.kind {
            SubscriptionKind::Handler { method, handler } => !state
                .request_handlers
                .iter()
                .any(|entry| &entry.method == method && Arc::ptr_eq(&entry.handler, handler)),
            SubscriptionKind::Notification { listener, set, .. } => {
                let listeners = set.lock().unwrap_or_else(PoisonError::into_inner);
                !listeners.contains(|entry| Arc::ptr_eq(entry, listener))
            }
            SubscriptionKind::Error(listener) => !state
                .error_listeners
                .contains(|entry| Arc::ptr_eq(entry, listener)),
            SubscriptionKind::Close(listener) => !state
                .close_listeners
                .iter()
                .any(|entry| Arc::ptr_eq(entry, listener)),
        }
    }
}

impl std::fmt::Debug for McpSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("McpSubscription")
            .field(&self.is_unsubscribed())
            .finish()
    }
}

/// 定时器：唯一的共享状态是"下一次截止时刻"（`None` = 没有定时器）。
struct TimerSlot {
    deadline: watch::Sender<Option<tokio::time::Instant>>,
}

impl TimerSlot {
    fn new() -> Self {
        let (deadline, _receiver) = watch::channel(None);
        TimerSlot { deadline }
    }

    /// 写入下一次截止时刻（`None` = 没有定时器）。
    ///
    /// 用 `send_replace` 而不是 `send`：`send` 在没有接收者时**不写入**（返回 `Err`），而这里
    /// 首次写入发生在 `subscribe()` 之前。
    fn set(&self, at: Option<tokio::time::Instant>) {
        self.deadline.send_replace(at);
    }

    fn subscribe(&self) -> watch::Receiver<Option<tokio::time::Instant>> {
        self.deadline.subscribe()
    }
}

/// `PendingRequest`。
struct PendingEntry {
    sender: Option<oneshot::Sender<Result<Value, McpTransportError>>>,
    timeout_ms: f64,
    timer: Arc<TimerSlot>,
    /// `cancellable`：`initialize` 不可取消（不发 `notifications/cancelled`）。
    cancellable: bool,
    on_progress: Option<ProgressCallback>,
    progress_token: Option<JsonRpcId>,
    /// `entry.sendController`：结算后取消，让传输不再派发/重试该消息。
    send_signal: CancellationToken,
}

/// `incoming` 表的条目（服务端请求的 `AbortController`）。
///
/// `reason` 只由第一次 abort 写入：服务端 `notifications/cancelled` 与客户端关闭都可能取消
/// 同一个请求，但两者都先检查 `token.is_cancelled()`，因此
/// [`RequestContext::reason`] 始终是第一次取消的原因。
struct IncomingEntry {
    token: CancellationToken,
    reason: Arc<Mutex<Option<Value>>>,
}

struct HandlerEntry {
    method: String,
    handler: RequestHandler,
}

/// `transport.send(...)` 的 future（`Option` 让它只被 poll 一次，之后分支让位）。
type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), McpTransportError>> + Send + 'a>>;

/// 复刻 V8 `Set` 的实时迭代语义（§30.3 的实测行为）：
///
/// - 迭代期间**删除**尚未访问的元素 → 不会被调用；
/// - 迭代期间**新增**的元素 → 追加在末尾，同一次迭代里**会**被访问；
/// - 删除后重新添加同一函数 → 视作新元素，会再被访问一次。
///
/// 做法是给每次插入分配一个单调序号，迭代时用"序号大于上次访问值的最小项"作游标：
/// 删除不会让后续元素跳过，重插会拿到更大的序号从而被再次访问。
struct ListenerSet<L> {
    next_seq: u64,
    entries: Vec<(u64, L)>,
}

impl<L: Clone> ListenerSet<L> {
    fn new() -> Self {
        ListenerSet {
            next_seq: 0,
            entries: Vec::new(),
        }
    }

    /// `set.add(listener)`：已存在（按 `identical` 判等）时不重复添加。
    fn add(&mut self, listener: L, identical: impl Fn(&L, &L) -> bool) {
        if self
            .entries
            .iter()
            .any(|(_, entry)| identical(entry, &listener))
        {
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push((seq, listener));
    }

    /// `set.delete(listener)`。
    fn remove(&mut self, matches: impl Fn(&L) -> bool) {
        self.entries.retain(|(_, entry)| !matches(entry));
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn contains(&self, matches: impl Fn(&L) -> bool) -> bool {
        self.entries.iter().any(|(_, entry)| matches(entry))
    }

    /// 迭代器语义的下一步：序号严格大于 `after` 的第一个元素。
    fn next_after(&self, after: Option<u64>) -> Option<(u64, L)> {
        self.entries
            .iter()
            .filter(|(seq, _)| after.is_none_or(|after| *seq > after))
            .min_by_key(|(seq, _)| *seq)
            .map(|(seq, entry)| (*seq, entry.clone()))
    }
}

/// 某个 method 的 `Set<NotificationListener>`（TS 的 `notificationListeners.get(method)`）。
///
/// 监听器放在 `Arc<Mutex<..>>` 里而不是直接摊在客户端状态里：TS 的 `Set` 对象在
/// method 键被删除后仍然存在（disposer 闭包着它，正在进行的迭代也还引用它），删除映射不等于
/// 销毁集合。共享的 `Arc` 复刻这一点。
struct NotificationGroup {
    method: String,
    /// 这个 `Set` 对象本身。method 键被删除后重建会得到**新的** `Set`，而旧 disposer 仍
    /// 闭包着旧的那个（见 [`SubscriptionKind::Notification`]）。
    listeners: Arc<Mutex<ListenerSet<NotificationListener>>>,
}

/// `Set<ErrorListener>`（实时迭代，§30.3）。
type ErrorListenerList = ListenerSet<ErrorListener>;

/// `Set<CloseListener>`（快照迭代：`[...this.closeListeners]`）。
type CloseListenerList = Vec<CloseListener>;

/// 客户端可变状态。
///
/// 所有字段只在 `ClientInner::lock()` 的临界区内读写。`pending`（本客户端发出的请求）与
/// `incoming`（服务端发来的请求）都以 [`JsonRpcId`] 为键；`progress_requests` 把进度令牌
/// 映射回请求 id，只在该请求带 `on_progress` 时登记。`request_handlers` 与
/// `notification_listeners` 用 `Vec` 保持注册顺序（对应 TS 的 `Map`/`Set` 迭代序）。
///
/// `disposers` 只在连接建立时填入、`close()` 时取走并退订；`status` 的状态转换见
/// [`ConnectionState`]。
struct ClientState {
    status: ConnectionState,
    transport: Option<Arc<dyn McpTransport>>,
    next_request_id: u64,
    server_info: Option<Implementation>,
    server_capabilities: Option<ServerCapabilities>,
    instructions: Option<String>,
    protocol_version: Option<String>,
    pending: HashMap<JsonRpcId, PendingEntry>,
    progress_requests: HashMap<JsonRpcId, JsonRpcId>,
    incoming: HashMap<JsonRpcId, IncomingEntry>,
    request_handlers: Vec<HandlerEntry>,
    notification_listeners: Vec<NotificationGroup>,
    error_listeners: ErrorListenerList,
    close_listeners: CloseListenerList,
    disposers: Vec<Subscription>,
}

/// 共享内部状态（`McpClient` 的所有克隆指向同一份）。
struct ClientInner {
    options: McpClientOptions,
    state: Mutex<ClientState>,
}

impl ClientInner {
    fn lock(&self) -> MutexGuard<'_, ClientState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 一页分页结果（`validateListPage` 的返回）。
struct ListPage {
    items: Vec<Map<String, Value>>,
    next_cursor: Option<String>,
}

/// `listPage(...)` 已发出、尚未解析的一页请求。
struct StartedPage {
    request: Result<oneshot::Receiver<Result<Value, McpTransportError>>, McpTransportError>,
}

type ItemValidator = fn(&Map<String, Value>) -> bool;

/// 等一页并校验（`validateListPage`）。
async fn drive_list_page(
    method: &str,
    key: &str,
    is_item: ItemValidator,
    started: StartedPage,
) -> Result<ListPage, McpTransportError> {
    let value = match started.request {
        Ok(receiver) => await_pending(receiver).await?,
        Err(error) => return Err(error),
    };
    validate_list_page(method, key, &value, is_item)
}

/// `listAll(...)`：第一页已同步发出，后续页在 await 里逐页发出。
///
/// 每一页请求都用**同一个** `options`（同一个 signal/timeout/progress 回调，§13.4）。
///
/// 与 TS 的循环形状一致：请求在**每轮开头**发出。第 [`MAX_LIST_PAGES`] 页仍带
/// `nextCursor` 时循环直接结束并抛错，**不会**再发第 1001 个请求（评审 finding 1：
/// 修复前在每轮末尾预取下一页，导致多派发一次、多消耗一个 id）。
async fn drive_list_all(
    client: &McpClient,
    method: &str,
    key: &str,
    is_item: ItemValidator,
    options: &McpRequestOptions,
    first: StartedPage,
) -> Result<Vec<Map<String, Value>>, McpTransportError> {
    let mut items: Vec<Map<String, Value>> = Vec::new();
    let mut cursors: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    let mut next = Some(first);
    for _page in 0..MAX_LIST_PAGES {
        let started = match next.take() {
            Some(started) => started,
            None => client.start_list_page(method, key, is_item, cursor.as_deref(), options),
        };
        let page = drive_list_page(method, key, is_item, started).await?;
        items.extend(page.items);
        let Some(next_cursor) = page.next_cursor else {
            return Ok(items);
        };
        if !cursors.insert(next_cursor.clone()) {
            return Err(McpTransportError::new(format!(
                "MCP {method} returned duplicate cursor: {next_cursor}"
            )));
        }
        cursor = Some(next_cursor);
    }
    Err(McpTransportError::new(format!(
        "MCP {method} exceeded {MAX_LIST_PAGES} pages"
    )))
}

/// `McpClient`。
///
/// `McpClient` 可克隆；所有克隆共享同一份状态、监听器与 pending 表（TS 的对象引用语义）。
///
/// 一个客户端绑定一条传输：`connect` 时注入，`close` 时取走，且不能重连。公开方法全是
/// `&self`，可分为三类：
///
/// - 连接管理：[`McpClient::connect`]、[`McpClient::close`] 与状态 getter；
/// - 出站：[`McpClient::request`]、[`McpClient::notify`]、[`McpClient::ping`]、
///   [`McpClient::call_tool`]、`list_*`、[`McpClient::read_resource`]；
/// - 入站：[`McpClient::set_request_handler`]、[`McpClient::on_notification`]、
///   [`McpClient::on_error`]、[`McpClient::on_close`]。
///
/// 请求类方法在调用时同步完成校验/占号/登记，返回的 future 只负责等结果；在 `Idle`/
/// `Connecting`/`Closed` 下调用会得到 `McpConnectionClosedError("MCP client is <state>")`
/// （`initialize` 内部是唯一例外，它以 `allow_connecting` 在握手期间放行）。
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<ClientInner>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("options", &self.inner.options)
            .field("connection_state", &self.connection_state())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 校验（§13.4 / §13.5）
// ---------------------------------------------------------------------------

/// `new McpError(JSON_RPC_ERROR_CODES.invalidRequest, message)`。
fn invalid(message: impl Into<String>) -> McpTransportError {
    McpTransportError::from(McpError::new(
        crate::jsonrpc::JsonRpcErrorCodes::INVALID_REQUEST,
        message,
    ))
}

/// `validateInitializeResult`：先校验响应形状，再转成类型化结构体。
///
/// 必需键：`protocolVersion` 字符串、`capabilities` 对象、`serverInfo` 对象且其 `name` 与
/// `version` 都是字符串。`instructions` 可缺省，出现时必须是字符串。任一不符返回
/// `Invalid MCP initialize result`（`McpError`，code `-32600`）。
///
/// 校验通过后由 `InitializeResult::from_value` 读取；随后把服务器显式写成 `null` 的已知
/// 可选字段补回 `extra`（TS 原样返回服务器对象，见模块文档的键序偏差）。
fn validate_initialize_result(value: &Value) -> Result<InitializeResult, McpTransportError> {
    let Some(map) = value.as_object() else {
        return Err(McpTransportError::from(
            McpError::invalid_initialize_result(),
        ));
    };
    let server_info_ok = map.get("serverInfo").is_some_and(|info| {
        info.as_object().is_some_and(|info| {
            info.get("name").is_some_and(Value::is_string)
                && info.get("version").is_some_and(Value::is_string)
        })
    });
    let instructions_ok = match map.get("instructions") {
        None => true,
        Some(Value::String(_)) => true,
        Some(_) => false,
    };
    let valid = map.get("protocolVersion").is_some_and(Value::is_string)
        && map.get("capabilities").is_some_and(Value::is_object)
        && server_info_ok
        && instructions_ok;
    if !valid {
        return Err(McpTransportError::from(
            McpError::invalid_initialize_result(),
        ));
    }
    let mut result = InitializeResult::from_value(value)
        .ok_or_else(|| McpTransportError::from(McpError::invalid_initialize_result()))?;
    // TS 的 `validateInitializeResult` 原样返回服务器对象：`serverInfo.title: null`、
    // `capabilities.tools: null`、`capabilities.tools.listChanged: null`、`instructions: null`
    // 都保留（评审 finding 3）。
    if let Some(map) = value.as_object() {
        patch_nulls(
            &mut result.extra,
            map,
            &[("instructions", result.instructions.is_some())],
        );
        if let Some(server_info) = map.get("serverInfo") {
            patch_implementation(server_info, &mut result.server_info);
        }
        if let Some(capabilities) = map.get("capabilities") {
            patch_server_capabilities(capabilities, &mut result.capabilities);
        }
    }
    Ok(result)
}

/// `validateListPage(method, key, value, isItem)`：校验一页列表并返回元素的原始对象。
///
/// `value` 必须是对象、`value[key]` 必须是数组，每个元素必须是对象且通过 `is_item`
/// （工具/资源/模板的形状检查）；否则分别得到 `Invalid MCP <method> result` 与
/// `Invalid entry in MCP <method> result`。
///
/// `nextCursor` 缺省、`null` 或空串都表示最后一页（有些服务器用它代替省略 cursor）；其他
/// 非字符串值是错误 `Invalid MCP <method> cursor`。元素保留原始键序，类型化转换由调用方
/// （`to_tool`/`to_resource`/...）完成。
fn validate_list_page(
    method: &str,
    key: &str,
    value: &Value,
    is_item: ItemValidator,
) -> Result<ListPage, McpTransportError> {
    let Some(map) = value.as_object() else {
        return Err(McpTransportError::from(McpError::invalid_result(method)));
    };
    let Some(Value::Array(items)) = map.get(key) else {
        return Err(McpTransportError::from(McpError::invalid_result(method)));
    };
    let mut validated = Vec::with_capacity(items.len());
    for item in items {
        let Some(object) = item.as_object() else {
            return Err(McpTransportError::from(McpError::invalid_result_entry(
                method,
            )));
        };
        if !is_item(object) {
            return Err(McpTransportError::from(McpError::invalid_result_entry(
                method,
            )));
        }
        validated.push(object.clone());
    }
    // 有些服务器用 `null` 或 `""` 结束分页，而不是省略 cursor。
    let next_cursor = match map.get("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor)) if cursor.is_empty() => None,
        Some(Value::String(cursor)) => Some(cursor.clone()),
        Some(_) => return Err(McpTransportError::from(McpError::invalid_cursor(method))),
    };
    Ok(ListPage {
        items: validated,
        next_cursor,
    })
}

/// `isTool`：`typeof tool.name === "string" && isObject(tool.inputSchema)`。
fn is_tool(tool: &Map<String, Value>) -> bool {
    tool.get("name").is_some_and(Value::is_string)
        && tool.get("inputSchema").is_some_and(Value::is_object)
}

/// `isPrompt`：`name` 是字符串。
///
/// `arguments` 的成员形状在 [`to_prompt`] 里按宽松规则读取（不是对象的成员被跳过）；
/// 列表页校验只要求 prompt 本身可识别，与 TS 的 `isPrompt` 一致。
fn is_prompt(prompt: &Map<String, Value>) -> bool {
    prompt.get("name").is_some_and(Value::is_string)
}

/// `isResource`：`uri` 是字符串且 `name` 缺省或为字符串。
fn is_resource(resource: &Map<String, Value>) -> bool {
    resource.get("uri").is_some_and(Value::is_string)
        && match resource.get("name") {
            None => true,
            Some(Value::String(_)) => true,
            Some(_) => false,
        }
}

/// `isResourceTemplate`：`uriTemplate` 是字符串且 `name` 缺省或为字符串。
fn is_resource_template(template: &Map<String, Value>) -> bool {
    template.get("uriTemplate").is_some_and(Value::is_string)
        && match template.get("name") {
            None => true,
            Some(Value::String(_)) => true,
            Some(_) => false,
        }
}

/// `listTools` 的元素：`Tool.from_value`（`isTool` 已保证 `name` 是字符串、`inputSchema` 是对象），
/// 再把显式 `null` 的已知可选字段（`title`/`description`/`outputSchema`/`annotations`/
/// `execution`/`_meta`，以及 `annotations`/`execution` 内层的）补回 `extra`。
fn to_tool(item: &Map<String, Value>) -> Tool {
    let mut tool = Tool::from_value(&Value::Object(item.clone())).unwrap_or_default();
    patch_nulls(
        &mut tool.extra,
        item,
        &[
            ("title", tool.title.is_some()),
            ("description", tool.description.is_some()),
            ("outputSchema", tool.output_schema.is_some()),
            ("annotations", tool.annotations.is_some()),
            ("execution", tool.execution.is_some()),
            ("_meta", tool.meta.is_some()),
        ],
    );
    if let (Some(source), Some(annotations)) = (item.get("annotations"), tool.annotations.as_mut())
    {
        patch_tool_annotations(source, annotations);
    }
    if let (Some(source), Some(execution)) = (item.get("execution"), tool.execution.as_mut()) {
        patch_tool_execution(source, execution);
    }
    tool
}

/// `listPrompts` 的元素：`Prompt::from_value`，再把显式 `null` 的已知可选字段补回 `extra`。
fn to_prompt(item: &Map<String, Value>) -> Prompt {
    let mut prompt = Prompt::from_value(&Value::Object(item.clone())).unwrap_or_default();
    patch_nulls(
        &mut prompt.extra,
        item,
        &[
            ("title", prompt.title.is_some()),
            ("description", prompt.description.is_some()),
            ("arguments", prompt.arguments.is_some()),
            ("_meta", prompt.meta.is_some()),
        ],
    );
    prompt
}

/// `validateGetPromptResult`：对象、`messages` 数组、每条消息 role/content 合法。
///
/// 错误文案是当前稳定契约：
/// - 非对象或 `messages` 不是数组 → `Invalid MCP prompts/get result`；
/// - 元素不是对象、`role` 不是 `user`/`assistant`、或缺少 `content` →
///   `Invalid message in MCP prompts/get result`。
fn validate_get_prompt_result(value: &Value) -> Result<GetPromptResult, McpTransportError> {
    let Some(map) = value.as_object() else {
        return Err(McpTransportError::from(McpError::invalid_result(
            "prompts/get",
        )));
    };
    let Some(Value::Array(messages)) = map.get("messages") else {
        return Err(McpTransportError::from(McpError::invalid_result(
            "prompts/get",
        )));
    };
    for message in messages {
        let valid = message.as_object().is_some_and(|message| {
            message
                .get("role")
                .and_then(Value::as_str)
                .is_some_and(|role| role == "user" || role == "assistant")
                && message.contains_key("content")
        });
        if !valid {
            return Err(McpTransportError::from(McpError::new(
                crate::jsonrpc::JsonRpcErrorCodes::INVALID_REQUEST,
                "Invalid message in MCP prompts/get result",
            )));
        }
    }
    // 校验通过后仍走宽松构造：`description` 等显式 null 由 patch 补回 extra。
    let mut result = GetPromptResult::from_value(value)
        .ok_or_else(|| McpTransportError::from(McpError::invalid_result("prompts/get")))?;
    if let Some(map) = value.as_object() {
        patch_nulls(
            &mut result.extra,
            map,
            &[
                ("description", result.description.is_some()),
                ("_meta", result.meta.is_some()),
            ],
        );
    }
    Ok(result)
}

/// `ToolAnnotations`。
fn patch_tool_annotations(source: &Value, target: &mut crate::protocol_types::ToolAnnotations) {
    if let Some(map) = source.as_object() {
        patch_nulls(
            &mut target.extra,
            map,
            &[
                ("title", target.title.is_some()),
                ("readOnlyHint", target.read_only_hint.is_some()),
                ("destructiveHint", target.destructive_hint.is_some()),
                ("idempotentHint", target.idempotent_hint.is_some()),
                ("openWorldHint", target.open_world_hint.is_some()),
            ],
        );
    }
}

/// `ToolExecution`。
fn patch_tool_execution(source: &Value, target: &mut crate::protocol_types::ToolExecution) {
    if let Some(map) = source.as_object() {
        patch_nulls(
            &mut target.extra,
            map,
            &[("taskSupport", target.task_support.is_some())],
        );
    }
}

/// `ContentAnnotations`（`Resource.annotations`、内容块的 `annotations`）。
fn patch_content_annotations(source: &Value, target: &mut ContentAnnotations) {
    if let Some(map) = source.as_object() {
        patch_nulls(
            &mut target.extra,
            map,
            &[
                ("audience", target.audience.is_some()),
                ("priority", target.priority.is_some()),
                ("lastModified", target.last_modified.is_some()),
            ],
        );
    }
}

/// `toResource(item)` = `{...item, name: item.name ?? item.uri}`。
///
/// `name` 在**已有键之后**添加（TS 的对象展开序）：已有 `name` 时后赋值**不改变键序**，
/// 因此这里原地覆盖；只有缺失时才追加。
///
/// TS 的结果是原始对象展开，显式 `null` 的可选字段（`title: null`、`_meta: null` …）会原样
/// 保留；[`Resource::from_value`] 把它们读成 `None` 后 `restore_unconsumed` 会跳过 `null`，
/// 所以这里把这类字段补回 `extra`，避免丢数据（键序仍是结构体声明序，见模块文档）。
fn to_resource(item: &Map<String, Value>) -> Resource {
    let mut map = item.clone();
    let fallback = map.get("uri").cloned().unwrap_or(Value::Null);
    match map.get_mut("name") {
        Some(slot) => {
            if slot.is_null() {
                *slot = fallback;
            }
        }
        None => {
            map.insert("name".to_owned(), fallback);
        }
    }
    let value = Value::Object(map.clone());
    let mut resource = Resource::from_value(&value).unwrap_or_default();
    preserve_explicit_nulls(&mut resource.extra, item, RESOURCE_OPTIONAL_KEYS);
    if let (Some(source), Some(annotations)) =
        (item.get("annotations"), resource.annotations.as_mut())
    {
        patch_content_annotations(source, annotations);
    }
    resource
}

/// `Resource` 里"TS 会原样保留显式 null"的可选键（`uri`/`name` 是必填，不在此列）。
const RESOURCE_OPTIONAL_KEYS: &[&str] = &[
    "title",
    "description",
    "mimeType",
    "size",
    "annotations",
    "_meta",
];

/// `toResourceTemplate(item)` = `{...item, name: item.name ?? item.uriTemplate}`。
fn to_resource_template(item: &Map<String, Value>) -> ResourceTemplate {
    let mut map = item.clone();
    let fallback = map.get("uriTemplate").cloned().unwrap_or(Value::Null);
    match map.get_mut("name") {
        Some(slot) => {
            if slot.is_null() {
                *slot = fallback;
            }
        }
        None => {
            map.insert("name".to_owned(), fallback);
        }
    }
    let value = Value::Object(map);
    let mut template = ResourceTemplate::from_value(&value).unwrap_or_default();
    preserve_explicit_nulls(&mut template.extra, item, RESOURCE_TEMPLATE_OPTIONAL_KEYS);
    if let (Some(source), Some(annotations)) =
        (item.get("annotations"), template.annotations.as_mut())
    {
        patch_content_annotations(source, annotations);
    }
    template
}

/// `ResourceTemplate` 的可选键（同上）。
const RESOURCE_TEMPLATE_OPTIONAL_KEYS: &[&str] =
    &["title", "description", "mimeType", "annotations", "_meta"];

/// 把源对象里**显式 `null`** 的已知可选键补进 `extra`。
///
/// TS 的 `{...item}` 保留这些 `null`；类型化结构体的 `Option` 字段读成 `None` 后
/// `restore_unconsumed` 会跳过 `null`（它只恢复"类型不符"的原值）。补进 `extra` 后序列化时
/// 它们会重新出现在输出里（位置在已知键之后，见模块文档）。
fn preserve_explicit_nulls(
    extra: &mut Map<String, Value>,
    item: &Map<String, Value>,
    keys: &[&str],
) {
    for key in keys {
        if item.get(*key).is_some_and(Value::is_null) && !extra.contains_key(*key) {
            extra.insert((*key).to_owned(), Value::Null);
        }
    }
}

/// 把 `source` 里**显式 `null`** 的键补进 `extra`（`known` 是 `(键, 类型化字段是否已消费)`）。
///
/// 这是 TS `{...item}` 与类型化结构体之间唯一剩下的语义差：`Option<T>` 把 `null` 读成 `None`
/// 后 `restore_unconsumed` 会跳过它，于是 `title: null` 之类的键被丢掉。补回后序列化时它们重新
/// 出现（位置在已知键之后，见模块文档的键序偏差）。
fn patch_nulls(
    extra: &mut Map<String, Value>,
    source: &Map<String, Value>,
    known: &[(&str, bool)],
) {
    for (key, consumed) in known {
        if !consumed && !extra.contains_key(*key) && source.get(*key).is_some_and(Value::is_null) {
            extra.insert((*key).to_owned(), Value::Null);
        }
    }
}

/// `Implementation`（`serverInfo`/`clientInfo`）的显式 `null`。
fn patch_implementation(source: &Value, target: &mut Implementation) {
    if let Some(map) = source.as_object() {
        patch_nulls(&mut target.extra, map, &[("title", target.title.is_some())]);
    }
}

/// `ListChangedCapability`。
fn patch_list_changed(source: &Value, target: &mut ListChangedCapability) {
    if let Some(map) = source.as_object() {
        patch_nulls(
            &mut target.extra,
            map,
            &[("listChanged", target.list_changed.is_some())],
        );
    }
}

/// `ResourcesCapability`。
fn patch_resources_capability(
    source: &Value,
    target: &mut crate::protocol_types::ResourcesCapability,
) {
    if let Some(map) = source.as_object() {
        patch_nulls(
            &mut target.extra,
            map,
            &[
                ("subscribe", target.subscribe.is_some()),
                ("listChanged", target.list_changed.is_some()),
            ],
        );
    }
}

/// `ServerCapabilities`（含嵌套的 `prompts`/`resources`/`tools`）。
fn patch_server_capabilities(source: &Value, target: &mut ServerCapabilities) {
    let Some(map) = source.as_object() else {
        return;
    };
    patch_nulls(
        &mut target.extra,
        map,
        &[
            ("experimental", target.experimental.is_some()),
            ("logging", target.logging.is_some()),
            ("prompts", target.prompts.is_some()),
            ("resources", target.resources.is_some()),
            ("tools", target.tools.is_some()),
            ("completions", target.completions.is_some()),
        ],
    );
    if let (Some(child), Some(prompts)) = (map.get("prompts"), target.prompts.as_mut()) {
        patch_list_changed(child, prompts);
    }
    if let (Some(child), Some(resources)) = (map.get("resources"), target.resources.as_mut()) {
        patch_resources_capability(child, resources);
    }
    if let (Some(child), Some(tools)) = (map.get("tools"), target.tools.as_mut()) {
        patch_list_changed(child, tools);
    }
}

/// `validateReadResourceResult`：`contents` 必须是数组，每项是对象且 `uri` 为字符串、
/// `text` 或 `blob` 至少一个是字符串；否则返回 `Invalid MCP resources/read result` 或
/// `Invalid contents in MCP resources/read result`。
///
/// 校验通过后转成类型化结构体，并把显式 `null` 的可选字段（顶层 `_meta`、每项的
/// `mimeType`/`_meta`）补回 `extra`，避免 TS `{...value}` 会保留的 `null` 丢失。
fn validate_read_resource_result(value: &Value) -> Result<ReadResourceResult, McpTransportError> {
    let Some(map) = value.as_object() else {
        return Err(invalid("Invalid MCP resources/read result"));
    };
    let Some(Value::Array(contents)) = map.get("contents") else {
        return Err(invalid("Invalid MCP resources/read result"));
    };
    for entry in contents {
        let Some(entry) = entry.as_object() else {
            return Err(invalid("Invalid contents in MCP resources/read result"));
        };
        let uri_ok = entry.get("uri").is_some_and(Value::is_string);
        let body_ok = entry.get("text").is_some_and(Value::is_string)
            || entry.get("blob").is_some_and(Value::is_string);
        if !uri_ok || !body_ok {
            return Err(invalid("Invalid contents in MCP resources/read result"));
        }
    }
    let mut result = ReadResourceResult::from_value(value)
        .ok_or_else(|| invalid("Invalid MCP resources/read result"))?;
    // TS 原样返回服务器对象：`contents[].mimeType: null`、`contents[]._meta: null`、
    // 顶层 `_meta: null` 都保留（评审 finding 3）。
    if let Some(source) = value.as_object() {
        preserve_explicit_nulls(&mut result.extra, source, &["_meta"]);
        if let Some(Value::Array(entries)) = source.get("contents") {
            for (entry, contents) in entries.iter().zip(result.contents.iter_mut()) {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                patch_resource_contents_nulls(entry, contents);
            }
        }
    }
    Ok(result)
}

/// `ResourceContents` 的显式 `null`（`mimeType`/`_meta`，以及 `text`/`blob` 之外的未知键）。
fn patch_resource_contents_nulls(source: &Map<String, Value>, contents: &mut ResourceContents) {
    match contents {
        ResourceContents::Text(text) => patch_nulls(
            &mut text.extra,
            source,
            &[
                ("mimeType", text.mime_type.is_some()),
                ("_meta", text.meta.is_some()),
            ],
        ),
        ResourceContents::Blob(blob) => patch_nulls(
            &mut blob.extra,
            source,
            &[
                ("mimeType", blob.mime_type.is_some()),
                ("_meta", blob.meta.is_some()),
            ],
        ),
        // `Other` 变体本来就是原始值，`null` 已在其中。
        ResourceContents::Other(_) => {}
    }
}

/// `ContentBlock` 的显式 `null`（`annotations`/`_meta` 及其内层；`resource_link` 的
/// `title`/`description`/`mimeType`/`size`）。
fn patch_content_block_nulls(source: &Value, block: &mut ContentBlock) {
    let Some(map) = source.as_object() else {
        return;
    };
    let (extra, annotations, meta) = match block {
        ContentBlock::Text(block) => (&mut block.extra, &mut block.annotations, &mut block.meta),
        ContentBlock::Image(block) => (&mut block.extra, &mut block.annotations, &mut block.meta),
        ContentBlock::Audio(block) => (&mut block.extra, &mut block.annotations, &mut block.meta),
        ContentBlock::ResourceLink(block) => {
            patch_nulls(
                &mut block.extra,
                map,
                &[
                    ("title", block.title.is_some()),
                    ("description", block.description.is_some()),
                    ("mimeType", block.mime_type.is_some()),
                    ("size", block.size.is_some()),
                    ("annotations", block.annotations.is_some()),
                    ("_meta", block.meta.is_some()),
                ],
            );
            (&mut block.extra, &mut block.annotations, &mut block.meta)
        }
        ContentBlock::EmbeddedResource(block) => {
            patch_nulls(
                &mut block.extra,
                map,
                &[
                    ("annotations", block.annotations.is_some()),
                    ("_meta", block.meta.is_some()),
                ],
            );
            if let Some(resource) = map.get("resource").and_then(Value::as_object) {
                patch_resource_contents_nulls(resource, &mut block.resource);
            }
            (&mut block.extra, &mut block.annotations, &mut block.meta)
        }
        // `Other` 变体本来就是原始值。
        ContentBlock::Other(_) => return,
    };
    patch_nulls(
        extra,
        map,
        &[
            ("annotations", annotations.is_some()),
            ("_meta", meta.is_some()),
        ],
    );
    if let (Some(source), Some(annotations)) = (map.get("annotations"), annotations.as_mut()) {
        patch_content_annotations(source, annotations);
    }
}

/// `validateCallToolResult`。
///
/// `content` 缺省（或 `null`）时补 `[]`；`structuredContent` 可以是任意 JSON 值。
fn validate_call_tool_result(value: &Value) -> Result<CallToolResult, McpTransportError> {
    const INVALID: &str = "Invalid MCP tools/call result";
    let Some(map) = value.as_object() else {
        return Err(invalid(INVALID));
    };
    // TS 的判定是 `value.content !== undefined && !Array.isArray(value.content)`：
    // 只有**缺失**才补 `[]`；显式 `null` 同样非法。
    match map.get("content") {
        None => {}
        Some(Value::Array(_)) => {}
        Some(_) => return Err(invalid(INVALID)),
    }
    let mut result = CallToolResult::from_value(value).ok_or_else(|| invalid(INVALID))?;
    // TS 的结果是 `{...value, content: []}`（`content` 缺失时）或原样返回，显式 `null` 的
    // `isError`/`_meta` 会保留；类型化结构体读成 `None` 后 `restore_unconsumed` 跳过 `null`，
    // 这里补回 `extra` 以免丢数据（键序仍是结构体声明序，见模块文档）。内容块里的
    // `annotations: null`/`_meta: null` 同理（评审 finding 3）。
    if let Some(source) = value.as_object() {
        preserve_explicit_nulls(&mut result.extra, source, &["isError", "_meta"]);
        if let Some(Value::Array(blocks)) = source.get("content") {
            for (block, converted) in blocks.iter().zip(result.content.iter_mut()) {
                patch_content_block_nulls(block, converted);
            }
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// 消息构造
// ---------------------------------------------------------------------------

/// `{"jsonrpc":"2.0","id":..,"method":..,"params"?:..}`。
fn request_message(id: &JsonRpcId, method: &str, params: Option<&Value>) -> Value {
    let mut message = Map::new();
    message.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    message.insert(
        "id".to_owned(),
        serde_json::to_value(id).unwrap_or(Value::Null),
    );
    message.insert("method".to_owned(), Value::String(method.to_owned()));
    if let Some(params) = params {
        message.insert("params".to_owned(), params.clone());
    }
    Value::Object(message)
}

/// `{"jsonrpc":"2.0","method":..,"params"?:..}`。
fn notification_message(method: &str, params: Option<&Value>) -> Value {
    let mut message = Map::new();
    message.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    message.insert("method".to_owned(), Value::String(method.to_owned()));
    if let Some(params) = params {
        message.insert("params".to_owned(), params.clone());
    }
    Value::Object(message)
}

/// `{...params, _meta: {...(isObject(params?._meta) ? params._meta : {}), progressToken}}`。
///
/// JS 的对象展开对**非对象**值同样有定义（`client.ts:405-409`）：字符串按 UTF-16 索引展开成
/// `{"0":"a","1":"b",…}`，数组按元素索引展开成 `{"0":7,"1":8,…}`，数字/布尔/`null` 展开成空
/// 对象。只有对象字面量才有 `_meta` 可合并。
fn with_progress_token(params: Option<&Value>, token: &JsonRpcId) -> Value {
    let mut object = Map::new();
    let mut meta = Map::new();
    if let Some(params) = params {
        match params {
            Value::Object(map) => {
                object = map.clone();
                // `isObject(params?._meta)`：只有 `_meta` 本身是对象时才复制它的键。
                if let Some(Value::Object(source)) = map.get("_meta") {
                    meta = source.clone();
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    object.insert(index.to_string(), value.clone());
                }
            }
            Value::String(text) => {
                // JS 字符串的索引以 UTF-16 code unit 计；代理对被拆成两个单元，单看一个单元不是
                // 合法 UTF-8，这里用 U+FFFD 顶替（serde_json 的字符串只能是 UTF-8，已知偏差）。
                for (index, unit) in text.encode_utf16().enumerate() {
                    object.insert(
                        index.to_string(),
                        Value::String(String::from_utf16_lossy(&[unit])),
                    );
                }
            }
            _ => {}
        }
    }
    meta.insert(
        "progressToken".to_owned(),
        serde_json::to_value(token).unwrap_or(Value::Null),
    );
    object.insert("_meta".to_owned(), Value::Object(meta));
    Value::Object(object)
}

/// `String(signal.reason ?? "Aborted")`（§13.5 的逐项实测表）。
///
/// - 没有 reason（裸 `abort()` 的 `DOMException`）→ `"AbortError: This operation was aborted"`；
/// - `null` → `"Aborted"`；
/// - 其余 → `String(value)`（`""`、`"0"`、`"false"`、`"NaN"`、`"[object Object]"` …）。
pub fn abort_reason_string(reason: Option<&Value>) -> String {
    match reason {
        None => "AbortError: This operation was aborted".to_owned(),
        Some(Value::Null) => "Aborted".to_owned(),
        Some(value) => kiana_json::js_string::js_string(Some(value)),
    }
}

/// `new McpTimeoutError(timeoutMs)`（`timeoutMs` 是 f64，消息里的毫秒按 JS 数字书写）。
fn timeout_error(timeout_ms: f64) -> McpTransportError {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let millis = if timeout_ms.is_finite() && timeout_ms > 0.0 {
        timeout_ms.trunc() as u64
    } else {
        0
    };
    McpTransportError::from(McpError::Timeout { timeout_ms: millis })
}

// ---------------------------------------------------------------------------
// 构造与 getter（§13.1 / §13.2）
// ---------------------------------------------------------------------------

impl McpClient {
    /// `new McpClient(options)`。
    ///
    /// 预置两个请求 handler：`ping`（恒答 `{}`）；配置了 `roots` 时还有 `roots/list`
    /// （每次返回一份新数组）。
    pub fn new(options: McpClientOptions) -> Self {
        let roots = options.roots.clone();
        let inner = Arc::new(ClientInner {
            options,
            state: Mutex::new(ClientState {
                status: ConnectionState::Idle,
                transport: None,
                next_request_id: 1,
                server_info: None,
                server_capabilities: None,
                instructions: None,
                protocol_version: None,
                pending: HashMap::new(),
                progress_requests: HashMap::new(),
                incoming: HashMap::new(),
                request_handlers: vec![HandlerEntry {
                    method: "ping".to_owned(),
                    handler: Arc::new(|_params, _context| {
                        Box::pin(async { Ok(Value::Object(Map::new())) })
                    }),
                }],
                notification_listeners: Vec::new(),
                error_listeners: ListenerSet::new(),
                close_listeners: Vec::new(),
                disposers: Vec::new(),
            }),
        });
        if let Some(roots) = roots {
            let handler: RequestHandler = Arc::new(move |_params, _context| {
                let roots = roots.clone();
                Box::pin(async move {
                    // `[...(typeof roots === "function" ? await roots() : roots)]`：每次返回
                    // 一份新数组；提供者可以 throw（→ `-32603` 错误响应）。
                    let list = match roots {
                        Roots::List(list) => list,
                        Roots::Provider(provider) => provider().await,
                        Roots::FallibleProvider(provider) => provider().await?,
                    };
                    let mut result = Map::new();
                    result.insert(
                        "roots".to_owned(),
                        Value::Array(
                            list.iter()
                                .map(|root| serde_json::to_value(root).unwrap_or(Value::Null))
                                .collect(),
                        ),
                    );
                    Ok(Value::Object(result))
                })
            });
            inner.lock().request_handlers.push(HandlerEntry {
                method: "roots/list".to_owned(),
                handler,
            });
        }
        McpClient { inner }
    }

    /// `options`（冻结的浅拷贝；Rust 侧是不可变字段，等价）。
    pub fn options(&self) -> &McpClientOptions {
        &self.inner.options
    }

    /// `connectionState`。
    pub fn connection_state(&self) -> ConnectionState {
        self.inner.lock().status
    }

    /// `serverInfo`（连接成功后才有值）。
    pub fn server_info(&self) -> Option<Implementation> {
        self.inner.lock().server_info.clone()
    }

    /// `serverCapabilities`（连接成功后才有值；§30.5：在 `connect` 兑现前已赋值）。
    pub fn server_capabilities(&self) -> Option<ServerCapabilities> {
        self.inner.lock().server_capabilities.clone()
    }

    /// `instructions`。
    pub fn instructions(&self) -> Option<String> {
        self.inner.lock().instructions.clone()
    }

    /// `protocolVersion`（服务器选定的版本）。
    pub fn protocol_version(&self) -> Option<String> {
        self.inner.lock().protocol_version.clone()
    }
}

// ---------------------------------------------------------------------------
// connect（§13.3）
// ---------------------------------------------------------------------------

impl McpClient {
    /// `connect(transport)`：启动传输、执行 `initialize` 握手、发送 `notifications/initialized`。
    ///
    /// 只允许从 `Idle` 调用；其他状态（含 `Closed`）返回
    /// `Cannot connect MCP client in <state> state`，不会碰传输。
    ///
    /// 任何失败都会经 `close()` 把状态置为 `"closed"`（§30.8：之后不能重连）。
    ///
    /// 失败来源包括 `transport.start()`、`initialize` 请求本身、结果形状校验、服务器选了不受
    /// 支持的协议版本、`notifications/initialized` 发送失败。成功返回值就是服务器的
    /// `initialize` 结果。`server_info`/`server_capabilities`/`instructions`/
    /// `protocol_version` 在 `initialize` 结果校验与版本检查通过后即写入；若只是其后的
    /// `notifications/initialized` 发送失败，`connect` 返回 `Err`，但四个 getter 已经按结果
    /// 赋值（`instructions` 在服务器未提供时仍为 `None`）。
    pub async fn connect(
        &self,
        transport: Arc<dyn McpTransport>,
    ) -> Result<InitializeResult, McpTransportError> {
        {
            let mut state = self.inner.lock();
            if state.status != ConnectionState::Idle {
                return Err(McpTransportError::new(format!(
                    "Cannot connect MCP client in {} state",
                    state.status
                )));
            }
            state.status = ConnectionState::Connecting;
            state.transport = Some(Arc::clone(&transport));
        }

        self.install_transport_listeners(&transport);

        let result = self.initialize(&transport).await;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                let _ = self.close().await;
                Err(error)
            }
        }
    }

    /// 把三个传输监听器（消息/错误/关闭）挂到 `transport` 上，并把退订句柄存入 `disposers`。
    ///
    /// 监听器只捕获 `Weak<ClientInner>`：`ClientState.transport` 强引用传输，传输又持有这些
    /// 回调，用强引用会形成环、客户端永不释放。`Weak::upgrade` 失败（客户端已 drop）时该事件
    /// 被静默忽略。
    ///
    /// 调用顺序保证监听器先于 `transport.start()` 安装（`connect` 先调本函数，再由
    /// `initialize` 启动传输），否则启动阶段到达的消息会没人接收。
    fn install_transport_listeners(&self, transport: &Arc<dyn McpTransport>) {
        let message_client = Arc::downgrade(&self.inner);
        let message_listener = transport.on_message(Arc::new(move |message: &Value| {
            match Weak::upgrade(&message_client) {
                Some(inner) => handle_message(&inner, message),
                None => Ok(()),
            }
        }));

        let error_client = Arc::downgrade(&self.inner);
        let error_listener =
            transport.on_error(Arc::new(
                move |error: &McpTransportError| match Weak::upgrade(&error_client) {
                    Some(inner) => emit_error(&inner, error.clone()),
                    None => Ok(()),
                },
            ));

        let close_client = Arc::downgrade(&self.inner);
        let close_listener =
            transport.on_close(Arc::new(move || match Weak::upgrade(&close_client) {
                // TS 的 `handleTransportClose` 没有 try/catch：error listener 的异常会从这里
                // 冲出 `transport.emitClose()`，传给调用 `server.close()` 的一方（§30.4）。
                Some(inner) => handle_transport_close(&inner),
                None => Ok(()),
            }));

        self.inner.lock().disposers = vec![message_listener, error_listener, close_listener];
    }

    /// `connect` 的握手主体：启动传输 → 发 `initialize` → 校验结果 → 记录服务器信息 →
    /// `set_protocol_version` → 发 `notifications/initialized` → 置 `Connected`。
    ///
    /// `initialize` 请求与 `notifications/initialized` 都传 `allow_connecting = true`，因为
    /// 此刻状态仍是 `Connecting`。服务器返回的 `protocolVersion` 不在受支持版本列表里时直接
    /// 报错；此时状态仍是 `Connecting`，由 [`McpClient::connect`] 调 `close()` 收尾。
    async fn initialize(
        &self,
        transport: &Arc<dyn McpTransport>,
    ) -> Result<InitializeResult, McpTransportError> {
        transport.start().await?;

        let mut capabilities = self.inner.options.capabilities.clone().unwrap_or_default();
        // `if (this.options.roots && capabilities.roots === undefined) capabilities.roots = {}`：
        // TS 用严格 `undefined` 判断，所以配置里显式写 `roots: null` **不会**被替换成 `{}`。
        // [`ClientCapabilities`] 把 `null` 与缺省都读成 `None`，但调用方仍可通过 `extra` 里的
        // `"roots"` 键表达"显式给了值"（`#[serde(flatten)]` 会把它原样写出）。
        let roots_explicit =
            capabilities.roots.is_some() || capabilities.extra.contains_key("roots");
        let auto_added_roots = self.inner.options.roots.is_some() && !roots_explicit;
        if auto_added_roots {
            capabilities.roots = Some(ListChangedCapability::default());
        }

        let mut client_info = Implementation::new(
            self.inner.options.name.clone(),
            self.inner.options.version.clone(),
        );
        client_info.title = self.inner.options.title.clone();

        // TS 的 params 是对象字面量 `{ protocolVersion, capabilities, clientInfo }`，其中
        // `capabilities` 来自 `{ ...options.capabilities }`，自动补的 `roots` 由
        // `capabilities.roots = {}` **追加在末尾**。Rust 侧 `ClientCapabilities` 是类型化
        // 结构体，只能按声明序序列化（调用方在 TS 对象字面量里的键序无法表达，见模块文档的
        // 偏差说明）；但"自动补的 roots 排在末尾"可以逐字复刻（评审 finding 3）。
        let mut capabilities_value =
            serde_json::to_value(&capabilities).unwrap_or(Value::Object(Map::new()));
        if auto_added_roots
            && let Value::Object(map) = &mut capabilities_value
            && let Some(roots) = map.shift_remove("roots")
        {
            map.insert("roots".to_owned(), roots);
        }
        let mut params = Map::new();
        params.insert(
            "protocolVersion".to_owned(),
            Value::String(self.inner.options.protocol_version.map_or_else(
                || LATEST_PROTOCOL_VERSION.to_owned(),
                |version| version.as_str().to_owned(),
            )),
        );
        params.insert("capabilities".to_owned(), capabilities_value);
        params.insert(
            "clientInfo".to_owned(),
            serde_json::to_value(&client_info).unwrap_or(Value::Null),
        );
        let params_value = Value::Object(params);

        let result = validate_initialize_result(
            &self
                .request_internal(
                    "initialize",
                    Some(params_value),
                    &McpRequestOptions::new(),
                    true,
                )
                .await?,
        )?;
        if !is_supported_protocol_version(&result.protocol_version) {
            return Err(McpTransportError::new(format!(
                "MCP server selected unsupported protocol version {}",
                result.protocol_version
            )));
        }
        // 先完成结果校验与版本检查，再一次性写入 getter 可见的状态；校验或版本检查失败时
        // 这些字段保持初始值（`None`），连接由 `connect` 的 `close()` 收尾。写入之后的
        // `notifications/initialized` 发送若失败，`connect` 仍返回 `Err`，但 getter 已经按
        // 结果赋值且不会回滚。
        {
            let mut state = self.inner.lock();
            state.protocol_version = Some(result.protocol_version.clone());
            state.server_info = Some(result.server_info.clone());
            state.server_capabilities = Some(result.capabilities.clone());
            state.instructions = result.instructions.clone();
        }
        transport.set_protocol_version(&result.protocol_version);
        self.notify_internal("notifications/initialized", None, true)
            .await?;
        self.inner.lock().status = ConnectionState::Connected;
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// 请求 / 通知 / 注册（§13.4）
// ---------------------------------------------------------------------------

impl McpClient {
    /// `request(method, params?, options?)`（`allowConnecting = false`）。
    ///
    /// `params` 是 `Some` 时原样放进 `params` 键，`None` 时省略该键。成功返回服务器响应的
    /// `result` 原始 JSON，不做 schema 校验（类型化转换由 `call_tool`/`read_resource` 等
    /// 便捷方法负责）。
    ///
    /// 返回的错误是"已失败的 future"而不是同步 `Err`：TS 的 `request()` 只会返回 Promise
    /// （rejected），不会同步抛出。同步的 `requireTransport`/预取消检查在**调用时**执行，
    /// 因此 id 是否被消耗、请求是否已经发出都有确定语义（评审 finding 2）。
    pub fn request(
        &self,
        method: &str,
        params: Option<Value>,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<Value, McpTransportError>> + Send + use<> {
        self.request_internal(method, params, options, false)
    }

    /// `notify(method, params?)`：发送一条无 id 的通知并等待传输接受。
    ///
    /// 只等待 `transport.send` 返回，不等任何响应（通知按定义没有响应）；要求状态是
    /// `Connected`。`params` 为 `None` 时省略 `params` 键。
    pub async fn notify(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), McpTransportError> {
        self.notify_internal(method, params.as_ref(), false).await
    }

    /// `setRequestHandler(method, handler)`：返回幂等 disposer（只有当前 handler 仍是它时才删除）。
    ///
    /// 同一 method 只保留一个 handler：已存在时**原地替换**（`Vec` 中的位置不变），首次注册
    /// 才追加到末尾。被替换的旧 handler 的 disposer 随后调用不会误删新 handler（删除前用
    /// `Arc::ptr_eq` 比较）。
    pub fn set_request_handler(&self, method: &str, handler: RequestHandler) -> McpSubscription {
        let mut state = self.inner.lock();
        if let Some(entry) = state
            .request_handlers
            .iter_mut()
            .find(|entry| entry.method == method)
        {
            entry.handler = Arc::clone(&handler);
        } else {
            state.request_handlers.push(HandlerEntry {
                method: method.to_owned(),
                handler: Arc::clone(&handler),
            });
        }
        McpSubscription {
            kind: SubscriptionKind::Handler {
                method: method.to_owned(),
                handler,
            },
            inner: Arc::clone(&self.inner),
        }
    }

    /// `onNotification(method, listener)`：按 method 注册；返回移除句柄。
    ///
    /// TS 的监听器表是 `Set`：同一个函数注册两次只占一个元素，其 disposer 移除该元素。
    /// 这里按 `Arc::ptr_eq` 复刻同一语义。disposer 还复刻了 TS 的**陈旧闭包**行为：
    /// 集合被删空后重建是新的 `Set`，旧 disposer 仍会（在它闭包里的空集合上）触发
    /// `notificationListeners.delete(method)`，从而清掉新注册的监听器（§30.3 的实测行为）。
    pub fn on_notification(&self, method: &str, listener: NotificationListener) -> McpSubscription {
        let mut state = self.inner.lock();
        let set = match state
            .notification_listeners
            .iter()
            .find(|group| group.method == method)
        {
            Some(group) => Arc::clone(&group.listeners),
            None => {
                let set: Arc<Mutex<ListenerSet<NotificationListener>>> =
                    Arc::new(Mutex::new(ListenerSet::new()));
                state.notification_listeners.push(NotificationGroup {
                    method: method.to_owned(),
                    listeners: Arc::clone(&set),
                });
                set
            }
        };
        set.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add(Arc::clone(&listener), Arc::ptr_eq);
        McpSubscription {
            kind: SubscriptionKind::Notification {
                method: method.to_owned(),
                listener,
                set,
            },
            inner: Arc::clone(&self.inner),
        }
    }

    /// `onError(listener)`（`Set` 语义：同一个函数只注册一次）。
    pub fn on_error(&self, listener: ErrorListener) -> McpSubscription {
        let mut state = self.inner.lock();
        state
            .error_listeners
            .add(Arc::clone(&listener), Arc::ptr_eq);
        McpSubscription {
            kind: SubscriptionKind::Error(listener),
            inner: Arc::clone(&self.inner),
        }
    }

    /// `onClose(listener)`：连接关闭时调用一次（传输掉落或 `close()` 都算）。
    ///
    /// `Set` 语义同 [`McpClient::on_error`]。
    pub fn on_close(&self, listener: CloseListener) -> McpSubscription {
        let mut state = self.inner.lock();
        if !state
            .close_listeners
            .iter()
            .any(|entry| Arc::ptr_eq(entry, &listener))
        {
            state.close_listeners.push(Arc::clone(&listener));
        }
        McpSubscription {
            kind: SubscriptionKind::Close(listener),
            inner: Arc::clone(&self.inner),
        }
    }

    /// `ping(options?)`。
    ///
    /// 与 TS 一样，请求在**调用时**就发出（`async ping()` 的函数体在第一个 `await` 之前同步
    /// 运行），因此返回的 future 即使立刻被 drop，请求也已经计入 pending。
    pub fn ping(
        &self,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<(), McpTransportError>> + Send + use<> {
        let started = self.start_request("ping", None, options, false);
        async move {
            match started {
                Ok(receiver) => await_pending(receiver).await.map(|_| ()),
                Err(error) => Err(error),
            }
        }
    }

    /// `listTools(options?)`：跟随 `nextCursor` 取完全部分页。
    ///
    /// 元素先按 `isTool`（`name` 字符串 + `inputSchema` 对象）校验，再转成 [`Tool`]。页数
    /// 上限 [`MAX_LIST_PAGES`]；同一 cursor 第二次出现会报
    /// `MCP tools/list returned duplicate cursor: <cursor>`，超过上限报
    /// `MCP tools/list exceeded 1000 pages`。
    pub fn list_tools(
        &self,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<Vec<Tool>, McpTransportError>> + Send + use<> {
        let first = self.start_list_page("tools/list", "tools", is_tool, None, options);
        let client = self.clone();
        let options = options.clone();
        async move {
            let items =
                drive_list_all(&client, "tools/list", "tools", is_tool, &options, first).await?;
            Ok(items.iter().map(to_tool).collect())
        }
    }

    /// `listResources(options?)`：全部资源，`name` 缺失时回退为 `uri`。
    ///
    /// 分页与校验规则同 [`McpClient::list_tools`]（元素按 `isResource`：`uri` 字符串，
    /// `name` 缺省或字符串）。返回类型化 [`Resource`]，服务端的未知键进 `extra`；键序偏差见
    /// 模块文档。
    pub fn list_resources(
        &self,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<Vec<Resource>, McpTransportError>> + Send + use<> {
        let first = self.start_list_page("resources/list", "resources", is_resource, None, options);
        let client = self.clone();
        let options = options.clone();
        async move {
            let items = drive_list_all(
                &client,
                "resources/list",
                "resources",
                is_resource,
                &options,
                first,
            )
            .await?;
            Ok(items.iter().map(to_resource).collect())
        }
    }

    /// 单页资源（从 `cursor` 开始）。
    ///
    /// 只发一次 `resources/list`，不跟随 `nextCursor`；`next_cursor` 原样返回（`null` 与空串
    /// 归一为 `None`），由调用方决定是否继续翻页。顶层的 `_meta` 不会保留：结果结构体的
    /// `meta`/`extra` 固定为空（与列表聚合路径的行为一致）。
    pub fn list_resources_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<ListResourcesResult, McpTransportError>> + Send + use<> {
        let first =
            self.start_list_page("resources/list", "resources", is_resource, cursor, options);
        async move {
            let page = drive_list_page("resources/list", "resources", is_resource, first).await?;
            Ok(ListResourcesResult {
                resources: page.items.iter().map(to_resource).collect(),
                next_cursor: page.next_cursor,
                meta: None,
                extra: Map::new(),
            })
        }
    }

    /// `listResourceTemplates(options?)`：全部模板。
    ///
    /// 分页与校验规则同 [`McpClient::list_tools`]（元素按 `isResourceTemplate`：
    /// `uriTemplate` 字符串，`name` 缺省或字符串）。返回类型化 [`ResourceTemplate`]，未知键
    /// 进 `extra`。
    pub fn list_resource_templates(
        &self,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<Vec<ResourceTemplate>, McpTransportError>> + Send + use<> {
        let first = self.start_list_page(
            "resources/templates/list",
            "resourceTemplates",
            is_resource_template,
            None,
            options,
        );
        let client = self.clone();
        let options = options.clone();
        async move {
            let items = drive_list_all(
                &client,
                "resources/templates/list",
                "resourceTemplates",
                is_resource_template,
                &options,
                first,
            )
            .await?;
            Ok(items.iter().map(to_resource_template).collect())
        }
    }

    /// 单页模板（从 `cursor` 开始）。
    ///
    /// 只发一次 `resources/templates/list`；`next_cursor` 归一规则与
    /// [`McpClient::list_resources_page`] 相同，顶层 `_meta` 同样不保留。
    pub fn list_resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpTransportError>> + Send + use<>
    {
        let first = self.start_list_page(
            "resources/templates/list",
            "resourceTemplates",
            is_resource_template,
            cursor,
            options,
        );
        async move {
            let page = drive_list_page(
                "resources/templates/list",
                "resourceTemplates",
                is_resource_template,
                first,
            )
            .await?;
            Ok(ListResourceTemplatesResult {
                resource_templates: page.items.iter().map(to_resource_template).collect(),
                next_cursor: page.next_cursor,
                meta: None,
                extra: Map::new(),
            })
        }
    }

    /// `listPrompts(options?)`：跟随 `nextCursor` 取完全部分页。
    ///
    /// 元素按 `is_prompt`（`name` 字符串）校验；分页上限与重复 cursor 规则同
    /// [`McpClient::list_tools`]。
    pub fn list_prompts(
        &self,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<Vec<Prompt>, McpTransportError>> + Send + use<> {
        let first = self.start_list_page("prompts/list", "prompts", is_prompt, None, options);
        let client = self.clone();
        let options = options.clone();
        async move {
            let items = drive_list_all(
                &client,
                "prompts/list",
                "prompts",
                is_prompt,
                &options,
                first,
            )
            .await?;
            Ok(items.iter().map(to_prompt).collect())
        }
    }

    /// 单页 prompt（从 `cursor` 开始）。只发一次 `prompts/list`。
    pub fn list_prompts_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<ListPromptsResult, McpTransportError>> + Send + use<> {
        let first = self.start_list_page("prompts/list", "prompts", is_prompt, cursor, options);
        async move {
            let page = drive_list_page("prompts/list", "prompts", is_prompt, first).await?;
            Ok(ListPromptsResult {
                prompts: page.items.iter().map(to_prompt).collect(),
                next_cursor: page.next_cursor,
                meta: None,
                extra: Map::new(),
            })
        }
    }

    /// `getPrompt(name, args?, options?)`。
    ///
    /// `params` 是 `{name, arguments?}`（与 `tools/call` 同形，`args: None` 时省略键）。
    /// 结果必须是对象、`messages` 是数组，每条消息是对象、`role` 为 `user`/`assistant`
    /// 且含 `content`；否则返回 `Invalid MCP prompts/get result` 或
    /// `Invalid message in MCP prompts/get result`（`McpError`，code `-32600`）。
    pub fn get_prompt(
        &self,
        name: &str,
        args: Option<Map<String, Value>>,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<GetPromptResult, McpTransportError>> + Send + use<> {
        let mut params = Map::new();
        params.insert("name".to_owned(), Value::String(name.to_owned()));
        if let Some(args) = args {
            params.insert("arguments".to_owned(), Value::Object(args));
        }
        let started =
            self.start_request("prompts/get", Some(Value::Object(params)), options, false);
        async move {
            let value = match started {
                Ok(receiver) => await_pending(receiver).await?,
                Err(error) => return Err(error),
            };
            validate_get_prompt_result(&value)
        }
    }

    /// `readResource(uri, options?)`。
    ///
    /// `params` 只有 `uri`。结果经校验：`contents` 必须是数组，每项是对象且 `uri` 为字符串、
    /// `text` 或 `blob` 至少一个是字符串；否则返回 `Invalid MCP resources/read result` 或
    /// `Invalid contents in MCP resources/read result`（`McpError`，code `-32600`）。
    pub fn read_resource(
        &self,
        uri: &str,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<ReadResourceResult, McpTransportError>> + Send + use<> {
        let mut params = Map::new();
        params.insert("uri".to_owned(), Value::String(uri.to_owned()));
        let started = self.start_request(
            "resources/read",
            Some(Value::Object(params)),
            options,
            false,
        );
        async move {
            let value = match started {
                Ok(receiver) => await_pending(receiver).await?,
                Err(error) => return Err(error),
            };
            validate_read_resource_result(&value)
        }
    }

    /// `callTool(name, args?, options?)`。
    ///
    /// `params` 是 `{name, arguments?}`：`args: None` 时省略 `arguments` 键（TS 的
    /// `callTool(name, undefined)`）；显式 `null` 参数无法表达（见模块文档）。
    /// `content` 缺省时结果补 `[]`；显式 `content: null` 视为非法
    /// （`Invalid MCP tools/call result`）。
    pub fn call_tool(
        &self,
        name: &str,
        args: Option<Map<String, Value>>,
        options: &McpRequestOptions,
    ) -> impl Future<Output = Result<CallToolResult, McpTransportError>> + Send + use<> {
        let mut params = Map::new();
        params.insert("name".to_owned(), Value::String(name.to_owned()));
        if let Some(args) = args {
            params.insert("arguments".to_owned(), Value::Object(args));
        }
        let started = self.start_request("tools/call", Some(Value::Object(params)), options, false);
        async move {
            let value = match started {
                Ok(receiver) => await_pending(receiver).await?,
                Err(error) => return Err(error),
            };
            validate_call_tool_result(&value)
        }
    }

    /// `close()`：脱离传输、退订监听器、拒绝所有 pending 请求，然后关闭传输。幂等。
    ///
    /// TS 的 `markClosed` 之后才 `await transport?.close()`，且 `markClosed` 里 error listener
    /// 抛出的异常**不会**被吞掉——它会在 `await transport.close()` **之前**冲出 `close()`。
    /// 这里同样处理（§30.4）。
    ///
    /// 幂等：第二次调用时 `transport` 已被取走，`mark_closed` 因 `was_closed` 提前返回，
    /// close 监听器不会再触发（测试 `close_is_idempotent`）。返回错误有两个来源：close
    /// 监听器抛错后某个 error 监听器又抛错（此时剩余 close 监听器与 `transport.close()` 都会
    /// 被跳过），以及 `transport.close()` 自身的失败。
    pub async fn close(&self) -> Result<(), McpTransportError> {
        let transport = {
            let mut state = self.inner.lock();
            state.transport.take()
        };
        self.dispose_transport_listeners();
        mark_closed(
            &self.inner,
            McpTransportError::from(McpError::connection_closed()),
        )?;
        match transport {
            Some(transport) => transport.close().await,
            None => Ok(()),
        }
    }

    /// `listPage(...)` 的第一页：请求在**调用时**发出（见 [`McpClient::start_request`]）。
    ///
    /// 只构造 `params.cursor`（`cursor: None` 时省略整个 `params`）。`key`/`is_item` 在这个
    /// 函数里不参与逻辑（`let _ = (key, is_item)`），元素校验发生在 `drive_list_page` 中。
    fn start_list_page(
        &self,
        method: &str,
        key: &str,
        is_item: ItemValidator,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> StartedPage {
        let params = cursor.map(|cursor| {
            let mut params = Map::new();
            params.insert("cursor".to_owned(), Value::String(cursor.to_owned()));
            Value::Object(params)
        });
        let _ = (key, is_item);
        StartedPage {
            request: self.start_request(method, params, options, false),
        }
    }

    /// `requireTransport(allowConnecting)`：返回当前传输的克隆，或
    /// `McpConnectionClosedError("MCP client is <state>")`。
    ///
    /// `allow_connecting` 只供握手内部使用：`initialize` 与 `notifications/initialized` 必须
    /// 在状态仍是 `Connecting` 时发出；公开 API 一律传 `false`。状态名来自
    /// [`ConnectionState::as_str`]，错误消息逐字与 TS 对齐（测试
    /// `require_transport_messages_match_the_typescript_vectors`）。
    fn require_transport(
        &self,
        allow_connecting: bool,
    ) -> Result<Arc<dyn McpTransport>, McpTransportError> {
        let state = self.inner.lock();
        if let Some(transport) = &state.transport
            && (state.status == ConnectionState::Connected
                || (allow_connecting && state.status == ConnectionState::Connecting))
        {
            return Ok(Arc::clone(transport));
        }
        Err(McpTransportError::from(McpError::connection_closed_with(
            format!("MCP client is {}", state.status),
        )))
    }

    /// `requireTransport` + 发送一条无 id 的通知；`transport.send` 的返回值直接透出。
    async fn notify_internal(
        &self,
        method: &str,
        params: Option<&Value>,
        allow_connecting: bool,
    ) -> Result<(), McpTransportError> {
        let transport = self.require_transport(allow_connecting)?;
        transport
            .send(
                notification_message(method, params),
                McpTransportSendOptions::none(),
            )
            .await
    }

    /// `requestInternal` 的**同步部分**：`requireTransport`、预取消检查、分配 id、注册 pending
    /// 条目、`armTimeout`，最后 spawn 等待部分。
    ///
    /// 它必须同步执行：TS 的 `requestInternal` 是 `async` 函数，但函数体在**第一个 `await`
    /// 之前**（也就是 `new Promise(...)` 之前）全部同步运行，`new Promise` 的 executor 同样
    /// 同步运行。因此 `client.callTool(...)` 在返回 Promise 之前就已经占用了 id、登记了 pending、
    /// 设好定时器并调用了 `transport.send`。如果 Rust 把这些留到首次 poll，调用方在同一个 tick
    /// 里取消信号时请求根本不会被发出（评审 finding 2：TS 发 `tools/call` + `cancelled`，Rust
    /// 只回 `AbortError`）。
    ///
    /// 失败（`requireTransport` 抛错 / 信号已取消）在这里同步返回 `Err`；公开的 `request`
    /// 把它包成一个"已失败的 future"，与 TS 的 rejected Promise 等价。
    #[allow(clippy::too_many_lines)]
    fn start_request(
        &self,
        method: &str,
        params: Option<Value>,
        options: &McpRequestOptions,
        allow_connecting: bool,
    ) -> Result<oneshot::Receiver<Result<Value, McpTransportError>>, McpTransportError> {
        let transport = self.require_transport(allow_connecting)?;
        if options
            .signal
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(McpTransportError::from(McpError::aborted()));
        }
        let id = {
            let mut state = self.inner.lock();
            let id = state.next_request_id;
            state.next_request_id += 1;
            id
        };
        // JS 的数字是 f64；id 从 1 开始，序列化时按整数写出。
        #[allow(clippy::cast_precision_loss)]
        let jsonrpc_id = JsonRpcId::Number(id as f64);
        let progress_token = options.on_progress.as_ref().map(|_| jsonrpc_id.clone());
        let request_params = match &progress_token {
            None => params,
            Some(token) => Some(with_progress_token(params.as_ref(), token)),
        };
        let message = request_message(&jsonrpc_id, method, request_params.as_ref());
        let timeout_ms = options
            .timeout_ms
            .or(self.inner.options.request_timeout_ms)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
        let cancellable = method != "initialize";
        let (sender, receiver) = oneshot::channel();
        let timer = Arc::new(TimerSlot::new());
        let send_signal = CancellationToken::new();
        {
            let mut state = self.inner.lock();
            state.pending.insert(
                jsonrpc_id.clone(),
                PendingEntry {
                    sender: Some(sender),
                    timeout_ms,
                    timer: Arc::clone(&timer),
                    cancellable,
                    on_progress: options.on_progress.clone(),
                    progress_token: progress_token.clone(),
                    send_signal: send_signal.clone(),
                },
            );
            if let Some(token) = &progress_token {
                state
                    .progress_requests
                    .insert(token.clone(), jsonrpc_id.clone());
            }
        }
        // `armTimeout` 同样是同步的。
        timer.set(timeout_deadline(timeout_ms));

        let (result_sender, result_receiver) = oneshot::channel();
        let driver_inner = Arc::clone(&self.inner);
        let driver = PendingDriver {
            inner: driver_inner,
            id: jsonrpc_id,
            timeout_ms,
            cancellable,
            signal: options.signal.clone(),
            signal_reason: options.signal_reason.clone(),
            timer,
            receiver,
        };
        // `transport.send(...)` 的 future 由这个 task **持有到完成**：结算（响应先到、超时、
        // 取消、连接关闭）只取消 `send_signal`，不会撤销已经派发的发送（评审 finding 2）。
        // TS 的 `transport.send(...).catch(...)` 同样不因 `cancelPending` 而丢弃 promise：
        // 传输可以继续读迟到的答复并发 `emitMessage`（此时 pending 已删除 →
        // `Received response for unknown MCP request <id>`）。drop 掉 future 会中止
        // StreamableHttpTransport 里 reqwest 的交换（连接被关）。
        tokio::spawn(async move {
            let mut send_future: Option<SendFuture<'_>> = Some(Box::pin(transport.send(
                message,
                McpTransportSendOptions::with_signal(Some(send_signal)),
            )));
            let outcome = driver.run(&mut send_future).await;
            // 先把结果交给调用方，再继续把已派发的发送跑完（不阻塞调用方）。
            let _ = result_sender.send(outcome);
            if let Some(send_future) = send_future {
                // 与 `.catch((error) => this.cancelPending(id, error, false))` 一致：
                // 结算后的失败是 no-op（条目已不在 pending 里）。
                let _ = send_future.await;
            }
        });
        Ok(result_receiver)
    }

    /// 等待 `start_request` 的等待部分。
    ///
    /// 等待循环在**独立 task** 里运行：调用方 drop 掉 `request()` 的 future 不影响它，
    /// 超时/取消/发送失败仍会结算 pending 条目（§13.8）。
    fn request_internal(
        &self,
        method: &str,
        params: Option<Value>,
        options: &McpRequestOptions,
        allow_connecting: bool,
    ) -> impl Future<Output = Result<Value, McpTransportError>> + Send + use<> {
        let started = self.start_request(method, params, options, allow_connecting);
        async move {
            match started {
                Ok(receiver) => await_pending(receiver).await,
                Err(error) => Err(error),
            }
        }
    }

    /// 取走 `disposers` 并逐个退订传输监听器（只在 `close()` 里调用）。
    ///
    /// 先在锁内 `take`、出锁后再退订，避免持着客户端锁去执行传输侧的代码；`disposers` 已
    /// 清空，所以重复调用是 no-op。
    fn dispose_transport_listeners(&self) {
        let disposers = {
            let mut state = self.inner.lock();
            std::mem::take(&mut state.disposers)
        };
        for disposer in disposers {
            disposer.unsubscribe();
        }
    }
}

/// 等 `start_request` 的驱动 task 给出结果。
///
/// 驱动 task 消失（运行时关闭）等价于连接关闭：TS 里 Promise 的 executor 不会凭空消失，
/// 这里给一个与 `markClosed` 相同的错误，避免调用方永远挂起。
async fn await_pending(
    receiver: oneshot::Receiver<Result<Value, McpTransportError>>,
) -> Result<Value, McpTransportError> {
    match receiver.await {
        Ok(outcome) => outcome,
        Err(_) => Err(McpTransportError::from(McpError::connection_closed())),
    }
}

/// 一次请求的等待部分（对应 TS `new Promise((resolve, reject) => {...})` 里的那些监听）。
///
/// 它在独立 task 里运行：调用方 drop 掉 `request()` 的 future 不影响它，超时/取消/发送失败
/// 仍会结算 pending 条目（§13.8）。结果通过 oneshot 送回给调用方；没人接收也无妨。
///
/// `transport.send(...)` 的 future 由**调用方**（spawn 的 task）持有并通过 `&mut` 传入：
/// 结算时不能 drop 它（见 [`McpClient::start_request`] 的注释）。
struct PendingDriver {
    inner: Arc<ClientInner>,
    id: JsonRpcId,
    timeout_ms: f64,
    cancellable: bool,
    signal: Option<CancellationToken>,
    signal_reason: Option<Value>,
    timer: Arc<TimerSlot>,
    receiver: oneshot::Receiver<Result<Value, McpTransportError>>,
}

impl PendingDriver {
    /// 等待循环：五个分支里谁先结算这个请求。
    ///
    /// `biased` 固定了轮询顺序，因此同一时刻有多个分支就绪时优先级是：
    ///
    /// 1. `receiver`（响应或 `mark_closed` 已结算）→ 直接返回；
    /// 2. `transport.send` 完成且失败 → `cancel_pending(..., notify = false)` 结算，然后继续
    ///    循环等 `receiver` 取回错误；
    /// 3. `signal` 取消 → 把 `signal_reason` 转成字符串、按需发 `notifications/cancelled`；
    ///    只有条目仍在 pending（`cancel_pending` 返回 `true`）才立刻返回，否则继续等
    ///    `receiver`；
    /// 4. 定时器槽变化（进度续期或结算）→ 重新读截止时刻；
    /// 5. 截止时刻到达 → 超时结算；被别的路径先结算时同样继续等 `receiver`。
    ///
    /// `send_future` 由调用方持有并在本函数返回后继续跑完：结算只取消 `send_signal`，不会
    /// drop 已经派发的发送（见 [`McpClient::start_request`] 的注释）。
    async fn run(
        self,
        send_future: &mut Option<SendFuture<'_>>,
    ) -> Result<Value, McpTransportError> {
        let PendingDriver {
            inner,
            id,
            timeout_ms,
            cancellable,
            signal,
            signal_reason,
            timer,
            mut receiver,
        } = self;
        let mut deadline_rx = timer.subscribe();
        let abort_token = signal;

        loop {
            let current_deadline = *deadline_rx.borrow_and_update();
            let sleep = async {
                match current_deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(sleep);
            let send_branch = async {
                match send_future.as_mut() {
                    Some(future) => future.await,
                    None => std::future::pending::<Result<(), McpTransportError>>().await,
                }
            };
            tokio::pin!(send_branch);
            let abort_branch = async {
                match &abort_token {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                biased;
                result = &mut receiver => {
                    return match result {
                        Ok(outcome) => outcome,
                        // 发送端被丢弃（`markClosed` 已 drain）：等价于连接关闭。
                        Err(_) => Err(McpTransportError::from(McpError::connection_closed())),
                    };
                }
                outcome = &mut send_branch => {
                    *send_future = None;
                    if let Err(error) = outcome {
                        cancel_pending(&inner, &id, error, false, None);
                    }
                }
                () = abort_branch => {
                    let reason = abort_reason_string(signal_reason.as_ref());
                    let error = McpTransportError::from(McpError::aborted());
                    let settled = cancel_pending(
                        &inner,
                        &id,
                        error.clone(),
                        cancellable,
                        Some(&reason),
                    );
                    if settled {
                        return Err(error);
                    }
                    // 请求已经结算（例如响应先到）：继续等 receiver 给出结果。
                }
                changed = deadline_rx.changed() => {
                    if changed.is_err() {
                        // 定时器槽已丢弃（请求结算）：继续等 receiver 的结果。
                        deadline_rx = timer.subscribe();
                    }
                }
                () = &mut sleep => {
                    // `entry.cancellable`：进度通知可能已经把条目换掉，这里读当前值。
                    let notify = inner
                        .lock()
                        .pending
                        .get(&id)
                        .is_some_and(|entry| entry.cancellable);
                    let error = timeout_error(timeout_ms);
                    let settled = cancel_pending(
                        &inner,
                        &id,
                        error.clone(),
                        notify,
                        Some("Request timed out"),
                    );
                    if settled {
                        return Err(error);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 消息处理（§13.7）
// ---------------------------------------------------------------------------

/// 把 `timeout_ms` 变成截止时刻（`None` = 不设定时器）。
///
/// `!Number.isFinite(ms) || ms <= 0` → 无定时器（`armTimeout` 的检查）。
///
/// 其余值按 Node `setTimeout` 的两个实测规则换算（Node 26）：
/// - `Timeout` 构造函数：`!(after >= 1 && after <= 2^31-1)` → 改成 **1 ms**。所以
///   `0 < ms < 1` 也会等 1 ms，而 `ms > 2^31-1` 会在 1 ms 后触发（§13.7 的钳位）；
/// - `insert()`：`msecs = MathTrunc(msecs)`——**向下截断**到整毫秒（不是四舍五入/向上取整；
///   实测 `setTimeout(fn, 5.9)` 与 `setTimeout(fn, 5)` 的触发时间分布相同）。
///
/// 因此 `timeoutMs = 5.5` 在 TS 里约 5 ms 后超时，这里同样按 5 ms 调度。
fn timeout_duration(timeout_ms: f64) -> Option<Duration> {
    if !timeout_ms.is_finite() || timeout_ms <= 0.0 {
        return None;
    }
    let effective = if timeout_ms < 1.0 || timeout_ms > MAX_TIMER_DELAY_MS {
        1.0
    } else {
        timeout_ms.trunc()
    };
    Some(Duration::from_secs_f64(effective / 1000.0))
}

fn timeout_deadline(timeout_ms: f64) -> Option<tokio::time::Instant> {
    timeout_duration(timeout_ms).map(|duration| tokio::time::Instant::now() + duration)
}

/// `handleMessage(message)`：response → request → notification → 非法。
fn handle_message(inner: &Arc<ClientInner>, message: &Value) -> Result<(), McpTransportError> {
    match classify_json_rpc_message(message) {
        Some(crate::jsonrpc::JsonRpcClassification::Response) => handle_response(inner, message),
        Some(crate::jsonrpc::JsonRpcClassification::Request) => {
            // TS 的 `void this.handleRequest(message)` 在第一个 `await` 之前**同步**执行：
            // 查 handler、`incoming.set(...)` 都是同步的。这里把同步前奏做完（见
            // [`begin_request`]），只有真正的等待部分放进 task，因此紧随其后的
            // `notifications/cancelled` 仍能取消它（§30.30）。
            let inner = Arc::clone(inner);
            let Some(started) = begin_request(&inner, message) else {
                return Ok(());
            };
            tokio::spawn(async move {
                let _ = finish_request(&inner, started).await;
            });
            Ok(())
        }
        Some(crate::jsonrpc::JsonRpcClassification::Notification) => {
            let method = message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            handle_notification(inner, &method, message.get("params"))
        }
        None => emit_error(
            inner,
            McpTransportError::from(McpError::new(
                crate::jsonrpc::JsonRpcErrorCodes::INVALID_REQUEST,
                "Received invalid JSON-RPC message",
            )),
        ),
    }
}

/// `handleResponse(message)`：解析响应、按 id 取走 pending 条目并结算。
///
/// 无法解析成响应 → `emit_error` 报 `-32600 Received invalid JSON-RPC message`。id 不在
/// `pending` 里（迟到的答复、重复响应，或该请求已被超时/取消先结算）→ 报
/// `Received response for unknown MCP request <id>`。`emit_error` 自身的错误（error listener
/// 抛出）会作为返回值向上传播到传输的消息派发点。
fn handle_response(inner: &Arc<ClientInner>, message: &Value) -> Result<(), McpTransportError> {
    let typed = match parse_json_rpc_message_typed(message) {
        Ok(JsonRpcMessage::Response(response)) => response,
        _ => {
            return emit_error(
                inner,
                McpTransportError::from(McpError::new(
                    crate::jsonrpc::JsonRpcErrorCodes::INVALID_REQUEST,
                    "Received invalid JSON-RPC message",
                )),
            );
        }
    };
    let (id, outcome) = response_outcome(typed);
    let entry = {
        let mut state = inner.lock();
        state.pending.remove(&id)
    };
    let Some(entry) = entry else {
        return emit_error(
            inner,
            McpTransportError::new(format!("Received response for unknown MCP request {id}")),
        );
    };
    remove_pending(inner, &id, entry, outcome);
    Ok(())
}

/// response → `(id, 结算值)`。
fn response_outcome(response: JsonRpcResponse) -> (JsonRpcId, Result<Value, McpTransportError>) {
    match response {
        JsonRpcResponse::Success(success) => (success.id, Ok(success.result)),
        JsonRpcResponse::Error(error) => {
            let data = error.error.data.clone();
            // 冻结契约里 `McpError.code` 是 `i64`，而 JSON-RPC 的 code 在 TS 里是 f64；
            // 非整数 code 在这里被截断（见模块文档的已知偏差）。
            #[allow(clippy::cast_possible_truncation)]
            let code = error.error.code as i64;
            let mut mcp = McpError::new(code, error.error.message);
            if let (McpError::JsonRpc { data: slot, .. }, Some(data)) = (&mut mcp, data) {
                *slot = Some(data);
            }
            (error.id, Err(McpTransportError::from(mcp)))
        }
    }
}

/// `removePending(id, entry)` + 结算。
///
/// 先清掉 `progress_requests` 里对应的令牌，再停掉定时器（`timer.set(None)`：正在等待的
/// driver 会被 watch 唤醒并重新读取截止时刻，读到 `None` 后不再计时，最终由 oneshot 的
/// `outcome` 结算；只有 watch sender 被丢弃时它才会重新 `subscribe()`——driver 自己持有
/// `TimerSlot` 的 `Arc`，因此等待期间该分支不会发生）、取消 `send_signal`（传输此后不应再
/// 重试或继续派发这条消息），最后通过 oneshot 把 `outcome` 交给调用方。条目已不在表里时
/// 重复调用也安全。
fn remove_pending(
    inner: &Arc<ClientInner>,
    id: &JsonRpcId,
    mut entry: PendingEntry,
    outcome: Result<Value, McpTransportError>,
) {
    {
        let mut state = inner.lock();
        state.pending.remove(id);
        if let Some(token) = &entry.progress_token {
            state.progress_requests.remove(token);
        }
    }
    entry.timer.set(None);
    entry.send_signal.cancel();
    if let Some(sender) = entry.sender.take() {
        let _ = sender.send(outcome);
    }
}

/// `cancelPending(id, error, notifyServer, reason?)`。
///
/// 返回 `true` 表示这次调用真的结算了一个仍在 `pending` 的条目（调用方据此提前返回）；
/// `false` 表示它已被响应、超时或其他取消先结算。
///
/// `notify_server` 为真时**后台**发送 `notifications/cancelled`：`params` 是
/// `{requestId, reason?}`，`reason` 为空字符串时省略该键。发送失败交给 `emit_error`；由于
/// 发生在后台 task 里，error listener 再抛错也只会被丢弃。
fn cancel_pending(
    inner: &Arc<ClientInner>,
    id: &JsonRpcId,
    error: McpTransportError,
    notify_server: bool,
    reason: Option<&str>,
) -> bool {
    let entry = {
        let mut state = inner.lock();
        state.pending.remove(id)
    };
    let Some(entry) = entry else {
        return false;
    };
    let transport = inner.lock().transport.clone();
    remove_pending(inner, id, entry, Err(error));
    if let (true, Some(transport)) = (notify_server, transport) {
        let mut params = Map::new();
        params.insert(
            "requestId".to_owned(),
            serde_json::to_value(id).unwrap_or(Value::Null),
        );
        // `...(reason ? {reason} : {})`：空字符串也会省略 `reason` 键。
        if let Some(reason) = reason.filter(|reason| !reason.is_empty()) {
            params.insert("reason".to_owned(), Value::String(reason.to_owned()));
        }
        let message = notification_message("notifications/cancelled", Some(&Value::Object(params)));
        // `void transport.send(...).catch((sendError) => this.emitError(sendError))`：
        // 发送失败必须交给 error listener（§13.7）。这是后台任务，listener 再抛也无处可传，
        // 与 TS 的未处理 rejection 等价，这里丢弃。
        let error_inner = Arc::clone(inner);
        tokio::spawn(async move {
            if let Err(send_error) = transport
                .send(message, McpTransportSendOptions::none())
                .await
            {
                let _ = emit_error(&error_inner, send_error);
            }
        });
    }
    true
}

/// `handleRequest` 的同步前奏（TS 在第一个 `await` 之前执行的部分）。
struct StartedRequest {
    transport: Arc<dyn McpTransport>,
    id: JsonRpcId,
    /// 有 handler 时的处理函数与上下文；`None` 表示要回 `-32601`。
    handler: Option<(RequestHandler, RequestContext)>,
    params: Option<Value>,
    method: String,
}

/// TS 的 `handleRequest` 在第一个 `await` 之前的部分：查 handler、`incoming.set(...)`。
///
/// 返回 `None` 表示无需处理（没有 transport，或消息没有合法 id）。
fn begin_request(inner: &Arc<ClientInner>, message: &Value) -> Option<StartedRequest> {
    let transport = inner.lock().transport.clone()?;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let id = message.get("id").and_then(value_to_json_rpc_id)?;
    let handler = {
        let state = inner.lock();
        state
            .request_handlers
            .iter()
            .find(|entry| entry.method == method)
            .map(|entry| Arc::clone(&entry.handler))
    };
    let params = message.get("params").cloned();
    let handler = handler.map(|handler| {
        let controller = CancellationToken::new();
        let reason = Arc::new(Mutex::new(None));
        inner.lock().incoming.insert(
            id.clone(),
            IncomingEntry {
                token: controller.clone(),
                reason: Arc::clone(&reason),
            },
        );
        (
            handler,
            RequestContext {
                signal: controller,
                reason,
            },
        )
    });
    Some(StartedRequest {
        transport,
        id,
        handler,
        params,
        method,
    })
}

/// `handleRequest(message)` 的等待部分（服务端 → 客户端）。
async fn finish_request(
    inner: &Arc<ClientInner>,
    started: StartedRequest,
) -> Result<(), McpTransportError> {
    let StartedRequest {
        transport,
        id,
        handler,
        params,
        method,
    } = started;
    let Some((handler, context)) = handler else {
        let error = json_object([
            (
                "code",
                Value::Number(serde_json::Number::from(
                    crate::jsonrpc::JsonRpcErrorCodes::METHOD_NOT_FOUND,
                )),
            ),
            (
                "message",
                Value::String(format!("Method not found: {method}")),
            ),
        ]);
        let response = json_object([
            ("jsonrpc", Value::String("2.0".to_owned())),
            ("id", id_value(&id)),
            ("error", error),
        ]);
        return send_or_emit(inner, &transport, response).await;
    };

    // TS 的结构是 `try { await handler; await send(success) } catch { await send(error) }
    // finally { this.incoming.delete(id) }`：**成功响应发送失败**也会落进 catch（服务端会收到
    // **两条** 响应），而 `incoming` 条目在 try 与 catch 都结束后才删除。
    let outcome = handler(params.as_ref(), context).await;
    let finished = match outcome {
        Ok(value) => {
            // `result ?? {}`：nullish 合并，`undefined` 与 `null` 都替换成 `{}`。
            let result = if value.is_null() {
                Value::Object(Map::new())
            } else {
                value
            };
            let success = json_object([
                ("jsonrpc", Value::String("2.0".to_owned())),
                ("id", id_value(&id)),
                ("result", result),
            ]);
            match transport
                .send(success, McpTransportSendOptions::none())
                .await
            {
                Ok(()) => Ok(()),
                Err(send_error) => {
                    let response = error_response(&id, &send_error);
                    match transport
                        .send(response, McpTransportSendOptions::none())
                        .await
                    {
                        Ok(()) => Ok(()),
                        Err(send_error) => emit_error(inner, send_error),
                    }
                }
            }
        }
        // `await transport.send(...error...).catch((sendError) => this.emitError(sendError))`
        Err(error) => {
            let response = error_response(&id, &error);
            match transport
                .send(response, McpTransportSendOptions::none())
                .await
            {
                Ok(()) => Ok(()),
                Err(send_error) => emit_error(inner, send_error),
            }
        }
    };
    // `finally`：成功路径、成功响应发送失败、handler 抛错三条路径都要删除条目，否则服务端随后
    // 发来的 `notifications/cancelled` 会命中一个已经结束的 controller（评审 finding 1）。
    inner.lock().incoming.remove(&id);
    finished
}

/// `{jsonrpc:"2.0", id, error}`：`McpError` → `{code, message, data}`，其他错误 → `-32603`。
fn error_response(id: &JsonRpcId, error: &McpTransportError) -> Value {
    let (code, message, data) = match error.as_mcp() {
        Some(McpError::JsonRpc {
            code,
            message,
            data,
        }) => (*code, message.clone(), data.clone()),
        Some(other) => (
            crate::jsonrpc::JsonRpcErrorCodes::INTERNAL_ERROR,
            other.to_string(),
            None,
        ),
        None => (
            crate::jsonrpc::JsonRpcErrorCodes::INTERNAL_ERROR,
            error.message(),
            None,
        ),
    };
    let mut fields = vec![
        (
            "code".to_owned(),
            Value::Number(serde_json::Number::from(code)),
        ),
        ("message".to_owned(), Value::String(message)),
    ];
    if let Some(data) = data {
        fields.push(("data".to_owned(), data));
    }
    json_object([
        ("jsonrpc", Value::String("2.0".to_owned())),
        ("id", id_value(id)),
        ("error", Value::Object(fields.into_iter().collect())),
    ])
}

/// 发送一条响应；失败时 `emitError(sendError)`（`-32601` 分支用）。
async fn send_or_emit(
    inner: &Arc<ClientInner>,
    transport: &Arc<dyn McpTransport>,
    message: Value,
) -> Result<(), McpTransportError> {
    match transport
        .send(message, McpTransportSendOptions::none())
        .await
    {
        Ok(()) => Ok(()),
        Err(error) => emit_error(inner, error),
    }
}

/// 按插入序构造 JSON 对象（`json!` 之外的可读写法，避免宏里的键序陷阱）。
fn json_object<const N: usize>(fields: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (key, value) in fields {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

/// `JsonRpcId` → JSON 值（数字按 ECMAScript 文本写出）。
fn id_value(id: &JsonRpcId) -> Value {
    serde_json::to_value(id).unwrap_or(Value::Null)
}

/// `handleNotification(message)`：先处理内置通知，再按 method 派发给注册的监听器。
///
/// 内置的两条是 `notifications/progress`（续期 + 回调）与 `notifications/cancelled`（取消
/// 服务端请求）。它们先于普通监听器执行，因此监听器看到的是已经生效的副作用。
///
/// 监听器返回 `Err` 时先交给 `emit_error`；只要 error listener 不抛错，循环继续调用后续
/// 监听器。只有 error listener 自己抛错才会中断派发，并把该错误返回给传输。
fn handle_notification(
    inner: &Arc<ClientInner>,
    method: &str,
    params: Option<&Value>,
) -> Result<(), McpTransportError> {
    if method == "notifications/progress" {
        handle_progress(inner, params)?;
    } else if method == "notifications/cancelled" {
        handle_cancelled(inner, params);
    }
    // TS 直接迭代 `Set`：迭代期间新增的 listener 会在同一次迭代里被访问，删除尚未访问的则跳过
    // （§30.3 的实测行为）。这里用 [`ListenerSet::next_after`] 的实时游标复刻，而不是快照。
    let set = {
        let state = inner.lock();
        state
            .notification_listeners
            .iter()
            .find(|group| group.method == method)
            .map(|group| Arc::clone(&group.listeners))
    };
    let Some(set) = set else {
        return Ok(());
    };
    let mut cursor: Option<u64> = None;
    loop {
        let next = set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_after(cursor);
        let Some((seq, listener)) = next else {
            break;
        };
        cursor = Some(seq);
        if let Err(error) = listener(params) {
            emit_error(inner, error)?;
        }
    }
    Ok(())
}

/// `handleProgress(params)`：只为仍在 pending 里的请求续期并回调。
///
/// 以下情况都返回 `Ok(())` 且不做任何事：`params` 不是对象、`progressToken` 不是合法
/// [`JsonRpcId`]、`progress` 不是数字、令牌没有登记（请求没带 `on_progress` 或已结算）。
/// 命中时把截止时刻重置为“现在 + 原 `timeoutMs`”，再以**原始 `params`** 调用回调；回调返回
/// 的 `Err` 交给 `emit_error`。
fn handle_progress(
    inner: &Arc<ClientInner>,
    params: Option<&Value>,
) -> Result<(), McpTransportError> {
    let Some(Value::Object(map)) = params else {
        return Ok(());
    };
    let Some(token) = map
        .get("progressToken")
        .filter(|value| is_json_rpc_id(value))
    else {
        return Ok(());
    };
    if !map.get("progress").is_some_and(Value::is_number) {
        return Ok(());
    }
    let Some(token) = value_to_json_rpc_id(token) else {
        return Ok(());
    };
    let request = {
        let state = inner.lock();
        let Some(request_id) = state.progress_requests.get(&token) else {
            return Ok(());
        };
        let request_id = request_id.clone();
        let entry = state.pending.get(&request_id);
        entry.map(|entry| {
            (
                request_id,
                Arc::clone(&entry.timer),
                entry.timeout_ms,
                entry.on_progress.clone(),
            )
        })
    };
    let Some((_request_id, timer, timeout_ms, on_progress)) = request else {
        return Ok(());
    };
    // 进度通知只为**仍在 pending 里**的请求续期（结算后的进度被静默忽略，§30.2）。
    timer.set(timeout_deadline(timeout_ms));
    if let Some(callback) = on_progress {
        // `entry.onProgress?.(params as unknown as ProgressNotification)`：TS 只做类型断言，
        // 回调收到的就是服务器发来的 `params` 原样（未知键、类型不符的 `total`/`message`、
        // 原始键序都保留）。因此这里传原始 `Value` 而不是重建的类型化结构体。
        let params = Value::Object(map.clone());
        // 回调抛出的错误交给 `emitError`。
        if let Err(error) = callback(&params) {
            emit_error(inner, error)?;
        }
    }
    Ok(())
}

/// `handleCancelled(params)`：取消正在处理的服务端请求。
///
/// `incoming.get(requestId)?.abort(params.reason)`：`AbortController.abort()` 的语义是
/// **首次调用生效，后续 abort 是 no-op**，因此只有第一个 `notifications/cancelled` 的 reason
/// 会被记录。没有 `reason` 字段时是 `abort(undefined)`，`signal.reason` 仍是默认的
/// `DOMException`（`name = "AbortError"`、`message = "This operation was aborted"`），
/// 而不是"没有 reason"（§13.5 的表）；显式 `reason: null` 则给出 `null`。
fn handle_cancelled(inner: &Arc<ClientInner>, params: Option<&Value>) {
    let Some(Value::Object(map)) = params else {
        return;
    };
    let Some(id) = map.get("requestId").filter(|value| is_json_rpc_id(value)) else {
        return;
    };
    let Some(id) = value_to_json_rpc_id(id) else {
        return;
    };
    let state = inner.lock();
    if let Some(entry) = state.incoming.get(&id) {
        // 只在首次取消时写入 reason（后续 `abort()` 不再改变 `signal.reason`）。
        {
            let mut slot = entry.reason.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() && !entry.token.is_cancelled() {
                *slot = Some(
                    map.get("reason")
                        .cloned()
                        .unwrap_or_else(default_abort_reason),
                );
            }
        }
        entry.token.cancel();
    }
}

/// `AbortController.abort(undefined)` 给出的默认 `DOMException` 的 JSON 化表示
/// （`{"name":"AbortError","message":"This operation was aborted"}`）。
fn default_abort_reason() -> Value {
    json_object([
        ("name", Value::String("AbortError".to_owned())),
        (
            "message",
            Value::String("This operation was aborted".to_owned()),
        ),
    ])
}

/// `handleTransportClose()`：传输关闭事件 → `mark_closed`，错误与 `close()` 使用同一个
/// `McpConnectionClosedError`。因此状态机（置 `Closed`）、pending/incoming 结算与 close
/// 监听器行为与显式 `close()` 等价；区别是显式 `close()` 还会先 `state.transport.take()` 并
/// `dispose_transport_listeners()` 退订传输监听器，而这条路径只调用 `mark_closed`：传输关闭后
/// `ClientState.transport` 仍持有传输、`disposers` 也仍未退订（之后再调显式 `close()` 才会
/// 取走传输并退订）。
fn handle_transport_close(inner: &Arc<ClientInner>) -> Result<(), McpTransportError> {
    mark_closed(
        inner,
        McpTransportError::from(McpError::connection_closed()),
    )
}

/// `markClosed(error)`：幂等；拒绝 pending、取消 incoming、按快照调用 close listener。
///
/// 无论是否已经是 `Closed`，pending 与 incoming 都会被 drain，保证表里不残留条目；但 close
/// 监听器只在**第一次**从其他状态进入 `Closed` 时调用。某个 close 监听器抛出的错误经
/// `emit_error` 上报；若某个 error listener 随后也抛错，该错误会中断剩余的 close 监听器。
fn mark_closed(
    inner: &Arc<ClientInner>,
    error: McpTransportError,
) -> Result<(), McpTransportError> {
    let (was_closed, pending, incoming, listeners) = {
        let mut state = inner.lock();
        let was_closed = state.status == ConnectionState::Closed;
        state.status = ConnectionState::Closed;
        let pending: Vec<(JsonRpcId, PendingEntry)> = state.pending.drain().collect();
        let incoming: Vec<IncomingEntry> = state.incoming.drain().map(|(_, entry)| entry).collect();
        // `[...this.closeListeners]`：close listener 是**快照**迭代（与 notification/error 不同），
        // 因此迭代期间新增/删除都不影响本次派发。
        let listeners: Vec<CloseListener> = state.close_listeners.clone();
        (was_closed, pending, incoming, listeners)
    };
    for (id, entry) in pending {
        remove_pending(inner, &id, entry, Err(error.clone()));
    }
    for entry in incoming {
        // TS 的 `controller.abort(error)` 在**已取消**的控制器上是 no-op：被
        // `notifications/cancelled` 取消过的服务端请求保留第一次的 `signal.reason`
        // （评审 finding 6）。这里同样只在尚未取消时写入关闭原因。
        if !entry.token.is_cancelled() {
            let reason = error.as_mcp().map_or(Value::Null, |mcp| {
                let mut object = Map::new();
                object.insert("name".to_owned(), Value::String(mcp.name().to_owned()));
                object.insert("message".to_owned(), Value::String(mcp.to_string()));
                Value::Object(object)
            });
            *entry.reason.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
            entry.token.cancel();
        }
    }
    if was_closed {
        return Ok(());
    }
    // `catch (listenerError) { this.emitError(listenerError) }` —— 而 `emitError` 自己**不**
    // 捕获 listener 异常，因此 error listener 抛出的错误会冲出 `markClosed`、中断后续 close
    // listener，并在 `close()` 路径上让 `await transport?.close()` 不再执行（§30.4 实测）。
    for listener in listeners {
        if let Err(listener_error) = listener() {
            emit_error(inner, listener_error)?;
        }
    }
    Ok(())
}

/// `emitError(error)`：不捕获 listener 异常（§30.4）。
///
/// 迭代语义与 TS 的 `for (const listener of this.errorListeners) listener(normalized)` 一致：
/// 实时游标，迭代期间新增的 listener 也会被访问（§30.3 实测）。
fn emit_error(inner: &Arc<ClientInner>, error: McpTransportError) -> Result<(), McpTransportError> {
    let mut cursor: Option<u64> = None;
    loop {
        let next = {
            let state = inner.lock();
            state.error_listeners.next_after(cursor)
        };
        let Some((seq, listener)) = next else {
            return Ok(());
        };
        cursor = Some(seq);
        listener(&error)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn abort_reason_matches_the_typescript_table() {
        // §13.5 的逐项实测表（本机 Node 26 + TS 参考实现，`scratch/kiana-mcp-client/probe.mjs`）。
        assert_eq!(
            abort_reason_string(None),
            "AbortError: This operation was aborted"
        );
        assert_eq!(abort_reason_string(Some(&Value::Null)), "Aborted");
        assert_eq!(abort_reason_string(Some(&json!(""))), "");
        assert_eq!(abort_reason_string(Some(&json!(0))), "0");
        assert_eq!(abort_reason_string(Some(&json!(false))), "false");
        assert_eq!(abort_reason_string(Some(&json!("stop"))), "stop");
        assert_eq!(abort_reason_string(Some(&json!({}))), "[object Object]");
    }

    #[test]
    fn timeouts_without_a_timer() {
        // `!Number.isFinite(ms) || ms <= 0` → 不设定时器。
        assert!(timeout_deadline(f64::INFINITY).is_none());
        assert!(timeout_deadline(f64::NEG_INFINITY).is_none());
        assert!(timeout_deadline(f64::NAN).is_none());
        assert!(timeout_deadline(0.0).is_none());
        assert!(timeout_deadline(-5.0).is_none());
        assert!(timeout_deadline(30_000.0).is_some());
    }

    #[test]
    fn oversized_timeouts_are_clamped_to_one_millisecond() {
        // Node 把 `delay > 2^31-1` 钳到 1 ms；`2^31 - 1` 本身仍按原值。
        assert_eq!(
            timeout_duration(MAX_TIMER_DELAY_MS),
            Some(Duration::from_millis(MAX_TIMER_DELAY_MS as u64))
        );
        assert_eq!(
            timeout_duration(1e10),
            Some(Duration::from_millis(1)),
            "1 ms（不是 10^10 ms）"
        );
    }

    #[test]
    fn fractional_timeouts_are_truncated_like_node() {
        // Node 的 `Timeout` 构造函数把 `!(after >= 1 && after <= 2^31-1)` 改成 1 ms，
        // `insert()` 再把余下的值 `MathTrunc` 到整毫秒。实测（Node 26）：
        // `setTimeout(fn, 5.9)` 与 `setTimeout(fn, 5)` 的触发分布相同。
        assert_eq!(
            timeout_duration(0.5),
            Some(Duration::from_millis(1)),
            "0.5 ms → 1 ms"
        );
        for (input, expected) in [(5.9, 5), (5.5, 5), (5.0, 5), (1.999, 1)] {
            assert_eq!(
                timeout_duration(input),
                Some(Duration::from_millis(expected)),
                "timeout_duration({input})"
            );
        }
    }

    #[test]
    fn timeout_error_message_is_verbatim() {
        assert_eq!(
            timeout_error(5.0).to_string(),
            "MCP request timed out after 5ms"
        );
        assert_eq!(timeout_error(5.0).name(), "McpTimeoutError");
    }

    #[test]
    fn list_page_validation_errors_are_verbatim() {
        let error = validate_list_page("tools/list", "tools", &json!({}), is_tool)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Invalid MCP tools/list result");
        let error = validate_list_page("tools/list", "tools", &json!({"tools": [1]}), is_tool)
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Invalid entry in MCP tools/list result");
        let error = validate_list_page(
            "tools/list",
            "tools",
            &json!({"tools": [], "nextCursor": 5}),
            is_tool,
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "Invalid MCP tools/list cursor");
        assert_eq!(error.name(), "McpError");
    }

    #[test]
    fn list_page_treats_null_and_empty_cursor_as_the_end() {
        let page = validate_list_page(
            "tools/list",
            "tools",
            &json!({"tools": [], "nextCursor": null}),
            is_tool,
        )
        .unwrap();
        assert!(page.next_cursor.is_none());
        let page = validate_list_page(
            "tools/list",
            "tools",
            &json!({"tools": [], "nextCursor": ""}),
            is_tool,
        )
        .unwrap();
        assert!(page.next_cursor.is_none());
        let page = validate_list_page(
            "tools/list",
            "tools",
            &json!({"tools": [], "nextCursor": "next"}),
            is_tool,
        )
        .unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("next"));
    }

    #[test]
    fn read_resource_validation_errors_are_verbatim() {
        let error = validate_read_resource_result(&json!({})).err().unwrap();
        assert_eq!(error.to_string(), "Invalid MCP resources/read result");
        let error = validate_read_resource_result(&json!({"contents": [{"uri": "u"}]}))
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "Invalid contents in MCP resources/read result"
        );
    }

    #[test]
    fn call_tool_content_null_is_invalid_but_missing_is_empty() {
        let result = validate_call_tool_result(&json!({"structuredContent": 1})).unwrap();
        assert!(result.content.is_empty());
        assert_eq!(result.structured_content, Some(json!(1)));
        let error = validate_call_tool_result(&json!({"content": null}))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Invalid MCP tools/call result");
    }

    #[test]
    fn progress_message_merges_meta_and_token() {
        let id = JsonRpcId::Number(7.0);
        assert_eq!(
            with_progress_token(None, &id),
            json!({"_meta": {"progressToken": 7}})
        );
        assert_eq!(
            with_progress_token(Some(&json!({"a": 1, "_meta": {"x": "y"}})), &id),
            json!({"a": 1, "_meta": {"x": "y", "progressToken": 7}})
        );
        // `_meta` 不是对象时从空对象开始（`isObject(params?._meta) ? params._meta : {}`）。
        assert_eq!(
            with_progress_token(Some(&json!({"a": 1, "_meta": 5})), &id),
            json!({"a": 1, "_meta": {"progressToken": 7}})
        );
    }

    #[test]
    fn request_messages_keep_typescript_key_order() {
        let id = JsonRpcId::Number(2.0);
        assert_eq!(
            kiana_json::js_object::stringify(&request_message(&id, "ping", None)),
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#
        );
        assert_eq!(
            kiana_json::js_object::stringify(&request_message(
                &id,
                "resources/read",
                Some(&json!({"uri": "file:///a"}))
            )),
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"file:///a"}}"#
        );
        assert_eq!(
            kiana_json::js_object::stringify(&notification_message(
                "notifications/cancelled",
                Some(&json!({"requestId": 2, "reason": "stop"}))
            )),
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"stop"}}"#
        );
    }

    #[test]
    fn resource_name_falls_back_to_the_uri() {
        let item = json!({"uri": "file:///b", "mimeType": "text/plain"});
        let resource = to_resource(item.as_object().unwrap());
        assert_eq!(resource.name, "file:///b");
        let item = json!({"uri": "file:///a", "name": "a"});
        assert_eq!(to_resource(item.as_object().unwrap()).name, "a");
    }

    #[test]
    fn initialize_result_validation_is_verbatim() {
        let error = validate_initialize_result(&json!({"protocolVersion": 5}))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Invalid MCP initialize result");
        assert_eq!(error.name(), "McpError");
        let error = validate_initialize_result(&json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "serverInfo": {"name": "s", "version": "1"},
            "instructions": 5,
        }))
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "Invalid MCP initialize result");
    }

    #[test]
    fn request_options_builders_are_chainable() {
        let signal = CancellationToken::new();
        let options = McpRequestOptions::new()
            .with_signal_reason(signal.clone(), json!("stop"))
            .with_timeout_ms(5.0);
        assert_eq!(options.signal_reason, Some(json!("stop")));
        assert_eq!(options.timeout_ms, Some(5.0));
        assert!(options.signal.is_some());
        assert!(options.on_progress.is_none());
    }
}
