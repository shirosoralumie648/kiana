//! 移植 `packages/coding-agent/test/export-html-cjk.test.ts`（#10154）。
//!
//! TS 测试把导出的 HTML 放进 `node:vm`，直接调用 viewer 的 `safeMarkedParse`，
//! 验证 `marked-cjk-friendly` 在中文标点旁的强调行为。Rust 不执行 JS；按任务规定
//! 改为：
//!
//! 1. 断言 `template.js` 与 `vendor/marked-cjk-friendly.min.js` 逐字节一致（SHA-256）；
//! 2. 断言导出的 base64 载荷与 TS 导出逐字节相同 —— CJK 文本、条目 id 与
//!    header 原样幸存；
//! 3. 断言 viewer 里确实注册了 CJK tokenizer（行为在浏览器端执行）。
//!
//! describe/it 标题保持逐字（见测试函数上的注释）。

use std::path::Path;

use base64::Engine as _;
use kiana_export::index::{ExportOptions, export_from_file};
use sha2::{Digest, Sha256};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/cjk-session.jsonl"
);
const EXPECTED_PAYLOAD: &str = include_str!("golden/cjk.payload.json");

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn export_payload() -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.html");
    let path = export_from_file(
        FIXTURE,
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect("export");
    let html = std::fs::read_to_string(path).expect("read html");
    let marker = "<script id=\"session-data\" type=\"application/json\">";
    let start = html.find(marker).expect("payload marker") + marker.len();
    let end = html[start..].find("</script>").expect("payload end") + start;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .expect("base64"),
    )
    .expect("utf8")
}

// it("renders emphasis delimited next to CJK punctuation")
#[test]
fn renders_emphasis_delimited_next_to_cjk_punctuation() {
    // 资产字节一致：CJK 强调逻辑（marked-cjk-friendly 的 emStrong tokenizer）与 TS 相同。
    assert_eq!(
        sha256_hex(kiana_export::assets::TEMPLATE_JS.as_bytes()),
        "2d9c2e1a0836b2bcabbee8cbff7558227aa980dbd82b8c0c3f6a3e39bb8210f2"
    );
    assert_eq!(
        sha256_hex(kiana_export::assets::MARKED_CJK_FRIENDLY_MIN_JS.as_bytes()),
        "93d2c938754cb24eef5b9ffecc268a98ef6beeb75dad614b0e874176aeeba565"
    );
    assert!(kiana_export::assets::TEMPLATE_JS.contains("markedCjkFriendly"));
    assert!(kiana_export::assets::TEMPLATE_JS.contains("emStrong"));

    // 数据形状：CJK 文本原样进入 base64 载荷（不转义、不转码）。
    let payload = export_payload();
    assert_eq!(payload, EXPECTED_PAYLOAD);
    assert!(payload.contains("**这是测试。**后面"));
    assert!(payload.contains("*斜体。*后面"));
    assert!(payload.contains("字**「引用」**字"));
    // header/entry id 与 leaf 幸存。
    assert!(payload.contains("\"id\":\"cjk-test\""));
    assert!(payload.contains("\"id\":\"e1\""));
    assert!(payload.contains("\"id\":\"e2\""));
    assert!(payload.contains("\"id\":\"e3\""));
    assert!(payload.contains("\"leafId\":\"e3\""));
    // 顺序：e1 → e2 → e3。
    let e1 = payload.find("\"id\":\"e1\"").unwrap();
    let e2 = payload.find("\"id\":\"e2\"").unwrap();
    let e3 = payload.find("\"id\":\"e3\"").unwrap();
    assert!(e1 < e2 && e2 < e3);
}

// it("keeps CommonMark flanking rules for non-CJK text")
#[test]
fn keeps_commonmark_flanking_rules_for_non_cjk_text() {
    // 非 CJK 的 flanking 规则由同一份 vendored 代码（marked + CJK tokenizer）决定；
    // 字节一致即行为一致。这里断言 viewer 使用 `strictStrikethroughRegex` 与
    // `breaks: true` 的既有配置，防止资产被误换版本。
    assert!(kiana_export::assets::TEMPLATE_JS.contains("strictStrikethroughRegex"));
    assert!(kiana_export::assets::TEMPLATE_JS.contains("breaks: true"));
    assert!(Path::new(FIXTURE).exists());
}
