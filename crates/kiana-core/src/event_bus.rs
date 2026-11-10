//! `core/event-bus.ts`：进程级发布/订阅总线（`create_event_bus` / `EventBus`）。
//!
//! TS: `packages/coding-agent/src/core/event-bus.ts`。
//!
//! ## 语义（逐条对齐 TS 的 `EventEmitter`）
//!
//! - 同一 channel 的 handler 按**注册顺序同步**调用；`emit` 返回时它们已经全部被调用过。
//!   TS 把 handler 包成 `async data => { try { await handler(data) } catch ... }`，在第一个
//!   `await` 之前也是同步执行的，因此 Rust 侧不做 `tokio::spawn`（那会丢掉同步顺序）。
//! - `emit` 取监听器快照后再调用，所以 handler 在回调里 `on` / 退订 / `clear` 不会影响本次
//!   派发（Node 的 `EventEmitter` 同样先复制监听器数组）。
//! - handler 出错（Rust 里即 panic）不会击穿 `emit`：用 `catch_unwind` 兜住，并以
//!   `tracing::error!` 输出逐字前缀 `Event handler error ({channel}): {err}`（TS 的
//!   `console.error` 走 `ARCHITECTURE.md` §4.4 的 tracing 映射）。panic 的默认 hook 仍会打印
//!   线程信息，这一点与 TS 的 `try/catch` 不同（进程级 panic hook 不能被局部替换而不影响其它线程）。
//! - `on` 返回取消订阅闭包；重复调用是无害的（幂等），只移除对应的那一次订阅。
//! - `clear` 移除**全部** channel 的全部监听器。
//!
//! ## 与 TS 的差异
//!
//! Node 的 `EventEmitter` 对 `"error"` channel 有特例：没有监听器时 `emit("error", err)` 会**抛错**。
//! 本实现把 `event_bus` 当作普通 channel 处理（不抛）。业务代码没有使用 `"error"` channel。
//!
//! TS 的 `createEventBus` 把**同一个对象引用**传给一次 `emit` 的所有 handler，因此一个 handler
//! 对数据的就地修改对后续 handler 可见。Rust 的 handler 类型是
//! `Arc<dyn Fn(Value) + Send + Sync>`（规划者冻结的跨 crate 契约），`serde_json::Value` 没有
//! 共享所有权，所以每次派发给 handler 的是独立克隆：修改对后续 handler 不可见。树内没有修改
//! 事件数据的 handler（事件的消费方只读），故这一差异不可观察。
//!
//! ## 锁与重入
//!
//! 只有一个 `Mutex<BusState>` 保护监听器表。`emit` 取快照后**立即释放锁**，handler 在
//! 锁外调用；因此 handler 内可以再次 `emit`、`on`、退订或 `clear`，回调执行期间不会因
//! 总线锁死锁，也只是影响后续派发。
//!
//! 但“不在锁内调用用户代码”只对 **handler 函数本身**成立：退订闭包的
//! `listeners.retain(..)` 与 `clear` 的 `listeners.clear()` 是在持锁状态下 drop 被移除的
//! [`EventHandler`]（`Arc<dyn Fn>`）。若此时它是最后一个引用，闭包捕获值的 `Drop` 就会在
//! 锁内执行；该 `Drop` 若重入本总线的 `on`/`emit`/`clear`/退订，同一线程对非重入的
//! `std::sync::Mutex` 再次加锁就会自锁。`on` 与退订闭包只在修改表时短暂持锁。锁毒化按
//! “恢复内层数据”处理（见 `lock`）。

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

/// 事件处理函数。
///
/// TS 的 handler 是 `(data: unknown) => void`（可以是 async 函数）。Rust 侧用同步闭包表达；
/// 需要在 handler 里做异步工作的调用方自行 `tokio::spawn`（与 TS 的“不等待 handler”一致）。
pub type EventHandler = Arc<dyn Fn(Value) + Send + Sync>;

/// 取消订阅闭包：调用后移除对应的那一次订阅；重复调用无副作用。
pub type Unsubscribe = Box<dyn Fn() + Send + Sync>;

/// 发布/订阅总线。TS: `event-bus.ts#EventBus`（`emit` / `on`）。
///
/// `clear` 在 TS 里属于 `EventBusController`；Rust 的 trait 对象无法“向下”加方法，因此把
/// `clear` 放进 `EventBus`，并用 [`EventBusController`] 保留 TS 的类型名（见其文档）。
pub trait EventBus: Send + Sync {
    /// 同步地把 `data` 发给 `channel` 的全部监听器。
    ///
    /// 先复制监听器列表再逐个调用：回调里的订阅/退订/`clear` 只对下一次 `emit` 生效。
    /// 每个 handler 收到 `data` 的独立克隆（见模块文档的 TS 差异）；单个 handler panic
    /// 会被 `catch_unwind` 隔离，不影响其余 handler，也不会把 panic 传给 `emit` 的调用方。
    fn emit(&self, channel: &str, data: Value);

    /// 订阅 `channel`；返回取消订阅闭包。
    ///
    /// 同一 channel 内按注册顺序调用。闭包重复调用无副作用，且只移除本次订阅。
    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe;

    /// 移除全部监听器。TS: `EventBusController#clear`。
    ///
    /// 对正在进行中的 `emit` 无效（它已持有快照）。
    fn clear(&self);
}

/// TS: `event-bus.ts#EventBusController`（`EventBus` + `clear`）。
///
/// 因为 [`EventBus`] 已经包含 `clear`（Rust 的 trait 继承无法给 `dyn EventBus` 增加方法），
/// 这里用 blanket impl 保留名字：任何 `EventBus` 都是 `EventBusController`，下游可写
/// `Arc<dyn EventBus>` 或 `Arc<dyn EventBusController>`。
pub trait EventBusController: EventBus {}

impl<T: EventBus + ?Sized> EventBusController for T {}

#[derive(Default)]
struct BusState {
    listeners: HashMap<String, Vec<Listener>>,
    next_id: u64,
}

struct Listener {
    /// 订阅序号：退订闭包只持有它并按键删除，因此同一个 handler 值注册多次时可以逐次退订。
    id: u64,
    handler: EventHandler,
}

/// [`create_event_bus`] 返回的具体实现；用 `Arc<DefaultEventBus>` 或 `Arc<dyn EventBus>` 持有。
///
/// 状态放在 `Arc<Mutex<..>>` 里，共享靠外层 `Arc`；`Debug` 只打印每个 channel 的监听器数量，
/// 不调用 handler。
#[derive(Default)]
pub struct DefaultEventBus {
    state: Arc<Mutex<BusState>>,
}

impl std::fmt::Debug for DefaultEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("EventBus")
            .field(
                "channels",
                &state
                    .listeners
                    .iter()
                    .map(|(channel, listeners)| (channel.as_str(), listeners.len()))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 持锁期间只操作监听器表，不调用 handler 函数本身（移除监听器时被 drop 的 `Drop`
    // 例外，见模块文档）；毒化来自持锁期间的 panic，这里恢复内层数据继续使用。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 把 panic payload 转成可打印文本（`&str` / `String` / 其它）。
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

impl EventBus for DefaultEventBus {
    fn emit(&self, channel: &str, data: Value) {
        let snapshot: Vec<EventHandler> = {
            let state = lock(&self.state);
            state
                .listeners
                .get(channel)
                .map(|listeners| {
                    listeners
                        .iter()
                        .map(|listener| listener.handler.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        for handler in snapshot {
            let payload = catch_unwind(AssertUnwindSafe(|| handler(data.clone())));
            if let Err(payload) = payload {
                tracing::error!(
                    "Event handler error ({}): {}",
                    channel,
                    panic_message(payload.as_ref())
                );
            }
        }
    }

    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe {
        let state = Arc::clone(&self.state);
        let id = {
            let mut guard = lock(&state);
            let id = guard.next_id;
            guard.next_id += 1;
            guard
                .listeners
                .entry(channel.to_string())
                .or_default()
                .push(Listener { id, handler });
            id
        };
        let channel = channel.to_string();
        Box::new(move || {
            let mut guard = lock(&state);
            let Some(listeners) = guard.listeners.get_mut(&channel) else {
                return;
            };
            listeners.retain(|listener| listener.id != id);
            if listeners.is_empty() {
                guard.listeners.remove(&channel);
            }
        })
    }

    fn clear(&self) {
        lock(&self.state).listeners.clear();
    }
}

/// 建立一个新的空事件总线。TS: `event-bus.ts#createEventBus`。
///
/// 返回具体类型，便于同时满足 `Arc<DefaultEventBus>`、`Arc<dyn EventBus>` 与
/// `Arc<dyn EventBusController>` 三种持有方式（后两者由 unsize coercion 得到）。
pub fn create_event_bus() -> Arc<DefaultEventBus> {
    Arc::new(DefaultEventBus::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counter_handler(
        counter: Arc<AtomicUsize>,
        payloads: Arc<Mutex<Vec<Value>>>,
    ) -> EventHandler {
        Arc::new(move |data| {
            counter.fetch_add(1, Ordering::SeqCst);
            payloads.lock().expect("lock").push(data);
        })
    }

    #[test]
    fn calls_handlers_synchronously_in_registration_order() {
        let bus = create_event_bus();
        let order = Arc::new(Mutex::new(Vec::new()));
        for index in 0..3 {
            let order = Arc::clone(&order);
            let _subscription = bus.on(
                "chan",
                Arc::new(move |_: Value| order.lock().expect("lock").push(index)),
            );
        }
        bus.emit("chan", Value::Null);
        assert_eq!(*order.lock().expect("lock"), vec![0, 1, 2]);
    }

    #[test]
    fn unsubscribe_removes_only_its_own_listener_and_is_idempotent() {
        let bus = create_event_bus();
        let calls = Arc::new(AtomicUsize::new(0));
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let first = bus.on(
            "chan",
            counter_handler(Arc::clone(&calls), Arc::clone(&payloads)),
        );
        let _second = bus.on("chan", counter_handler(Arc::clone(&calls), payloads));
        bus.emit("chan", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        first();
        first();
        bus.emit("chan", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn clear_removes_every_channel() {
        let bus = create_event_bus();
        let calls = Arc::new(AtomicUsize::new(0));
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let _a = bus.on(
            "a",
            counter_handler(Arc::clone(&calls), Arc::clone(&payloads)),
        );
        let _b = bus.on("b", counter_handler(Arc::clone(&calls), payloads));
        bus.clear();
        bus.emit("a", Value::Null);
        bus.emit("b", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn channels_are_isolated_and_handlers_are_not_awaited() {
        let bus = create_event_bus();
        let calls = Arc::new(AtomicUsize::new(0));
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let _subscription = bus.on(
            "a",
            counter_handler(Arc::clone(&calls), Arc::clone(&payloads)),
        );
        bus.emit("b", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        bus.emit("a", serde_json::json!({"x": 1}));
        assert_eq!(
            *payloads.lock().expect("lock"),
            vec![serde_json::json!({"x": 1})]
        );
    }

    #[test]
    fn controller_and_bus_trait_objects_are_interchangeable() {
        let bus: Arc<dyn EventBusController> = create_event_bus();
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let _subscription = bus.on(
            "c",
            Arc::new(move |_| {
                handler_calls.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("c", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        bus.clear();
        bus.emit("c", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // `create_event_bus` also coerces to the narrower TS trait object.
        let as_bus: Arc<dyn EventBus> = create_event_bus();
        as_bus.emit("nothing-listens", Value::Null);
        let unsubscribe = as_bus.on("x", Arc::new(|_| {}));
        unsubscribe();
    }

    #[test]
    fn a_panicking_handler_does_not_break_emit() {
        let bus = create_event_bus();
        let calls = Arc::new(AtomicUsize::new(0));
        let _boom = bus.on("chan", Arc::new(|_: Value| panic!("boom")));
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let _counter = bus.on("chan", counter_handler(Arc::clone(&calls), payloads));
        bus.emit("chan", Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
