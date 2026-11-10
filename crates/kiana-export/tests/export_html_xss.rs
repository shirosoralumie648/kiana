//! 移植 `packages/coding-agent/test/export-html-xss.test.ts`。
//!
//! 所有断言都是对 `template.js`（逐字节内嵌的 viewer 脚本）的正则匹配，因此可以
//! 原样移植。describe/it 标题保持逐字（见测试函数上的注释）。

use regex::Regex;

use kiana_export::assets::TEMPLATE_JS;

fn matches(pattern: &str) -> bool {
    Regex::new(pattern)
        .unwrap_or_else(|error| panic!("invalid regex {pattern}: {error}"))
        .is_match(TEMPLATE_JS)
}

fn does_not_match(pattern: &str) -> bool {
    !matches(pattern)
}

// it("overrides the marked link renderer to use scheme allow-list sanitization")
#[test]
fn overrides_the_marked_link_renderer_to_use_scheme_allow_list_sanitization() {
    assert!(matches(r"link\s*\(\s*token\s*\)"));
    assert!(matches(r"sanitizeMarkdownUrl\(token\.href\)"));
    assert!(matches(r"\^\(https\?\|mailto\|tel\|ftp\)"));
}

// it("overrides the marked image renderer to use scheme allow-list sanitization")
#[test]
fn overrides_the_marked_image_renderer_to_use_scheme_allow_list_sanitization() {
    assert!(matches(r"image\s*\(\s*token\s*\)"));
    assert!(matches(r"sanitizeMarkdownUrl\(token\.href\)"));
}

// it("strips C0 controls before checking and emitting markdown URLs")
#[test]
fn strips_c0_controls_before_checking_and_emitting_markdown_urls() {
    assert!(TEMPLATE_JS.contains("replace(/[\\x00-\\x1f\\x7f]/g, '')"));
    // TS: `not.toMatch(/\^\\s\*\(javascript\|vbscript\|data\):/i)`。JS 源里 `\\s` 与 `\*`
    // 是**字面量**反斜杠 + s / 星号（不是空白/量词），`\|` 是字面量竖线；因此等价的正则
    // 必须匹配字面文本 `^\s*(javascript|vbscript|data):`，且大小写不敏感。
    let forbidden = r"(?i)\^\\s\*\(javascript\|vbscript\|data\):";
    let forbidden_regex = Regex::new(forbidden).expect("valid regex");
    // 自检正则等价性：命中被禁止的源码文本（大小写不敏感），但不命中宽松的
    // “caret + 任意空白 + 分组”解释。
    assert!(forbidden_regex.is_match("^\\s*(javascript|vbscript|data):"));
    assert!(forbidden_regex.is_match("^\\S*(JavaScript|VBSCRIPT|Data):"));
    assert!(!forbidden_regex.is_match("^   (javascript):"));
    assert!(!forbidden_regex.is_match(TEMPLATE_JS));
}

// it("escapes href attributes in the custom link renderer")
#[test]
fn escapes_href_attributes_in_the_custom_link_renderer() {
    assert!(matches(r"escapeHtml\(href\)"));
}

// it("escapes image mimeType attributes")
#[test]
fn escapes_image_mime_type_attributes() {
    assert!(does_not_match(r"\$\{img\.mimeType\}"));
    assert!(matches(r"escapeHtml\(img\.mimeType"));
}

// it("escapes image data attributes")
#[test]
fn escapes_image_data_attributes() {
    assert!(does_not_match(r#";base64,\$\{img\.data\}""#));
    assert!(matches(
        ";base64,\\$\\{escapeHtml\\(img\\.data \\|\\| (?:''|\"\")\\)\\}\""
    ));
}

// it("escapes entry IDs before inserting them into attributes")
#[test]
fn escapes_entry_ids_before_inserting_them_into_attributes() {
    assert!(does_not_match(r#"id="\$\{entryId\}""#));
    assert!(does_not_match(r#"data-entry-id="\$\{entryId\}""#));
    assert!(matches(r"entry-\$\{escapeHtml\(entry\.id\)\}"));
    assert!(matches(r#"data-entry-id="\$\{escapeHtml\(entryId\)\}""#));
}

// it("escapes tree metadata rendered from session fields")
#[test]
fn escapes_tree_metadata_rendered_from_session_fields() {
    assert!(does_not_match(r"\[\$\{msg\.toolName \|\| 'tool'\}\]"));
    assert!(does_not_match(r"\[\$\{msg\.role\}\]"));
    assert!(does_not_match(r"\[model: \$\{entry\.modelId\}\]"));
    assert!(does_not_match(r"\[thinking: \$\{entry\.thinkingLevel\}\]"));
    assert!(does_not_match(r"\[\$\{entry\.type\}\]"));
    assert!(matches(r"\$\{escapeHtml\(msg\.toolName \|\| 'tool'\)\}"));
    assert!(matches(r"\$\{escapeHtml\(msg\.role\)\}"));
    assert!(matches(r"\$\{escapeHtml\(entry\.modelId\)\}"));
    assert!(matches(r"\$\{escapeHtml\(entry\.thinkingLevel\)\}"));
    assert!(matches(r"\$\{escapeHtml\(entry\.type\)\}"));
}

// it("escapes model names in the exported header")
#[test]
fn escapes_model_names_in_the_exported_header() {
    assert!(does_not_match(
        r"\$\{globalStats\.models\.join\(', '\) \|\| 'unknown'\}"
    ));
    assert!(matches(
        r"\$\{escapeHtml\(globalStats\.models\.join\(', '\) \|\| 'unknown'\)\}"
    ));
}
