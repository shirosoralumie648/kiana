//! `packages/coding-agent/test/git-update.test.ts` 的 Rust 移植。
//!
//! 这些测试使用**真实 git** 与本地 bare-ish 仓库（无网络）；`allowNetwork()` 在 TS 里
//! 只是清除 `KIANA_OFFLINE`，这里 `TestEnv` 默认不设置 `KIANA_OFFLINE`。

#![cfg(unix)]
#![allow(clippy::await_holding_lock)] // TestEnv 在 await 期间持有环境锁以串行化进程环境。

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestEnv, is_enabled, temporary_git_dir};
use kiana_config::settings::schema::{PackageSource, Settings};
use kiana_resources::package::{PackageManager, ResolveExtensionSourcesOptions};

const GIT_SOURCE: &str = "git:github.com/test/extension";

fn git(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
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

fn init_git_repo(repo_dir: &Path) {
    std::fs::create_dir_all(repo_dir).expect("mkdir");
    git(&["init", "--initial-branch=main"], repo_dir);
    git(
        &["config", "--local", "user.email", "test@test.com"],
        repo_dir,
    );
    git(&["config", "--local", "user.name", "Test"], repo_dir);
}

fn create_commit(repo_dir: &Path, filename: &str, content: &str, message: &str) -> String {
    let path = repo_dir.join(filename);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, content).expect("write");
    git(&["add", filename], repo_dir);
    git(&["commit", "-m", message], repo_dir);
    git(&["rev-parse", "HEAD"], repo_dir)
}

fn current_commit(repo_dir: &Path) -> String {
    git(&["rev-parse", "HEAD"], repo_dir)
}

fn file_content(repo_dir: &Path, filename: &str) -> String {
    std::fs::read_to_string(repo_dir.join(filename)).expect("read file")
}

struct Fixture {
    env: TestEnv,
    remote_dir: PathBuf,
    installed_dir: PathBuf,
    settings_manager: kiana_config::settings::SettingsManager,
    package_manager: kiana_resources::package::DefaultPackageManager,
}

impl Fixture {
    fn new() -> Self {
        let env = TestEnv::new_with_real_binaries();
        let remote_dir = Path::new(&env.root()).join("remote");
        let installed_dir = Path::new(&env.agent_dir).join("git/github.com/test/extension");
        let settings_manager = env.settings_manager(Settings::default());
        let package_manager = env.manager(&settings_manager);
        Self {
            env,
            remote_dir,
            installed_dir,
            settings_manager,
            package_manager,
        }
    }

    /// 建“远端”仓库，并把它克隆到安装目录（模拟 `install()` 的结果）。
    fn setup_remote_and_install(&self) {
        init_git_repo(&self.remote_dir);
        create_commit(&self.remote_dir, "extension.ts", "// v1", "Initial commit");
        std::fs::create_dir_all(self.installed_dir.parent().expect("parent")).expect("mkdir");
        git(
            &[
                "clone",
                &self.remote_dir.to_string_lossy(),
                &self.installed_dir.to_string_lossy(),
            ],
            self.env.temp.path(),
        );
        git(
            &["config", "--local", "user.email", "test@test.com"],
            &self.installed_dir,
        );
        git(
            &["config", "--local", "user.name", "Test"],
            &self.installed_dir,
        );
        self.settings_manager
            .set_packages(vec![PackageSource::Text(GIT_SOURCE.to_string())]);
    }
}

/// 在 PATH 首位放一个会记录 argv 的 `git` 包装脚本（其余程序照常执行）。
fn install_logging_git(env: &TestEnv) {
    use std::os::unix::fs::PermissionsExt;
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let real_git = std::env::split_paths(&path_var)
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("real git in PATH");
    // POSIX sh（dash）不支持 `$'...'`；用字面换行构造单次 write 的日志块。
    let log_block = r#"log="${KIANA_FAKE_LOG:-}"
if [ -n "$log" ]; then
	block="CMD $(basename "$0")
CWD $PWD
"
	for argument in "$@"; do
		block="${block}ARG ${argument}
"
	done
	block="${block}END
"
	printf '%s' "$block" >>"$log"
fi
"#;
    let git_wrapper = format!(
        "#!/bin/sh\n{log_block}exec '{}' \"$@\"\n",
        real_git.to_string_lossy()
    );
    let npm_wrapper = format!("#!/bin/sh\n{log_block}exit 0\n");
    for (name, content) in [("git", git_wrapper), ("npm", npm_wrapper)] {
        let path = Path::new(&env.bin_dir).join(name);
        std::fs::write(&path, content).expect("write wrapper");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod wrapper");
    }
    let current = std::env::var("PATH").unwrap_or_default();
    env.set_env("PATH", &format!("{}:{current}", env.bin_dir));
}

#[tokio::test]
async fn should_skip_reset_clean_and_install_when_already_up_to_date() {
    let fixture = Fixture::new();
    init_git_repo(&fixture.remote_dir);
    std::fs::write(
        fixture.remote_dir.join("package.json"),
        "{\"name\":\"test-extension\",\"version\":\"1.0.0\"}",
    )
    .expect("write");
    create_commit(
        &fixture.remote_dir,
        "extension.ts",
        "// v1",
        "Initial commit",
    );
    std::fs::create_dir_all(fixture.installed_dir.parent().expect("parent")).expect("mkdir");
    git(
        &[
            "clone",
            &fixture.remote_dir.to_string_lossy(),
            &fixture.installed_dir.to_string_lossy(),
        ],
        fixture.env.temp.path(),
    );
    fixture
        .settings_manager
        .set_packages(vec![PackageSource::Text(GIT_SOURCE.to_string())]);
    install_logging_git(&fixture.env);
    let head_before = current_commit(&fixture.installed_dir);
    fixture.package_manager.update(None).await.expect("update");

    let command_lines: Vec<String> = fixture
        .env
        .invocations()
        .iter()
        .map(common::Invocation::command_line)
        .collect();
    assert!(
        command_lines.contains(
            &"git fetch --prune --no-tags origin +refs/heads/main:refs/remotes/origin/main"
                .to_string()
        ),
        "commands: {command_lines:?}"
    );
    assert!(!command_lines.contains(&"git fetch --prune origin".to_string()));
    assert!(
        !command_lines
            .iter()
            .any(|line| line == "git reset --hard @{upstream}")
    );
    assert!(
        !command_lines
            .iter()
            .any(|line| line == "git reset --hard origin/HEAD")
    );
    assert!(!command_lines.contains(&"git clean -fdx".to_string()));
    assert!(
        !fixture
            .env
            .invocations()
            .iter()
            .any(|invocation| invocation.program == "npm"),
        "npm install must not run"
    );
    assert_eq!(current_commit(&fixture.installed_dir), head_before);
}

#[tokio::test]
async fn should_update_to_latest_commit_when_remote_has_new_commits() {
    let fixture = Fixture::new();
    fixture.setup_remote_and_install();
    assert_eq!(
        file_content(&fixture.installed_dir, "extension.ts"),
        "// v1"
    );
    let new_commit = create_commit(
        &fixture.remote_dir,
        "extension.ts",
        "// v2",
        "Second commit",
    );

    fixture.package_manager.update(None).await.expect("update");

    assert_eq!(current_commit(&fixture.installed_dir), new_commit);
    assert_eq!(
        file_content(&fixture.installed_dir, "extension.ts"),
        "// v2"
    );
}

#[tokio::test]
async fn should_handle_complete_history_rewrite() {
    let fixture = Fixture::new();
    fixture.setup_remote_and_install();
    create_commit(&fixture.remote_dir, "extension.ts", "// v2", "v2");
    create_commit(&fixture.remote_dir, "extension.ts", "// v3", "v3");

    fixture.package_manager.update(None).await.expect("update");
    assert_eq!(
        file_content(&fixture.installed_dir, "extension.ts"),
        "// v3"
    );

    // 维护者强推了完全不同的历史。
    git(&["reset", "--hard", "HEAD~2"], &fixture.remote_dir);
    create_commit(
        &fixture.remote_dir,
        "extension.ts",
        "// rewrite-a",
        "Rewrite A",
    );
    let final_commit = create_commit(
        &fixture.remote_dir,
        "extension.ts",
        "// rewrite-b",
        "Rewrite B",
    );

    fixture.package_manager.update(None).await.expect("update");

    assert_eq!(current_commit(&fixture.installed_dir), final_commit);
    assert_eq!(
        file_content(&fixture.installed_dir, "extension.ts"),
        "// rewrite-b"
    );
}

#[tokio::test]
async fn should_checkout_the_configured_pinned_git_ref_during_full_and_targeted_updates() {
    let fixture = Fixture::new();
    init_git_repo(&fixture.remote_dir);
    let v1_commit = create_commit(
        &fixture.remote_dir,
        "extension.ts",
        "// v1",
        "Initial commit",
    );
    git(&["tag", "v1"], &fixture.remote_dir);
    let v2_commit = create_commit(
        &fixture.remote_dir,
        "extension.ts",
        "// v2",
        "Second commit",
    );
    git(&["tag", "v2"], &fixture.remote_dir);

    std::fs::create_dir_all(fixture.installed_dir.parent().expect("parent")).expect("mkdir");
    git(
        &[
            "clone",
            &fixture.remote_dir.to_string_lossy(),
            &fixture.installed_dir.to_string_lossy(),
        ],
        fixture.env.temp.path(),
    );
    git(
        &["config", "--local", "user.email", "test@test.com"],
        &fixture.installed_dir,
    );
    git(
        &["config", "--local", "user.name", "Test"],
        &fixture.installed_dir,
    );
    git(&["checkout", "v1"], &fixture.installed_dir);
    assert_eq!(current_commit(&fixture.installed_dir), v1_commit);

    let pinned_source = format!("{GIT_SOURCE}@v2");
    fixture
        .settings_manager
        .set_packages(vec![PackageSource::Text(pinned_source.clone())]);

    fixture.package_manager.update(None).await.expect("update");
    assert_eq!(current_commit(&fixture.installed_dir), v2_commit);
    assert_eq!(
        file_content(&fixture.installed_dir, "extension.ts"),
        "// v2"
    );

    git(&["checkout", "v1"], &fixture.installed_dir);
    fixture
        .package_manager
        .update(Some(&pinned_source))
        .await
        .expect("update");
    assert_eq!(current_commit(&fixture.installed_dir), v2_commit);
    assert_eq!(
        file_content(&fixture.installed_dir, "extension.ts"),
        "// v2"
    );
}

#[tokio::test]
async fn should_refresh_cached_temporary_git_sources_when_resolving() {
    let fixture = Fixture::new();
    init_git_repo(&fixture.remote_dir);
    std::fs::write(
        fixture.remote_dir.join("package.json"),
        "{\"kiana\":{\"extensions\":[\"./kiana-extensions\"]}}",
    )
    .expect("write");
    git(&["add", "package.json"], &fixture.remote_dir);
    create_commit(
        &fixture.remote_dir,
        "kiana-extensions/session-breakdown.ts",
        "// v1",
        "Initial commit",
    );

    let cached_dir =
        temporary_git_dir(&fixture.env.agent_dir, "github.com", "test/extension", None);
    std::fs::create_dir_all(Path::new(&cached_dir).parent().expect("parent")).expect("mkdir");
    git(
        &["clone", &fixture.remote_dir.to_string_lossy(), &cached_dir],
        fixture.env.temp.path(),
    );
    assert_eq!(
        file_content(
            Path::new(&cached_dir),
            "kiana-extensions/session-breakdown.ts"
        ),
        "// v1"
    );
    create_commit(
        &fixture.remote_dir,
        "kiana-extensions/session-breakdown.ts",
        "// v2",
        "Second commit",
    );

    let result = fixture
        .package_manager
        .resolve_extension_sources(
            &[GIT_SOURCE.to_string()],
            ResolveExtensionSourcesOptions {
                temporary: true,
                local: false,
            },
        )
        .await
        .expect("resolve");

    assert_eq!(
        file_content(
            Path::new(&cached_dir),
            "kiana-extensions/session-breakdown.ts"
        ),
        "// v2"
    );
    assert!(is_enabled(&result.extensions, "session-breakdown.ts"));
}
