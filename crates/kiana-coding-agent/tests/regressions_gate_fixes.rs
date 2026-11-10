//! 评审回归（gate kiana-coding-agent）：六处与 TS 参考实现不一致的启动期行为。
//!
//! 每个用例都对照 `packages/coding-agent/src` 的实际行为（见各测试注释里的行号），
//! 以进程级退出码 + 逐字 stdout/stderr 固定下来。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 一次子进程运行的结果。
struct ChildResult {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ChildResult {
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_root")
}

fn create_workspace() -> PathBuf {
    let root = tempfile::tempdir().expect("temp dir").keep();
    fs::create_dir_all(root.join("agent")).expect("agent dir");
    fs::create_dir_all(root.join("project")).expect("project dir");
    root
}

/// 构造 `kiana` 子进程（输出重定向到文件），返回命令与输出文件路径。
fn prepare_cli(
    args: &[&str],
    root: &Path,
    stdin: Stdio,
    name: &str,
) -> (Command, PathBuf, PathBuf) {
    let stdout_path = root.join(format!("{name}.out"));
    let stderr_path = root.join(format!("{name}.err"));
    let stdout = fs::File::create(&stdout_path).expect("stdout file");
    let stderr = fs::File::create(&stderr_path).expect("stderr file");
    let mut command = Command::new(env!("CARGO_BIN_EXE_kiana"));
    command
        .args(args)
        .current_dir(root.join("project"))
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", root)
        .env("KIANA_CODING_AGENT_DIR", root.join("agent"))
        .env("KIANA_PACKAGE_DIR", package_root())
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .env("KIANA_OFFLINE", "1")
        .env("FORCE_COLOR", "0")
        .env("TERM", "dumb")
        .stdin(stdin)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    (command, stdout_path, stderr_path)
}

/// 等待子进程退出（10 s 超时）并收集输出。
fn wait_for_child(
    mut child: std::process::Child,
    stdout_path: &Path,
    stderr_path: &Path,
) -> ChildResult {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("kiana did not exit within 10s");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    ChildResult {
        code: status.code().unwrap_or(-1),
        stdout: fs::read(stdout_path).unwrap_or_default(),
        stderr: fs::read(stderr_path).unwrap_or_default(),
    }
}

/// 运行 `kiana`；`stdin_bytes` 写入子进程 stdin 后立即关闭。
fn run_cli(args: &[&str], root: &Path, stdin_bytes: Option<&[u8]>, name: &str) -> ChildResult {
    let stdin = if stdin_bytes.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let (mut command, stdout_path, stderr_path) = prepare_cli(args, root, stdin, name);
    let mut child = command.spawn().expect("spawn kiana");
    if let Some(bytes) = stdin_bytes {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin.write_all(bytes).expect("write stdin");
        drop(stdin);
    }
    wait_for_child(child, &stdout_path, &stderr_path)
}

/// 运行 `kiana`，stdin 由后台线程按 `(bytes, delay_before_next_write)` 分阶段写入。
///
/// 用来复现“确认回答之后管道里仍有数据 / 写端仍未关闭”的时序：先写 `y\n`，等待
/// 一段时间后再写后续内容，最后关闭写端。
fn run_cli_staged_stdin(
    args: &[&str],
    root: &Path,
    chunks: Vec<(Vec<u8>, Duration)>,
    name: &str,
) -> ChildResult {
    let (mut command, stdout_path, stderr_path) = prepare_cli(args, root, Stdio::piped(), name);
    let mut child = command.spawn().expect("spawn kiana");
    let mut stdin = child.stdin.take().expect("piped stdin");
    std::thread::spawn(move || {
        for (bytes, delay) in chunks {
            std::thread::sleep(delay);
            if stdin.write_all(&bytes).is_err() {
                // 子进程提前退出时忽略 EPIPE。
                return;
            }
        }
    });
    wait_for_child(child, &stdout_path, &stderr_path)
}

fn write_settings(root: &Path, json: &str) {
    fs::write(root.join("agent/settings.json"), json).expect("settings");
}

/// `main.ts:876`：`settingsManager.getHttpIdleTimeoutMs()` 抛出的
/// `Invalid httpIdleTimeoutMs setting: <value>` 逃出 `main()`，Node 以未捕获异常退出 1；
/// Rust 曾用 `unwrap_or(DEFAULT)` 吞掉错误并继续打印 help。
#[test]
fn invalid_http_idle_timeout_setting_aborts_startup() {
    let root = create_workspace();
    write_settings(&root, r#"{"httpIdleTimeoutMs":"abc"}"#);
    let result = run_cli(&["--help"], &root, None, "http-idle");
    assert_eq!(result.code, 1);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    assert_eq!(
        result.stderr_text(),
        "Error: Invalid httpIdleTimeoutMs setting: abc\n"
    );
}

/// `main.ts:744-780`：`resolveProjectTrusted` 里的 `trustStore.get()` 抛错没有 try/catch，
/// `createRuntime` 的 promise 拒绝 → 退出 1。Rust 曾把它降级为 Warning 并继续启动。
#[test]
fn trust_store_read_error_aborts_startup_instead_of_warning() {
    let root = create_workspace();
    fs::write(root.join("agent/trust.json"), "{not-json").expect("trust.json");
    // `hasTrustRequiringProjectResources` 为真才会走 `resolveProjectTrust` 回调。
    fs::create_dir_all(root.join("project/.kiana/extensions")).expect("extensions dir");
    fs::write(
        root.join("project/.kiana/extensions/myext.ts"),
        "export default function () {}\n",
    )
    .expect("extension");

    let result = run_cli(&["--help"], &root, None, "trust-store");
    assert_eq!(result.code, 1);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    let stderr = result.stderr_text();
    assert!(
        stderr.starts_with("Error: Failed to read trust store "),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("Expected property name"),
        "stderr was: {stderr}"
    );
    assert!(
        !stderr.contains("Warning:"),
        "the error must not be downgraded to a warning: {stderr}"
    );
}

/// 同一个回调里的 `trustStore.set()`（`remember: true`）失败也必须中止启动。
///
/// 个人扩展在预信任阶段返回 `{trusted:"yes", remember:true}`，agent 目录只读使
/// `trust.json.lock` 无法创建：TS 的 `set` 抛 EACCES，进程退出 1、stdout 为空。
#[cfg(unix)]
#[test]
fn trust_store_write_error_aborts_startup() {
    use std::os::unix::fs::PermissionsExt;

    let root = create_workspace();
    let agent = root.join("agent");
    fs::create_dir_all(agent.join("extensions")).expect("agent extensions dir");
    fs::write(
        agent.join("extensions/trust_yes.ts"),
        "export default function (kiana) { kiana.on(\"project_trust\", async () => ({ trusted: \"yes\", remember: true })); }\n",
    )
    .expect("personal extension");
    fs::create_dir_all(root.join("project/.kiana/extensions")).expect("project extensions dir");
    fs::write(
        root.join("project/.kiana/extensions/myext.ts"),
        "export default function () {}\n",
    )
    .expect("project extension");

    let original_permissions = fs::metadata(&agent).expect("stat agent dir").permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_mode(0o555);
    fs::set_permissions(&agent, read_only).expect("chmod agent dir");

    // root 无视目录权限：此时该回归无法复现，跳过断言。
    let probe = agent.join(".write-probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&agent, original_permissions).expect("restore agent dir");
        return;
    }

    let result = run_cli(&["--help"], &root, None, "trust-write");
    fs::set_permissions(&agent, original_permissions).expect("restore agent dir");

    assert_eq!(result.code, 1);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    let stderr = result.stderr_text();
    assert!(
        stderr.starts_with("Error: EACCES: permission denied, mkdir '"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains("trust.json.lock"), "stderr was: {stderr}");
}

/// 在 agent 目录里放一个“属于其它项目”的会话，使 `--session <id>` 走 global → fork 询问。
fn write_foreign_session(root: &Path, other_cwd: &Path) {
    let dir = root.join("agent/sessions/other");
    fs::create_dir_all(&dir).expect("sessions dir");
    let header = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"abc123\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"{}\"}}\n",
        other_cwd.display()
    );
    fs::write(dir.join("2026-01-01T00-00-00-000Z_abc123.jsonl"), header).expect("session file");
}

fn session_file_count(root: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(&root.join("agent/sessions"), &mut count);
    count
}

/// `main.ts:286-297`：readline 的 `question()` 回调只在收到行终止符时触发。stdin 在 EOF
/// 前没有换行符时回调永不执行：既不打印 `Aborted.`，也不 fork（进程静默退出 0）。
#[test]
fn prompt_confirm_ignores_a_line_without_terminator() {
    let root = create_workspace();
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other dir");
    write_foreign_session(&root, &other);
    let before = session_file_count(&root);

    // `printf y`（无换行）：不 fork、不打印 Aborted.。
    let result = run_cli(
        &["--session", "abc123"],
        &root,
        Some(b"y"),
        "prompt-no-newline",
    );
    assert_eq!(result.code, 0);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    let stderr = result.stderr_text();
    assert!(
        stderr.contains("Session found in different project:"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains("Fork this session into current directory? [y/N] "));
    assert!(
        !stderr.contains("Aborted."),
        "an unterminated line must not be treated as an answer: {stderr}"
    );
    assert_eq!(
        session_file_count(&root),
        before,
        "no session must be forked"
    );

    // `printf 'y\n'`：正常回答 → fork（会话文件 +1）。
    let result = run_cli(
        &["--session", "abc123"],
        &root,
        Some(b"y\n"),
        "prompt-newline",
    );
    assert_eq!(result.code, 0);
    assert!(
        !result.stderr_text().contains("Aborted."),
        "stderr was: {}",
        result.stderr_text()
    );
    assert_eq!(
        session_file_count(&root),
        before + 1,
        "a terminated `y` must fork the session"
    );
}

/// `main.ts:285-297 + 897-905`：`promptConfirm` 之后 `readPipedStdin` 只在流尚未 EOF 时
/// 才会 resolve。写端在回答之后仍然打开、稍后才写入剩余内容时，TS 会继续启动流程并把
/// 剩余 stdin 当作初始消息；旧实现无条件按 `ClosedByPrompt` 静默退出 0，丢掉了
/// fork 之后的所有工作（连“没有模型/凭据”的错误都不打印）。
#[test]
fn prompt_confirm_continues_with_stdin_written_after_the_answer() {
    let root = create_workspace();
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other dir");
    write_foreign_session(&root, &other);
    let before = session_file_count(&root);

    let started = Instant::now();
    let result = run_cli_staged_stdin(
        &["--session", "abc123"],
        &root,
        vec![
            (b"y\n".to_vec(), Duration::ZERO),
            (b"hello\n".to_vec(), Duration::from_millis(1500)),
        ],
        "prompt-late-stdin",
    );
    let elapsed = started.elapsed();

    // fork 之后继续：没有可用模型/凭据时以 1 退出（与 TS 的
    // `No API key found for the selected model.` 一致）。
    assert_eq!(
        result.code,
        1,
        "TS continues after the fork; stderr was: {}",
        result.stderr_text()
    );
    // 进程必须等写端关闭（≈1.5 s）后才继续：证明剩余 stdin 真的被读取，
    // 而不是跳过读取直接进入后续步骤。
    assert!(
        elapsed >= Duration::from_millis(1400),
        "startup continued without waiting for the remaining stdin ({elapsed:?})"
    );
    let stderr = result.stderr_text();
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    assert!(stderr.contains("Fork this session into current directory? [y/N] "));
    assert!(
        stderr.contains("No API key found for the selected model."),
        "the remaining stdin must be consumed and startup must continue: {stderr}"
    );
    assert_eq!(
        session_file_count(&root),
        before + 1,
        "the fork itself must still happen"
    );
}

/// 同一个时序，但剩余 stdin 远大于管道缓冲（300 KB）：回答之后管道里仍有大量未读数据，
/// TS 的 `readPipedStdin` 会读尽剩余内容并继续（不是“EOF 已发生”）。
#[test]
fn prompt_confirm_continues_when_the_pipe_still_holds_data() {
    let root = create_workspace();
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other dir");
    write_foreign_session(&root, &other);

    let mut payload = b"y\n".to_vec();
    payload.extend(std::iter::repeat_n(b'x', 300_000));
    payload.push(b'\n');
    let result = run_cli_staged_stdin(
        &["--session", "abc123"],
        &root,
        vec![(payload, Duration::ZERO)],
        "prompt-large-stdin",
    );

    assert_eq!(result.code, 1, "stderr was: {}", result.stderr_text());
    assert!(
        result
            .stderr_text()
            .contains("No API key found for the selected model."),
        "stderr was: {}",
        result.stderr_text()
    );
}

/// `readline` 会把首个数据块里行终止符之后的字节留在内部缓冲，`rl.close()` 时丢弃；
/// `printf 'y\nhello\n'` 的 "hello\n" 与回答同块到达，因此 `readPipedStdin` 看不到它，
/// 流已 EOF → TS 静默退出 0。逐字节读取会把 "hello" 泄漏成初始消息，必须避免。
#[test]
fn prompt_confirm_discards_the_rest_of_the_first_readline_chunk() {
    let root = create_workspace();
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other dir");
    write_foreign_session(&root, &other);
    let before = session_file_count(&root);

    let result = run_cli(
        &["--session", "abc123"],
        &root,
        Some(b"y\nhello\n"),
        "prompt-same-chunk",
    );

    assert_eq!(
        result.code,
        0,
        "TS exits silently after the readline close; stderr was: {}",
        result.stderr_text()
    );
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    let stderr = result.stderr_text();
    assert!(
        !stderr.contains("No API key found") && !stderr.contains("Aborted."),
        "the same-chunk remainder must be discarded, not used as a prompt: {stderr}"
    );
    assert_eq!(session_file_count(&root), before + 1);
}

/// `session-manager.ts:1932-1952`：`listAll("")` 里 `customSessionDir` 按 JS 真值判断，
/// 空串（settings.json 的 `{"sessionDir":""}`）回退到默认全局 sessions 目录；旧实现把
/// `Some("")` 传给 `list_all_in`，扫描空路径得到空列表 → “No session found matching”。
#[test]
fn empty_settings_session_dir_still_scans_all_projects_for_session_ids() {
    let root = create_workspace();
    let other = root.join("other");
    fs::create_dir_all(&other).expect("other dir");
    write_settings(&root, r#"{"sessionDir":""}"#);
    write_foreign_session(&root, &other);

    // `--session`：全局命中 → 询问是否 fork；stdin 为 /dev/null 时静默退出 0。
    let result = run_cli(&["--session", "abc123"], &root, None, "empty-dir-session");
    assert_eq!(result.code, 0, "stderr was: {}", result.stderr_text());
    let stderr = result.stderr_text();
    assert!(
        stderr.contains("Session found in different project:"),
        "global lookup did not run: {stderr}"
    );
    assert!(
        !stderr.contains("No session found matching"),
        "list_all_in(\"\") was used: {stderr}"
    );

    // `--fork`：全局命中 → 直接 fork（无询问）。stdin 为 /dev/null 时 print 模式没有
    // 初始消息，TS 也静默退出 0；关键是必须真的 fork，而不是报 “No session found”。
    let before = session_file_count(&root);
    let result = run_cli(&["--fork", "abc123"], &root, None, "empty-dir-fork");
    assert_eq!(result.code, 0, "stderr was: {}", result.stderr_text());
    let stderr = result.stderr_text();
    assert!(
        !stderr.contains("No session found matching"),
        "list_all_in(\"\") was used: {stderr}"
    );
    assert_eq!(
        session_file_count(&root),
        before + 1,
        "the global --fork must succeed; stderr was: {stderr}"
    );
}

/// `main.ts:183-200`：`if (!credential)` 与 `credential ? {...} : {}` 都按 JS 真值判断，
/// 返回的空凭据是假值 → `not_ready` / 省略 `credentials` / 退出 1；非 `--json` 仍打印空行。
#[test]
fn auth_check_treats_an_empty_credential_as_missing() {
    let root = create_workspace();
    fs::write(
        root.join("agent/auth.json"),
        r#"{"openai-codex":{"type":"oauth","access":"","refresh":"x","expires":4102444800000}}"#,
    )
    .expect("auth.json");

    let json = run_cli(
        &[
            "auth",
            "check",
            "--provider",
            "openai-codex",
            "--no-refresh",
            "--credentials",
            "--json",
        ],
        &root,
        None,
        "auth-empty-json",
    );
    assert_eq!(json.code, 1);
    assert_eq!(
        json.stdout_text(),
        "{\"status\":\"not_ready\",\"provider\":\"openai-codex\",\"reason\":\"credential_not_available\"}\n"
    );
    assert!(json.stderr.is_empty());

    let plain = run_cli(
        &[
            "auth",
            "check",
            "--provider",
            "openai-codex",
            "--no-refresh",
            "--credentials",
        ],
        &root,
        None,
        "auth-empty-plain",
    );
    assert_eq!(plain.code, 1);
    assert_eq!(plain.stdout_text(), "\n");
    assert!(plain.stderr.is_empty());
}

/// `session-manager.ts:1009,1080`：`normalizePath(sessionDir)` 之后用 Node `path.join`
/// 拼会话文件名；`join` 会消掉 `.` 段（`./rel_sess` → `rel_sess`）。Rust 的
/// `PathBuf::push` 会保留 `./`，导致 RPC `get_state.sessionFile` 多出前缀。
#[test]
fn rpc_session_file_path_is_lexically_normalized() {
    let root = create_workspace();
    let result = run_cli(
        &["--mode", "rpc", "--session-dir", "./rel_sess"],
        &root,
        Some(b"{\"type\":\"get_state\",\"id\":\"1\"}\n"),
        "rpc-session-dir",
    );
    assert_eq!(result.code, 0);
    let stdout = result.stdout_text();
    assert!(
        stdout.contains("\"sessionFile\":\"rel_sess/"),
        "sessionFile must be normalized: {stdout}"
    );
    assert!(
        !stdout.contains("\"sessionFile\":\"./rel_sess/"),
        "sessionFile kept a `./` prefix: {stdout}"
    );
    assert_eq!(result.stderr_text(), "");
}

/// `package-manager.ts:946` / `324`：`for (const pkg of settings.packages ?? [])` 与
/// `splitPatterns(entries)` 对不可迭代值抛出的 V8 TypeError（Rust 曾静默回退为空）。
#[test]
fn non_iterable_settings_values_abort_startup() {
    let root = create_workspace();
    write_settings(&root, r#"{"packages":42}"#);
    let packages = run_cli(&["--help"], &root, None, "packages-42");
    assert_eq!(packages.code, 1);
    assert!(packages.stdout.is_empty(), "stdout must stay empty");
    assert_eq!(
        packages.stderr_text(),
        "Error: number 42 is not iterable (cannot read property Symbol(Symbol.iterator))\n"
    );

    let root = create_workspace();
    write_settings(&root, r#"{"extensions":42}"#);
    let extensions = run_cli(&["--help"], &root, None, "extensions-42");
    assert_eq!(extensions.code, 1);
    assert!(extensions.stdout.is_empty(), "stdout must stay empty");
    assert_eq!(extensions.stderr_text(), "Error: entries is not iterable\n");

    // 对象值：V8 的值描述只写 `object`。
    let root = create_workspace();
    write_settings(&root, r#"{"packages":{"a":1}}"#);
    let packages_object = run_cli(&["--help"], &root, None, "packages-object");
    assert_eq!(packages_object.code, 1);
    assert!(packages_object.stdout.is_empty());
    assert_eq!(
        packages_object.stderr_text(),
        "Error: object is not iterable (cannot read property Symbol(Symbol.iterator))\n"
    );
}
