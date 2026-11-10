//! 移植 `packages/coding-agent/test/footer-data-provider.test.ts`（8 个测试）。
//!
//! 用 PATH 前置的假 `git` 可执行文件驱动分支解析：脚本读取所调用 cwd 下的 `.fake-branch`
//! （存在则输出并退出 0，否则退出 1），并把调用记录追加到 `.fake-git-calls`。
//! 去抖/重试用 `tokio::time::pause()` + `advance`（见 `ca-resources.md` §8.4 第 13 条）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use kiana_ext_api::ui::ReadonlyFooterDataProvider;
use kiana_resources::footer_data_provider::FooterDataProvider;

// ---------------------------------------------------------------------------
// fake git
// ---------------------------------------------------------------------------

const FAKE_GIT_SCRIPT: &str = r#"#!/bin/sh
if [ "$1" = "--no-optional-locks" ] && [ "$2" = "symbolic-ref" ]; then
  printf '%s %s\n' "$PWD" "$*" >> .fake-git-calls
  if [ -f .fake-branch ]; then
    cat .fake-branch
    exit 0
  fi
  exit 1
fi
exit 1
"#;

fn install_fake_git() -> &'static PathBuf {
    static FAKE_DIR: OnceLock<PathBuf> = OnceLock::new();
    FAKE_DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("kiana-footer-fake-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create fake git dir");
        let script = dir.join("git");
        std::fs::write(&script, FAKE_GIT_SCRIPT).expect("write fake git");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script).expect("stat").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("chmod");
        }
        let path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{path}", dir.to_string_lossy());
        // SAFETY: 测试二进制内只在这里一次性设置 PATH（`OnceLock` 保证单线程初始化）；
        // 其它测试在此之前不会 spawn `git`。
        unsafe {
            std::env::set_var("PATH", new_path);
        }
        dir
    })
}

fn write_branch_file(repo_dir: &Path, branch: &str) {
    std::fs::write(repo_dir.join(".fake-branch"), format!("{branch}\n")).expect("branch file");
}

fn git_calls(repo_dir: &Path) -> Vec<String> {
    match std::fs::read_to_string(repo_dir.join(".fake-git-calls")) {
        Ok(content) => content.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

fn clear_git_calls(repo_dir: &Path) {
    let _ = std::fs::remove_file(repo_dir.join(".fake-git-calls"));
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

struct WorktreeFixture {
    worktree_dir: PathBuf,
}

fn create_plain_reftable_repo(temp_dir: &Path) -> PathBuf {
    let repo_dir = temp_dir.join("repo");
    std::fs::create_dir_all(repo_dir.join(".git/reftable")).expect("reftable");
    std::fs::write(repo_dir.join(".git/HEAD"), "ref: refs/heads/.invalid\n").expect("head");
    repo_dir
}

fn create_plain_repo(temp_dir: &Path) -> PathBuf {
    let repo_dir = temp_dir.join("repo");
    std::fs::create_dir_all(repo_dir.join(".git")).expect("git dir");
    std::fs::write(repo_dir.join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
    repo_dir
}

fn create_reftable_worktree(temp_dir: &Path) -> WorktreeFixture {
    let repo_dir = temp_dir.join("repo");
    let common_git_dir = repo_dir.join(".git");
    let git_dir = common_git_dir.join("worktrees/src");
    let worktree_dir = temp_dir.join("worktree");
    let reftable_dir = common_git_dir.join("reftable");

    std::fs::create_dir_all(&git_dir).expect("git dir");
    std::fs::create_dir_all(&reftable_dir).expect("reftable dir");
    std::fs::create_dir_all(&worktree_dir).expect("worktree dir");

    std::fs::write(
        worktree_dir.join(".git"),
        format!("gitdir: {}\n", git_dir.to_string_lossy()),
    )
    .expect("git file");
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/.invalid\n").expect("head");
    std::fs::write(git_dir.join("commondir"), "../..\n").expect("commondir");
    std::fs::write(reftable_dir.join("tables.list"), "0\n").expect("tables.list");

    let _ = &reftable_dir;
    WorktreeFixture { worktree_dir }
}

/// 等待条件成立（真实时间，最多 5s）。
async fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "Timed out waiting for condition"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// 在当前 runtime 上让出若干次，使后台任务有机会推进。
///
/// 子进程退出是真实 IO：暂停时钟会阻止 IO driver 前进（`advance` 只推进定时器），
/// 因此这里临时恢复真实时间，等待后台任务完成后再暂停。
async fn settle() {
    tokio::time::resume();
    for _ in 0..100 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    tokio::time::pause();
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uses_head_directly_in_a_regular_repo_from_a_nested_directory() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = create_plain_repo(temp.path());
    let nested_dir = repo_dir.join("src/nested");
    std::fs::create_dir_all(&nested_dir).expect("nested");

    let provider = FooterDataProvider::new(nested_dir.to_string_lossy().into_owned());
    let branch = provider.get_git_branch();
    provider.dispose();
    assert_eq!(branch.as_deref(), Some("main"));
    assert!(git_calls(&repo_dir).is_empty());
}

#[tokio::test]
async fn resolves_the_branch_via_git_when_head_is_invalid_in_a_reftable_repo() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = create_plain_reftable_repo(temp.path());
    write_branch_file(&repo_dir, "main");

    let provider = FooterDataProvider::new(repo_dir.to_string_lossy().into_owned());
    let branch = provider.get_git_branch();
    provider.dispose();
    assert_eq!(branch.as_deref(), Some("main"));

    let calls = git_calls(&repo_dir);
    let expected = format!(
        "{} --no-optional-locks symbolic-ref --quiet --short HEAD",
        repo_dir.to_string_lossy()
    );
    assert_eq!(calls.first(), Some(&expected));
}

#[tokio::test]
async fn resolves_the_branch_via_git_in_a_reftable_backed_worktree() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = create_reftable_worktree(temp.path());
    write_branch_file(&fixture.worktree_dir, "main");

    let provider = FooterDataProvider::new(fixture.worktree_dir.to_string_lossy().into_owned());
    let branch = provider.get_git_branch();
    provider.dispose();
    assert_eq!(branch.as_deref(), Some("main"));
}

#[tokio::test]
async fn treats_an_unresolved_invalid_reftable_head_as_detached() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = create_plain_reftable_repo(temp.path());

    let provider = FooterDataProvider::new(repo_dir.to_string_lossy().into_owned());
    let branch = provider.get_git_branch();
    provider.dispose();
    assert_eq!(branch.as_deref(), Some("detached"));
}

#[tokio::test(start_paused = true)]
async fn does_not_notify_listeners_when_reftable_updates_keep_the_same_branch() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = create_reftable_worktree(temp.path());
    write_branch_file(&fixture.worktree_dir, "main");

    let provider = FooterDataProvider::new(fixture.worktree_dir.to_string_lossy().into_owned());
    assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
    clear_git_calls(&fixture.worktree_dir);

    let notifications = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&notifications);
    let _unsubscribe = provider.on_branch_change(std::sync::Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));

    provider.trigger_refresh();
    // 让后台任务运行到 `sleep`（注册定时器）后，再推进时钟。
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(501)).await;
    settle().await;

    assert_eq!(git_calls(&fixture.worktree_dir).len(), 1);
    assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
    assert_eq!(notifications.load(std::sync::atomic::Ordering::SeqCst), 0);
    provider.dispose();
}

#[tokio::test(start_paused = true)]
async fn debounces_rapid_reftable_updates_into_a_single_async_refresh() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = create_reftable_worktree(temp.path());
    write_branch_file(&fixture.worktree_dir, "main");

    let provider = FooterDataProvider::new(fixture.worktree_dir.to_string_lossy().into_owned());
    assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
    clear_git_calls(&fixture.worktree_dir);

    provider.trigger_refresh();
    provider.trigger_refresh();
    provider.trigger_refresh();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(499)).await;
    assert!(git_calls(&fixture.worktree_dir).is_empty());
    tokio::time::advance(Duration::from_millis(2)).await;
    settle().await;
    assert_eq!(git_calls(&fixture.worktree_dir).len(), 1);
    tokio::time::advance(Duration::from_millis(650)).await;
    settle().await;
    assert_eq!(git_calls(&fixture.worktree_dir).len(), 1);
    provider.dispose();
}

#[tokio::test]
async fn updates_the_cached_branch_when_the_reftable_directory_changes() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = create_reftable_worktree(temp.path());
    write_branch_file(&fixture.worktree_dir, "main");

    let provider = FooterDataProvider::new(fixture.worktree_dir.to_string_lossy().into_owned());
    assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
    write_branch_file(&fixture.worktree_dir, "foo");
    clear_git_calls(&fixture.worktree_dir);

    let notifications = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&notifications);
    let _unsubscribe = provider.on_branch_change(std::sync::Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));

    // 真正走 reftable 目录 / tables.list 的 fs watcher（事件在整包测试负载下可能延迟）。
    std::fs::write(
        fixture
            .worktree_dir
            .join("../repo/.git/reftable/tables.list"),
        "1\n",
    )
    .expect("tables.list");
    wait_until(|| provider.get_git_branch().as_deref() == Some("foo")).await;

    assert_eq!(git_calls(&fixture.worktree_dir).len(), 1);
    assert_eq!(notifications.load(std::sync::atomic::Ordering::SeqCst), 1);
    provider.dispose();
}

#[tokio::test(start_paused = true)]
async fn retries_git_watchers_5_seconds_after_an_async_fs_watch_error() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = create_plain_repo(temp.path());

    let provider = FooterDataProvider::new(repo_dir.to_string_lossy().into_owned());
    assert!(provider.has_head_watcher());

    provider.simulate_git_watcher_error();
    assert!(!provider.has_head_watcher());
    assert!(provider.has_git_watcher_retry());
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_millis(4999)).await;
    assert!(!provider.has_head_watcher());
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert!(provider.has_head_watcher());
    assert!(!provider.has_git_watcher_retry());
    provider.dispose();
}

#[tokio::test]
async fn read_only_trait_exposes_statuses_and_provider_count() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let repo_dir = create_plain_repo(temp.path());
    let provider = FooterDataProvider::new(repo_dir.to_string_lossy().into_owned());
    provider.set_extension_status("a", Some("first"));
    provider.set_extension_status("b", Some("second"));
    provider.set_extension_status("a", None);
    provider.set_available_provider_count(3);

    assert_eq!(
        provider.get_extension_statuses().keys().collect::<Vec<_>>(),
        vec!["b"]
    );
    assert_eq!(provider.get_available_provider_count(), 3);
    let trait_view: &dyn ReadonlyFooterDataProvider = &provider;
    assert_eq!(trait_view.get_available_provider_count(), 3);
    assert_eq!(trait_view.get_git_branch().as_deref(), Some("main"));
    provider.dispose();
}

#[tokio::test]
async fn find_git_paths_handles_worktrees_and_missing_head() {
    install_fake_git();
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = create_reftable_worktree(temp.path());
    let paths = kiana_resources::footer_data_provider::find_git_paths(&fixture.worktree_dir)
        .expect("git paths");
    assert_eq!(paths.repo_dir, fixture.worktree_dir);
    assert_eq!(paths.common_git_dir, temp.path().join("repo/.git"));

    let missing = temp.path().join("missing-head");
    std::fs::create_dir_all(missing.join(".git")).expect("dir");
    assert!(kiana_resources::footer_data_provider::find_git_paths(&missing).is_none());

    // `.git` 文件不是 `gitdir:` 格式时继续向上找。
    let outer = temp.path().join("outer");
    std::fs::create_dir_all(outer.join(".git")).expect("outer git");
    std::fs::write(outer.join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
    let inner = outer.join("inner");
    std::fs::create_dir_all(&inner).expect("inner");
    std::fs::write(inner.join(".git"), "not a gitdir file\n").expect("git file");
    let paths = kiana_resources::footer_data_provider::find_git_paths(&inner).expect("outer repo");
    assert_eq!(paths.repo_dir, outer);
}
