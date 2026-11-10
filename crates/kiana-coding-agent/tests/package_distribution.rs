//! `test/package-distribution.test.ts` 的 Rust 适配（`ARCHITECTURE.md` §8 G5.2）。
//!
//! TS 断言 npm 包的 `bin`/`main`/`exports` 布局；Rust 版是纯二进制，不发布 npm 包，因此
//! 改为断言：
//! - 两个二进制 `kiana`/`kiana-rpc` 都存在且 `--version` 输出 `1.4.0\n`、退出码 0；
//! - `get_package_dir()` 优先使用 `KIANA_PACKAGE_DIR`，README/docs/CHANGELOG 都位于其下；
//! - 未设置 `KIANA_PACKAGE_DIR` 时回退到可执行文件所在目录（`target/debug/kiana` 上方没有
//!   `package.json`，因此与 §G5.2 记录的发布要求一致）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn package_root_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_root")
}

fn run_version(binary: &str) -> (i32, String) {
    let output = Command::new(binary)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", std::env::temp_dir())
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .output()
        .expect("run binary");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn ships_both_binaries_that_report_the_release_version() {
    let kiana = Path::new(env!("CARGO_BIN_EXE_kiana"));
    let kiana_rpc = Path::new(env!("CARGO_BIN_EXE_kiana-rpc"));
    assert!(
        kiana.is_file(),
        "kiana binary missing at {}",
        kiana.display()
    );
    assert!(
        kiana_rpc.is_file(),
        "kiana-rpc binary missing at {}",
        kiana_rpc.display()
    );

    let (kiana_code, kiana_stdout) = run_version(&kiana.to_string_lossy());
    assert_eq!(kiana_code, 0);
    assert_eq!(kiana_stdout, "1.4.0\n");

    let (rpc_code, rpc_stdout) = run_version(&kiana_rpc.to_string_lossy());
    assert_eq!(rpc_code, 0);
    assert_eq!(rpc_stdout, "1.4.0\n");
}

/// 环境变量在测试进程内是全局状态；本文件的用例串行执行。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn get_package_dir_honors_kiana_package_dir_and_falls_back_to_the_exe_directory() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = package_root_fixture();
    let original = std::env::var_os("KIANA_PACKAGE_DIR");

    // SAFETY: 本测试持有 ENV_LOCK，且测试进程内没有其它线程读取该变量。
    unsafe { std::env::set_var("KIANA_PACKAGE_DIR", &fixture) };
    let resolved = kiana_core::config::get_package_dir();
    assert_eq!(
        resolved,
        PathBuf::from(kiana_core::paths::normalize_path(
            &fixture.to_string_lossy(),
            &Default::default()
        )),
        "KIANA_PACKAGE_DIR must win over the executable location"
    );
    for path in [
        kiana_core::config::get_readme_path(),
        kiana_core::config::get_docs_path(),
        kiana_core::config::get_changelog_path(),
    ] {
        assert!(
            path.starts_with(&resolved),
            "{} is not under {}",
            path.display(),
            resolved.display()
        );
    }
    assert!(kiana_core::config::get_readme_path().is_file());
    assert!(kiana_core::config::get_docs_path().is_dir());
    assert!(kiana_core::config::get_changelog_path().is_file());

    // 未设置时回退到可执行文件所在目录（这里没有 npm 包布局）。
    // SAFETY: 见上。
    unsafe { std::env::remove_var("KIANA_PACKAGE_DIR") };
    let executable = std::env::current_exe().expect("current exe");
    let expected = executable.parent().expect("exe parent").to_path_buf();
    assert_eq!(
        kiana_core::config::get_package_dir(),
        expected,
        "without KIANA_PACKAGE_DIR get_package_dir must fall back to the executable directory"
    );

    // SAFETY: 恢复调用方的环境。
    match original {
        Some(value) => unsafe { std::env::set_var("KIANA_PACKAGE_DIR", value) },
        None => unsafe { std::env::remove_var("KIANA_PACKAGE_DIR") },
    }
}
