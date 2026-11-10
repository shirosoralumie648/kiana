//! `core/source-info.ts` 的**再导出面**。
//!
//! 定义在 `kiana_core::source_info`（理由见 `ARCHITECTURE.md` §3.5）；本模块只 re-export，
//! 保持 TS 的模块路径（`kiana_resources::source_info::SourceInfo` 与源码同名），不新增语义。
//!
//! TS: `packages/coding-agent/src/core/source-info.ts`。
//!
//! # 为什么是再导出
//! `SourceInfo` 等类型的读者分布在 `kiana-theme`（L6）、`kiana-ext-api`（L7）、
//! `kiana-extensions`（L9）与 `kiana-resources`（L10）。定义放在本 crate 会让前三个
//! 反向依赖，DAG 成环；因此实现统一在 `kiana-core`，这里只保持与 TS 同名的模块路径。
//!
//! # 再导出项的契约（细节见 `kiana_core::source_info`）
//! - `SourceInfo` / `PathMetadata` 以 camelCase 序列化，可选字段为 `None` 时整个键省略；
//! - `get_synthetic_path_source` 要求尖括号路径**闭合**，`is_synthetic_path` 只要求以 `<`
//!   开头，两者刻意不对称，不要“修正”；
//! - `create_synthetic_source_info` 的 `scope` / `origin` 默认 `temporary` / `top-level`；
//!   `SyntheticSourceInfoOptions::default()` 的 `source` 是空串且不校验，调用方必须自行填。
//!
//! 本文件没有逻辑；新增再导出前确认不会与其它模块的同名符号在 `index.rs` 扁平化时冲突。

pub use kiana_core::source_info::{
    BUILTIN_PATH_PREFIX, PathMetadata, SourceInfo, SourceOrigin, SourceScope,
    SyntheticSourceInfoOptions, create_source_info, create_synthetic_source_info,
    get_synthetic_path_source, is_synthetic_path,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_path_helpers_are_re_exported() {
        assert!(is_synthetic_path("builtin:mcp"));
        assert_eq!(
            get_synthetic_path_source("<inline:foo>"),
            Some("inline".to_string())
        );
        let info = create_synthetic_source_info(
            "builtin:mcp",
            SyntheticSourceInfoOptions {
                source: "builtin".to_string(),
                ..SyntheticSourceInfoOptions::default()
            },
        );
        assert_eq!(info.scope, SourceScope::Temporary);
        assert_eq!(info.origin, SourceOrigin::TopLevel);
        assert_eq!(BUILTIN_PATH_PREFIX, "builtin:");
    }
}
