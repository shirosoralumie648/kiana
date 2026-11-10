//! `kiana-theme` 集成测试的共享辅助。
//!
//! 每个 `tests/*.rs` 是**独立进程**，但同一个测试二进制里的用例会并行执行并共享进程级
//! 全局状态（终端颜色、终端明暗、已注册主题、全局主题、JSON 校验器）。因此所有会改动这些
//! 全局量的用例都要先拿 [`lock`]，并在开始/结束处调用 [`reset_theme_state`]。
//!
//! 环境变量（`KIANA_CODING_AGENT_DIR`、`COLORFGBG`）也是进程级的，并且 edition 2024 里
//! `std::env::set_var` 是 `unsafe` 的：用 [`EnvGuard`] 在同一把锁下设置并在 drop 时还原。

#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use kiana_theme::{
    clear_theme_json_validator, init_theme, set_registered_themes, set_terminal_color_scheme,
    set_terminal_colors, set_theme_json_validator, validate_theme_json,
};
use kiana_tui::{TerminalCapabilities, TerminalColors};

/// 序列化所有改动进程级全局状态的测试。
pub fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 把主题子系统的全局状态恢复到测试起点（`initTheme("dark")` 是 TS 测试的 `afterEach`）。
pub fn reset_theme_state() {
    set_terminal_colors(&TerminalColors::default());
    set_terminal_color_scheme(None);
    set_registered_themes(Vec::new()).expect("registered themes reset");
    clear_theme_json_validator();
    init_theme(Some("dark"), false);
}

/// 安装 TS 生产路径使用的完整主题校验器（`main.ts` 的 `setThemeJsonValidator(validateThemeJson)`）。
pub fn install_default_validator() {
    set_theme_json_validator(Box::new(validate_theme_json));
}

/// 固定的终端颜色能力（避免依赖运行环境的 `TERM`/`COLORTERM`）。
pub fn set_truecolor(true_color: bool) {
    kiana_tui::set_capabilities(TerminalCapabilities {
        images: None,
        true_color,
        hyperlinks: false,
    });
}

/// 进程级环境变量的临时设置（drop 时还原）。
pub struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    /// 设置 `key=value`。
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: 所有修改环境变量的测试都在 [`lock`] 下串行执行。
        unsafe { std::env::set_var(key, value) };
        EnvGuard { key, previous }
    }

    /// 删除 `key`。
    pub fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: 同上。
        unsafe { std::env::remove_var(key) };
        EnvGuard { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: 同上。
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// 写一个主题文件，返回路径。
pub fn write_theme_file(dir: &Path, file_name: &str, content: &str) -> PathBuf {
    let path = dir.join(file_name);
    std::fs::write(&path, content).expect("theme file written");
    path
}

/// 内置 `dark.json` 的 JSON 值。
pub fn dark_json() -> serde_json::Value {
    serde_json::from_str(kiana_theme::DARK_JSON).expect("dark.json parses")
}

/// 内置 `light.json` 的 JSON 值。
pub fn light_json() -> serde_json::Value {
    serde_json::from_str(kiana_theme::LIGHT_JSON).expect("light.json parses")
}

/// 内置主题（按名字）。
pub fn builtin_json(name: &str) -> serde_json::Value {
    match name {
        "dark" => dark_json(),
        "light" => light_json(),
        other => panic!("unknown builtin theme {other}"),
    }
}
