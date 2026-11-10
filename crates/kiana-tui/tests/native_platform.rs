//! `test/native-platform.test.ts` 的移植（4 个 TS `test(`）。
//!
//! TS 用 N-API 动态加载 `.node`；Rust 在编译期选定后端，因此：
//!
//! - 第 1 个测试针对随包分发的 linux-arm64 预编译产物，Rust 没有对应产物。这里把同样的
//!   ELF64 program-header 检查施加到 **当前可执行文件** 上，且只在 linux/aarch64 上有意义
//!   （其它目标上是 no-op，见 `remainingGaps`）。
//! - 其余 3 个测试按 TS 的 `skip` 条件在运行期跳过（平台/feature 不匹配时直接返回）。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_tui::native::{
    get_native_clipboard, get_native_module_candidates, get_native_platform_helper,
    module_path::native_backend_compiled,
};

/// `KIANA_PROGRAM_STATUS`/`DISPLAY` 之类的进程级环境变量：同一测试二进制内必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 最小 `block_on`：这些 future 在支持的平台上要么立刻完成、要么由后台线程唤醒并轮询。
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};

    let mut context = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

/// `Linux ARM64 prebuild supports 64 KB system pages`
///
/// TS 读取 `native/linux/prebuilds/linux-arm64/linux-platform-x11.node`（ELF64）并检查每个
/// `PT_LOAD` 段：`(p_vaddr - p_offset) % 65536 == 0` 且 `p_align >= 65536`。Rust 构建没有
/// `.node` 产物，等价的对象是构建出的可执行文件；只在 linux/aarch64 上执行该检查。
#[test]
fn linux_arm64_prebuild_supports_64_kb_system_pages() {
    if !cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        return;
    }
    let binary = std::fs::read(std::env::current_exe().expect("current exe"))
        .expect("read the current executable");
    assert_eq!(&binary[0..4], b"\x7fELF");

    let read_u16 = |offset: usize| u16::from_le_bytes([binary[offset], binary[offset + 1]]);
    let read_u32 = |offset: usize| {
        u32::from_le_bytes([
            binary[offset],
            binary[offset + 1],
            binary[offset + 2],
            binary[offset + 3],
        ])
    };
    let read_u64 = |offset: usize| {
        u64::from_le_bytes([
            binary[offset],
            binary[offset + 1],
            binary[offset + 2],
            binary[offset + 3],
            binary[offset + 4],
            binary[offset + 5],
            binary[offset + 6],
            binary[offset + 7],
        ])
    };

    let offset = read_u64(32) as usize; // e_phoff
    let entry_size = read_u16(54) as usize; // e_phentsize
    let count = read_u16(56) as usize; // e_phnum
    let mut load_segments = 0usize;
    for index in 0..count {
        let entry = offset + index * entry_size;
        if read_u32(entry) != 1 {
            continue;
        }
        load_segments += 1;
        let file_offset = read_u64(entry + 8);
        let address = read_u64(entry + 16);
        assert_eq!((address.wrapping_sub(file_offset)) % 65536, 0);
        assert!(read_u64(entry + 48) >= 65536);
    }
    assert!(load_segments > 0);
}

/// `writes Windows clipboard text through the native helper without command fallbacks`
///
/// TS 的 skip：`process.platform !== "win32" || KIANA_TEST_NATIVE_CLIPBOARD !== "1"`。
#[test]
fn writes_windows_clipboard_text_through_the_native_helper_without_command_fallbacks() {
    if !cfg!(target_os = "windows")
        || std::env::var_os("KIANA_TEST_NATIVE_CLIPBOARD").as_deref()
            != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    let clipboard = get_native_clipboard().expect("win32 backend with feature win32");
    assert!(clipboard.set_text("probe".to_string()).is_some());

    for text in ["clipboard café 日本語", "", "second write"] {
        let set = clipboard
            .set_text(text.to_string())
            .expect("win32 exports setText");
        block_on(set).expect("setText resolves");
        let value = block_on(clipboard.get_text()).expect("getText resolves");
        assert_eq!(value.into_option().as_deref(), Some(text));
        let image = block_on(clipboard.get_image()).expect("getImage resolves");
        assert_eq!(image, kiana_tui::native::ClipboardValue::Empty);
    }
}

/// `uses the native platform helper directly as the clipboard API`
///
/// TS 的 skip：只允许 darwin/win32 的 arm64/x64。
#[test]
fn uses_the_native_platform_helper_directly_as_the_clipboard_api() {
    if !(cfg!(any(target_os = "macos", target_os = "windows"))
        && (cfg!(target_arch = "aarch64") || cfg!(target_arch = "x86_64")))
    {
        return;
    }

    let helper = get_native_platform_helper().expect("platform helper on darwin/win32");
    // `typeof clipboard.getText/getImage/setText === "function"`：trait 方法恒存在；这里断言
    // 该平台确实导出了可写的 setText（TS 的两个平台都导出）。
    assert!(helper.set_text(String::new()).is_some());
    let clipboard = get_native_clipboard().expect("clipboard on a platform with a helper");
    assert!(
        Arc::ptr_eq(&clipboard, &helper),
        "getNativeClipboard() === getNativePlatformHelper()"
    );
    let again = get_native_clipboard().expect("cached clipboard");
    assert!(Arc::ptr_eq(&again, &clipboard), "the helper is cached");
}

/// `Linux loads X11 lazily and rechecks DISPLAY`
///
/// TS 通过替换 `process.platform` 与伪造 `require.cache` 模拟；Rust 直接操作 `DISPLAY`：
/// 每次 `getNativeClipboard()` 都重新检查，实例本身缓存。
#[test]
fn linux_loads_x11_lazily_and_rechecks_display() {
    if !cfg!(target_os = "linux") {
        return;
    }
    if !cfg!(feature = "x11") {
        // 未编译 X11 后端：与 TS 在没有 `.node` 产物时的 `undefined` 等价。
        assert!(get_native_clipboard().is_none());
        return;
    }

    let _lock = env_lock();
    let previous_display = std::env::var_os("DISPLAY");
    let previous_wayland = std::env::var_os("WAYLAND_DISPLAY");

    // Wayland 粘贴走 wl-paste：没有 DISPLAY 时 `undefined`。
    unsafe {
        std::env::remove_var("DISPLAY");
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
    }
    assert!(get_native_clipboard().is_none());

    // TS 用真值判断：`DISPLAY=""` 与未设置等价（`if (!process.env.DISPLAY) return undefined`）。
    unsafe { std::env::set_var("DISPLAY", "") };
    assert!(
        get_native_clipboard().is_none(),
        "an empty DISPLAY must behave like an unset one"
    );

    unsafe { std::env::set_var("DISPLAY", ":0") };
    let clipboard = get_native_clipboard().expect("DISPLAY=:0 creates the X11 helper");

    unsafe { std::env::set_var("DISPLAY", ":1") };
    let again = get_native_clipboard().expect("DISPLAY=:1 reuses the cached helper");
    assert!(
        Arc::ptr_eq(&clipboard, &again),
        "the helper instance is cached"
    );

    unsafe { std::env::remove_var("DISPLAY") };
    assert!(
        get_native_clipboard().is_none(),
        "DISPLAY is rechecked on every call"
    );

    unsafe {
        match previous_display {
            Some(value) => std::env::set_var("DISPLAY", value),
            None => std::env::remove_var("DISPLAY"),
        }
        match previous_wayland {
            Some(value) => std::env::set_var("WAYLAND_DISPLAY", value),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
    }
}

/// 未编译后端时 `native_backend_compiled()` 与 `getNativeClipboard()` 必须一致：两者都由同一编译期后端开关决定，出现分歧即是实现 bug。
#[test]
fn native_backend_compiled_matches_the_helper_availability() {
    if !native_backend_compiled() {
        assert!(get_native_platform_helper().is_none());
        if !cfg!(all(target_os = "linux", feature = "x11")) {
            assert!(get_native_clipboard().is_none());
        }
    }
}

/// `getNativeModuleCandidates` 在 Rust 侧仍保留同样的相对推导（真正的后端不走这条路）。
///
/// 返回值已经按 `node:path.join` 规范化（不再含 `..`），与 TS 的 `assert.deepEqual` 语义一致。
#[test]
fn native_module_candidates_still_derive_from_the_module_directory() {
    use kiana_tui::native::module_path::NativeModuleCandidateOptions;

    let options = NativeModuleCandidateOptions {
        module_dir: Some("/pkg/dist".into()),
        exec_path: Some("/usr/bin/kiana".into()),
        resolve_package: None,
    };
    let candidates = get_native_module_candidates(Path::new("native/x.node"), &options);
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0], Path::new("/pkg/native/x.node"));
    assert_eq!(candidates[1], Path::new("/pkg/dist/native/x.node"));
    assert_eq!(candidates[2], Path::new("/usr/bin/native/x.node"));
}
