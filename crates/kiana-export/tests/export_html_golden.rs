//! HTML 导出的字节级 golden 测试（§7.3 / §8.4）。
//!
//! 固定夹具 + 固定主题 → 导出的 HTML 必须与 golden 基线逐字节相同。
//!
//! 期望值来自 Node 26.12.1 运行 `/home/ctyun/kiana-work/upstream-reference` 的
//! `core/export-html/index.ts`（与只读参考的 export-html 目录逐字节相同）：
//!
//! - `tests/golden/export_html.sha256`：`golden-session.jsonl` + 主题 `dark` 的
//!   HTML SHA-256；
//! - `tests/golden/golden-session.payload.json`：base64 解码后的
//!   `JSON.stringify(sessionData)` 文本。
//!
//! 说明：任务文本给出的期望哈希 `b38289…` 无法由该参考实现复现；产品改名后
//! Rust 侧实测为 `d46e3a89…`（UTF-16 长度 285,780、字节长度 285,822）。按
//! “TS 源码优先”的原则采用实测值，并在交付报告中记录该偏差。
//!
//! `golden-usage-session.jsonl` 覆盖 `usage` / `model_change` 条目与
//! 0.0-vs-0 的数字格式（`tests/golden/export_html_usage.sha256`）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use kiana_export::index::{
    ExportError, ExportOptions, ExportSessionState, ExportToolInfo, export_from_file,
    export_session_to_html,
};
use kiana_session_core::session_manager::SessionManager;
use sha2::{Digest, Sha256};

const GOLDEN_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/golden-session.jsonl"
);
const USAGE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/golden-usage-session.jsonl"
);
const EXPECTED_SHA: &str = include_str!("golden/export_html.sha256");
const EXPECTED_USAGE_SHA: &str = include_str!("golden/export_html_usage.sha256");
const EXPECTED_PAYLOAD: &str = include_str!("golden/golden-session.payload.json");
const EXPECTED_USAGE_PAYLOAD: &str = include_str!("golden/golden-usage.payload.json");

/// 进程 cwd 是全局状态；只有默认输出文件名的测试会临时切目录。
static CWD_LOCK: Mutex<()> = Mutex::new(());

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn export(fixture: &str, output: &Path, theme_name: Option<&str>) -> Result<String, ExportError> {
    export_from_file(
        fixture,
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: theme_name.map(str::to_string),
            tool_renderer: None,
        }),
    )
}

fn payload_of(html: &str) -> String {
    let marker = "<script id=\"session-data\" type=\"application/json\">";
    let start = html.find(marker).expect("payload marker") + marker.len();
    let end = html[start..].find("</script>").expect("payload end") + start;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .expect("base64"),
    )
    .expect("utf8 payload")
}

#[test]
fn golden_session_html_matches_ts_byte_for_byte() {
    let dir = tempfile::tempdir().expect("tempdir");
    let explicit = dir.path().join("explicit.html");
    let path = export(GOLDEN_FIXTURE, &explicit, Some("dark")).expect("export");
    let html = std::fs::read_to_string(&path).expect("read html");

    // 期望的 HTML：字节长度 285,822 / UTF-16 长度 285,780。
    assert_eq!(html.len(), 285_822, "byte length");
    assert_eq!(html.encode_utf16().count(), 285_780, "UTF-16 length");
    assert_eq!(sha256_hex(html.as_bytes()), EXPECTED_SHA.trim());

    // 当前主题也是 dark 时（不传 themeName）必须得到同一个哈希。
    kiana_theme::init_theme(Some("dark"), false);
    let current = dir.path().join("current.html");
    let path = export(GOLDEN_FIXTURE, &current, None).expect("export");
    let current_html = std::fs::read_to_string(&path).expect("read html");
    assert_eq!(sha256_hex(current_html.as_bytes()), EXPECTED_SHA.trim());

    // 单独验证 base64 载荷：与 TS 的 `JSON.stringify(sessionData)` 逐字节相同。
    let payload = payload_of(&html);
    assert_eq!(payload, EXPECTED_PAYLOAD);
    // 未提供 AgentState / toolRenderer 时三个键整体省略。
    assert!(!payload.contains("\"systemPrompt\""));
    assert!(!payload.contains("\"tools\""));
    assert!(!payload.contains("\"renderedTools\""));
    // JS 数字格式：0.001 / 0.002 / 0.003，整数 0 不写成 0.0。
    assert!(payload.contains(
        "\"cost\":{\"input\":0.001,\"output\":0.002,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0.003}"
    ));
}

#[test]
fn golden_usage_and_model_change_survive_with_js_number_formatting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("usage.html");
    let path = export(USAGE_FIXTURE, &output, Some("dark")).expect("export");
    let html = std::fs::read_to_string(&path).expect("read html");

    assert_eq!(sha256_hex(html.as_bytes()), EXPECTED_USAGE_SHA.trim());
    let payload = payload_of(&html);
    assert_eq!(payload, EXPECTED_USAGE_PAYLOAD);
    // `usage` 条目：0 与小数都按 JS 规则输出。
    assert!(payload.contains(
        "\"type\":\"usage\",\"id\":\"u1\",\"parentId\":\"e1\",\"timestamp\":\"1970-01-01T00:00:00.003Z\",\"kind\":\"cache_warm\",\"provider\":\"anthropic\",\"model\":\"test-model\",\"usage\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":10,\"totalTokens\":10,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0.125,\"total\":0.125}}"
    ));
    assert!(payload.contains(
        "\"type\":\"model_change\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"1970-01-01T00:00:00.001Z\",\"provider\":\"anthropic\",\"modelId\":\"test-model\""
    ));
    assert!(!payload.contains(":0.0,"));
    assert!(!payload.contains(":0.0}"));
}

#[test]
fn export_options_from_output_path_matches_legacy_overload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("legacy.html");
    let path = export_from_file(
        GOLDEN_FIXTURE,
        Some(ExportOptions::from(output.to_string_lossy().into_owned())),
    )
    .expect("export");
    assert_eq!(PathBuf::from(&path), output);
    let from_str = dir.path().join("legacy2.html");
    let from_str_text = from_str.to_string_lossy().into_owned();
    let path2 = export_from_file(
        GOLDEN_FIXTURE,
        Some(ExportOptions::from(from_str_text.as_str())),
    )
    .expect("export");
    assert_eq!(PathBuf::from(&path2), from_str);
}

#[test]
fn missing_file_reports_verbatim_message() {
    let missing = "/definitely/not/here/session.jsonl";
    let error = export(missing, Path::new("/tmp/unused.html"), Some("dark")).expect_err("missing");
    assert!(matches!(error, ExportError::FileNotFound { .. }));
    assert_eq!(error.to_string(), format!("File not found: {missing}"));
}

#[test]
fn in_memory_session_and_empty_session_errors_are_verbatim() {
    let manager = SessionManager::in_memory("/tmp", None, None).expect("in memory");
    let error = export_session_to_html(&manager, None, None).expect_err("in-memory");
    assert_eq!(error.to_string(), "Cannot export in-memory session to HTML");

    // 文件被删除 → Nothing to export yet。
    let dir = tempfile::tempdir().expect("tempdir");
    let session_path = dir.path().join("session.jsonl");
    std::fs::copy(GOLDEN_FIXTURE, &session_path).expect("copy");
    let manager = SessionManager::open(session_path.to_str().unwrap(), None, None).expect("open");
    std::fs::remove_file(&session_path).expect("remove");
    let error = export_session_to_html(&manager, None, None).expect_err("missing session file");
    assert_eq!(
        error.to_string(),
        "Nothing to export yet - start a conversation first"
    );
}

#[test]
fn unknown_theme_propagates_theme_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("bad-theme.html");
    let error = export(GOLDEN_FIXTURE, &output, Some("no-such-theme")).expect_err("bad theme");
    assert_eq!(error.to_string(), "Theme not found: no-such-theme");
    assert!(!output.exists(), "failed export must not write a file");
}

#[test]
fn output_path_is_normalized_but_not_resolved() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `normalizePath` 不解析 `..`：返回值与传入字符串相同。
    let raw = format!("{}/sub/../out.html", dir.path().display());
    std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
    let path = export(GOLDEN_FIXTURE, Path::new(&raw), Some("dark")).expect("export");
    assert_eq!(path, raw);
    assert!(Path::new(&path).exists());
}

#[test]
fn missing_output_directory_is_not_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("missing-dir").join("out.html");
    let error = export(GOLDEN_FIXTURE, &output, Some("dark")).expect_err("write failure");
    // TS 的 `writeFileSync` 抛 Node 文案（errno + syscall + 引号路径），CLI 原样打印。
    assert!(matches!(error, ExportError::Io(_)));
    assert_eq!(
        error.to_string(),
        format!(
            "ENOENT: no such file or directory, open '{}'",
            output.display()
        )
    );
    assert!(!output.exists());
}

#[test]
fn writing_to_a_directory_reports_node_eisdir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = export(GOLDEN_FIXTURE, dir.path(), Some("dark")).expect_err("write failure");
    assert!(matches!(error, ExportError::Io(_)));
    assert_eq!(
        error.to_string(),
        format!(
            "EISDIR: illegal operation on a directory, open '{}'",
            dir.path().display()
        )
    );
}

#[test]
fn default_output_name_uses_session_basename_in_cwd() {
    let _guard = CWD_LOCK.lock().expect("cwd lock");
    let dir = tempfile::tempdir().expect("tempdir");
    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("chdir");
    // 显式主题，避免与本二进制里其他测试共享的全局 `currentThemeName` 竞争。
    let result = export_from_file(
        GOLDEN_FIXTURE,
        Some(ExportOptions {
            output_path: None,
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    );
    let restore = std::env::set_current_dir(&previous);
    restore.expect("restore cwd");

    let path = result.expect("export");
    assert_eq!(path, "kiana-session-golden-session.html");
    let html = std::fs::read_to_string(dir.path().join(&path)).expect("default output exists");
    assert_eq!(sha256_hex(html.as_bytes()), EXPECTED_SHA.trim());
}

#[test]
fn agent_state_adds_system_prompt_and_tools_in_key_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manager = SessionManager::open(GOLDEN_FIXTURE, None, None).expect("open");
    let state = ExportSessionState {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        tools: Some(vec![ExportToolInfo {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }]),
    };
    let output = dir.path().join("state.html");
    let path = export_session_to_html(
        &manager,
        Some(&state),
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect("export");
    let html = std::fs::read_to_string(path).expect("read html");
    let payload = payload_of(&html);
    let leaf = payload.find("\"leafId\":\"e2\"").expect("leafId");
    let system_prompt = payload.find("\"systemPrompt\":").expect("systemPrompt");
    let tools = payload.find("\"tools\":").expect("tools");
    assert!(leaf < system_prompt && system_prompt < tools);
    assert!(payload.contains("\"systemPrompt\":\"You are a helpful assistant.\""));
    assert!(payload.contains(
        "\"tools\":[{\"name\":\"read\",\"description\":\"Read a file\",\"parameters\":{\"type\":\"object\",\"properties\":{}}}]"
    ));
    assert!(!payload.contains("\"renderedTools\""));
}
