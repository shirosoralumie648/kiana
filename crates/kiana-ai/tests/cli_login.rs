//! `cli.ts` 的可执行文件测试（离线，全部可运行）。
//!
//! `packages/ai` 的 `cli.ts` 没有对应的 vitest 文件；本测试覆盖 CLI 的可观察契约
//! （`spec/ai-core.md` §3.1 的逐字文本）：
//! - 无参数 / `help` / `--help` / `-h` → 用法 + provider 列表，退出码 0；
//! - `list` → 每行 `<id padEnd(20)> <name>`，只含 OAuth provider；
//! - 未知命令 → stderr `Error: Unknown command: <cmd>`，退出码 1；
//! - `login <unknown>` → stderr `Error: Unknown provider: <id>`，退出码 1；
//! - `login`（无 provider）的交互选择：打印编号列表与 `Enter number (1-9): `，越界选择
//!   与 TS 一样得到 `Unknown provider: `；
//! - `login` 在 stdin EOF 时：提示照常打印，进程以 0 退出、不打印错误、不写 `auth.json`
//!   （TS 的 `rl.question` 永不 resolve，事件循环耗尽后静默退出）；
//! - `node:readline` 的缓冲/close 语义（Node 26 实测）：没有 pending question 时到达的
//!   行被丢弃；接口观察到 EOF 后再提问 → `Error: readline was closed` + 退出码 1；
//!   在已结束的流上新建的接口则静默挂起（退出码 0）。
//!
//! 每个用例在独立的临时目录里运行（CLI 的 `auth.json` 是相对于 cwd 的）。

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct RunResult {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    /// 运行目录下 `auth.json` 的内容（不存在为 `None`）。
    auth_json: Option<String>,
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let dir = std::env::temp_dir().join(format!("kiana-ai-cli-{name}-{unique}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// 等待子进程结束（最多 60 秒，避免回归导致测试套件挂死）。
fn wait_with_timeout(mut child: Child) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            return child.wait_with_output().expect("wait for kiana-ai");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("kiana-ai did not exit within 60s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn run(name: &str, args: &[&str], stdin: Option<&str>) -> RunResult {
    let dir = temp_dir(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_kiana-ai"));
    command
        .args(args)
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().expect("spawn kiana-ai");
    if let Some(input) = stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        // 写完即关闭管道：模拟 EOF（TS 侧 `rl.question` 在 EOF 上永不 resolve）。
        drop(child.stdin.take());
    }
    let output = wait_with_timeout(child);
    let auth_json = std::fs::read_to_string(dir.join("auth.json")).ok();
    let _ = std::fs::remove_dir_all(&dir);
    RunResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
        auth_json,
    }
}

/// `list` 的每行：`${id.padEnd(20)} ${name}`（无缩进）。
fn expected_provider_lines() -> Vec<String> {
    kiana_ai::cli::oauth_providers()
        .iter()
        .map(|provider| format!("{} {}", pad_end_20(provider.id()), provider.name()))
        .collect()
}

/// usage 的 provider 行：`  ${id.padEnd(20)} ${name}`（两个前导空格）。
fn expected_usage_provider_lines() -> Vec<String> {
    expected_provider_lines()
        .into_iter()
        .map(|line| format!("  {line}"))
        .collect()
}

fn pad_end_20(text: &str) -> String {
    let length = text.encode_utf16().count();
    if length >= 20 {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(20 - length))
    }
}

#[test]
fn prints_usage_for_no_arguments_and_help_flags() {
    for args in [vec![], vec!["help"], vec!["--help"], vec!["-h"]] {
        let result = run("help", &args, None);
        assert_eq!(result.code, Some(0), "stderr: {}", result.stderr);
        let lines: Vec<&str> = result.stdout.lines().collect();
        assert_eq!(
            lines[0],
            "Usage: npx @earendil-works/kiana-ai <command> [provider]"
        );
        assert!(result.stdout.contains("Commands:"));
        assert!(
            result
                .stdout
                .contains("  login [provider]  Login to an OAuth provider")
        );
        assert!(
            result
                .stdout
                .contains("  list              List available providers")
        );
        assert!(result.stdout.contains("Providers:"));
        let expected = expected_usage_provider_lines();
        assert_eq!(expected.len(), 9);
        for line in expected {
            assert!(
                result.stdout.lines().any(|candidate| candidate == line),
                "missing provider line {line:?}\n{}",
                result.stdout
            );
        }
    }
}

#[test]
fn lists_only_oauth_providers_with_padded_ids() {
    let result = run("list", &["list"], None);
    assert_eq!(result.code, Some(0), "stderr: {}", result.stderr);
    let expected = expected_provider_lines();
    let actual: Vec<&str> = result.stdout.lines().collect();
    assert_eq!(actual, expected);
    // 非 OAuth provider（例如 amazon-bedrock）不出现在列表里。
    assert!(!result.stdout.contains("amazon-bedrock"));
    // 每行的 id 列宽 20（含一个分隔空格）。
    assert!(actual.iter().all(|line| line.len() > 21));
}

#[test]
fn unknown_command_reports_error_and_exits_one() {
    let result = run("unknown-command", &["nope"], None);
    assert_eq!(result.code, Some(1));
    assert_eq!(result.stderr, "Error: Unknown command: nope\n");
    assert!(result.stdout.is_empty());
}

#[test]
fn login_with_unknown_provider_reports_error_and_exits_one() {
    let result = run("unknown-provider", &["login", "not-a-provider"], None);
    assert_eq!(result.code, Some(1));
    assert_eq!(result.stderr, "Error: Unknown provider: not-a-provider\n");
}

#[test]
fn login_selection_prints_numbered_providers_and_rejects_out_of_range_choice() {
    let result = run("selection", &["login"], Some("99\n"));
    assert_eq!(result.code, Some(1), "stderr: {}", result.stderr);
    let providers = kiana_ai::cli::oauth_providers();
    for (index, provider) in providers.iter().enumerate() {
        let line = format!("  {}. {}", index + 1, provider.name());
        assert!(
            result.stdout.lines().any(|candidate| candidate == line),
            "missing selection line {line:?}\n{}",
            result.stdout
        );
    }
    assert!(
        result
            .stdout
            .contains(&format!("Enter number (1-{}): ", providers.len())),
        "missing prompt: {}",
        result.stdout
    );
    // TS: `Unknown provider: ${providerId ?? ""}`。
    assert_eq!(result.stderr, "Error: Unknown provider: \n");
}

#[test]
fn empty_command_prints_usage_like_no_argument() {
    // `args[0] === ""` 在 JS 里为假，同样进入 usage 分支。
    let result = run("empty-command", &[""], None);
    assert_eq!(result.code, Some(0), "stderr: {}", result.stderr);
    assert!(
        result
            .stdout
            .starts_with("Usage: npx @earendil-works/kiana-ai <command> [provider]")
    );
}

/// 回归（minor）：`kiana-ai login` 无参数且 stdin 已 EOF。
///
/// TS: `rl.question` 永不 resolve；事件循环耗尽后进程以 0 退出，stdout 只有编号列表与
/// 提示符，stderr 为空，且不会创建 `auth.json`（实测 `printf '' | node cli.ts login`）。
#[test]
fn login_selection_with_eof_stdin_exits_zero_without_error() {
    let result = run("selection-eof", &["login"], Some(""));
    assert_eq!(result.code, Some(0), "stderr: {}", result.stderr);
    assert_eq!(result.stderr, "");
    let providers = kiana_ai::cli::oauth_providers();
    for (index, provider) in providers.iter().enumerate() {
        let line = format!("  {}. {}", index + 1, provider.name());
        assert!(
            result.stdout.lines().any(|candidate| candidate == line),
            "missing selection line {line:?}\n{}",
            result.stdout
        );
    }
    assert!(
        result
            .stdout
            .contains(&format!("Enter number (1-{}): ", providers.len())),
        "missing prompt: {}",
        result.stdout
    );
    assert!(result.auth_json.is_none(), "auth.json must not be created");
}

/// 回归（minor）：登录流程停在第一个提示时 stdin EOF，同样静默以 0 退出。
///
/// anthropic 的登录先提示选择登录方式（不触网），因此这里可以离线验证 `answerPrompt`
/// 的 EOF 路径（实测 TS 输出相同并退出 0）。
#[test]
fn login_provider_prompt_with_eof_stdin_exits_zero_without_error() {
    let result = run("login-eof", &["login", "anthropic"], Some(""));
    assert_eq!(result.code, Some(0), "stderr: {}", result.stderr);
    assert_eq!(result.stderr, "");
    assert!(
        result.stdout.contains("Select Anthropic login method:"),
        "missing prompt: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("Enter number (1-2): "),
        "missing prompt: {}",
        result.stdout
    );
    assert!(result.auth_json.is_none(), "auth.json must not be created");
}

/// 回归（major）：接口观察到 stdin EOF 后再提问，抛 `ERR_USE_AFTER_CLOSE`。
///
/// TS 实测（`printf '2\n3\n' | node cli.ts login anthropic`）：方法选择读到 `2`，
/// 同一 chunk 里剩余的 `3` 被丢弃，stdin EOF 让接口 close；随后 copy-code 流程的
/// manual-code 提问同步抛错，顶层打印 `Error: readline was closed` 并以 1 退出——
/// 不打印 manual-code 提示、不交换 token、不写 `auth.json`。
#[test]
fn login_provider_prompt_after_stdin_eof_reports_readline_was_closed() {
    for input in ["2\n3\n", "2\n"] {
        let result = run("readline-closed", &["login", "anthropic"], Some(input));
        assert_eq!(result.code, Some(1), "input {input:?}: {}", result.stderr);
        assert_eq!(
            result.stderr, "Error: readline was closed\n",
            "input {input:?}"
        );
        assert!(
            result.stdout.contains("Select Anthropic login method:"),
            "input {input:?}: {}",
            result.stdout
        );
        assert!(
            result.stdout.contains("Enter number (1-2): "),
            "input {input:?}: {}",
            result.stdout
        );
        // copy-code 流程的 auth_url 通知照常打印（本地生成，不触网）。
        assert!(
            result.stdout.contains("Open this URL in your browser:"),
            "input {input:?}: {}",
            result.stdout
        );
        // 接口已 close：manual-code 提问既没有打印提示，也没有走到 token 交换。
        assert!(
            !result
                .stdout
                .contains("Paste the code Anthropic shows after you sign in:"),
            "input {input:?}: {}",
            result.stdout
        );
        assert!(
            !result
                .stdout
                .contains("Exchanging authorization code for tokens..."),
            "input {input:?}: {}",
            result.stdout
        );
        assert!(
            result.auth_json.is_none(),
            "input {input:?}: auth.json must not be created"
        );
    }
}

/// 回归（major）：接口 close 时缓冲的残留行被丢弃，不会流到下一次 `createInterface`。
///
/// TS 实测（`printf '1\n2\n' | node cli.ts login`）：第一个接口在读取 provider 编号时
/// 一并消费了 `2`（没有 pending question → 丢弃），`rl.close()` 后 login 新建的接口
/// 收不到任何行，方法选择提示后静默挂起，进程以 0 退出：没有 OAuth URL、没有
/// `auth.json`（旧实现会把残留的 `2` 当成方法选择，继续 copy-code 流程并打印 OAuth URL）。
#[test]
fn login_selection_discards_buffered_lines_when_the_interface_closes() {
    let result = run("selection-buffered", &["login"], Some("1\n2\n"));
    assert_eq!(result.code, Some(0), "stderr: {}", result.stderr);
    assert_eq!(result.stderr, "");
    assert!(
        result.stdout.contains(&format!(
            "Enter number (1-{}): ",
            kiana_ai::cli::oauth_providers().len()
        )),
        "{}",
        result.stdout
    );
    assert!(
        result.stdout.contains("Select Anthropic login method:"),
        "{}",
        result.stdout
    );
    // 残留的 `2` 没有回答 login 里的方法选择：输出停在提示符上。
    assert!(
        result.stdout.ends_with("Enter number (1-2): "),
        "{}",
        result.stdout
    );
    // 没有选中 copy-code / browser：不打印授权 URL。
    assert!(
        !result.stdout.contains("Open this URL in your browser:"),
        "{}",
        result.stdout
    );
    assert!(result.auth_json.is_none(), "auth.json must not be created");
}
