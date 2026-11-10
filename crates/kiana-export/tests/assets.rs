//! 资产逐字节校验：大小、SHA-256、占位符数量与 vendor 版本头。
//!
//! TS 源：`core/export-html/{template.html,template.css,template.js}` 与
//! `vendor/{marked,marked-cjk-friendly,highlight}.min.js`。Rust 用 `include_str!`
//! 内嵌同一份文件；任何字节变化都会破坏 HTML 导出的逐字节一致性。

use sha2::{Digest, Sha256};

use kiana_export::assets::{
    HIGHLIGHT_MIN_JS, MARKED_CJK_FRIENDLY_MIN_JS, MARKED_MIN_JS, TEMPLATE_CSS, TEMPLATE_HTML,
    TEMPLATE_JS,
};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn assets_match_reference_sizes_and_hashes() {
    let expected: [(&str, &str, usize, &str); 6] = [
        (
            "template.html",
            TEMPLATE_HTML,
            2271,
            "47d208235b066c856dae69c60281a3b99d164b99be9b81153d880375c9327105",
        ),
        (
            "template.css",
            TEMPLATE_CSS,
            23131,
            "c9d26cc314336192021d604c0f1a4a63115172b24d03500571379f9172c06fc9",
        ),
        (
            "template.js",
            TEMPLATE_JS,
            86667,
            "2d9c2e1a0836b2bcabbee8cbff7558227aa980dbd82b8c0c3f6a3e39bb8210f2",
        ),
        (
            "vendor/marked.min.js",
            MARKED_MIN_JS,
            44640,
            "438eedfcf932a414d0d0bfeea32dc365c063563b8ca713b4687fc8f8b501e5e4",
        ),
        (
            "vendor/marked-cjk-friendly.min.js",
            MARKED_CJK_FRIENDLY_MIN_JS,
            4754,
            "93d2c938754cb24eef5b9ffecc268a98ef6beeb75dad614b0e874176aeeba565",
        ),
        (
            "vendor/highlight.min.js",
            HIGHLIGHT_MIN_JS,
            121727,
            "837a6fa5b0c736b52bbde2b2b6190f305da3fc9ed41681db5321507057b5c846",
        ),
    ];
    for (name, contents, size, hash) in expected {
        assert_eq!(contents.len(), size, "{name} size");
        assert_eq!(sha256_hex(contents.as_bytes()), hash, "{name} sha256");
    }
}

#[test]
fn template_placeholders_occur_exactly_once() {
    let html_placeholders = [
        "{{CSS}}",
        "{{JS}}",
        "{{SESSION_DATA}}",
        "{{MARKED_JS}}",
        "{{MARKED_CJK_FRIENDLY_JS}}",
        "{{HIGHLIGHT_JS}}",
    ];
    for placeholder in html_placeholders {
        assert_eq!(
            count(TEMPLATE_HTML, placeholder),
            1,
            "{placeholder} in template.html"
        );
    }
    assert_eq!(
        count(TEMPLATE_HTML, "{{"),
        6,
        "template.html placeholder total"
    );

    let css_placeholders = [
        "{{THEME_VARS}}",
        "{{BODY_BG}}",
        "{{CONTAINER_BG}}",
        "{{INFO_BG}}",
    ];
    for placeholder in css_placeholders {
        assert_eq!(
            count(TEMPLATE_CSS, placeholder),
            1,
            "{placeholder} in template.css"
        );
    }
    assert_eq!(
        count(TEMPLATE_CSS, "{{"),
        4,
        "template.css placeholder total"
    );

    // template.js 与三个 vendor 文件不得含任何 `{{`，否则会被后续替换误伤。
    for (name, contents) in [
        ("template.js", TEMPLATE_JS),
        ("vendor/marked.min.js", MARKED_MIN_JS),
        (
            "vendor/marked-cjk-friendly.min.js",
            MARKED_CJK_FRIENDLY_MIN_JS,
        ),
        ("vendor/highlight.min.js", HIGHLIGHT_MIN_JS),
    ] {
        assert_eq!(count(contents, "{{"), 0, "{name} must not contain {{{{");
    }
}

#[test]
fn vendor_version_headers_are_pinned() {
    assert!(MARKED_MIN_JS.contains("marked v18.0.11"));
    assert!(MARKED_CJK_FRIENDLY_MIN_JS.contains("marked-cjk-friendly v0.1.2"));
    assert!(HIGHLIGHT_MIN_JS.contains("Highlight.js v11.9.0"));
}

#[test]
fn assets_contain_the_markers_the_ported_tests_assert() {
    // whitespace 测试（template.css）。
    assert!(TEMPLATE_CSS.contains(".ansi-line"));
    assert!(TEMPLATE_CSS.contains("white-space: pre-wrap"));
    assert!(TEMPLATE_CSS.contains("white-space: pre;"));
    assert!(TEMPLATE_CSS.contains(".output-preview > div:not(.expand-hint)"));
    assert!(TEMPLATE_CSS.contains(".output-full > div:not(.expand-hint)"));

    // xss / skill-block / system-message 测试（template.js）。
    for marker in [
        "sanitizeMarkdownUrl",
        "parseSkillBlock",
        "skillBlock.userMessage",
        "hasUserContent",
        "tree-role-skill",
        "renderSystemPromptBlock",
        "renderToolsList",
        "describeSystemUpdate",
        "renderSystemMessageUpdate",
        "getSystemMessageText",
        "isLeadingSystemEntry",
        "safeMarkedParse",
    ] {
        assert!(TEMPLATE_JS.contains(marker), "template.js marker {marker}");
    }
}

#[test]
fn js_string_replacement_hazards_are_known() {
    // `generateHtml` 用的是 JS `String.prototype.replace(string, string)`：replacement 里
    // 的 `$` 序列会被 GetSubstitution 解释。这两个文件里的 `$$`/`$&` 正是 TS 导出比
    // “原样插入”多 12 字节的原因（见 index::js_replace_first 的单元测试与 golden 测试）。
    assert_eq!(count(TEMPLATE_JS, "$$"), 1);
    assert_eq!(count(TEMPLATE_JS, "$&"), 0);
    assert_eq!(count(HIGHLIGHT_MIN_JS, "$$"), 1);
    assert_eq!(count(HIGHLIGHT_MIN_JS, "$&"), 1);
    for (name, contents) in [
        ("template.html", TEMPLATE_HTML),
        ("template.css", TEMPLATE_CSS),
        ("template.js", TEMPLATE_JS),
        ("vendor/marked.min.js", MARKED_MIN_JS),
        (
            "vendor/marked-cjk-friendly.min.js",
            MARKED_CJK_FRIENDLY_MIN_JS,
        ),
        ("vendor/highlight.min.js", HIGHLIGHT_MIN_JS),
    ] {
        // `` $` `` / `$'` 会插入匹配前后的整段模板，若出现会让导出的占位符语义失控。
        assert_eq!(count(contents, "$`"), 0, "{name} must not contain $`");
        assert_eq!(count(contents, "$'"), 0, "{name} must not contain $'");
    }
}
