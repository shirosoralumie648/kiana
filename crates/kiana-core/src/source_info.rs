//! `core/source-info.ts` 的**完整纯函数面**（`kiana-core` 持有，`kiana-resources` 只再导出）。
//!
//! 归属理由（`ARCHITECTURE.md` §3.5）：`SourceInfo` / `SourceScope` / `SourceOrigin` /
//! `PathMetadata` / `BUILTIN_PATH_PREFIX` 与 `create*SourceInfo` / `getSyntheticPathSource` /
//! `isSyntheticPath` 的读者分布在 `kiana-theme`（L6）、`kiana-ext-api`（L7）、`kiana-extensions`（L9）
//! 与 `kiana-resources`（L10）。若定义在 `kiana-resources`，上述 crate 都会反向依赖它，DAG 立刻成环；
//! 这些符号本身不依赖 `package-manager`，因此放在最底层可用的 `kiana-core`。
//!
//! TS 的 `source-info.ts` 与 `package-manager.ts` 互相引用（`PathMetadata` ↔
//! `BUILTIN_PATH_PREFIX`）；Rust 侧把两半都收进本模块，`kiana-resources` 的包管理只消费。
//!
//! TS: `packages/coding-agent/src/core/source-info.ts`、`core/package-manager.ts#PathMetadata`。
//!
//! ## 序列化契约
//!
//! [`SourceInfo`] / [`PathMetadata`] 用 camelCase 序列化，键序等于结构体字段声明顺序
//! （serde 按声明顺序输出）；`tests/source_info.rs` 逐字节断言了 JSON 字符串。
//! `baseDir` / `packageRoot` 为 `None` 时整个键被省略（`skip_serializing_if`）。
//! [`SourceInfo`] 被 `kiana-extensions` 再导出，并作为 `SlashCommandInfo.source_info`
//! 出现在扩展 API 与 RPC 结构中，因此改字段名、字段顺序或可选性都属于对外 JSON 契约变更。
//!
//! ## 两个判定函数刻意不对称
//!
//! [`get_synthetic_path_source`] 要求尖括号路径**闭合**（同时以 `<` 开头、`>` 结尾），
//! [`is_synthetic_path`] 只要求以 `<` 开头。因此 `"<inline"` 是合成路径却取不到 source
//! （返回 `None`）。调用方 `kiana-extensions` 的 `create_extension` 正是这样组合使用的：
//! 用 `is_synthetic_path` 决定 `base_dir` 是否为 `None`，用 `get_synthetic_path_source`
//! 决定 `source`（取不到时回退 `"local"`）。这是 TS 的原样行为，不要“修正”。
//!
//! ## 默认值与必填项
//!
//! [`create_synthetic_source_info`] 的 `scope` / `origin` 缺省为 `temporary` / `top-level`。
//! [`SyntheticSourceInfoOptions`] 派生了 `Default`，但 `source` 的默认值是空串且没有任何
//! 校验（TS 中该字段必填）；用 `..Default::default()` 时调用方必须自己填 `source`。

use serde::{Deserialize, Serialize};

/// 资源的作用域。TS: `source-info.ts#SourceScope`（`"user" | "project" | "temporary"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
    /// `"user"`。
    User,
    /// `"project"`。
    Project,
    /// `"temporary"`。
    Temporary,
}

/// 资源的来源层级。TS: `source-info.ts#SourceOrigin`（`"package" | "top-level"`）。
///
/// `top-level` 带连字符，因此逐字 `rename`，不能只靠 `rename_all`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceOrigin {
    /// `"package"`。
    #[serde(rename = "package")]
    Package,
    /// `"top-level"`。
    #[serde(rename = "top-level")]
    TopLevel,
}

/// 一个资源的来源信息。TS: `source-info.ts#SourceInfo`。
///
/// 字段顺序与 TS 接口声明一致（`ARCHITECTURE.md` §4.3 第 3 条）：`path`、`source`、`scope`、
/// `origin`、`baseDir`。`path` 是 `String` 而非 `PathBuf`：合成路径（`builtin:read`、
/// `<inline:foo>`）不是文件系统路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    /// 资源路径，可能是 `<builtin:...>` 或 `<inline:...>` 形式的合成路径。
    pub path: String,
    /// 来源标签：`"local"`、`"auto"`、`"builtin"`、`"cli"`、`"npm:foo"`、`"git:..."` 或扩展自定义。
    pub source: String,
    /// 作用域。
    pub scope: SourceScope,
    /// 来源层级。
    pub origin: SourceOrigin,
    /// 解析基准目录（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

/// 包管理器解析资源时附带的元数据。TS: `package-manager.ts#PathMetadata`。
///
/// §3.5 决定由本 crate 持有（`kiana-resources` 只再导出）。字段顺序是序列化契约：
/// `source`、`scope`、`origin`、`baseDir`、`packageRoot`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMetadata {
    /// 来源标签。
    pub source: String,
    /// 作用域。
    pub scope: SourceScope,
    /// 来源层级。
    pub origin: SourceOrigin,
    /// 解析基准目录（可选）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
    /// 包根目录（可选；仅包内资源有值）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_root: Option<String>,
}

/// 内置工具/扩展路径的前缀，例如 `builtin:read`、`builtin:mcp`。
/// TS: `source-info.ts#BUILTIN_PATH_PREFIX`。
pub const BUILTIN_PATH_PREFIX: &str = "builtin:";

/// 不指向文件的路径（合成路径）的 `source`。
///
/// `builtin:<name>` → `"builtin"`；`<prefix:name>` → 去掉尖括号后的第一段（空段 → `"temporary"`）；
/// 其它路径（普通文件路径）→ `None`。TS: `source-info.ts#getSyntheticPathSource`。
///
/// TS 只在**同时**以 `<` 开头且以 `>` 结尾时进入前缀分支；`isSyntheticPath` 则只要求以 `<`
/// 开头（见 [`is_synthetic_path`]）。两者刻意不对称，按 TS 保留。
///
/// 边界：以 `<` 开头但不以 `>` 结尾时返回 `None`。切分用字节下标，但 `<`、`>` 都是单字节
/// ASCII，`&path[1..len - 1]` 的两端一定落在 char 边界上，不会 panic。
/// 返回的 `Some` 是新分配的 `String`（`builtin` 分支也不例外）。
pub fn get_synthetic_path_source(path: &str) -> Option<String> {
    if path.starts_with(BUILTIN_PATH_PREFIX) {
        return Some("builtin".to_string());
    }
    if path.starts_with('<') && path.ends_with('>') {
        // `slice(1, -1)`：去掉首尾各一个 ASCII 字符，按 UTF-16 与按 UTF-8 结果相同。
        let inner = &path[1..path.len() - 1];
        let first = inner.split(':').next().unwrap_or("");
        return Some(if first.is_empty() {
            "temporary".to_string()
        } else {
            first.to_string()
        });
    }
    None
}

/// 路径是否是合成路径：以 `builtin:` 开头，或以 `<` 开头。
///
/// 注意 TS 只检查**开头**的 `<`，不要求闭合（`"<"` 也算合成路径）。TS:
/// `source-info.ts#isSyntheticPath`。
pub fn is_synthetic_path(path: &str) -> bool {
    path.starts_with(BUILTIN_PATH_PREFIX) || path.starts_with('<')
}

/// 由路径与元数据构造 [`SourceInfo`]；`base_dir` 原样透传。
/// TS: `source-info.ts#createSourceInfo`。
///
/// `PathMetadata::package_root` 不进入结果：[`SourceInfo`] 只有 `path` / `source` /
/// `scope` / `origin` / `base_dir` 五个字段，包根需要从元数据本身另行读取。
pub fn create_source_info(path: &str, metadata: &PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: metadata.source.clone(),
        scope: metadata.scope,
        origin: metadata.origin,
        base_dir: metadata.base_dir.clone(),
    }
}

/// [`create_synthetic_source_info`] 的选项。TS: `createSyntheticSourceInfo(path, options)`。
///
/// `source` 必填；`scope` 缺省 `temporary`，`origin` 缺省 `top-level`，`base_dir` 无默认。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntheticSourceInfoOptions {
    /// 来源标签（必填）。`Default` 派生出的值是空串，本函数不会校验，调用方需自己填。
    pub source: String,
    /// 作用域；缺省 `Temporary`。
    pub scope: Option<SourceScope>,
    /// 来源层级；缺省 `TopLevel`。
    pub origin: Option<SourceOrigin>,
    /// 解析基准目录。
    pub base_dir: Option<String>,
}

/// 构造合成路径（`builtin:`、`<inline:...>` 等）的 [`SourceInfo`]。
/// TS: `source-info.ts#createSyntheticSourceInfo`。
///
/// 不做任何校验：`path` 与 `source` 原样使用（`source` 为空串也接受），`base_dir` 原样
/// 透传；只补 `scope` / `origin` 的缺省值。
pub fn create_synthetic_source_info(path: &str, options: SyntheticSourceInfoOptions) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: options.source,
        scope: options.scope.unwrap_or(SourceScope::Temporary),
        origin: options.origin.unwrap_or(SourceOrigin::TopLevel),
        base_dir: options.base_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_source_uses_first_segment() {
        assert_eq!(
            get_synthetic_path_source("builtin:read"),
            Some("builtin".into())
        );
        assert_eq!(
            get_synthetic_path_source("builtin:"),
            Some("builtin".into())
        );
        assert_eq!(
            get_synthetic_path_source("<inline:foo>"),
            Some("inline".into())
        );
        assert_eq!(
            get_synthetic_path_source("<inline:1>"),
            Some("inline".into())
        );
        assert_eq!(get_synthetic_path_source("<>"), Some("temporary".into()));
        assert_eq!(get_synthetic_path_source("<:x>"), Some("temporary".into()));
        assert_eq!(get_synthetic_path_source("/tmp/x.ts"), None);
        // Not closed: `getSyntheticPathSource` returns undefined, but `isSyntheticPath` is true.
        assert_eq!(get_synthetic_path_source("<inline"), None);
    }

    #[test]
    fn synthetic_path_only_checks_the_prefix() {
        assert!(is_synthetic_path("builtin:read"));
        assert!(is_synthetic_path("<inline:foo>"));
        assert!(is_synthetic_path("<"));
        assert!(!is_synthetic_path("/tmp/<x>"));
        assert!(!is_synthetic_path(""));
    }
}
