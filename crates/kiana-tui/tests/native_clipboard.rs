//! `native/clipboard.h` 的三态契约与私有工作线程语义（`native-clipboard-linux.test.ts` 中
//! 不依赖真实 X server 的部分 + `native-platform.test.ts` 的 Rust 侧不变量）。
//!
//! TS 的 C fixture 通过 `state()`/`release()` 观察 `worker_busy`；Rust 侧用
//! [`LinuxX11Clipboard`] 的“忙 → 立刻 Unavailable”行为做等价断言（无需 DISPLAY：连接失败
//! 同样是 `Unavailable`，与 C 里 `open_clipboard` 失败一致）。

use kiana_tui::native::{
    ClipboardError, ClipboardValue, ModifierKey, get_native_clipboard, get_native_platform_helper,
    is_native_modifier_pressed, module_path::native_backend_compiled,
};

/// `undefined` / `null` / 值 → `Option`（`native-platform.ts` 的 `?? null`）。
#[test]
fn clipboard_value_folds_to_option() {
    assert_eq!(ClipboardValue::<String>::Unavailable.into_option(), None);
    assert_eq!(ClipboardValue::<String>::Empty.into_option(), None);
    assert_eq!(
        ClipboardValue::Value("café".to_string()).into_option(),
        Some("café".to_string())
    );
}

/// 默认态是三态里的 `Unavailable`（`clipboard_job` 零初始化即 `CLIPBOARD_UNAVAILABLE`）。
#[test]
fn default_clipboard_value_is_unavailable() {
    assert_eq!(
        ClipboardValue::<String>::default(),
        ClipboardValue::Unavailable
    );
}

/// `complete_clipboard_work` 的 `status != 0` 文案是当前稳定契约（用户可见），改动需同步测试。
#[test]
fn cancelled_error_keeps_the_verbatim_message() {
    let error = ClipboardError::cancelled();
    assert_eq!(error.message, "Clipboard operation cancelled");
    assert_eq!(error.to_string(), "Clipboard operation cancelled");
    assert_eq!(
        ClipboardError::could_not_create_value().to_string(),
        "Could not create clipboard value"
    );
}

/// 没有编译任何后端时：helper、clipboard 都是 `None`，修饰键恒 `false`（`catch` 语义）。
#[test]
fn clipboard_is_unavailable_without_a_platform_backend() {
    if native_backend_compiled() {
        // 有后端：Linux 还要求 DISPLAY；darwin/win32 恒可用。两者都已由
        // `native_platform.rs` 覆盖，这里只断言 Linux 无 DISPLAY 的路径。
        if cfg!(all(target_os = "linux", feature = "x11")) && std::env::var_os("DISPLAY").is_none()
        {
            assert!(get_native_clipboard().is_none());
        }
        return;
    }
    assert!(get_native_platform_helper().is_none());
    assert!(get_native_clipboard().is_none());
    for key in [
        ModifierKey::Shift,
        ModifierKey::Command,
        ModifierKey::Control,
        ModifierKey::Option,
    ] {
        assert!(!is_native_modifier_pressed(key));
    }
}

/// `clipboard-worker.h`：`worker_busy` 时其它调用**立即**返回 `Unavailable`，不排队。
/// 这里用无 DISPLAY 的 Linux 环境：第一次读取占住 worker 后立即发起第二次读取。
#[cfg(all(target_os = "linux", feature = "x11"))]
#[tokio::test]
async fn a_busy_worker_returns_unavailable_without_queueing() {
    use kiana_tui::native::NativeClipboard;
    use kiana_tui::native::linux_x11::LinuxX11Clipboard;

    let clipboard = LinuxX11Clipboard::new();
    assert!(clipboard.is_ready(), "the worker is ready once constructed");

    // `get_text` 在返回 future 之前就执行 `begin_read`（等价于 C 的 `clipboard_execute` 前半段）。
    let first = clipboard.get_text();
    let second = clipboard.get_image();

    let started = std::time::Instant::now();
    let second_result = second.await;
    assert_eq!(second_result, Ok(ClipboardValue::Unavailable));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "a busy worker must not wait"
    );

    // 第一次读取最终完成（有 DISPLAY 时读真实 X11 剪贴板：Empty/Value/Unavailable 都合法）。
    let first_result = first.await;
    assert!(
        first_result.is_ok(),
        "the private reader must settle: {first_result:?}"
    );
    assert!(clipboard.is_ready());
}

/// TS 的 promise 与 executor 无关：`getText()`/`getImage()` 在**没有 Tokio runtime** 时也必须
/// 能正常推进（旧实现用 `tokio::time::timeout`，在 runtime 外 poll 会 panic）。
#[cfg(all(target_os = "linux", feature = "x11"))]
#[test]
fn clipboard_futures_do_not_require_a_tokio_runtime() {
    use kiana_tui::native::NativeClipboard;
    use kiana_tui::native::linux_x11::LinuxX11Clipboard;

    /// 用 noop waker 忙轮询（没有 Tokio runtime，也没有任何 executor），证明 future 自足。
    fn poll_without_runtime<F: std::future::Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let mut future = Box::pin(future);
        let started = std::time::Instant::now();
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(value) => return value,
                std::task::Poll::Pending => {
                    assert!(
                        started.elapsed() < std::time::Duration::from_secs(10),
                        "the clipboard future must settle without a Tokio time driver"
                    );
                    std::thread::yield_now();
                }
            }
        }
    }

    let clipboard = LinuxX11Clipboard::new();
    // 无 DISPLAY 时 worker 会以 Unavailable 结束；有 DISPLAY 时读真实 selection。两种情况都
    // 只验证 future 能在没有 Tokio time driver 的情况下 settle（不 panic）。
    assert!(poll_without_runtime(clipboard.get_text()).is_ok());
    assert!(poll_without_runtime(clipboard.get_image()).is_ok());
}
