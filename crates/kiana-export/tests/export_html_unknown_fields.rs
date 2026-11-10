//! 导出载荷必须逐字回放 `JSON.parse` 得到的原始对象。
//!
//! TS 的 `SessionManager.getEntries()` / `getHeader()` 返回的就是解析后的普通对象，
//! `JSON.stringify(sessionData)` 因而保留未知字段、未知条目类型与**文件里的键序**。
//! Rust 侧 `get_entries_json()` / `get_header_json()` 用 `StoredFileEntry::raw` 复刻该
//! 行为；本测试用含未知字段的会话文件与 TS 参考实现逐字节比对。
//!
//! 期望值来自 Node 26 运行 `/home/ctyun/kiana-work/upstream-reference` 的
//! `core/export-html/index.ts`（主题 `dark`）：
//! `tests/golden/unknown-fields.sha256` 与 `tests/golden/unknown-fields.payload.json`。

use base64::Engine as _;
use kiana_export::index::{ExportOptions, export_from_file};
use sha2::{Digest, Sha256};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/unknown-fields-session.jsonl"
);
const EXPECTED_SHA: &str = include_str!("golden/unknown-fields.sha256");
const EXPECTED_PAYLOAD: &str = include_str!("golden/unknown-fields.payload.json");

const RAW_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/raw-entries-session.jsonl"
);
const RAW_EXPECTED_SHA: &str = include_str!("golden/raw-entries.sha256");
const RAW_EXPECTED_PAYLOAD: &str = include_str!("golden/raw-entries.payload.json");

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn export_payload(fixture: &str) -> (String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.html");
    let path = export_from_file(
        fixture,
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
    let payload = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .expect("base64"),
    )
    .expect("utf8 payload");
    (html, payload)
}

#[test]
fn unknown_fields_and_key_order_survive_the_export_payload() {
    let (html, payload) = export_payload(FIXTURE);
    assert_eq!(sha256_hex(html.as_bytes()), EXPECTED_SHA.trim());
    assert_eq!(payload, EXPECTED_PAYLOAD);

    // header：未知键在 `cwd` 之后原样保留。
    assert!(payload.contains("\"cwd\":\"/tmp/kiana-u\",\"extraHeader\":42"));
    // 条目级未知键位于 `timestamp` 与 `message` 之间。
    assert!(payload.contains(
        "\"timestamp\":\"1970-01-01T00:00:00.001Z\",\"extraEntry\":\"keep-me\",\"message\":"
    ));
    // 消息级未知键位于 `timestamp` 之后。
    assert!(payload.contains("\"content\":\"hi\",\"timestamp\":1,\"extraMessage\":true"));
    // 已知条目类型的未知键。
    assert!(payload.contains("\"targetId\":\"e1\",\"label\":\"x\",\"extraLabel\":7"));
    // 未知条目类型整条原样保留。
    assert!(payload.contains("\"type\":\"mystery\",\"id\":\"m1\",\"parentId\":\"l1\",\"timestamp\":\"1970-01-01T00:00:00.003Z\",\"foo\":\"bar\""));
    // leafId 仍是最后一个键（位于 entries 之后）。
    assert!(payload.ends_with("\"leafId\":\"m1\"}"));
}

/// 会话文件里出现非对象真值（TS 的 `FileEntry` 运行期是 `JSON.parse` 的结果）时，
/// 导出载荷逐字保留它们；最后一条没有字符串 id 时 `leafId` 按 `undefined` 处理，
/// `JSON.stringify` **省略该键**（不是 `null`）。
#[test]
fn non_object_entries_and_undefined_leaf_id_match_ts() {
    let (html, payload) = export_payload(RAW_FIXTURE);
    assert_eq!(sha256_hex(html.as_bytes()), RAW_EXPECTED_SHA.trim());
    assert_eq!(payload, RAW_EXPECTED_PAYLOAD);

    assert!(payload.contains("},\"hello\",[1,2],42,true,{}]"));
    // `null` 与空行在解析阶段被跳过（TS `if (entry)`）：`true` 之后直接是 `{}`。
    assert!(!payload.contains("true,null"));
    // `leafId` 键整体省略（没有 `"leafId"`，载荷以 entries 数组的 `]}` 结束）。
    assert!(!payload.contains("\"leafId\""));
    assert!(payload.ends_with("true,{}]}"));
}
