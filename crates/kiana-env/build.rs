//! 把 npm 包版本暴露为 `KIANA_ENV_VERSION`（`hello` 帧与 `kiana-env --version` 上报）。
//!
//! TS 的 daemon `build.rs` 直接读 `../package.json`（= `packages/env/package.json`）的 `version`，
//! 因此参照实现的上报值永远是 npm 包版本（`1.4.0`），不可能是 crate 版本；客户端只在部署时按
//! `hello.version` 记录远端版本，`kiana-env --version` 也打印它。
//!
//! Rust 工作区不再位于 npm 包内，`../package.json` 不存在。这里按与 `kiana_ai::KIANA_AI_VERSION` /
//! `kiana_core::config::VERSION` 相同的模式（`ARCHITECTURE.md` §8 第 24 条）把它写成**字面量**：
//!
//! 1. `KIANA_ENV_VERSION`（CI / 发布脚本注入，取自 `packages/env/package.json` 的 `version`）优先，
//!    保留 spec §2.2.1 的注入契约；
//! 2. 未注入时用 `NPM_VERSION`——移植时 `packages/env/package.json#version` 的值；
//! 3. **不**回退 `CARGO_PKG_VERSION`：workspace 的 `0.1.0` 与 npm 包无关，上报它会让
//!    `kiana-env --version` 与 `hello.version` 偏离参照实现（正是 `env_version_matches_npm_package`
//!    要抓的差异）。改 npm 包版本时同步改这一行；测试会在能找到 `packages/env/package.json` 时
//!    对比两者。

/// 移植时 `packages/env/package.json#version` 的值（参照实现 `kiana-env --version` 的输出）。
const NPM_VERSION: &str = "1.4.0";

fn main() {
    println!("cargo:rerun-if-env-changed=KIANA_ENV_VERSION");
    let version = std::env::var("KIANA_ENV_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| NPM_VERSION.to_string());
    println!("cargo:rustc-env=KIANA_ENV_VERSION={version}");
}
