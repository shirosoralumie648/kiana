//! 移植 `packages/coding-agent/test/export-html-system-messages.test.ts`（#11261）。
//!
//! TS 测试用 `node:vm` 执行 `template.js` 的四个 helper。Rust 不引入 JS 引擎；替代
//! 方案（任务规定的做法）是：
//!
//! 1. **资产字节一致**：`template.js` 的 SHA-256 与参考版本相同（因此 JS helper 的
//!    行为与参考版本完全一致）；
//! 2. **数据形状**：完整比对 base64 载荷与 TS 导出结果（system message 的
//!    `content`/`sections`/`toolsAdded`/`toolsRemoved` 与 compaction 的
//!    `systemMessage` 逐字节保留）；
//! 3. **marker 断言**：把 JS helper 的逐字逻辑（`getSystemMessageText`、
//!    `renderSystemMessageUpdate`、`describeSystemUpdate`、`isLeadingSystemEntry`、
//!    `carriesSystemState`）在源码中定位出来；
//! 4. 数据本身与 `kiana-ai` 的 Rust helper（`get_system_message_text` /
//!    `render_system_message_update`）对齐——TS 测试正是拿这两个函数做对照。
//!
//! describe/it 标题保持逐字（见测试函数上的注释）。

use std::path::Path;

use base64::Engine as _;
use kiana_ai::types::SystemMessage;
use kiana_export::index::{ExportOptions, export_from_file};
use regex::Regex;
use sha2::{Digest, Sha256};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/system-messages-session.jsonl"
);
const EXPECTED_PAYLOAD: &str = include_str!("golden/system-messages.payload.json");
const TEMPLATE_JS_SHA256: &str = "2d9c2e1a0836b2bcabbee8cbff7558227aa980dbd82b8c0c3f6a3e39bb8210f2";

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
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&html[start..end])
        .expect("base64");
    String::from_utf8(decoded).expect("utf8 payload")
}

fn matches(pattern: &str) -> bool {
    Regex::new(pattern)
        .expect("valid regex")
        .is_match(kiana_export::assets::TEMPLATE_JS)
}

// it("renders system message text the same way the model receives it")
#[test]
fn renders_system_message_text_the_same_way_the_model_receives_it() {
    // 资产字节一致（JS helper 与 TS 逐字节相同）。
    assert_eq!(
        sha256_hex(kiana_export::assets::TEMPLATE_JS.as_bytes()),
        TEMPLATE_JS_SHA256
    );
    assert!(kiana_export::assets::TEMPLATE_JS.contains("function systemContentText(content)"));
    assert!(kiana_export::assets::TEMPLATE_JS.contains("function getSystemMessageText("));
    assert!(kiana_export::assets::TEMPLATE_JS.contains("function renderSystemMessageUpdate("));

    // 数据形状：systemMessage 完整 survived base64 round-trip。
    let payload = export_payload();
    assert_eq!(
        payload, EXPECTED_PAYLOAD,
        "payload must match TS byte for byte"
    );

    let value: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    let initial: SystemMessage =
        serde_json::from_value(value["entries"][0]["message"].clone()).expect("initial system");
    let update: SystemMessage =
        serde_json::from_value(value["entries"][2]["message"].clone()).expect("update system");

    // TS 测试用 kiana-ai 的 JS helper 做对照；Rust 侧对应函数输出同一文本。
    assert_eq!(
        kiana_ai::utils::text::get_system_message_text(&initial),
        "You are a helpful assistant.\n\n<rules>\nbe concise\n</rules>\n\n<project_context>\nv1\n</project_context>"
    );
    assert_eq!(
        kiana_ai::utils::text::render_system_message_update(&update),
        "Extra instructions\n\nUpdated system prompt section \"project_context\":\n\n<project_context>\nv2\n</project_context>\n\nRemoved system prompt section \"rules\"."
    );
}

// it("renders the first system entry on a branch as the prompt and later ones as updates")
#[test]
fn renders_the_first_system_entry_on_a_branch_as_the_prompt_and_later_ones_as_updates() {
    assert!(kiana_export::assets::TEMPLATE_JS.contains("function isLeadingSystemEntry("));

    let payload = export_payload();
    let value: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    let entries = value["entries"].as_array().expect("entries");
    // s1（首个 system）在 s2（后续 update）之前，且父链连续。
    assert_eq!(entries[0]["id"], "s1");
    assert_eq!(entries[0]["message"]["role"], "system");
    assert_eq!(entries[2]["id"], "s2");
    assert_eq!(entries[2]["message"]["role"], "system");
    // 描述改动的数据来自 content / sections / toolsAdded / toolsRemoved。
    assert_eq!(
        entries[2]["message"]["content"][0]["text"],
        "Extra instructions"
    );
    assert_eq!(
        entries[2]["message"]["sections"]["project_context"],
        "<project_context>\nv2\n</project_context>"
    );
    assert!(entries[2]["message"]["sections"]["rules"].is_null());
    assert_eq!(entries[2]["message"]["toolsAdded"][0]["name"], "bash");
    assert_eq!(entries[2]["message"]["toolsRemoved"][0]["name"], "read");
    // JS 侧 describeSystemUpdate 的逐字分支。
    assert!(kiana_export::assets::TEMPLATE_JS.contains("'instructions'"));
    assert!(kiana_export::assets::TEMPLATE_JS.contains("tools ${toolChanges.join(' ')}"));
}

// it("treats a compaction snapshot as earlier system state")
#[test]
fn treats_a_compaction_snapshot_as_earlier_system_state() {
    assert!(kiana_export::assets::TEMPLATE_JS.contains("function carriesSystemState("));
    assert!(matches(
        r"entry\.type === 'compaction' && entry\.systemMessage"
    ));

    let payload = export_payload();
    let value: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    let compaction = &value["entries"][3];
    assert_eq!(compaction["type"], "compaction");
    assert_eq!(compaction["id"], "c1");
    assert_eq!(compaction["systemMessage"]["role"], "system");
    assert_eq!(
        compaction["systemMessage"]["sections"]["rules"],
        "<rules>\nbe concise\n</rules>"
    );
}

// it("renders system entries in the transcript")
#[test]
fn renders_system_entries_in_the_transcript() {
    assert!(matches(
        r"if \(msg\.role === 'system'\) \{\s*let html = '';"
    ));
    assert!(
        kiana_export::assets::TEMPLATE_JS
            .contains("renderSystemPromptBlock('System Prompt', text)")
    );
    assert!(
        kiana_export::assets::TEMPLATE_JS
            .contains("renderToolsList('Available Tools', msg.toolsAdded)")
    );
    // 夹具文件确实能被导出（防御性：路径存在）。
    assert!(Path::new(FIXTURE).exists());
}
