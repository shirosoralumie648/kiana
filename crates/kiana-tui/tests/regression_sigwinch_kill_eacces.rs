//! `test/regression-sigwinch-kill-eacces.test.ts` 的移植（3 个 it）。
//!
//! TS 通过替换 `process.kill` 注入失败；Rust 侧用
//! [`refresh_terminal_dimensions_with`] 的注入闭包复现同一场景：刷新尺寸是 best-effort，
//! 任何 `kill(2)` 错误（EACCES/EPERM/ESRCH）都必须被吞掉，且 Windows 上不得调用 kill。

use std::cell::Cell;
use std::io;

use kiana_tui::terminal::{refresh_terminal_dimensions, refresh_terminal_dimensions_with};

/// `does not throw when kill(2) returns EACCES for self-signal`
#[test]
fn does_not_throw_when_kill_returns_eacces_for_self_signal() {
    let called = Cell::new(false);
    // 不 panic 即通过（TS 的 `assert.doesNotThrow`）。
    refresh_terminal_dimensions_with(&|| {
        called.set(true);
        Err(io::Error::from_raw_os_error(libc_eacces()))
    });
    if !cfg!(target_os = "windows") {
        assert!(called.get(), "POSIX must attempt the signal");
    }
}

/// `does not call kill on win32`
///
/// TS 用 `process.platform = "win32"` 模拟；Rust 的平台在编译期确定，所以同名断言按当前
/// 目标分别验证：Windows 上闭包不得被调用，POSIX 上必须被调用。
#[test]
fn does_not_call_kill_on_win32() {
    let kill_called = Cell::new(false);
    refresh_terminal_dimensions_with(&|| {
        kill_called.set(true);
        Ok(())
    });

    if cfg!(target_os = "windows") {
        assert!(!kill_called.get(), "kill should not be called on win32");
    } else {
        assert!(kill_called.get(), "POSIX platforms signal SIGWINCH");
    }
}

/// `preserves other error codes`
#[test]
fn preserves_other_error_codes() {
    // EPERM 同样被忽略：刷新是 best-effort（TS 的 `assert.doesNotThrow`）。
    refresh_terminal_dimensions_with(&|| Err(io::Error::from_raw_os_error(libc_eperm())));
    // 真实入口也不能 panic（POSIX 下真的向自身发 SIGWINCH；默认处置是忽略）。
    refresh_terminal_dimensions();
}

#[cfg(unix)]
fn libc_eacces() -> i32 {
    libc::EACCES
}

#[cfg(unix)]
fn libc_eperm() -> i32 {
    libc::EPERM
}

#[cfg(not(unix))]
fn libc_eacces() -> i32 {
    13
}

#[cfg(not(unix))]
fn libc_eperm() -> i32 {
    1
}
