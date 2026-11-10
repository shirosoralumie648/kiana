//! 回归 #7193：扩展自有的事件总线监听器在 reload / dispose 时被移除。
//!
//! TS: `packages/coding-agent/test/suite/regressions/7193-event-bus-lifecycle.test.ts`。
//! 只移植 `createEventBus` 的生命周期断言；扩展 loader / harness / `Application` 的
//! “stale after session replacement or reload” 属于 `kiana-extensions` 单元。
//!
//! 测试名保留 TS 的 `it` 标题。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_core::event_bus::{EventBus, Unsubscribe, create_event_bus};

struct Counters {
    extension: Arc<AtomicUsize>,
    host: Arc<AtomicUsize>,
}

impl Counters {
    fn new() -> Self {
        Self {
            extension: Arc::new(AtomicUsize::new(0)),
            host: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The host listener the harness installs once; it survives reloads and dispose.
    fn subscribe_host(&self, bus: &kiana_core::event_bus::DefaultEventBus) -> Unsubscribe {
        let host = Arc::clone(&self.host);
        bus.on(
            "reload:test",
            Arc::new(move |_| {
                host.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    /// An extension-owned listener; the extension runtime tracks it (`trackEventBusSubscription`)
    /// and unsubscribes it when the runtime is invalidated.
    fn subscribe_extension(&self, bus: &kiana_core::event_bus::DefaultEventBus) -> Unsubscribe {
        let extension = Arc::clone(&self.extension);
        bus.on(
            "reload:test",
            Arc::new(move |_| {
                extension.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    /// `eventBus.emit` + `setImmediate` in the TS test. The bus delivers synchronously, so the
    /// returned delta is stable without yielding.
    fn emit(&self, bus: &kiana_core::event_bus::DefaultEventBus) -> (usize, usize) {
        let extension_before = self.extension.load(Ordering::SeqCst);
        let host_before = self.host.load(Ordering::SeqCst);
        bus.emit("reload:test", serde_json::Value::Null);
        (
            self.extension.load(Ordering::SeqCst) - extension_before,
            self.host.load(Ordering::SeqCst) - host_before,
        )
    }
}

#[test]
fn removes_extension_owned_event_bus_listeners_on_reload_and_dispose() {
    let bus = create_event_bus();
    let counters = Counters::new();
    let _host = counters.subscribe_host(&bus);
    // The extension runtime owns its subscriptions; `reload` replaces them.
    let mut extension_subscription = Some(counters.subscribe_extension(&bus));

    assert_eq!(counters.emit(&bus), (1, 1));

    // First reload: unsubscribe the old extension listener, load the new extension.
    if let Some(unsubscribe) = extension_subscription.take() {
        unsubscribe();
    }
    extension_subscription = Some(counters.subscribe_extension(&bus));
    assert_eq!(counters.emit(&bus), (1, 1));

    // Second reload behaves the same.
    if let Some(unsubscribe) = extension_subscription.take() {
        unsubscribe();
    }
    extension_subscription = Some(counters.subscribe_extension(&bus));
    assert_eq!(counters.emit(&bus), (1, 1));

    // Dispose: no extension listener remains; the host listener stays.
    if let Some(unsubscribe) = extension_subscription.take() {
        unsubscribe();
    }
    assert_eq!(counters.emit(&bus), (0, 1));
}
