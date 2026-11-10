//! 私有 git checkpoint + agent-write ledger 的端到端测试。
//!
//! 覆盖 DoD：
//! - checkpoint 不修改用户 git status/refs/stash；
//! - 无变化不产生 commit；每 turn/目录最多一次；
//! - 跳过 root/home、node_modules/target/.git、文件数超限；
//! - agent 最后写入的文件被还原，用户手改的文件跳过并报告；
//! - nested git repo 拒绝恢复；restore 失败可回滚（用成功恢复后的显式 rollback 验证）；
//! - retention 不删活跃 checkpoint，用父目录 `(dev, ino)` 区分删除与未挂载。

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use kiana_session::checkpoint::{
    CheckpointError, CheckpointManager, CheckpointOptions, CheckpointOutcome, CheckpointSkip,
    RetentionPolicy,
};

fn run_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_user_repo(root: &Path) {
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.name", "Test User"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

fn manager(temp: &tempfile::TempDir, options: CheckpointOptions) -> CheckpointManager {
    CheckpointManager::new(temp.path().join("kiana-state"), options)
}

fn private_git_names(manager: &CheckpointManager, workdir: &Path, commit: &str) -> Vec<String> {
    let metadata = manager
        .latest_checkpoint(workdir)
        .expect("latest")
        .expect("metadata");
    assert_eq!(metadata.commit, commit);
    let root = manager.state_dir().join("projects");
    let project = fs::read_dir(&root)
        .expect("projects")
        .flatten()
        .next()
        .expect("project entry")
        .path();
    let git_dir = project.join("repo.git");
    let output = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", commit])
        .env("GIT_DIR", &git_dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("ls-tree");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn checkpoint_does_not_touch_user_git_state() {
    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    init_user_repo(&workdir);
    write(&workdir.join("tracked.txt"), "one\n");
    write(&workdir.join("staged.txt"), "staged\n");
    run_git(&workdir, &["add", "tracked.txt", "staged.txt"]);
    run_git(&workdir, &["commit", "-q", "-m", "initial"]);
    // 先制造一个真实 stash，确保 refs/stash 存在。
    write(&workdir.join("tracked.txt"), "stashed\n");
    run_git(&workdir, &["stash", "push", "-q", "-m", "user stash"]);
    // dirty working tree：一个已修改、一个 untracked、一个 staged。
    write(&workdir.join("tracked.txt"), "modified\n");
    write(&workdir.join("untracked.txt"), "untracked\n");
    run_git(&workdir, &["add", "untracked.txt"]);
    let status_before = run_git(&workdir, &["status", "--porcelain=v1"]);
    let head_before = run_git(&workdir, &["rev-parse", "HEAD"]);
    let refs_before = run_git(&workdir, &["for-each-ref"]);
    let stash_before = run_git(&workdir, &["stash", "list"]);
    assert!(!stash_before.is_empty(), "test must have a real stash");

    let manager = manager(&temp, CheckpointOptions::default());
    let outcome = manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("checkpoint");
    assert!(matches!(outcome, CheckpointOutcome::Created(_)));

    assert_eq!(
        run_git(&workdir, &["status", "--porcelain=v1"]),
        status_before
    );
    assert_eq!(run_git(&workdir, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(run_git(&workdir, &["for-each-ref"]), refs_before);
    assert_eq!(run_git(&workdir, &["stash", "list"]), stash_before);
}

#[test]
fn per_turn_dedup_and_unchanged_tree_produces_no_commit() {
    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    write(&workdir.join("a.txt"), "one\n");
    let manager = manager(&temp, CheckpointOptions::default());

    let CheckpointOutcome::Created(first) = manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("first")
    else {
        panic!("expected created");
    };
    assert_eq!(first.snapshots, 1);

    // 同 turn 第二次直接去重，不再执行 git。
    match manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("dedup")
    {
        CheckpointOutcome::AlreadyCheckedThisTurn { turn_id, .. } => assert_eq!(turn_id, "turn-1"),
        other => panic!("unexpected outcome: {other:?}"),
    }

    // 新 turn、树没变：只 touch metadata，不产生新 commit。
    let CheckpointOutcome::Unchanged(second) = manager
        .ensure_checkpoint(&workdir, "turn-2")
        .expect("unchanged")
    else {
        panic!("expected unchanged");
    };
    assert_eq!(second.commit, first.commit);
    assert_eq!(second.tree, first.tree);
    assert_eq!(second.snapshots, 1);

    write(&workdir.join("a.txt"), "two\n");
    let CheckpointOutcome::Created(third) = manager
        .ensure_checkpoint(&workdir, "turn-3")
        .expect("third")
    else {
        panic!("expected created");
    };
    assert_ne!(third.commit, first.commit);
    assert_eq!(third.snapshots, 2);
}

#[test]
fn node_modules_target_and_git_are_not_snapshotted() {
    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    init_user_repo(&workdir);
    write(&workdir.join("src/main.rs"), "fn main() {}\n");
    write(&workdir.join("node_modules/pkg/index.js"), "module\n");
    write(&workdir.join("target/debug/build.bin"), "binary\n");
    write(&workdir.join(".git/secret"), "never\n");
    let manager = manager(&temp, CheckpointOptions::default());
    let CheckpointOutcome::Created(metadata) = manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("checkpoint")
    else {
        panic!("expected created");
    };
    let names = private_git_names(&manager, &workdir, &metadata.commit);
    assert!(names.iter().any(|name| name == "src/main.rs"));
    assert!(!names.iter().any(|name| name.starts_with("node_modules/")));
    assert!(!names.iter().any(|name| name.starts_with("target/")));
    assert!(!names.iter().any(|name| name.starts_with(".git/")));
}

#[test]
fn root_and_home_are_skipped() {
    let temp = tempfile::tempdir().expect("temp");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home");
    // SAFETY: test-only process-wide env; tests run in separate binaries but parallel
    // threads inside this binary could race. This test does not read HOME elsewhere.
    unsafe { std::env::set_var("HOME", &home) };
    let manager = manager(&temp, CheckpointOptions::default());
    match manager.ensure_checkpoint(&home, "turn-1").expect("home") {
        CheckpointOutcome::Skipped(CheckpointSkip::TooBroad { .. }) => {}
        other => panic!("expected too broad for home, got {other:?}"),
    }
    match manager.ensure_checkpoint("/", "turn-1").expect("root") {
        CheckpointOutcome::Skipped(CheckpointSkip::TooBroad { .. }) => {}
        other => panic!("expected too broad for root, got {other:?}"),
    }
}

#[test]
fn file_count_limit_skips_before_git_runs() {
    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    for index in 0..5 {
        write(&workdir.join(format!("f{index}.txt")), "x");
    }
    let manager = manager(
        &temp,
        CheckpointOptions {
            max_files: 2,
            ..Default::default()
        },
    );
    match manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("checkpoint")
    {
        CheckpointOutcome::Skipped(CheckpointSkip::TooManyFiles { files, limit }) => {
            assert_eq!(limit, 2);
            assert!(files > 2);
        }
        other => panic!("expected too many files, got {other:?}"),
    }
    assert!(
        manager
            .latest_checkpoint(&workdir)
            .expect("latest")
            .is_none()
    );
}

#[test]
fn safe_restore_restores_agent_files_and_skips_user_edits() {
    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    write(&workdir.join("a.txt"), "one\n");
    write(&workdir.join("b.txt"), "two\n");
    let manager = manager(&temp, CheckpointOptions::default());
    let CheckpointOutcome::Created(checkpoint) = manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("checkpoint")
    else {
        panic!("expected created");
    };

    let mut ledger = manager.ledger(&workdir).expect("ledger");
    // agent 修改 a 和 b，并新建 c。
    write(&workdir.join("a.txt"), "agent-a\n");
    ledger
        .record_agent_write(workdir.join("a.txt"), &sha_of("agent-a\n"))
        .expect("record a");
    write(&workdir.join("b.txt"), "agent-b\n");
    ledger
        .record_agent_write(workdir.join("b.txt"), &sha_of("agent-b\n"))
        .expect("record b");
    write(&workdir.join("c.txt"), "agent-c\n");
    ledger
        .record_agent_write(workdir.join("c.txt"), &sha_of("agent-c\n"))
        .expect("record c");
    ledger.save().expect("save ledger");

    // 用户随后手改 b。
    write(&workdir.join("b.txt"), "user-b\n");

    let report = manager
        .restore(&workdir, Some(&checkpoint.commit), &ledger)
        .expect("restore");
    assert_eq!(
        fs::read_to_string(workdir.join("a.txt")).expect("a"),
        "one\n"
    );
    assert_eq!(
        fs::read_to_string(workdir.join("b.txt")).expect("b"),
        "user-b\n"
    );
    assert!(!workdir.join("c.txt").exists());
    assert_eq!(
        report.restored,
        vec!["a.txt".to_string(), "c.txt".to_string()]
    );
    assert_eq!(report.skipped_user_edits, vec!["b.txt".to_string()]);
    assert!(report.rollback_commit.is_some());

    // rollback 把恢复前的内容写回。
    let rollback = report.rollback().expect("rollback");
    assert!(rollback.restored.contains(&"a.txt".to_string()));
    assert_eq!(
        fs::read_to_string(workdir.join("a.txt")).expect("a"),
        "agent-a\n"
    );
    assert_eq!(
        fs::read_to_string(workdir.join("c.txt")).expect("c"),
        "agent-c\n"
    );
    assert_eq!(
        fs::read_to_string(workdir.join("b.txt")).expect("b"),
        "user-b\n"
    );
}

#[test]
fn restore_refuses_nested_git_repository() {
    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    write(&workdir.join("top.txt"), "top\n");
    let nested = workdir.join("nested");
    fs::create_dir_all(&nested).expect("nested");
    init_user_repo(&nested);
    write(&nested.join("inner.txt"), "inner\n");
    run_git(&nested, &["add", "inner.txt"]);
    run_git(&nested, &["commit", "-q", "-m", "inner"]);

    let manager = manager(&temp, CheckpointOptions::default());
    let CheckpointOutcome::Created(checkpoint) = manager
        .ensure_checkpoint(&workdir, "turn-1")
        .expect("checkpoint")
    else {
        panic!("expected created");
    };
    let ledger = manager.ledger(&workdir).expect("ledger");
    let error = manager
        .restore(&workdir, Some(&checkpoint.commit), &ledger)
        .expect_err("nested repo must be refused");
    assert!(
        matches!(error, CheckpointError::NestedRepository { .. }),
        "unexpected error: {error}"
    );
    assert!(
        nested.join("inner.txt").exists(),
        "refused restore must not touch files"
    );
}

#[test]
fn prune_keeps_active_and_unmounted_but_removes_deleted_projects() {
    let temp = tempfile::tempdir().expect("temp");
    let manager = Arc::new(manager(&temp, CheckpointOptions::default()));

    // 活跃项目：即使 retention=0 也不删。
    let active = temp.path().join("active");
    fs::create_dir_all(&active).expect("active");
    write(&active.join("a.txt"), "a\n");
    let CheckpointOutcome::Created(_) = manager
        .ensure_checkpoint(&active, "turn-1")
        .expect("active checkpoint")
    else {
        panic!("expected created");
    };

    // 被删除的项目：父目录仍在且非空 -> 可判定删除。
    let deleted_root = temp.path().join("deleted-root");
    fs::create_dir_all(&deleted_root).expect("deleted root");
    let deleted = deleted_root.join("project");
    fs::create_dir_all(&deleted).expect("deleted project");
    write(&deleted.join("d.txt"), "d\n");
    let CheckpointOutcome::Created(_) = manager
        .ensure_checkpoint(&deleted, "turn-1")
        .expect("deleted checkpoint")
    else {
        panic!("expected created");
    };
    fs::remove_dir_all(&deleted).expect("delete project");
    write(&deleted_root.join("other.txt"), "other\n");

    // 未挂载：父目录路径存在但 (dev, ino) 变了（rename 后重建空目录）。
    let mount_root = temp.path().join("mount-root");
    fs::create_dir_all(&mount_root).expect("mount root");
    let mounted = mount_root.join("project");
    fs::create_dir_all(&mounted).expect("mounted project");
    write(&mounted.join("m.txt"), "m\n");
    let CheckpointOutcome::Created(_) = manager
        .ensure_checkpoint(&mounted, "turn-1")
        .expect("mounted checkpoint")
    else {
        panic!("expected created");
    };
    fs::rename(&mount_root, temp.path().join("mount-root-old")).expect("rename parent");
    fs::create_dir_all(&mount_root).expect("new empty parent");

    let report = manager
        .prune(&RetentionPolicy {
            retention_ms: 0,
            max_total_bytes: u64::MAX,
        })
        .expect("prune");

    let project_dirs: Vec<PathBuf> = fs::read_dir(manager.state_dir().join("projects"))
        .expect("projects")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(project_dirs.len(), 2, "active + unmounted must remain");
    assert_eq!(report.removed_projects.len(), 1);
    assert_eq!(report.kept_unreachable.len(), 1);
    assert!(active.join("a.txt").exists());
}

fn sha_of(text: &str) -> String {
    // 复用 crate 的实现，保证与 ledger 期望完全一致。
    kiana_session::subagent::result::sha256_hex(text.as_bytes())
}

#[test]
fn mutating_tool_helper_only_checkpoints_writes_and_exec() {
    use kiana_session::checkpoint::{ensure_checkpoint_before_tool, is_mutating_tool};
    assert!(is_mutating_tool("write"));
    assert!(is_mutating_tool("bash"));
    assert!(!is_mutating_tool("read"));
    assert!(!is_mutating_tool("grep"));

    let temp = tempfile::tempdir().expect("temp");
    let workdir = temp.path().join("project");
    fs::create_dir_all(&workdir).expect("project");
    write(&workdir.join("a.txt"), "a\n");
    let manager = manager(&temp, CheckpointOptions::default());
    assert!(
        ensure_checkpoint_before_tool(&manager, &workdir, "turn-1", "read")
            .expect("read")
            .is_none()
    );
    let outcome = ensure_checkpoint_before_tool(&manager, &workdir, "turn-1", "write")
        .expect("write")
        .expect("some outcome");
    assert!(matches!(outcome, CheckpointOutcome::Created(_)));
}
