//! `core/kiana-manifest.ts` 的公开 API 测试（TS 无对应测试文件，按 `ARCHITECTURE.md` §4.8 第 9 条新写）。
//!
//! TS: `packages/coding-agent/src/core/kiana-manifest.ts`。

use std::fs;
use std::path::PathBuf;

use kiana_core::kiana_manifest::{KianaManifest, read_kiana_manifest};
use serde_json::json;
use tempfile::TempDir;

fn write_package_json(contents: &[u8]) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("package.json");
    fs::write(&path, contents).expect("write package.json");
    (dir, path)
}

fn read_str(contents: &str) -> Option<KianaManifest> {
    let (_dir, path) = write_package_json(contents.as_bytes());
    read_kiana_manifest(&path)
}

#[test]
fn reads_the_four_declared_resource_fields_in_ts_order() {
    let manifest =
        read_str(r#"{"kiana":{"themes":["t"],"prompts":["p"],"skills":["s"],"extensions":["e"]}}"#)
            .expect("manifest");
    assert_eq!(
        manifest,
        KianaManifest {
            extensions: Some(vec!["e".into()]),
            skills: Some(vec!["s".into()]),
            prompts: Some(vec!["p".into()]),
            themes: Some(vec!["t".into()]),
        }
    );
    assert_eq!(
        serde_json::to_string(&manifest).expect("serialize"),
        r#"{"extensions":["e"],"skills":["s"],"prompts":["p"],"themes":["t"]}"#
    );
}

#[test]
fn an_empty_array_is_declared_but_invalid_entries_are_omitted() {
    let manifest =
        read_str(r#"{"kiana":{"extensions":[],"skills":["a",1],"prompts":"x","themes":null}}"#)
            .expect("manifest");
    assert_eq!(
        manifest,
        KianaManifest {
            extensions: Some(Vec::new()),
            ..KianaManifest::default()
        }
    );
    assert_eq!(
        serde_json::to_string(&manifest).expect("serialize"),
        r#"{"extensions":[]}"#
    );
}

#[test]
fn an_empty_kiana_object_is_some_but_a_missing_kiana_is_none() {
    assert_eq!(read_str(r#"{"kiana":{}}"#), Some(KianaManifest::default()));
    assert_eq!(read_str(r#"{}"#), None);
}

#[test]
fn non_object_kiana_values_are_none() {
    for contents in [
        r#"{"kiana":null}"#,
        r#"{"kiana":[]}"#,
        r#"{"kiana":[]}"#,
        r#"{"kiana":"x"}"#,
        r#"{"kiana":1}"#,
        r#"{"kiana":true}"#,
    ] {
        assert_eq!(read_str(contents), None, "contents: {contents}");
    }
}

#[test]
fn top_level_non_objects_are_none() {
    for contents in ["null", "[]", "1", "\"x\"", "true"] {
        assert_eq!(read_str(contents), None, "contents: {contents}");
    }
}

#[test]
fn any_read_or_parse_error_is_none() {
    // syntax error
    assert_eq!(read_str("not json"), None);
    // empty file
    assert_eq!(read_str(""), None);
    // trailing comma (JSON.parse rejects it)
    assert_eq!(read_str(r#"{"kiana":{},}"#), None);
    // missing file
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(read_kiana_manifest(&dir.path().join("missing.json")), None);
    // a directory
    assert_eq!(read_kiana_manifest(dir.path()), None);
}

#[test]
fn strips_a_leading_utf8_bom() {
    let (_dir, path) = write_package_json(b"\xEF\xBB\xBF{\"kiana\":{\"skills\":[\"s\"]}}");
    assert_eq!(
        read_kiana_manifest(&path),
        Some(KianaManifest {
            skills: Some(vec!["s".into()]),
            ..KianaManifest::default()
        })
    );
}

#[test]
fn invalid_utf8_is_decoded_lossily_like_node() {
    // Node's readFileSync(path, "utf-8") replaces invalid bytes with U+FFFD instead of failing.
    // The replacement inside the string keeps the JSON valid.
    let (_dir, path) = write_package_json(b"{\"kiana\":{\"skills\":[\"a\xFFb\"]}}");
    assert_eq!(
        read_kiana_manifest(&path),
        Some(KianaManifest {
            skills: Some(vec!["a\u{FFFD}b".into()]),
            ..KianaManifest::default()
        })
    );
}

#[test]
fn parses_with_json_parse_semantics_not_serde_json_limits() {
    // Deep nesting: V8 has no 128-level recursion limit; `serde_json::from_str` does.
    let deep = format!(
        "{{\"kiana\":{{\"skills\":[\"s\"]}},\"deep\":{}1{}}}",
        "[".repeat(200),
        "]".repeat(200)
    );
    assert_eq!(
        read_str(&deep),
        Some(KianaManifest {
            skills: Some(vec!["s".into()]),
            ..KianaManifest::default()
        })
    );

    // Lone surrogate escapes: JSON.parse accepts them; js_parse stores U+FFFD (ARCHITECTURE §8.31).
    assert_eq!(
        read_str(r#"{"kiana":{"skills":["\ud800"]}}"#),
        Some(KianaManifest {
            skills: Some(vec!["\u{FFFD}".into()]),
            ..KianaManifest::default()
        })
    );
    assert_eq!(
        read_str(r#"{"kiana":{"skills":["s"]},"\ud800":1}"#),
        Some(KianaManifest {
            skills: Some(vec!["s".into()]),
            ..KianaManifest::default()
        })
    );

    // Numeric overflow: JSON.parse("1e400") is Infinity, which is not a parse error.
    assert_eq!(
        read_str(r#"{"kiana":{"skills":["s"]},"x":1e400}"#),
        Some(KianaManifest {
            skills: Some(vec!["s".into()]),
            ..KianaManifest::default()
        })
    );
    assert_eq!(read_str(r#"{"kiana":1e400}"#), None);

    // Duplicate keys: last value wins (JSON.parse semantics).
    assert_eq!(
        read_str(r#"{"kiana":{"skills":["a"],"skills":["b"]}}"#),
        Some(KianaManifest {
            skills: Some(vec!["b".into()]),
            ..KianaManifest::default()
        })
    );
}

#[test]
fn ignores_unknown_fields_in_pkg_and_kiana() {
    let manifest =
        read_str(r#"{"name":"x","kiana":{"extensions":["e"],"extra":{"a":1}}}"#).expect("manifest");
    assert_eq!(
        serde_json::to_value(&manifest).expect("serialize"),
        json!({"extensions": ["e"]})
    );
}
