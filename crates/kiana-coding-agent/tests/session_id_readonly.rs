//! `test/session-id-readonly.test.ts`：`--session-id` 的只读/持久化语义。
//!
//! 回归点（逐条对应 TS）：
//! - 元数据命令（`--help`/`--list-models`/`--no-session`）使用内存会话，不落盘；
//! - 缺失的 ID 会创建会话并打印 `creating a new session` 警告；已存在则静默重开；
//! - 精确 ID 查找只读会话头（不构建完整列表）；
//! - 重命名后的会话文件仍能按 ID 重开；
//! - 自定义会话目录中按 cwd 过滤；
//! - fork 目标已存在时进程内以 1 退出。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{Message, UserContent, UserMessage, UserRole};
use kiana_coding_agent::app::create_session_manager;
use kiana_coding_agent::cli::args::Args;
use kiana_coding_agent::cli::setup::Output;
use kiana_config::settings::{Settings, SettingsManager};
use kiana_session_core::session_manager::SessionManager;

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_root")
}

fn create_temp_dir() -> PathBuf {
    // 与 TS 一样取物理路径：会话 cwd 过滤是文本比较，`/tmp` 在部分平台是符号链接。
    let root = tempfile::tempdir().expect("temp dir").keep();
    fs::canonicalize(&root).expect("canonicalize")
}

fn args_with(overrides: impl FnOnce(&mut Args)) -> Args {
    let mut args = Args::default();
    overrides(&mut args);
    args
}

fn persist_session(session: &mut SessionManager, content: &str) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    session
        .append_message(AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text(content.to_string()),
            timestamp,
        })))
        .expect("append message");
}

fn has_session_with_id(root: &Path, session_id: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && has_session_with_id(&path, session_id) {
            return true;
        }
        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(first_line) = content.split('\n').next() else {
            continue;
        };
        let Ok(header) = serde_json::from_str::<serde_json::Value>(first_line) else {
            continue;
        };
        if header.get("type").and_then(|value| value.as_str()) == Some("session")
            && header.get("id").and_then(|value| value.as_str()) == Some(session_id)
        {
            return true;
        }
    }
    false
}

#[test]
fn does_not_persist_a_custom_id_for_metadata_commands() {
    let root = create_temp_dir();
    let project = root.join("project");
    let agent = root.join("agent");
    fs::create_dir_all(&project).expect("project");
    fs::create_dir_all(&agent).expect("agent");
    let stdout_path = root.join("out");
    let stderr_path = root.join("err");
    let stdout = fs::File::create(&stdout_path).expect("out");
    let stderr = fs::File::create(&stderr_path).expect("err");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kiana"))
        .args(["--session-id", "read-only-help", "--help"])
        .current_dir(&project)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &root)
        .env("KIANA_CODING_AGENT_DIR", &agent)
        .env("KIANA_PACKAGE_DIR", package_root())
        .env("KIANA_OFFLINE", "1")
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .env("FORCE_COLOR", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn kiana");
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

    assert_eq!(status.code(), Some(0));
    assert!(!has_session_with_id(
        &agent.join("sessions"),
        "read-only-help"
    ));
}

#[tokio::test]
async fn creates_missing_ids_and_reopens_existing_ids_in_process() {
    let root = create_temp_dir();
    let project = root.join("project");
    let session_dir = root.join("sessions");
    fs::create_dir_all(&project).expect("project");
    let settings = SettingsManager::in_memory(Settings::default());

    let read_only_output = Output::capture();
    let read_only = create_session_manager(
        &args_with(|args| {
            args.session_id = Some("read-only".to_string());
            args.help = true;
        }),
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        &settings,
        &read_only_output,
    )
    .await
    .expect("read-only manager");
    assert_eq!(read_only.get_session_id(), "read-only");
    assert!(read_only.get_session_file().is_none());

    let create_output = Output::capture();
    let mut created = create_session_manager(
        &args_with(|args| args.session_id = Some("persisted-id".to_string())),
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        &settings,
        &create_output,
    )
    .await
    .expect("created manager");
    persist_session(&mut created, "persist me");
    assert!(
        create_output
            .captured_stderr()
            .contains("creating a new session"),
        "expected warning, got: {}",
        create_output.captured_stderr()
    );

    let reopen_output = Output::capture();
    let reopened = create_session_manager(
        &args_with(|args| args.session_id = Some("persisted-id".to_string())),
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        &settings,
        &reopen_output,
    )
    .await
    .expect("reopened manager");
    assert_eq!(reopened.get_session_file(), created.get_session_file());
    assert_eq!(reopen_output.captured_stderr(), "");
}

#[tokio::test]
async fn looks_up_exact_ids_without_building_full_session_listings() {
    let root = create_temp_dir();
    let project = root.join("project");
    let session_dir = root.join("sessions");
    fs::create_dir_all(&project).expect("project");

    // TS 用 `vi.spyOn(SessionManager, "list")` 拒绝列表调用并断言它从未发生；Rust 的
    // `SessionManager::list` 是静态函数、没有注入点，`kiana-session-core` 为此提供
    // `listing_call_probe`（test hook）：下面断言调用前后 `list`/`list_all` 的计数不变。
    // 同时保留可观察的语义断言：精确查找必须命中**相同** ID 的会话，而不是按前缀命中列表里
    // 的第一条（列表按文件名倒序，后创建的长 ID 会话会排在前面）。
    let mut longer = SessionManager::create(
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("prefix-id-longer".to_string()),
            parent_session: None,
        }),
    )
    .expect("longer session");
    persist_session(&mut longer, "large transcript contents must not be loaded");

    let mut exact = SessionManager::create(
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("prefix-id".to_string()),
            parent_session: None,
        }),
    )
    .expect("exact session");
    persist_session(&mut exact, "exact transcript");

    // 目录里的两个会话都匹配 `prefix-id` 前缀；只有精确查找能选中 `prefix-id` 本身。
    let output = Output::capture();
    let probe_before = kiana_session_core::session_manager::listing_call_probe::counts();
    let created = create_session_manager(
        &args_with(|args| args.session_id = Some("prefix-id".to_string())),
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        &SettingsManager::in_memory(Settings::default()),
        &output,
    )
    .await
    .expect("exact-id session");
    let probe_after = kiana_session_core::session_manager::listing_call_probe::counts();
    assert_eq!(
        probe_after, probe_before,
        "exact-id lookup must not build the full session listing (SessionManager::list call counts changed)"
    );
    assert_eq!(created.get_session_id(), "prefix-id");
    assert_eq!(
        created.get_session_file().map(Path::new),
        exact.get_session_file().map(Path::new)
    );
    assert_ne!(
        created.get_session_file().map(Path::new),
        longer.get_session_file().map(Path::new)
    );
    assert_eq!(output.captured_stderr(), "");
}

#[tokio::test]
async fn reopens_an_exact_id_from_a_renamed_session_file() {
    let root = create_temp_dir();
    let project = root.join("project");
    let session_dir = root.join("sessions");
    fs::create_dir_all(&project).expect("project");
    let mut original = SessionManager::create(
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("renamed-id".to_string()),
            parent_session: None,
        }),
    )
    .expect("original session");
    persist_session(&mut original, "persist me");
    let renamed_path = session_dir.join("imported-session.jsonl");
    fs::rename(
        original.get_session_file().expect("session file"),
        &renamed_path,
    )
    .expect("rename");

    let reopened = create_session_manager(
        &args_with(|args| args.session_id = Some("renamed-id".to_string())),
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        &SettingsManager::in_memory(Settings::default()),
        &Output::capture(),
    )
    .await
    .expect("reopened session");
    assert_eq!(
        reopened.get_session_file().map(Path::new),
        Some(renamed_path.as_path())
    );
}

#[test]
fn filters_exact_ids_by_cwd_in_a_custom_session_directory() {
    let root = create_temp_dir();
    let project_a = root.join("project-a");
    let project_b = root.join("project-b");
    let session_dir = root.join("sessions");
    fs::create_dir_all(&project_a).expect("project a");
    fs::create_dir_all(&project_b).expect("project b");
    let mut foreign = SessionManager::create(
        &project_b.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("foreign-id".to_string()),
            parent_session: None,
        }),
    )
    .expect("foreign session");
    persist_session(&mut foreign, "foreign session");

    assert_eq!(
        SessionManager::find_by_id(
            &project_a.to_string_lossy(),
            "foreign-id",
            Some(&session_dir.to_string_lossy()),
        )
        .expect("find by id"),
        None
    );
    assert_eq!(
        SessionManager::find_by_id(
            &project_b.to_string_lossy(),
            "foreign-id",
            Some(&session_dir.to_string_lossy()),
        )
        .expect("find by id"),
        foreign.get_session_file().map(str::to_string)
    );
}

#[tokio::test]
async fn rejects_an_existing_fork_target_in_process() {
    let root = create_temp_dir();
    let project = root.join("project");
    let session_dir = root.join("sessions");
    fs::create_dir_all(&project).expect("project");
    let mut source = SessionManager::create(
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("source-id".to_string()),
            parent_session: None,
        }),
    )
    .expect("source session");
    persist_session(&mut source, "source");
    let mut target = SessionManager::create(
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("existing-id".to_string()),
            parent_session: None,
        }),
    )
    .expect("target session");
    persist_session(&mut target, "target");

    let output = Output::capture();
    let error = create_session_manager(
        &args_with(|args| {
            args.fork = Some("source-id".to_string());
            args.session_id = Some("existing-id".to_string());
        }),
        &project.to_string_lossy(),
        Some(&session_dir.to_string_lossy()),
        &SettingsManager::in_memory(Settings::default()),
        &output,
    )
    .await
    .expect_err("fork target must be rejected");
    assert_eq!(error.code, 1);
    assert!(
        output
            .captured_stderr()
            .contains("Session already exists with id 'existing-id'")
    );
}

// ---------------------------------------------------------------------------
// `KIANA_CODING_AGENT_SESSION_DIR` 的空串语义（TS: `envSessionDir ? ... : undefined`）
// ---------------------------------------------------------------------------

/// 运行一次 `kiana` 子进程并返回 `(退出码, stdout, stderr)`。
fn run_kiana(
    project: &Path,
    agent: &Path,
    envs: &[(&str, &str)],
    args: &[&str],
) -> (i32, String, String) {
    let stdout_path = project.join("kiana-test.out");
    let stderr_path = project.join("kiana-test.err");
    let stdout = fs::File::create(&stdout_path).expect("stdout file");
    let stderr = fs::File::create(&stderr_path).expect("stderr file");
    let mut command = Command::new(env!("CARGO_BIN_EXE_kiana"));
    command
        .args(args)
        .current_dir(project)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", project)
        .env("KIANA_CODING_AGENT_DIR", agent)
        .env("KIANA_PACKAGE_DIR", package_root())
        .env("KIANA_OFFLINE", "1")
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .env("FORCE_COLOR", "0")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn kiana");
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
    (
        status.code().unwrap_or(-1),
        fs::read_to_string(&stdout_path).unwrap_or_default(),
        fs::read_to_string(&stderr_path).unwrap_or_default(),
    )
}

/// 默认会话目录：`<agent>/sessions/--<cwd 去掉前导 / 并把 / \ : 换成 ->--`。
fn default_session_dir(agent: &Path, cwd: &Path) -> PathBuf {
    let stripped = cwd
        .to_string_lossy()
        .strip_prefix('/')
        .map(str::to_string)
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    let encoded: String = stripped
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect();
    agent.join("sessions").join(format!("--{encoded}--"))
}

fn write_session_header(dir: &Path, id: &str, cwd: &Path) {
    fs::create_dir_all(dir).expect("session dir");
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": cwd.to_string_lossy(),
    });
    fs::write(dir.join(format!("{id}.jsonl")), format!("{header}\n")).expect("session file");
}

/// 回归：导出的空 `KIANA_CODING_AGENT_SESSION_DIR` 必须等同于未设置（TS 的真值判断），
/// 回退到设置里的 `sessionDir`，而不是短路成 `Some("")`。
#[test]
fn empty_session_dir_env_falls_back_to_the_settings_session_dir() {
    let root = create_temp_dir();
    let project = root.join("project");
    let agent = root.join("agent");
    let custom = root.join("custom-sessions");
    fs::create_dir_all(&project).expect("project");
    fs::create_dir_all(&agent).expect("agent");
    fs::write(
        agent.join("settings.json"),
        format!(
            "{{\"sessionDir\":{}}}",
            serde_json::to_string(&custom.to_string_lossy()).expect("json")
        ),
    )
    .expect("settings");
    write_session_header(&custom, "custom-id", &project);

    let (code, _stdout, stderr) = run_kiana(
        &project,
        &agent,
        &[("KIANA_CODING_AGENT_SESSION_DIR", "")],
        &["--session-id", "custom-id", "-p", "hi"],
    );

    // 找到既有会话后不会打印 “creating a new session” 警告；随后因没有模型以 1 退出。
    assert_eq!(code, 1, "stderr was: {stderr}");
    assert!(
        !stderr.contains("creating a new session"),
        "settings sessionDir was ignored: {stderr}"
    );
}

/// 回归：空 `KIANA_CODING_AGENT_SESSION_DIR` 时全局 `--session` 查找仍走 `listAll()`
/// （扫描所有项目），而不是 `listAllIn("")`（永远空列表）。
#[test]
fn empty_session_dir_env_still_scans_all_projects_for_session_ids() {
    let root = create_temp_dir();
    let project_a = root.join("project-a");
    let project_b = root.join("project-b");
    let agent = root.join("agent");
    fs::create_dir_all(&project_a).expect("project a");
    fs::create_dir_all(&project_b).expect("project b");
    fs::create_dir_all(&agent).expect("agent");
    write_session_header(
        &default_session_dir(&agent, &project_a),
        "globid",
        &project_a,
    );

    let (code, _stdout, stderr) = run_kiana(
        &project_b,
        &agent,
        &[("KIANA_CODING_AGENT_SESSION_DIR", "")],
        &["--session", "globid"],
    );

    // stdin 为 /dev/null：TS 的 readline 收不到回答，不打印 Aborted.，以 0 退出。
    assert_eq!(code, 0, "stderr was: {stderr}");
    assert!(
        stderr.contains("Session found in different project:"),
        "global session lookup did not run: {stderr}"
    );
    assert!(
        !stderr.contains("No session found matching"),
        "list_all_in(\"\") was used: {stderr}"
    );
    assert!(
        !stderr.contains("Aborted."),
        "EOF must not print Aborted.: {stderr}"
    );
}
