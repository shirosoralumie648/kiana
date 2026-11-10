//! `transports/{transport,stdio,in-memory,streamable-http}.ts`。
//!
//! 本文件是**模块根**：声明三个传输子模块，并实现 `transport.ts` 的跨单元契约
//! （[`McpTransport`]、[`TransportEvents`]、[`McpTransportSendOptions`]、
//! [`DEFAULT_MAX_MESSAGE_BYTES`]，见 `spec/telemetry-mcp.md` §14）。
//!
//! 子模块由各自的单元实现：
//! - [`stdio`] — `transports/stdio.ts`（`StdioTransport`）；
//! - [`in_memory`] — `transports/in-memory.ts`（`InMemoryTransport`）；
//! - [`streamable_http`] — `transports/streamable-http.ts`（SSE + `StreamableHttpTransport`）。
//!
//! # 在系统中的位置
//!
//! [`McpTransport`] 是客户端与具体传输之间的唯一接口：`crate::client::McpClient` 只持有
//! `Arc<dyn McpTransport>`，因此 stdio / streamable-http / in-memory 可以互换，客户端不
//! 认识任何具体传输类型。本文件另外提供的 [`TransportEvents`] 是三个实现共用的监听器簿记；
//! crate 外 `kiana-builtin-extensions` 的 `mcp/runtime.rs` 自己实现 `McpTransport` 时也复用
//! 它（见该 crate 的 `FailingTransport`）。
//!
//! # 为什么 `send` 收 `serde_json::Value`
//!
//! TS 的 `send(message: JsonRpcMessage)` 收的是**已解析的对象**，传输再 `JSON.stringify` 它。
//! Rust 侧同理传 `serde_json::Value` 而不是 [`crate::jsonrpc::JsonRpcMessage`]：
//! 1. 客户端会构造服务端**从不会**构造的消息（例如带 `result` 的响应），把类型收窄成
//!    请求/通知/响应枚举反而妨碍发送；
//! 2. `Value` 保留调用方写入的键序（`serde_json` 的 `preserve_order`），传输直接
//!    `kiana_json::js_object::stringify` 就能逐字复刻 `JSON.stringify(message)`
//!    （`jsonrpc,id,method,params` / `jsonrpc,method,params` / `jsonrpc,id,result|error`，
//!    整数样式键在前）。
//!
//! # 监听器语义（§30.3 / §30.4）
//!
//! TS 的 `emitMessage`/`emitError`/`emitClose` **直接迭代** `Set`：迭代期间删除的监听器不再
//! 被调用，迭代期间**新增**的监听器（追加在末尾）会在同一次迭代里被访问，`emitClose` 另受
//! "每个 transport 最多一次"约束。Rust 侧用「实时游标 + 按 id 重定位」复刻了同一语义
//! （见 [`TransportEvents::emit_message`]），不是快照克隆。监听器的异常**不会**被吞掉：
//! 它们作为 [`McpTransportError`] 返回给 emit 的调用方（TS 直接抛给 emit 的调用点）。
//!
//! # 阅读顺序
//!
//! 先读 [`McpTransport`]（trait 契约）与 [`McpTransportSendOptions`]，再读 [`TransportEvents`]
//! 的实时迭代实现与 [`Subscription`]，最后按需进入三个子模块。`DEFAULT_MAX_MESSAGE_BYTES`
//! 与三个监听器别名是给实现和调用方共用的小常量与类型别名。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth_provider::McpTransportError;

pub mod in_memory;
pub mod stdio;
pub mod streamable_http;

/// `DEFAULT_MAX_MESSAGE_BYTES = 16 * 1024 * 1024`。
///
/// 公开在模块里但**不**从 crate 根再导出（§30.1）。
///
/// 它是"单条入站消息"的默认上限：`StdioTransportOptions::max_message_bytes` 未设置时用它
/// 限制一行 stdout 的字节数，`StreamableHttpTransportOptions::max_message_bytes` 未设置时
/// 用它作为单个 SSE 事件的缓冲上限。
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// `McpTransportMessageListener = (message: JsonRpcMessage) => void`。
///
/// 收到一条已解析的消息；`Err` 表示监听器自己抛出（§30.4：异常向上传播）。
/// 参数是借用的 `&Value`，只在本次调用内有效；监听器要保留消息必须自己 `clone()`。
pub type McpTransportMessageListener =
    Arc<dyn Fn(&Value) -> Result<(), McpTransportError> + Send + Sync>;

/// `McpTransportErrorListener = (error: Error) => void`。
///
/// 参数是已归一化的错误信封 [`McpTransportError`]（TS 的 `emitError` 会先对入参做
/// `toError`）。`Err` 表示监听器自己抛出：本次 emit 立即返回该错误，后续监听器不再执行。
pub type McpTransportErrorListener =
    Arc<dyn Fn(&McpTransportError) -> Result<(), McpTransportError> + Send + Sync>;

/// `McpTransportCloseListener = () => void`。
///
/// `Err` 表示监听器自己抛出：`emit_close` 把它返回给调用方、剩余 close 监听器不再执行；
/// 但"本实例的 close 已发出"这一事实不会回退（见 [`TransportEvents::emit_close`]）。
pub type McpTransportCloseListener = Arc<dyn Fn() -> Result<(), McpTransportError> + Send + Sync>;

/// `McpTransportSendOptions`：`send(message, options?)` 的可选第二参数。
///
/// Rust 没有可选参数，所以调用方显式传 [`McpTransportSendOptions::none`] 或
/// [`McpTransportSendOptions::with_signal`]（结构体本身实现了 `Default` 与 `Clone`）。
/// `signal` 是发送方持有的 `CancellationToken` 克隆：取消由发送方发起（请求被取消、超时、
/// 已答复或客户端关闭），传输只应观察 [`McpTransportSendOptions::is_aborted`]。Rust 的
/// 类型系统不阻止传输自己调用 `cancel()`，这是与 TS `AbortSignal` 一样的调用约定，而非强制。
#[derive(Debug, Clone, Default)]
pub struct McpTransportSendOptions {
    /// 发送方不再等待答复（请求被取消、超时、已答复或客户端关闭）时取消。
    ///
    /// 观察本字段的传输（目前是 `StreamableHttpTransport`）在那之后**不得**再派发消息
    /// （例如在授权刷新后重试时）；它不会撤销已经派发的消息。`StdioTransport` 与
    /// `InMemoryTransport` 按 TS 语义完全忽略本字段（§15.2、§30.6）。
    pub signal: Option<CancellationToken>,
}

impl McpTransportSendOptions {
    /// 无 signal（`send(message)` 不传第二个参数）。
    pub fn none() -> Self {
        McpTransportSendOptions::default()
    }

    /// 带 signal。
    pub fn with_signal(signal: Option<CancellationToken>) -> Self {
        McpTransportSendOptions { signal }
    }

    /// `signal?.aborted`。
    pub fn is_aborted(&self) -> bool {
        self.signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
    }
}

/// 取消监听器的句柄（TS 里 `onMessage` 返回的 `() => void`）。
///
/// 幂等：第二次 [`Subscription::unsubscribe`] 是 no-op（TS 的 `Set.delete` 同样幂等）。
///
/// **丢弃句柄不会自动退订**：没有 `Drop` 实现，监听器会一直留在表里，直到显式调用
/// [`Subscription::unsubscribe`]——与 TS 中丢弃 disposer 函数不触发删除一致。`Clone` 出来
/// 的句柄共享同一个监听器 id，任何一份退订都会生效。
#[derive(Clone)]
pub struct Subscription {
    kind: ListenerKind,
    events: Arc<TransportEventsInner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerKind {
    Message(u64),
    Error(u64),
    Close(u64),
}

impl Subscription {
    /// `disposer()`：从监听器集合移除。
    ///
    /// 在 emit 过程中调用也立即生效：尚未轮到的监听器不会再被调用（V8 `Set` 在迭代期间
    /// `delete` 的语义，见测试 `unsubscribe_takes_effect_immediately_even_mid_emit`）。
    pub fn unsubscribe(&self) {
        match self.kind {
            ListenerKind::Message(id) => self.events.remove(ListenerKind::Message(id)),
            ListenerKind::Error(id) => self.events.remove(ListenerKind::Error(id)),
            ListenerKind::Close(id) => self.events.remove(ListenerKind::Close(id)),
        }
    }

    /// 是否已退订（对应 TS 里"这个 disposer 已被调用过"）。
    pub fn is_unsubscribed(&self) -> bool {
        !self.events.contains(self.kind)
    }
}

/// 监听器表（互斥保护）。每项是 `(id, listener)`，`Vec` 的顺序就是注册顺序（TS `Set` 的
/// 插入序）；id 让 emit 能在每个监听器返回后按"它现在在哪"重新定位迭代游标——这正是
/// V8 `Set` 实时迭代要求的语义，不能换成 emit 开始时的一次性快照。
#[derive(Default)]
struct ListenerStore {
    messages: Vec<(u64, McpTransportMessageListener)>,
    errors: Vec<(u64, McpTransportErrorListener)>,
    closes: Vec<(u64, McpTransportCloseListener)>,
}

/// `TransportEvents` 的共享状态。
///
/// `listeners` 的锁只保护表本身：`*_at` 在锁内克隆监听器的 `Arc` 后立即释放，监听器在锁外
/// 调用，因此在监听器里重入 `on_message`/`unsubscribe` 不会死锁。锁中毒（只有持锁期间 panic
/// 才可能发生；当前临界区只是 `Vec` 操作）时，注册/删除会被跳过、查询返回 `None`，
/// emit 循环随之提前结束——`if let Ok` / `.ok()?` 是有意的"忽略中毒"。
#[derive(Default)]
struct TransportEventsInner {
    listeners: Mutex<ListenerStore>,
    close_emitted: AtomicBool,
    next_id: AtomicU64,
}

impl TransportEventsInner {
    /// id 只用于相等比较（emit 游标重定位与退订），不参与其它内存的同步，所以 `Relaxed`
    /// 足够；唯一性由 `fetch_add` 保证。
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// 该 id 是否仍在监听器表里。
    fn contains(&self, kind: ListenerKind) -> bool {
        match kind {
            ListenerKind::Message(id) => self.message_position(id).is_some(),
            ListenerKind::Error(id) => self.error_position(id).is_some(),
            ListenerKind::Close(id) => self.close_position(id).is_some(),
        }
    }

    fn remove(&self, kind: ListenerKind) {
        if let Ok(mut store) = self.listeners.lock() {
            match kind {
                ListenerKind::Message(id) => store.messages.retain(|(key, _)| *key != id),
                ListenerKind::Error(id) => store.errors.retain(|(key, _)| *key != id),
                ListenerKind::Close(id) => store.closes.retain(|(key, _)| *key != id),
            }
        }
    }

    /// 实时读取第 `position` 个 message 监听器（V8 `Set` 迭代的游标语义）。
    fn message_at(&self, position: usize) -> Option<(u64, McpTransportMessageListener)> {
        let store = self.listeners.lock().ok()?;
        store
            .messages
            .get(position)
            .map(|(id, listener)| (*id, Arc::clone(listener)))
    }

    fn error_at(&self, position: usize) -> Option<(u64, McpTransportErrorListener)> {
        let store = self.listeners.lock().ok()?;
        store
            .errors
            .get(position)
            .map(|(id, listener)| (*id, Arc::clone(listener)))
    }

    fn close_at(&self, position: usize) -> Option<(u64, McpTransportCloseListener)> {
        let store = self.listeners.lock().ok()?;
        store
            .closes
            .get(position)
            .map(|(id, listener)| (*id, Arc::clone(listener)))
    }

    /// 监听器 `id` 当前的位置（已被删除时为 `None`）。
    fn message_position(&self, id: u64) -> Option<usize> {
        let store = self.listeners.lock().ok()?;
        store.messages.iter().position(|(key, _)| *key == id)
    }

    fn error_position(&self, id: u64) -> Option<usize> {
        let store = self.listeners.lock().ok()?;
        store.errors.iter().position(|(key, _)| *key == id)
    }

    fn close_position(&self, id: u64) -> Option<usize> {
        let store = self.listeners.lock().ok()?;
        store.closes.iter().position(|(key, _)| *key == id)
    }
}

/// `TransportEvents`：各传输共用的监听器簿记。
///
/// `emit_close` 每个实例最多触发一次。监听器按注册顺序调用；某个监听器返回 `Err` 时立即返回
/// 该错误（后续监听器不再被调用），与 TS 的"异常中断循环并抛给调用方"一致。
///
/// 传输实现通常把它作为字段持有，并通过 [`TransportEvents::on_message`] 等三个方法把
/// 监听器注册暴露出去。TS 的 `emitMessage`/`emitError`/`emitClose` 是 `protected`；Rust 没有
/// 同等可见性，这里的 `emit_*` 是 `pub` 方法，类型系统不阻止 crate 外调用。约定与 TS 相同：
/// 只有传输实现自身应调用它们，其余代码通过 [`McpTransport`] 注册监听器。
#[derive(Default)]
pub struct TransportEvents {
    inner: Arc<TransportEventsInner>,
}

impl TransportEvents {
    /// 空的监听器集合。
    pub fn new() -> Self {
        TransportEvents::default()
    }

    /// `onMessage(listener)`：监听已解析的 JSON-RPC 消息。
    pub fn on_message<F>(&self, listener: F) -> Subscription
    where
        F: Fn(&Value) -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        let id = self.inner.next_id();
        if let Ok(mut store) = self.inner.listeners.lock() {
            store.messages.push((id, Arc::new(listener)));
        }
        Subscription {
            kind: ListenerKind::Message(id),
            events: Arc::clone(&self.inner),
        }
    }

    /// `onError(listener)`：监听传输错误（已归一化的错误对象）。
    pub fn on_error<F>(&self, listener: F) -> Subscription
    where
        F: Fn(&McpTransportError) -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        let id = self.inner.next_id();
        if let Ok(mut store) = self.inner.listeners.lock() {
            store.errors.push((id, Arc::new(listener)));
        }
        Subscription {
            kind: ListenerKind::Error(id),
            events: Arc::clone(&self.inner),
        }
    }

    /// `onClose(listener)`：连接关闭（无论 transport 掉落还是 `close()` 被调用）。
    ///
    /// 注册总是成功；但如果本实例已经 emit 过 close，之后再注册的监听器不会再被调用
    /// （`emit_close` 每个实例只真正迭代一次，见测试 `emit_close_fires_at_most_once`）。
    pub fn on_close<F>(&self, listener: F) -> Subscription
    where
        F: Fn() -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        let id = self.inner.next_id();
        if let Ok(mut store) = self.inner.listeners.lock() {
            store.closes.push((id, Arc::new(listener)));
        }
        Subscription {
            kind: ListenerKind::Close(id),
            events: Arc::clone(&self.inner),
        }
    }

    /// `protected emitMessage(message)`：按注册序调用，异常向上返回。
    ///
    /// 与 V8 的 `for (const listener of this.messageListeners)` 一致，迭代是**实时**的：
    /// 迭代期间删除的监听器不再被调用；迭代期间**新增**的监听器（追加在末尾）会在同一次
    /// emit 里被访问。游标按"上一个被调用的 id"重新定位，因此删除导致的位置前移也正确。
    ///
    /// 监听器在锁外调用，所以监听器 panic 会沿调用栈展开给 emit 的调用方（监听器表不会被
    /// 毒化）。TS 侧同样没有 try/catch：异常直接从 `emitMessage` 抛给调用点。
    pub fn emit_message(&self, message: &Value) -> Result<(), McpTransportError> {
        let mut position = 0usize;
        while let Some((id, listener)) = self.inner.message_at(position) {
            listener(message)?;
            position = match self.inner.message_position(id) {
                Some(index) => index + 1,
                // 监听器把自己（或更靠前的项）删了：下一个元素已前移到当前位置。
                None => position,
            };
        }
        Ok(())
    }

    /// `protected emitError(error)`：实时迭代语义同 [`Self::emit_message`]。
    pub fn emit_error(&self, error: &McpTransportError) -> Result<(), McpTransportError> {
        let mut position = 0usize;
        while let Some((id, listener)) = self.inner.error_at(position) {
            listener(error)?;
            position = match self.inner.error_position(id) {
                Some(index) => index + 1,
                None => position,
            };
        }
        Ok(())
    }

    /// `protected emitClose()`：每个实例最多触发一次。
    ///
    /// TS 的 `emitClose` 先检查 `closeEmitted`，然后**直接迭代** `Set`（没有快照：
    /// §30.3 说的是 `McpClient.markClosed` 做快照，不是这里）。因此同样用实时游标。
    pub fn emit_close(&self) -> Result<(), McpTransportError> {
        // 先置位、再迭代：close 监听器里再次调用 emit_close 会直接返回，不会递归；即使某个
        // 监听器返回 Err，本实例也已经算"发过 close"（与 TS 先赋值 `closeEmitted = true`
        // 再 `for` 的顺序一致）。
        if self.inner.close_emitted.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let mut position = 0usize;
        while let Some((id, listener)) = self.inner.close_at(position) {
            listener()?;
            position = match self.inner.close_position(id) {
                Some(index) => index + 1,
                None => position,
            };
        }
        Ok(())
    }

    /// `closeEmitted` 是否已置位。
    pub fn close_emitted(&self) -> bool {
        self.inner.close_emitted.load(Ordering::SeqCst)
    }
}

/// MCP 传输（`transports/transport.ts` 的 `McpTransport`）。
///
/// 所有方法都取 `&self`：传输以 `Arc<dyn McpTransport>` 共享给客户端与服务端请求 handler。
///
/// 本 crate 的实现：[`crate::transports::stdio::StdioTransport`]、
/// [`crate::transports::streamable_http::StreamableHttpTransport`]、
/// [`crate::transports::in_memory::InMemoryTransport`]（测试用）。crate 外
/// `kiana-builtin-extensions` 的 `mcp/runtime.rs` 也直接实现本 trait（一个只会失败的
/// `FailingTransport`）。`crate::client::McpClient` 只按 trait 对象使用传输，新增传输类型
/// 不需要改客户端。
// 同 `auth_provider`：`async-trait` 生成的 `#[must_use]` 触发 `double_must_use`。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// `start()`：打开底层连接。重复调用在 TS 里抛
    /// `` `MCP ... transport already started` ``。
    ///
    /// 本 crate 的三个实现并不一致：`StdioTransport` 与 `StreamableHttpTransport` 返回同名
    /// 错误，`InMemoryTransport` 的重复 `start` 只是再次置位 `started`（no-op，见其
    /// `start_twice_is_a_no_op_and_start_after_close_fails` 测试）。需要统一行为的调用方
    /// 应自己记录启动状态。
    async fn start(&self) -> Result<(), McpTransportError>;

    /// `send(message, options?)`。
    ///
    /// 收 [`Value`] 而不是 `JsonRpcMessage`：见模块文档（服务端风格的响应、键序逐字保留）。
    /// 观察 `options.signal` 的传输（目前是 `StreamableHttpTransport`）在 signal 已取消时
    /// **不得**再派发消息（例如授权刷新后的重试），但已经派发的消息不会被撤销；stdio 与
    /// in-memory 按 TS 语义完全忽略 `McpTransportSendOptions`（§15.2、§30.6），已取消的
    /// signal 仍会投递。细节见 [`McpTransportSendOptions`]。
    async fn send(
        &self,
        message: Value,
        options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError>;

    /// `close()`：幂等。
    ///
    /// 实现负责释放底层资源（子进程、在途 HTTP 请求/SSE 读循环、对端连接），并通过
    /// [`TransportEvents::emit_close`] 通知 close 监听器（每个实例至多一次）。返回 `Ok` 不
    /// 保证监听器已经跑完：stdio 在 spawn 失败等路径上由后台任务稍后发出 close 事件。
    async fn close(&self) -> Result<(), McpTransportError>;

    /// `onMessage(listener)`，返回退订句柄。
    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription;

    /// `onError(listener)`，返回退订句柄。
    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription;

    /// `onClose(listener)`，返回退订句柄。
    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription;

    /// `setProtocolVersion?(version)`：只有 HTTP 传输实现；默认 no-op。
    ///
    /// [`crate::client::McpClient`] 在 `initialize` 成功后无条件调用它（见 `client.rs` 的
    /// 握手流程），stdio 与 in-memory 忽略参数。`StreamableHttpTransport` 把版本存下来，
    /// 后续请求据此发 `MCP-Protocol-Version` 头（空串等价于未设置）。
    fn set_protocol_version(&self, _version: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[test]
    fn emit_message_calls_listeners_in_registration_order() {
        let events = TransportEvents::new();
        let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let first = Arc::clone(&calls);
        let _a = events.on_message(move |_| {
            first.lock().unwrap().push("a");
            Ok(())
        });
        let second = Arc::clone(&calls);
        let _b = events.on_message(move |_| {
            second.lock().unwrap().push("b");
            Ok(())
        });
        events
            .emit_message(&json!({"jsonrpc":"2.0","method":"m"}))
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn unsubscribe_takes_effect_immediately_even_mid_emit() {
        // TS 的 `Set` 迭代语义（§30.3）：迭代期间删除尚未访问的监听器 → 不会被调用。
        let events = TransportEvents::new();
        let called = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let slot: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));

        let first_calls = Arc::clone(&called);
        let first_slot = Arc::clone(&slot);
        let _first = events.on_message(move |_| {
            first_calls.lock().unwrap().push("first");
            if let Some(second) = first_slot.lock().unwrap().as_ref() {
                second.unsubscribe();
            }
            Ok(())
        });

        let second_calls = Arc::clone(&called);
        let second = events.on_message(move |_| {
            second_calls.lock().unwrap().push("second");
            Ok(())
        });
        *slot.lock().unwrap() = Some(second.clone());

        events.emit_message(&json!({})).unwrap();
        assert_eq!(*called.lock().unwrap(), vec!["first"]);
        assert!(second.is_unsubscribed());
    }

    #[test]
    fn listener_errors_propagate_and_stop_the_loop() {
        // §30.4：TS 的 `emitError`/`emitMessage` 不 try/catch，异常中断后续监听器并抛给调用方。
        let events = TransportEvents::new();
        let called = Arc::new(AtomicUsize::new(0));
        let _failing = events.on_message(|_| Err(McpTransportError::new("listener boom")));
        let after = Arc::clone(&called);
        let _after = events.on_message(move |_| {
            after.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        });
        let error = events.emit_message(&json!({})).unwrap_err();
        assert_eq!(error.to_string(), "listener boom");
        assert_eq!(called.load(AtomicOrdering::SeqCst), 0);
    }

    /// 回归测试（评审 finding #6）：V8 的 `Set` 迭代会访问**迭代期间新增**的监听器
    /// （实测 `["a","b","late"]`）。旧实现用 emit 开始时的 id 快照，新增的要等下一次 emit。
    #[test]
    fn listeners_added_during_an_emit_run_in_the_same_emit() {
        let events = Arc::new(TransportEvents::new());
        let called = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let first_calls = Arc::clone(&called);
        let late_events = Arc::clone(&events);
        let _first = events.on_message(move |_| {
            first_calls.lock().unwrap().push("a");
            let late_calls = Arc::clone(&first_calls);
            late_events.on_message(move |_| {
                late_calls.lock().unwrap().push("late");
                Ok(())
            });
            Ok(())
        });
        let second_calls = Arc::clone(&called);
        let _second = events.on_message(move |_| {
            second_calls.lock().unwrap().push("b");
            Ok(())
        });

        events.emit_message(&json!({})).unwrap();
        assert_eq!(*called.lock().unwrap(), vec!["a", "b", "late"]);
    }

    /// 回归测试（评审 finding #6）：监听器删掉自己时，游标不能跳过紧跟其后的那个。
    #[test]
    fn a_listener_that_removes_itself_does_not_skip_its_successor() {
        let events = Arc::new(TransportEvents::new());
        let called = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let self_slot: Arc<Mutex<Option<Subscription>>> = Arc::new(Mutex::new(None));
        let first_calls = Arc::clone(&called);
        let first_slot = Arc::clone(&self_slot);
        let first = events.on_message(move |_| {
            first_calls.lock().unwrap().push("a");
            if let Some(subscription) = first_slot.lock().unwrap().as_ref() {
                subscription.unsubscribe();
            }
            Ok(())
        });
        *self_slot.lock().unwrap() = Some(first);

        let second_calls = Arc::clone(&called);
        let _second = events.on_message(move |_| {
            second_calls.lock().unwrap().push("b");
            Ok(())
        });

        events.emit_message(&json!({})).unwrap();
        assert_eq!(*called.lock().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn emit_close_fires_at_most_once() {
        let events = TransportEvents::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let _listener = events.on_close(move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        });
        events.emit_close().unwrap();
        events.emit_close().unwrap();
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        assert!(events.close_emitted());
        // close 之后新增的监听器不会再被调用（emitClose 已经用掉唯一一次）。
        let late = Arc::new(AtomicUsize::new(0));
        let late_counter = Arc::clone(&late);
        let _late = events.on_close(move || {
            late_counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        });
        events.emit_close().unwrap();
        assert_eq!(late.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn error_listeners_receive_the_normalized_error() {
        let events = TransportEvents::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        let _listener = events.on_error(move |error| {
            sink.lock().unwrap().push(error.name().to_owned());
            Ok(())
        });
        events
            .emit_error(&McpTransportError::network("socket hang up"))
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["TypeError"]);
    }

    #[test]
    fn send_options_report_aborts() {
        let token = CancellationToken::new();
        let options = McpTransportSendOptions::with_signal(Some(token.clone()));
        assert!(!options.is_aborted());
        token.cancel();
        assert!(options.is_aborted());
        assert!(!McpTransportSendOptions::none().is_aborted());
    }
}

#[cfg(test)]
mod trait_tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    /// 最小可用的 `McpTransport`，用来证明 trait 可实现、对象安全、且 `set_protocol_version`
    /// 有 no-op 缺省实现（TS 的 `setProtocolVersion?`）。
    struct StubTransport {
        events: TransportEvents,
        received: Mutex<Vec<Value>>,
        started: AtomicBool,
    }

    impl StubTransport {
        fn new() -> Self {
            StubTransport {
                events: TransportEvents::new(),
                received: Mutex::new(Vec::new()),
                started: AtomicBool::new(false),
            }
        }
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl McpTransport for StubTransport {
        async fn start(&self) -> Result<(), McpTransportError> {
            self.started.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn send(
            &self,
            message: Value,
            _options: McpTransportSendOptions,
        ) -> Result<(), McpTransportError> {
            if !self.started.load(Ordering::SeqCst) {
                return Err(McpError::connection_closed().into());
            }
            self.received.lock().unwrap().push(message.clone());
            self.events.emit_message(&message)
        }

        async fn close(&self) -> Result<(), McpTransportError> {
            self.events.emit_close()
        }

        fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
            self.events.on_message(move |message| listener(message))
        }

        fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
            self.events.on_error(move |error| listener(error))
        }

        fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
            self.events.on_close(move || listener())
        }
    }

    use crate::jsonrpc::McpError;

    #[tokio::test]
    async fn transport_trait_is_object_safe_and_delivers_messages() {
        let transport: Arc<dyn McpTransport> = Arc::new(StubTransport::new());
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = Arc::clone(&seen);
        let subscription = transport.on_message(Arc::new(move |message: &Value| {
            sink.lock().unwrap().push(message.clone());
            Ok(())
        }));
        // 未 start 时发送 → McpConnectionClosedError（默认消息）。
        let error = transport
            .send(
                json!({"jsonrpc":"2.0","method":"m"}),
                McpTransportSendOptions::none(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "MCP connection closed");
        transport.start().await.unwrap();
        transport
            .send(
                json!({"jsonrpc":"2.0","method":"m"}),
                McpTransportSendOptions::none(),
            )
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        // 缺省实现的 set_protocol_version 是 no-op。
        transport.set_protocol_version("2025-06-18");
        subscription.unsubscribe();
        transport.close().await.unwrap();
    }
}
