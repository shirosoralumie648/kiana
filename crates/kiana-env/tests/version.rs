//! 包版本（spec §2.2.1、ARCHITECTURE.md §8 第 7、24 条）：`kiana-env` 上报的 `KIANA_ENV_VERSION` 必须等于
//! TS 的 `packages/env/package.json#version`，与参照 daemon 的 `build.rs` 一致。
//!
//! 参照 daemon 的 `build.rs` 直接读 `../package.json`，因此它**不可能**上报 crate 版本；本移植的
//! `build.rs` 用 `KIANA_ENV_VERSION` 注入（若设置）否则用 npm 包版本字面量（见 `crates/kiana-env/build.rs`）。
//!
//! 本测试是验收门槛，**不允许静默跳过**：能读到 `packages/env/package.json` 时对比该文件的
//! `version`；读不到时对比移植时记录的值 `RECORDED_NPM_VERSION`（= 参照实现的 `kiana-env --version`）。
//! 两种情况下都比对构建出的二进制本身（`--version` 输出与 `hello` 帧），因此一个上报 `0.1.0`
//! （crate 版本）的构建必然失败。

mod common;

use common::{Daemon, hello};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 移植时 `packages/env/package.json#version` 的值（参照实现 `kiana-env --version` 的输出）。
const RECORDED_NPM_VERSION: &str = "1.4.0";

/// `packages/env/package.json`：`KIANA_ENV_NPM_PACKAGE` 显式指定，否则在 `CARGO_MANIFEST_DIR`
/// 的祖先里找（工作区里 `kiana/` 没有 npm 包，包在隔壁的 TS 仓库里）。
fn npm_package() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("KIANA_ENV_NPM_PACKAGE") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest.ancestors() {
        for candidate in [
            ancestor.join("packages/env/package.json"),
            ancestor.join("kiana/packages/env/package.json"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn version_of(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("read package.json");
    let json: Value = serde_json::from_str(&text).expect("parse package.json");
    json["version"]
        .as_str()
        .expect("package.json has a version")
        .to_string()
}

/// 期望的版本：npm 包的 `version`（能找到时），否则是移植时记录的值。
fn expected_version() -> (&'static str, String) {
    match npm_package() {
        Some(package) => ("packages/env/package.json", version_of(&package)),
        None => (
            "the recorded npm package version",
            RECORDED_NPM_VERSION.to_string(),
        ),
    }
}

#[test]
fn env_version_matches_npm_package() {
    let (source, expected) = expected_version();
    assert_eq!(
        env!("KIANA_ENV_VERSION"),
        expected,
        "KIANA_ENV_VERSION must equal the version of {source}; the reference daemon's build.rs reads \
         packages/env/package.json, so a build reporting the crate version ({}) diverges from it. \
         When the npm package version changes, update `NPM_VERSION` in crates/kiana-env/build.rs and \
         `RECORDED_NPM_VERSION` here.",
        env!("CARGO_PKG_VERSION"),
    );

    // The observable surface: `kiana-env --version` prints it and the `hello` frame carries it.
    let mut daemon = Daemon::spawn_with_args(&["--version"]);
    assert_eq!(
        daemon.handshake(),
        format!("kiana-env {expected}\n"),
        "kiana-env --version must print the npm package version"
    );
    daemon.close_stdin();
    let status = daemon.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "kiana-env --version exits 0");

    let mut daemon = Daemon::start(&[]);
    let handshake = daemon.handshake();
    assert!(handshake.starts_with("KIANA-ENV "), "{handshake:?}");
    assert_eq!(
        hello(&mut daemon)["version"],
        Value::String(expected),
        "the hello frame reports the npm package version"
    );
}
