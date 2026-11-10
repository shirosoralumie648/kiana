//! `utils/wsl.ts` 的移植测试（TS 无对应测试；按 §4.8 第 9 条覆盖全部公开 API）。
//!
//! `is_wsl` 是 `ENV` 可注入的：环境变量分支完全确定；`/proc/version` 分支在非 WSL 主机上
//! 读取失败或匹配不到，测试按“与直接读取 /proc/version 的结果一致”断言，兼容 WSL 主机。

use std::ffi::OsString;

use kiana_core::changelog::is_wsl;

fn entries(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
}

/// 直接复刻 TS 的回退分支：读 `/proc/version` 并匹配 `(?i)microsoft|wsl`。
fn proc_version_matches() -> bool {
    let pattern = regex::Regex::new("(?i)microsoft|wsl").expect("valid regex");
    match std::fs::read_to_string("/proc/version") {
        Ok(release) => pattern.is_match(&release),
        Err(_) => false,
    }
}

#[test]
fn detects_wsl_from_distro_name() {
    let env = entries(&[("WSL_DISTRO_NAME", "Ubuntu")]);
    assert!(is_wsl(Some(&env)));
}

#[test]
fn detects_wsl_from_wsl_env() {
    let env = entries(&[("WSLENV", "PATH/l")]);
    assert!(is_wsl(Some(&env)));
}

#[test]
fn ignores_empty_markers_and_falls_back_to_proc_version() {
    let env = entries(&[("WSL_DISTRO_NAME", ""), ("WSLENV", "")]);
    assert_eq!(is_wsl(Some(&env)), proc_version_matches());
}

#[test]
fn falls_back_to_proc_version_without_markers() {
    assert_eq!(is_wsl(Some(&[])), proc_version_matches());
    assert_eq!(is_wsl(None), proc_version_matches());
}

#[test]
fn uses_the_process_environment_when_env_is_not_injected() {
    let process_marker = std::env::var_os("WSL_DISTRO_NAME")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
        || std::env::var_os("WSLENV")
            .map(|value| !value.is_empty())
            .unwrap_or(false);
    assert_eq!(is_wsl(None), process_marker || proc_version_matches());
}

#[test]
fn only_checks_the_two_marker_variables() {
    let env = entries(&[("SOME_OTHER_VAR", "microsoft")]);
    assert_eq!(is_wsl(Some(&env)), proc_version_matches());
}
