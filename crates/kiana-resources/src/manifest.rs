//! `KianaManifest` / `read_kiana_manifest` 的再导出面。
//!
//! 定义在 `kiana_core::kiana_manifest`（`core/kiana-manifest.ts`）：`kiana-extensions`（L9）的
//! `loader.ts` 与 `kiana-resources`（L10）的包管理都要读它。`ARCHITECTURE.md` §3.5 决定由
//! `kiana-core` 持有唯一实现，本模块只保持 TS 的模块路径（`kiana_resources::manifest::read_kiana_manifest`）。
//!
//! TS: `packages/coding-agent/src/core/kiana-manifest.ts`。
//!
//! # 为什么定义不在本 crate
//! `read_kiana_manifest` 同时被 `kiana-extensions`（L9）与 `kiana-resources`（L10）使用。
//! 若实现放在 L10，L9 就必须反向依赖本 crate，DAG 成环；因此唯一实现放最底层的
//! `kiana-core`，本模块只保留 TS 的模块路径。
//!
//! # 再导出项的语义（细节见 `kiana_core::kiana_manifest`）
//! - [`read_kiana_manifest`] 同步读取整个 `package.json` 并校验 `kiana` 字段；读失败、
//!   JSON 非法、顶层或 `kiana` 不是对象都返回 `None`，不保留错误原因；
//! - `kiana` 存在但四个字段都无效时返回 `Some(KianaManifest::default())`，与“没有 `kiana`
//!   键”的 `None` 可区分；
//! - 每个字段只有在值是“全部由字符串组成的数组”时才保留，否则整个字段为 `None`；
//!   空数组合法；`None` 字段序列化时省略。
//!
//! 本文件不含逻辑；改语义要改 `kiana-core`，并同步本 crate 与下游的测试。

pub use kiana_core::kiana_manifest::{KianaManifest, read_kiana_manifest};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_exports_read_kiana_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("package.json");
        std::fs::write(&path, "{\"kiana\":{\"skills\":[\"s\"]}}").expect("write");
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                skills: Some(vec!["s".to_string()]),
                ..KianaManifest::default()
            })
        );
    }
}
