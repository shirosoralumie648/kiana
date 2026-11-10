//! `core/kiana-manifest.ts`：`KianaManifest` / `read_kiana_manifest`（`package.json` 的 `kiana` 字段校验）。
//!
//! `kiana-extensions::loader`（L9）与 `kiana-resources` 包管理（L10）共用；`kiana-resources::manifest`
//! 只再导出（`ARCHITECTURE.md` §3.5）。
//!
//! TS: `packages/coding-agent/src/core/kiana-manifest.ts`。
//!
//! ## 语义（逐条对齐 TS）
//!
//! 1. `readFileSync(utf-8)` → `stripBom` → `JSON.parse`；**任何**异常（读失败、语法错误、非 UTF-8
//!    路径类型）都返回 `None`。Node 用 UTF-8 解码、非法字节变成 U+FFFD（而不是报错），因此这里
//!    用 [`String::from_utf8_lossy`]。`JSON.parse` 用 [`kiana_json::js_parse::parse`]（与
//!    `serde_json::from_str` 不同：接受孤立代理项转义、数值溢出、无 128 层递归上限）。
//!    孤立代理项无法存进 Rust `String`，按 `js_parse` 的既有约定替换成 U+FFFD（§8 第 31 条）。
//! 2. 顶层不是对象，或 `kiana` 不是对象（`null`、数组、标量都算）→ `None`。
//! 3. 四个字段按固定顺序 `extensions`、`skills`、`prompts`、`themes` 处理：值必须是**全部为
//!    字符串的数组**才写入；不满足的字段**整个省略**（不是空数组）。
//! 4. `kiana` 是对象但没有有效字段时返回 `Some(KianaManifest::default())`（空 manifest）。这与「没有
//!    `kiana` 键」返回 `None` 的语义不同：调用方据此区分「已声明但全空」与「没有声明」。
//!
//! ## 调用方与执行流程
//!
//! 调用方是 `kiana-extensions` 的扩展加载器与 `kiana-resources` 的资源包发现/解析
//! （`kiana-resources::manifest` 只做再导出）；它们传入 `package.json` 的路径，本模块完成
//! “读文件 → 去 BOM → `JSON.parse` → 取 `kiana` 对象 → 逐字段校验”这一条链路。
//!
//! ## 失败模式与不变量
//!
//! - 读取用 `std::fs::read` + [`String::from_utf8_lossy`]（同步读取整个文件）：路径不存在、
//!   路径是目录、无权限等 IO 错误与 JSON 语法错误一样，全部折叠成 `None`；签名没有错误
//!   类型，调用方**无法区分失败原因**。
//! - 顶层值不是对象，或 `kiana` 缺失/不是对象（`null`、数组、标量都算），返回 `None`。
//! - 字段校验是“全有或全无”：数组里混入任意非字符串元素时整个字段按未声明处理；
//!   空数组合法，得到 `Some(vec![])`。
//! - 重复键遵循 `JSON.parse` 语义（后值覆盖前值，由 `js_parse` 保证，测试覆盖）。
//! - `None` 字段在序列化时被省略，因此 `KianaManifest` 的 JSON 形状只包含有效字段。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::text::strip_bom;

/// `package.json#kiana` 声明的资源目录。TS: `kiana-manifest.ts#KianaManifest`。
///
/// 字段顺序是序列化契约：`extensions`、`skills`、`prompts`、`themes`。`None` 表示该字段不存在或
/// 值无效（TS 里是 `undefined`）。
///
/// 序列化时 `None` 字段被省略（`skip_serializing_if`），空数组仍输出 `[]`；因此
/// `serde_json::to_string` 的结果只含有效字段（见 `tests/kiana_manifest.rs`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KianaManifest {
    /// 扩展目录列表。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    /// 技能目录列表。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// prompt 模板目录列表。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    /// 主题目录列表。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
}

/// 读取并校验 `package.json` 的 `kiana` 字段。
/// TS: `kiana-manifest.ts#readKianaManifest`。
///
/// `package_json_path` 是 `package.json` 文件路径（不是目录）；函数同步读取整个文件
/// （阻塞当前线程）。读失败、JSON 非法、顶层不是对象或 `kiana` 不是对象都返回 `None`；
/// `kiana` 存在但没有有效字段时返回 `Some(默认值)`。返回 `None` 时不保留任何错误信息，
/// 需要区分“文件不存在”“语法错误”等场景的调用方应自行检查文件系统。
pub fn read_kiana_manifest(package_json_path: &Path) -> Option<KianaManifest> {
    let bytes = std::fs::read(package_json_path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = strip_bom(&text);
    let pkg: serde_json::Value = kiana_json::js_parse::parse(&text).ok()?;

    let pkg = pkg.as_object()?;
    let kiana = pkg.get("kiana")?.as_object()?;

    let mut manifest = KianaManifest::default();
    // 四个字段彼此独立：不是数组、或数组里混入非字符串时，slot 保持 `None`
    // （TS 里字段被省略），不会保留部分元素；空数组则写入 `Some(vec![])`。
    for (field, slot) in [
        ("extensions", &mut manifest.extensions),
        ("skills", &mut manifest.skills),
        ("prompts", &mut manifest.prompts),
        ("themes", &mut manifest.themes),
    ] {
        let Some(serde_json::Value::Array(entries)) = kiana.get(field) else {
            continue;
        };
        if entries.iter().all(serde_json::Value::is_string) {
            *slot = Some(
                entries
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect(),
            );
        }
    }
    Some(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("package.json");
        std::fs::write(&path, contents).expect("write");
        (dir, path)
    }

    #[test]
    fn reads_all_four_fields_in_order() {
        let (_dir, path) = write_temp(
            br#"{"kiana":{"extensions":["a"],"skills":["b"],"prompts":["c"],"themes":["d"]}}"#,
        );
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                extensions: Some(vec!["a".into()]),
                skills: Some(vec!["b".into()]),
                prompts: Some(vec!["c".into()]),
                themes: Some(vec!["d".into()]),
            })
        );
    }

    #[test]
    fn empty_array_is_kept_but_invalid_entries_omit_the_field() {
        let (_dir, path) = write_temp(
            br#"{"kiana":{"extensions":[],"skills":["a",1],"prompts":"x","themes":null}}"#,
        );
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                extensions: Some(Vec::new()),
                ..KianaManifest::default()
            })
        );
    }

    #[test]
    fn empty_kiana_object_is_some_default() {
        let (_dir, path) = write_temp(br#"{"kiana":{}}"#);
        assert_eq!(read_kiana_manifest(&path), Some(KianaManifest::default()));
    }

    #[test]
    fn missing_or_non_object_kiana_is_none() {
        for contents in [
            &br#"{}"#[..],
            br#"{"kiana":null}"#,
            br#"{"kiana":[]}"#,
            br#"{"kiana":"x"}"#,
            br#"null"#,
            br#"[]"#,
            br#"not json"#,
            b"",
        ] {
            let (_dir, path) = write_temp(contents);
            assert_eq!(
                read_kiana_manifest(&path),
                None,
                "contents: {:?}",
                String::from_utf8_lossy(contents)
            );
        }
    }

    #[test]
    fn strips_a_utf8_bom() {
        let (_dir, path) = write_temp(b"\xEF\xBB\xBF{\"kiana\":{\"skills\":[\"s\"]}}");
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                skills: Some(vec!["s".into()]),
                ..KianaManifest::default()
            })
        );
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_kiana_manifest(&dir.path().join("nope.json")), None);
    }

    #[test]
    fn parses_like_json_parse_not_like_a_128_level_serde_limit() {
        // Node accepts nesting beyond serde_json's recursion_limit; the manifest is still read.
        let deep = format!(
            "{{\"kiana\":{{\"skills\":[\"s\"]}},\"deep\":{}1{}}}",
            "[".repeat(200),
            "]".repeat(200)
        );
        let (_dir, path) = write_temp(deep.as_bytes());
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                skills: Some(vec!["s".into()]),
                ..KianaManifest::default()
            })
        );
    }

    #[test]
    fn accepts_lone_surrogate_escapes_like_json_parse() {
        // JSON.parse returns a string holding the lone surrogate; Rust strings cannot, so
        // `js_parse` follows the workspace convention and stores U+FFFD (ARCHITECTURE §8.31).
        let (_dir, path) = write_temp(br#"{"kiana":{"skills":["\ud800"]}}"#);
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                skills: Some(vec!["\u{FFFD}".into()]),
                ..KianaManifest::default()
            })
        );
        let (_dir, path) = write_temp(br#"{"kiana":{"skills":["s"]},"\ud800":1}"#);
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                skills: Some(vec!["s".into()]),
                ..KianaManifest::default()
            })
        );
    }

    #[test]
    fn accepts_numeric_overflow_like_json_parse() {
        // JSON.parse("1e400") is Infinity; js_parse stores non-finite numbers as null. Either way
        // the `kiana` object itself is still read.
        for contents in [
            &br#"{"kiana":{"skills":["s"]},"x":1e400}"#[..],
            br#"{"kiana":{"skills":["s"]},"x":-1e400}"#,
        ] {
            let (_dir, path) = write_temp(contents);
            assert_eq!(
                read_kiana_manifest(&path),
                Some(KianaManifest {
                    skills: Some(vec!["s".into()]),
                    ..KianaManifest::default()
                })
            );
        }
        // Infinity as `kiana` itself is not an object in either implementation.
        let (_dir, path) = write_temp(br#"{"kiana":1e400}"#);
        assert_eq!(read_kiana_manifest(&path), None);
    }

    #[test]
    fn ignores_unknown_fields() {
        let (_dir, path) = write_temp(br#"{"name":"x","kiana":{"extensions":["e"],"other":1}}"#);
        assert_eq!(
            read_kiana_manifest(&path),
            Some(KianaManifest {
                extensions: Some(vec!["e".into()]),
                ..KianaManifest::default()
            })
        );
    }
}
