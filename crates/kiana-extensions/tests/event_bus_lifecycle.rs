//! `7193-event-bus-lifecycle.test.ts` 的加载器层移植。
//!
//! TS 用例经 `harness.session.reload()` / `dispose()` 触发 `runtime.invalidate()`；本测试在
//! 加载器层直接复刻同样的时序（每次 reload 建新 runtime + 重新 load，dispose 时
//! `runtime_invalidate`）。断言：
//!
//! - 失效后旧扩展的事件总线订阅被移除（reload / dispose 都不再触发）；
//! - 宿主自己注册的监听器不受影响；
//! - 旧 `kiana` 句柄抛【逐字】stale 文案。
#![cfg(feature = "runtime")]
// `ExtensionFactory` 的冻结签名是 `Arc<dyn Fn(Arc<dyn ExtensionApi>) -> ...>`；`ExtensionApi`
// 没有 `Send`/`Sync` 超 trait（单线程 LocalSet），clippy 的 arc_with_non_send_sync 在此不适用。
#![allow(clippy::arc_with_non_send_sync)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use kiana_core::event_bus::{EventBus, create_event_bus};
use kiana_ext_api::types::{ExtensionApi, ExtensionFactory};
use kiana_extensions::loader::{
    create_extension_runtime, load_extension_from_factory, runtime_invalidate,
};
use serde_json::Value;

fn cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn removes_extension_owned_event_bus_listeners_on_reload_and_dispose() {
    let event_bus: Arc<dyn EventBus> = create_event_bus();
    let extension_calls = Arc::new(AtomicU32::new(0));
    let host_calls = Arc::new(AtomicU32::new(0));
    let first_api: Rc<RefCell<Option<Arc<dyn ExtensionApi>>>> = Rc::new(RefCell::new(None));
    let captured_first = Rc::clone(&first_api);

    let host_calls_for_host = Arc::clone(&host_calls);
    let _host_subscription = event_bus.on(
        "reload:test",
        Arc::new(move |_| {
            host_calls_for_host.fetch_add(1, Ordering::SeqCst);
        }),
    );

    let extension_calls_for_factory = Arc::clone(&extension_calls);
    let factory: ExtensionFactory = Arc::new(move |kiana| {
        let calls = Arc::clone(&extension_calls_for_factory);
        let captured = Rc::clone(&captured_first);
        Box::pin(async move {
            if captured.borrow().is_none() {
                *captured.borrow_mut() = Some(Arc::clone(&kiana));
            }
            let _ = kiana.events().on(
                "reload:test",
                Arc::new(move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                }),
            );
        })
    });

    let load = |factory: ExtensionFactory| async {
        let runtime = create_extension_runtime();
        let extension = load_extension_from_factory(
            factory,
            &cwd(),
            Arc::clone(&event_bus),
            Arc::clone(&runtime),
            None,
        )
        .await
        .expect("extension loads");
        (runtime, extension)
    };

    let emit = || {
        let extension_before = extension_calls.load(Ordering::SeqCst);
        let host_before = host_calls.load(Ordering::SeqCst);
        event_bus.emit("reload:test", Value::Null);
        (
            extension_calls.load(Ordering::SeqCst) - extension_before,
            host_calls.load(Ordering::SeqCst) - host_before,
        )
    };

    let (mut runtime, _extension) = load(Arc::clone(&factory)).await;
    assert_eq!(emit(), (1, 1));

    // 第一次 reload：旧 runtime 失效 + 新 runtime 重新加载。
    runtime_invalidate(&runtime, None);
    let first = first_api.borrow().clone().expect("first api captured");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = first.get_commands();
    }))
    .expect_err("stale api throws");
    assert!(panic_message(panic).contains("stale after session replacement or reload"));
    let (new_runtime, _extension) = load(Arc::clone(&factory)).await;
    runtime = new_runtime;
    assert_eq!(emit(), (1, 1));

    // 第二次 reload。
    runtime_invalidate(&runtime, None);
    let (new_runtime, _extension) = load(Arc::clone(&factory)).await;
    runtime = new_runtime;
    assert_eq!(emit(), (1, 1));

    // dispose：当前 runtime 失效后不再有扩展监听器。
    runtime_invalidate(&runtime, None);
    assert_eq!(emit(), (0, 1));
}

#[tokio::test(flavor = "current_thread")]
async fn tracked_unsubscriber_is_idempotent_and_removes_only_itself() {
    let event_bus: Arc<dyn EventBus> = create_event_bus();
    let extension_calls = Arc::new(AtomicU32::new(0));
    let other_calls = Arc::new(AtomicU32::new(0));
    let captured: Rc<RefCell<Option<Arc<dyn ExtensionApi>>>> = Rc::new(RefCell::new(None));

    let calls = Arc::clone(&extension_calls);
    let other = Arc::clone(&other_calls);
    let captured_for_factory = Rc::clone(&captured);
    load_extension_from_factory(
        Arc::new(move |kiana| {
            let calls = Arc::clone(&calls);
            let other = Arc::clone(&other);
            let captured = Rc::clone(&captured_for_factory);
            Box::pin(async move {
                let unsubscribe = kiana.events().on(
                    "channel",
                    Arc::new(move |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                    }),
                );
                let _ = kiana.events().on(
                    "channel",
                    Arc::new(move |_| {
                        other.fetch_add(1, Ordering::SeqCst);
                    }),
                );
                unsubscribe();
                unsubscribe();
                *captured.borrow_mut() = Some(kiana);
            })
        }),
        &cwd(),
        Arc::clone(&event_bus),
        create_extension_runtime(),
        None,
    )
    .await
    .expect("extension loads");

    event_bus.emit("channel", Value::Null);
    assert_eq!(extension_calls.load(Ordering::SeqCst), 0);
    assert_eq!(other_calls.load(Ordering::SeqCst), 1);
}
