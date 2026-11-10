//! `test/windows-shell-config.test.ts`（#9490：按真实的 Windows 目录识别 legacy WSL 启动器）。

use std::collections::HashMap;

use kiana_core::shell::{CommandTransport, Platform, shell_config_with};

fn env_from(system_root: Option<&str>) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = system_root
        .map(|root| [("SystemRoot".to_string(), root.to_string())])
        .into_iter()
        .flatten()
        .collect();
    move |key: &str| map.get(key).cloned().filter(|value| !value.is_empty())
}

fn config_for(system_root: Option<&str>, shell: &str) -> kiana_core::shell::ShellConfig {
    shell_config_with(
        Platform::Win32,
        &env_from(system_root),
        // accessSync 永远成功（与 windows-shell-config.test.ts 的 mock 一致）。
        &|_| true,
        &|_| None,
        Some(shell),
    )
    .expect("config")
}

#[test]
fn resolves_legacy_wsl_launchers_to_stdin_transport() {
    let cases: &[(Option<&str>, &str, bool)] = &[
        (Some("D:\\WinNT\\"), "d:/WINNT/System32/BASH.EXE", true),
        (Some("D:\\WinNT"), "D:\\WinNT\\Sysnative\\bash.exe", true),
        (None, "E:\\Windows\\System32\\bash.exe", true),
        (Some("D:\\WinNT"), "D:\\Git\\bin\\bash.exe", false),
        (Some("D:\\WinNT"), "E:\\Windows\\System32\\bash.exe", false),
        (Some("WinNT"), "C:\\Windows\\System32\\bash.exe", false),
    ];
    for (system_root, shell, expect_stdin) in cases {
        let config = config_for(*system_root, shell);
        assert_eq!(config.shell, *shell, "shell for {system_root:?}");
        if *expect_stdin {
            assert_eq!(
                config.args,
                vec!["-s".to_string()],
                "args for {system_root:?} / {shell:?}"
            );
            assert_eq!(config.command_transport, Some(CommandTransport::Stdin));
        } else {
            assert_eq!(
                config.args,
                vec!["-c".to_string()],
                "args for {system_root:?} / {shell:?}"
            );
            assert_eq!(config.command_transport, None);
        }
    }
}

#[test]
fn git_bash_in_program_files_is_preferred() {
    let env = |key: &str| match key {
        "ProgramFiles" => Some("C:\\Program Files".to_string()),
        _ => None,
    };
    let config = shell_config_with(
        Platform::Win32,
        &env,
        &|path| path == "C:\\Program Files\\Git\\bin\\bash.exe",
        &|_| panic!("should not search PATH when ProgramFiles has Git Bash"),
        None,
    )
    .expect("config");
    assert_eq!(config.shell, "C:\\Program Files\\Git\\bin\\bash.exe");
    assert_eq!(config.args, vec!["-c".to_string()]);
}

#[test]
fn program_files_x86_is_searched_second() {
    let env = |key: &str| match key {
        "ProgramFiles" => Some("C:\\Program Files".to_string()),
        "ProgramFiles(x86)" => Some("C:\\Program Files (x86)".to_string()),
        _ => None,
    };
    let config = shell_config_with(
        Platform::Win32,
        &env,
        &|path| path == "C:\\Program Files (x86)\\Git\\bin\\bash.exe",
        &|_| None,
        None,
    )
    .expect("config");
    assert_eq!(config.shell, "C:\\Program Files (x86)\\Git\\bin\\bash.exe");
}

#[test]
fn no_bash_mentions_every_searched_location() {
    let env = |key: &str| match key {
        "ProgramFiles" => Some("C:\\Program Files".to_string()),
        "ProgramFiles(x86)" => Some("C:\\Program Files (x86)".to_string()),
        _ => None,
    };
    let error = shell_config_with(Platform::Win32, &env, &|_| false, &|_| None, None)
        .expect_err("must fail");
    assert_eq!(
        error.to_string(),
        "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n  C:\\Program Files\\Git\\bin\\bash.exe\n  C:\\Program Files (x86)\\Git\\bin\\bash.exe"
    );
}
