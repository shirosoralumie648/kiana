//! `test/powershell-config.test.ts`（#9490：不假设盘符与 Windows 目录名）。

use std::collections::HashMap;

use kiana_core::shell::{POWERSHELL_ARGS, Platform, ShellConfigError, power_shell_config_with};

const PWSH7: &str = "D:\\Applications\\PowerShell\\7\\pwsh.exe";
const WINDOWS_POWERSHELL: &str = "E:\\WinNT\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";

fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    move |key: &str| map.get(key).cloned().filter(|value| !value.is_empty())
}

fn base_env() -> impl Fn(&str) -> Option<String> {
    env_from(&[
        ("ProgramFiles", "D:\\Applications"),
        ("SystemRoot", "E:\\WinNT"),
    ])
}

fn expected(shell: &str) -> (String, Vec<String>) {
    (
        shell.to_string(),
        POWERSHELL_ARGS.iter().map(|arg| arg.to_string()).collect(),
    )
}

#[test]
fn prefers_power_shell_7_on_path_over_known_installation_locations() {
    let shell = "E:\\Tools\\pwsh.exe";
    let config = power_shell_config_with(Platform::Win32, &base_env(), &|_| true, &|name| {
        (name == "pwsh.exe").then(|| shell.to_string())
    })
    .expect("config");
    assert_eq!((config.shell, config.args), expected(shell));
}

#[test]
fn finds_power_shell_7_under_program_files_before_power_shell_on_path() {
    let config = power_shell_config_with(
        Platform::Win32,
        &base_env(),
        &|path| path == PWSH7 || path == WINDOWS_POWERSHELL,
        &|name| (name == "powershell.exe").then(|| WINDOWS_POWERSHELL.to_string()),
    )
    .expect("config");
    assert_eq!((config.shell, config.args), expected(PWSH7));
}

#[test]
fn prefers_windows_power_shell_on_path_over_its_system_installation() {
    let shell = "E:\\Tools\\powershell.exe";
    let config = power_shell_config_with(
        Platform::Win32,
        &base_env(),
        &|path| path == shell || path == WINDOWS_POWERSHELL,
        &|name| (name == "powershell.exe").then(|| shell.to_string()),
    )
    .expect("config");
    assert_eq!((config.shell, config.args), expected(shell));
}

#[test]
fn finds_windows_power_shell_under_system_root_when_path_lookup_is_unavailable() {
    let config = power_shell_config_with(
        Platform::Win32,
        &base_env(),
        &|path| path == WINDOWS_POWERSHELL,
        &|_| None,
    )
    .expect("config");
    assert_eq!((config.shell, config.args), expected(WINDOWS_POWERSHELL));
}

#[test]
fn reports_an_error_when_no_power_shell_installation_exists() {
    let error = power_shell_config_with(Platform::Win32, &base_env(), &|_| false, &|_| None)
        .expect_err("must fail");
    assert_eq!(error, ShellConfigError::NoPowerShellExecutable);
    assert_eq!(
        error.to_string(),
        "No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH."
    );
}

#[test]
fn is_only_available_on_windows() {
    let error = power_shell_config_with(Platform::Linux, &base_env(), &|_| true, &|_| None)
        .expect_err("must fail");
    assert_eq!(error, ShellConfigError::PowerShellOnlyOnWindows);
    assert_eq!(
        error.to_string(),
        "The powershell tool is only available on Windows."
    );
}
