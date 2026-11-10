//! `runtime/prelude-source.ts` 的移植测试：逐字节锁定 `PRELUDE_SOURCE`。
//!
//! 对应 spec §2.2.7 与 ARCHITECTURE.md §8 第 22 条：解析后的 prelude 是 17233 个 UTF-16
//! 码元、全 ASCII、FNV-1a `8c6c9242`、SHA-256 `57c8dc8c…5281`。确定性指纹包含该哈希，
//! 所以任何字节偏移都会让 `prelude [0-9a-f]{8}` 相关断言失败。

use kiana_codemode::prelude::{
    MAX_OUTPUT_CHARS, MAX_OUTPUT_ITEMS, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS, PRELUDE_HASH,
    PRELUDE_SOURCE, PRELUDE_SOURCE_LEN, PRELUDE_SOURCE_SHA256, fnv1a, js_length,
};

#[test]
fn limits_match_typescript() {
    assert_eq!(MAX_STORE_VALUE_CHARS, 262_144);
    assert_eq!(MAX_STORE_TOTAL_CHARS, 1_048_576);
    assert_eq!(MAX_OUTPUT_CHARS, 16_777_216);
    assert_eq!(MAX_OUTPUT_ITEMS, 100_000);
}

#[test]
fn prelude_length_and_hashes_match_the_reference() {
    use sha2::{Digest, Sha256};
    assert_eq!(js_length(PRELUDE_SOURCE), 17_233);
    assert_eq!(js_length(PRELUDE_SOURCE), PRELUDE_SOURCE_LEN);
    assert_eq!(fnv1a(PRELUDE_SOURCE), "8c6c9242");
    assert_eq!(fnv1a(PRELUDE_SOURCE), PRELUDE_HASH);
    let digest = Sha256::digest(PRELUDE_SOURCE.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        hex,
        "57c8dc8c0e812117d82167fa751bc368f249d5aaee56d321066fc5280c785281"
    );
    assert_eq!(hex, PRELUDE_SOURCE_SHA256);
}

// spec §2.2.7：全部 ASCII、TAB 缩进、`\n` 换行、无 BOM、无 CR。
#[test]
fn prelude_is_ascii_with_tab_indentation_and_lf() {
    assert!(PRELUDE_SOURCE.is_ascii());
    assert_eq!(PRELUDE_SOURCE.len(), js_length(PRELUDE_SOURCE));
    assert!(!PRELUDE_SOURCE.contains('\r'));
    assert!(!PRELUDE_SOURCE.starts_with('\u{feff}'));
    assert!(PRELUDE_SOURCE.contains("\n\t"));
    assert!(
        !PRELUDE_SOURCE.contains("\n    "),
        "indentation must be tabs, not spaces"
    );
}

// spec §2.2.7 的首尾逐字边界。
#[test]
fn prelude_boundaries_are_verbatim() {
    let head = "(function (bridge, toolsJson, globalsJson, storeJson) {\n\t\"use strict\";\n\n\t// Freeze every object reachable";
    assert!(PRELUDE_SOURCE.starts_with(head), "head mismatch");
    let tail = "\t\t\treturn true;\n\t\t},\n\t};\n})";
    assert!(PRELUDE_SOURCE.ends_with(tail), "tail mismatch");
    assert_eq!(
        &PRELUDE_SOURCE[PRELUDE_SOURCE.len() - 10..],
        "\t},\n\t};\n})"
    );
}

// 12 处模板插值（5 个不同表达式）已替换成字面量；替换后不再有 `${`。
#[test]
fn template_interpolations_are_already_substituted() {
    assert!(!PRELUDE_SOURCE.contains("${"));
    let value_sites = PRELUDE_SOURCE.matches("262144").count();
    assert_eq!(value_sites, 2, "MAX_STORE_VALUE_CHARS appears twice");
    assert_eq!(PRELUDE_SOURCE.matches("1048576").count(), 2);
    assert_eq!(PRELUDE_SOURCE.matches("16777216").count(), 2);
    assert_eq!(PRELUDE_SOURCE.matches("100000").count(), 2);
    // `JSON.stringify(IMAGE_HELPER_EXPECTS)` 的四处插值 → 同一段（转义后的）文本出现 4 次。
    let needle = "image expects a non-empty image URL string";
    assert_eq!(PRELUDE_SOURCE.matches(needle).count(), 4);
    // 模板里 `${JSON.stringify(IMAGE_HELPER_EXPECTS)}` 在 JS 字符串字面量内部，因此双引号被
    // 反斜杠转义两次（`type: \\\"image\\\"`）。
    assert!(PRELUDE_SOURCE.contains("or an image block { type: \\\"image\\\", data, mimeType }"));
}

// 脚本可见 API 的名字都在 prelude 里（拼写回归的粗网）。
#[test]
fn prelude_exposes_the_script_api() {
    for needle in [
        "ALL_TOOLS",
        "\"use strict\"",
        "stalled",
        "settle",
        "codemode-prelude.js",
        "store() is for small state such as IDs or summaries.",
        "The script is waiting on a promise that can never settle",
        "Cannot assign to read only property",
    ] {
        assert!(
            PRELUDE_SOURCE.contains(needle),
            "prelude must contain {needle:?}"
        );
    }
}

// FNV-1a 逐 UTF-16 码元（Node 参照实现生成的值）。
#[test]
fn fnv1a_matches_javascript() {
    assert_eq!(fnv1a(""), "811c9dc5");
    assert_eq!(fnv1a("a"), "e40c292c");
    assert_eq!(fnv1a("foobar"), "bf9cf968");
    assert_eq!(fnv1a("😀"), "cb31c4b8");
    assert_eq!(js_length("😀"), 2);
}
