//! `packages/coding-agent/test/package-manager-ssh.test.ts` 的 Rust 移植。
//!
//! TS 通过 `(packageManager as any).parseSource` 直接断言解析结果；Rust 的解析器是私有
//! 实现，这里改为断言**可观察行为**（git clone 的 repo 参数、本地路径的解析、identity
//! 去重后的安装次数），等价覆盖同一组规则。

#![cfg(unix)]
#![allow(clippy::await_holding_lock)] // TestEnv 在 await 期间持有环境锁以串行化进程环境。

mod common;

use std::path::Path;

use common::TestEnv;
use kiana_config::settings::schema::PackageSource;
use kiana_config::settings::schema::Settings;
use kiana_resources::package::{InstallOptions, PackageManager};

fn text(value: &str) -> PackageSource {
    PackageSource::Text(value.to_string())
}

// ===========================================================================
// describe("protocol URLs without git: prefix")
// ===========================================================================

#[tokio::test]
async fn should_parse_https_url() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("https://github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");
    let clone = env.first_call("git").expect("git clone");
    assert_eq!(clone.args[0], "clone");
    assert_eq!(clone.args[1], "https://github.com/user/repo");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_ssh_url() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("ssh://git@github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");
    let clone = env.first_call("git").expect("git clone");
    assert_eq!(clone.args[1], "ssh://git@github.com/user/repo");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

// ===========================================================================
// describe("shorthand URLs with git: prefix")
// ===========================================================================

#[tokio::test]
async fn should_parse_git_at_host_path_format() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("git:git@github.com:user/repo", InstallOptions::default())
        .await
        .expect("install");
    let clone = env.first_call("git").expect("git clone");
    assert_eq!(clone.args[1], "git@github.com:user/repo");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_host_path_shorthand() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install("git:github.com/user/repo", InstallOptions::default())
        .await
        .expect("install");
    let clone = env.first_call("git").expect("git clone");
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clone.args[2], target.to_string_lossy());
}

#[tokio::test]
async fn should_parse_shorthand_with_ref() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    package_manager
        .install(
            "git:git@github.com:user/repo@v1.0.0",
            InstallOptions::default(),
        )
        .await
        .expect("install");
    let calls = env.calls("git");
    assert_eq!(calls[0].args[0], "clone");
    assert_eq!(calls[0].args[1], "git@github.com:user/repo");
    assert_eq!(
        calls[1].args,
        vec!["checkout".to_string(), "v1.0.0".to_string()]
    );
}

// ===========================================================================
// describe("unsupported without git: prefix")
// ===========================================================================

#[tokio::test]
async fn should_treat_git_at_host_path_as_local_without_git_prefix() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .install("git@github.com:user/repo", InstallOptions::default())
        .await
        .expect_err("local path does not exist");
    let resolved = Path::new(&env.root()).join("git@github.com:user/repo");
    assert_eq!(
        error.message(),
        format!("Path does not exist: {}", resolved.to_string_lossy())
    );
    assert!(env.calls("git").is_empty());
}

#[tokio::test]
async fn should_treat_host_path_shorthand_as_local_without_git_prefix() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    let package_manager = env.manager(&settings_manager);
    let error = package_manager
        .install("github.com/user/repo", InstallOptions::default())
        .await
        .expect_err("local path does not exist");
    let resolved = Path::new(&env.root()).join("github.com/user/repo");
    assert_eq!(
        error.message(),
        format!("Path does not exist: {}", resolved.to_string_lossy())
    );
    assert!(env.calls("git").is_empty());
}

// ===========================================================================
// describe("identity normalization")
// ===========================================================================

#[tokio::test]
async fn should_normalize_protocol_and_shorthand_prefixed_urls_to_same_identity() {
    let env = TestEnv::new();
    let settings_manager = env.settings_manager(Settings::default());
    settings_manager.set_packages(vec![
        text("git:git@github.com:user/repo"),
        text("https://github.com/user/repo"),
        text("ssh://git@github.com/user/repo"),
    ]);
    let package_manager = env.manager(&settings_manager);

    // 三种写法 identity 相同：`resolve()` 去重后只安装一次。
    package_manager.resolve(None).await.expect("resolve");
    let clones: Vec<_> = env
        .calls("git")
        .into_iter()
        .filter(|call| call.args.first().map(String::as_str) == Some("clone"))
        .collect();
    assert_eq!(clones.len(), 1);
    let target = Path::new(&env.agent_dir).join("git/github.com/user/repo");
    assert_eq!(clones[0].args[2], target.to_string_lossy());
}
