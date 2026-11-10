//! `test/shell-path-exists.test.ts`（nodejs/node#36790：Windows Store 的可执行别名）。
//!
//! TS 用 `vi.mock` 替换 `accessSync` / `spawnSync`；Rust 用 `shell_config_with` /
//! `power_shell_config_with` 的注入缝。

use std::collections::HashMap;

use kiana_core::child_process::{SpawnSyncOptions, SpawnSyncResult};
use kiana_core::shell::{
    Platform, ShellConfigError, find_executable_on_path_with, power_shell_config_with,
    shell_config_with,
};

fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    move |key: &str| map.get(key).cloned().filter(|value| !value.is_empty())
}

fn spawn_result(status: Option<i32>, stdout: &str) -> SpawnSyncResult {
    SpawnSyncResult {
        status,
        stdout: stdout.to_string(),
        stderr: String::new(),
        error: None,
    }
}

const ALIASES_DIR: &str = "D:\\Users\\test\\AppData\\Local\\Microsoft\\WindowsApps";

// nodejs/node#36790: runnable Store aliases pass access(F_OK) but fail the extra stat
// check existsSync performs on Windows only.
#[test]
fn accepts_bash_exe_found_on_path() {
    let alias = format!("{ALIASES_DIR}\\bash.exe");
    let env = env_from(&[]);
    let config = shell_config_with(
        Platform::Win32,
        &env,
        &|_| true,
        &|name| (name == "bash.exe").then(|| alias.clone()),
        None,
    )
    .expect("config");
    assert_eq!(config.shell, alias);
}

#[test]
fn accepts_pwsh_exe_found_on_path() {
    let alias = format!("{ALIASES_DIR}\\pwsh.exe");
    let env = env_from(&[]);
    let config = power_shell_config_with(Platform::Win32, &env, &|_| true, &|name| {
        (name == "pwsh.exe").then(|| alias.clone())
    })
    .expect("config");
    assert_eq!(config.shell, alias);
}

#[test]
fn accepts_an_explicitly_configured_shell_alias() {
    let alias = format!("{ALIASES_DIR}\\bash.exe");
    let env = env_from(&[]);
    let config = shell_config_with(Platform::Win32, &env, &|_| true, &|_| None, Some(&alias))
        .expect("config");
    assert_eq!(config.shell, alias);
}

#[test]
fn still_rejects_path_results_that_fail_the_existence_check() {
    // TS：`where pwsh.exe` 返回 Store 别名但 accessSync 失败 → 拒绝；随后
    // `where powershell.exe` 的结果通过检查，被采用（test/shell-path-exists.test.ts:65-75）。
    let alias = format!("{ALIASES_DIR}\\pwsh.exe");
    let windows_power_shell = "E:\\WinNT\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
    let env = env_from(&[]);
    let exists = |path: &str| path == windows_power_shell;
    let spawn = |_command: &str, args: &[String], _options: &SpawnSyncOptions| {
        if args.first().map(String::as_str) == Some("pwsh.exe") {
            spawn_result(Some(0), &format!("{alias}\r\n"))
        } else {
            spawn_result(Some(0), windows_power_shell)
        }
    };

    // 被 where 返回但 existence 检查失败的路径必须被拒绝。
    assert_eq!(
        find_executable_on_path_with(Platform::Win32, "pwsh.exe", &exists, &spawn),
        None
    );

    let which = |name: &str| find_executable_on_path_with(Platform::Win32, name, &exists, &spawn);
    let config =
        power_shell_config_with(Platform::Win32, &env, &|_| false, &which).expect("config");
    assert_eq!(config.shell, windows_power_shell);
}

// TS 的 Unix 分支信任 `which` 的 stdout，不再做 access(F_OK) 复检。
#[test]
fn unix_path_lookup_trusts_which_output() {
    let spawn = |_command: &str, _args: &[String], _options: &SpawnSyncOptions| {
        spawn_result(Some(0), "/usr/local/bin/bash\n")
    };
    assert_eq!(
        find_executable_on_path_with(Platform::Linux, "bash", &|_| false, &spawn),
        Some("/usr/local/bin/bash".to_string())
    );
}

#[test]
fn empty_custom_shell_path_falls_back_to_the_platform_search() {
    // TS 的 `if (customShellPath)` 把空串当未设置。
    let env = env_from(&[]);
    let config = shell_config_with(
        Platform::Linux,
        &env,
        &|path| path == "/bin/bash",
        &|_| None,
        Some(""),
    )
    .expect("config");
    assert_eq!(config.shell, "/bin/bash");
    assert_eq!(config.args, vec!["-c".to_string()]);
}

#[test]
fn accepts_bin_bash_when_access_succeeds() {
    let env = env_from(&[]);
    let config = shell_config_with(
        Platform::Linux,
        &env,
        &|path| path == "/bin/bash",
        &|_| None,
        None,
    )
    .expect("config");
    assert_eq!(config.shell, "/bin/bash");
}

#[test]
fn falls_back_to_bash_on_path_when_bin_bash_fails_access() {
    let env = env_from(&[]);
    let config = shell_config_with(
        Platform::Linux,
        &env,
        &|path| path != "/bin/bash",
        &|name| (name == "bash").then(|| "/usr/local/bin/bash".to_string()),
        None,
    )
    .expect("config");
    assert_eq!(config.shell, "/usr/local/bin/bash");
}

#[test]
fn rejects_an_explicitly_configured_shell_path_that_fails_access() {
    let env = env_from(&[]);
    let error = shell_config_with(
        Platform::Linux,
        &env,
        &|_| false,
        &|_| None,
        Some("/missing/bash"),
    )
    .expect_err("must fail");
    assert_eq!(
        error,
        ShellConfigError::CustomShellPathNotFound("/missing/bash".to_string())
    );
    assert_eq!(
        error.to_string(),
        "Custom shell path not found: /missing/bash"
    );
}

#[test]
fn unix_falls_back_to_sh_when_no_bash_exists() {
    let env = env_from(&[]);
    let config =
        shell_config_with(Platform::Linux, &env, &|_| false, &|_| None, None).expect("config");
    assert_eq!(config.shell, "sh");
    assert_eq!(config.args, vec!["-c".to_string()]);
    assert_eq!(config.command_transport, None);
}
