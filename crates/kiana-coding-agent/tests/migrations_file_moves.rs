//! `migrations.ts` 中三个文件移动/重命名迁移的行为测试（spec/ca-cli.md §2.7）：
//!
//! - `migrateSessionsFromAgentRoot`：`~/.kiana/agent/*.jsonl` → `sessions/--<cwd>--/`；
//! - `migrateToolsToBin`：`tools/{fd,rg}` → `bin/`；
//! - `migrateExtensionSystem`：`commands/` → `prompts/`，并收集弃用目录警告。
//!
//! TS 侧这些函数由 `config-value-migration.test.ts` 之外的启动路径覆盖；Rust 侧把它们
//! 直接作为可观察的文件系统契约固定下来。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use kiana_coding_agent::migrations::{
    migrate_extension_system, migrate_sessions_from_agent_root, migrate_tools_to_bin,
};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 串行化 `KIANA_CODING_AGENT_DIR` 的修改（进程级环境变量）。
struct AgentDirGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Option<OsString>,
    temp: TempDir,
    agent: PathBuf,
}

impl AgentDirGuard {
    fn new() -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let agent = temp.path().join("agent");
        fs::create_dir_all(&agent).expect("agent dir");
        let saved = std::env::var_os("KIANA_CODING_AGENT_DIR");
        // SAFETY: ENV_LOCK 串行化本二进制内所有环境变量修改。
        unsafe {
            std::env::set_var("KIANA_CODING_AGENT_DIR", &agent);
        }
        Self {
            _lock: lock,
            saved,
            temp,
            agent,
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn agent(&self) -> &Path {
        &self.agent
    }
}

impl Drop for AgentDirGuard {
    fn drop(&mut self) {
        // SAFETY: 仍持有 ENV_LOCK。
        unsafe {
            match &self.saved {
                Some(value) => std::env::set_var("KIANA_CODING_AGENT_DIR", value),
                None => std::env::remove_var("KIANA_CODING_AGENT_DIR"),
            }
        }
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, content).expect("write");
}

#[test]
fn moves_top_level_sessions_into_the_encoded_cwd_directory() {
    let guard = AgentDirGuard::new();
    let content = "{\"type\":\"session\",\"cwd\":\"/home/user/proj\"}\n{\"type\":\"message\"}\n";
    let source = guard.agent().join("2024-01-01T00-00-00-000Z_abc.jsonl");
    write(&source, content);

    migrate_sessions_from_agent_root();

    let moved = guard
        .agent()
        .join("sessions/--home-user-proj--/2024-01-01T00-00-00-000Z_abc.jsonl");
    assert!(
        moved.is_file(),
        "session must be moved to the encoded directory"
    );
    assert!(
        !source.exists(),
        "source must no longer exist at the agent root"
    );
    assert_eq!(fs::read_to_string(moved).expect("read moved"), content);
}

#[test]
fn session_migration_keeps_the_alias_encoding_for_relative_and_windows_like_cwds() {
    let guard = AgentDirGuard::new();
    // TS 直接用 header.cwd 的原始字符串编码：只去掉**一个**前导分隔符，再把 / \ : 换成 -。
    let source = guard.agent().join("rel.jsonl");
    write(
        &source,
        "{\"type\":\"session\",\"cwd\":\"C:\\\\work\\\\proj\"}\n",
    );

    migrate_sessions_from_agent_root();

    let moved = guard.agent().join("sessions/--C--work-proj--/rel.jsonl");
    assert!(moved.is_file());
}

#[test]
fn skips_sessions_with_empty_or_non_session_first_lines() {
    let guard = AgentDirGuard::new();
    let empty_first_line = guard.agent().join("empty-first.jsonl");
    write(
        &empty_first_line,
        "\n{\"type\":\"session\",\"cwd\":\"/p\"}\n",
    );
    let not_session = guard.agent().join("not-session.jsonl");
    write(&not_session, "{\"type\":\"message\"}\n");
    let missing_cwd = guard.agent().join("missing-cwd.jsonl");
    write(&missing_cwd, "{\"type\":\"session\"}\n");
    let invalid_json = guard.agent().join("invalid.jsonl");
    write(&invalid_json, "not json\n");

    migrate_sessions_from_agent_root();

    for path in [&empty_first_line, &not_session, &missing_cwd, &invalid_json] {
        assert!(path.exists(), "{} must be left in place", path.display());
    }
    assert!(!guard.agent().join("sessions").exists());
}

#[test]
fn session_migration_does_not_overwrite_an_existing_target() {
    let guard = AgentDirGuard::new();
    let source = guard.agent().join("dup.jsonl");
    write(&source, "{\"type\":\"session\",\"cwd\":\"/p\"}\nsource\n");
    let target = guard.agent().join("sessions/--p--/dup.jsonl");
    write(&target, "existing\n");

    migrate_sessions_from_agent_root();

    assert!(
        source.exists(),
        "source stays when the target already exists"
    );
    assert_eq!(fs::read_to_string(&target).expect("target"), "existing\n");
}

#[test]
fn moves_fd_and_rg_binaries_from_tools_to_bin() {
    let guard = AgentDirGuard::new();
    let tools = guard.agent().join("tools");
    write(&tools.join("fd"), "fd-binary");
    write(&tools.join("rg"), "rg-binary");
    write(&tools.join("keep.txt"), "keep");

    migrate_tools_to_bin();

    assert_eq!(
        fs::read_to_string(guard.agent().join("bin/fd")).expect("bin/fd"),
        "fd-binary"
    );
    assert_eq!(
        fs::read_to_string(guard.agent().join("bin/rg")).expect("bin/rg"),
        "rg-binary"
    );
    assert!(!tools.join("fd").exists());
    assert!(!tools.join("rg").exists());
    assert!(tools.join("keep.txt").exists());
}

#[test]
fn tools_migration_removes_a_leftover_when_the_target_already_exists() {
    let guard = AgentDirGuard::new();
    write(&guard.agent().join("tools/rg"), "old");
    write(&guard.agent().join("bin/rg"), "current");

    migrate_tools_to_bin();

    assert!(!guard.agent().join("tools/rg").exists());
    assert_eq!(
        fs::read_to_string(guard.agent().join("bin/rg")).expect("bin/rg"),
        "current"
    );
}

#[test]
fn renames_commands_to_prompts_for_global_and_project_scope() {
    let guard = AgentDirGuard::new();
    let project = guard.root().join("project");
    fs::create_dir_all(guard.agent().join("commands")).expect("global commands");
    fs::write(guard.agent().join("commands/global.md"), "g").expect("write global");
    fs::create_dir_all(project.join(".kiana/commands")).expect("project commands");
    fs::write(project.join(".kiana/commands/project.md"), "p").expect("write project");

    migrate_extension_system(&project);

    assert!(guard.agent().join("prompts/global.md").is_file());
    assert!(!guard.agent().join("commands").exists());
    assert!(project.join(".kiana/prompts/project.md").is_file());
    assert!(!project.join(".kiana/commands").exists());
}

#[test]
fn reports_deprecated_hooks_and_custom_tools_directories_in_ts_order() {
    let guard = AgentDirGuard::new();
    let project = guard.root().join("project");
    fs::create_dir_all(guard.agent().join("hooks")).expect("global hooks");
    fs::create_dir_all(project.join(".kiana/hooks")).expect("project hooks");
    fs::create_dir_all(project.join(".kiana/tools")).expect("project tools");
    fs::write(project.join(".kiana/tools/custom.js"), "x").expect("custom tool");
    // fd/rg 与隐藏文件不算自定义工具。
    fs::create_dir_all(guard.agent().join("tools")).expect("global tools");
    fs::write(guard.agent().join("tools/rg"), "x").expect("rg binary");
    fs::write(guard.agent().join("tools/.DS_Store"), "x").expect("hidden file");

    let warnings = migrate_extension_system(&project);

    assert_eq!(
        warnings,
        vec![
            "Global hooks/ directory found. Hooks have been renamed to extensions.".to_string(),
            "Project hooks/ directory found. Hooks have been renamed to extensions.".to_string(),
            "Project tools/ directory contains custom tools. Custom tools have been merged into extensions."
                .to_string(),
        ]
    );
}
