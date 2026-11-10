//! `utils/version.ts`——`@earendil-works/kiana-ai` 的包版本。
//!
//! TS 侧这个常量由 `scripts/sync-versions.js` 与 `packages/ai/package.json` 的 `version`
//! 保持同步，值是**字面量**（不是运行时读文件）。Rust 侧照抄同一字面量，保证 `kiana --version`、
//! detailed `User-Agent`（`kiana/1.4.0 (...)`，见 `utils/identity.rs`）沿用同一字面量，保持对外标识稳定。
//!
//! 决策（`ARCHITECTURE.md` §8 第 24 条 / GAPS G5.1）：**不做构建期注入**，也不读
//! `CARGO_PKG_VERSION`（workspace 版本是 `0.1.0`）。发布前由 CI 校验本常量等于
//! `packages/ai/package.json` 的 `version`；改版本时同步改这一行。
//!
//! # 修改影响
//!
//! 本常量经 crate 根与 [`crate::index`] 再导出，并参与 detailed `User-Agent`
//! （`utils/identity.rs` 的 `kiana/{version} (...)`）与扩展虚拟模块
//! `@earendil-works/kiana-ai` 的 `KIANA_AI_VERSION`。交互式 CLI 的版本展示用的是
//! `kiana_core::config::VERSION`（另一个独立字面量），两者必须在发布时一起升级；CI 分别
//! 校验它们等于各自 `package.json` 的 `version`。`tests/identity.rs` 的
//! `keeps_the_version_constant_in_sync_with_package_json` 逐字锁定当前值 `"1.4.0"`，
//! 因此改这一行会让该测试失败——这是有意的守卫。
//!
//! TS: `utils/version.ts#KIANA_AI_VERSION`

/// `KIANA_AI_VERSION`：`packages/ai/package.json` 的 `version`（1.4.0）。
pub const KIANA_AI_VERSION: &str = "1.4.0";
