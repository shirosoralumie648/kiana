//! dark.json / light.json（include_str! 原样保留）。
//!
//! TS: `packages/coding-agent/src/modes/interactive/theme/{dark,light}.json`（各 78 行）。
//!
//! 两个文件是**逐字节**拷贝到 `crates/kiana-theme/assets/` 的（`GAPS.md` G2.3：打包资源用
//! `include_str!` 嵌入；`kiana-theme` 是 L6、`kiana-core::config::get_themes_dir()` 只用来拼
//! `ThemeInfo.path` 的展示路径，内置主题**不**从磁盘读）。
//!
//! 内置主题不经过 `validateThemeJson()`（TS 的 `getBuiltinThemes()` 直接 `JSON.parse`），
//! 见 `crate::theme` 的加载逻辑。
//!
//! # 为什么用 `include_str!`
//!
//! 两份 JSON 在**编译期**被嵌入二进制：运行期不访问 `get_themes_dir()`，也不会遇到文件
//! 缺失、权限或编码错误，内置主题因此总是与二进制同版本。代价是改资源必须重新编译。
//!
//! # 被谁使用
//!
//! `crate::theme` 的私有 `builtin_themes()` 在首次访问时用 `kiana_json` 的 JS 语义解析
//! 这两份文本并缓存到 `OnceLock`；解析失败会 panic（那里的 `expect` 由单元测试兜底）。
//! 本模块只负责把文本带进编译单元，不解析、不校验。
//!
//! # 注意事项
//!
//! - 两个常量是 UTF-8 文本（`&str`），不是文件路径；使用方需要自己解析。
//! - 文件内容被测试逐字断言（首行 `$schema`、78 行、`appearance` 字段），不要手工重排
//!   键序或改缩进，否则测试会失败。
//! - 它们经 [`crate::index`] 再导出到 crate 根，下游可写 `kiana_theme::DARK_JSON`。

/// `dark.json` 的原始字节（UTF-8 文本）。
///
/// 编译期嵌入（`include_str!`），不访问磁盘；修改资源后必须重新编译才会生效。
/// 内容以 `crates/kiana-theme/assets/dark.json` 为准（含制表符缩进），键序与缩进由测试锁定，修改资源后必须重新编译。
pub const DARK_JSON: &str = include_str!("../assets/dark.json");

/// `light.json` 的原始字节（UTF-8 文本）。
///
/// 与 [`DARK_JSON`] 相同：编译期嵌入、运行期无 IO；修改后需重新编译。
pub const LIGHT_JSON: &str = include_str!("../assets/light.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_the_published_theme_documents() {
        // 锁定发布资源的稳定格式：`$schema` 首行、制表符缩进与 78 行，改动资源需同步更新断言。
        assert!(DARK_JSON.starts_with(
            "{\n\t\"$schema\": \"https://kiana.dev/schemas/theme.schema.json\",\n\t\"name\": \"dark\","
        ));
        assert!(LIGHT_JSON.starts_with("{\n\t\"$schema\": \"https://kiana.dev/schemas/theme.schema.json\",\n\t\"name\": \"light\","));
        assert_eq!(DARK_JSON.lines().count(), 78);
        assert_eq!(LIGHT_JSON.lines().count(), 78);
        // 与发布出去的 schema 引用一致。
        assert!(DARK_JSON.contains("\"appearance\": \"dark\""));
        assert!(LIGHT_JSON.contains("\"appearance\": \"light\""));
    }
}
