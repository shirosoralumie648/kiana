//! 移植 `packages/coding-agent/test/export-html-cost.test.ts`（#9980）。
//!
//! TS 测试从 `template.js` 截取 `computeStats` 到 `globalStats` 之间的源码，在
//! `node:vm` 里求值。Rust 不执行 JS；按任务规定改为：
//!
//! 1. 断言 `template.js` 仍包含同一对边界（函数完整、顺序正确）；
//! 2. 断言导出的 base64 载荷里 `usage.cost` 的 totals 与分量逐字幸存（JS 数字格式：
//!    `0` 而不是 `0.0`），并与 TS 导出逐字节相同。

use base64::Engine as _;
use kiana_export::index::{ExportOptions, export_from_file};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/cost-session.jsonl"
);
const EXPECTED_PAYLOAD: &str = include_str!("golden/cost.payload.json");

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

// it("uses recorded totals while falling back to component sums for old sessions")
#[test]
fn uses_recorded_totals_while_falling_back_to_component_sums_for_old_sessions() {
    let template_js = kiana_export::assets::TEMPLATE_JS;
    let function_start = template_js
        .find("function computeStats(entryList)")
        .expect("computeStats start");
    let function_end = template_js
        .find("const globalStats = computeStats(entries);")
        .expect("computeStats end");
    assert!(function_end > function_start);

    // 记录值（total 存在时优先）与回退值（分量求和）的 JS 逻辑存在。
    assert!(template_js.contains("typeof msg.usage.cost.total === 'number'"));
    assert!(template_js.contains("cost.input +="));

    let payload = export_payload();
    assert_eq!(
        payload, EXPECTED_PAYLOAD,
        "payload must match TS byte for byte"
    );

    // 第一条：total = 0.9（provider 汇报值），分量 0.1 + 0.2 + 0 + 0。
    assert!(payload.contains(
        "\"cost\":{\"input\":0.1,\"output\":0.2,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0.9}"
    ));
    // 第二条：total = 0.5。
    assert!(payload.contains(
        "\"cost\":{\"input\":0.4,\"output\":0.1,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0.5}"
    ));
    // JS 数字格式：整数 0 不得写成 0.0（时间戳里的 ".000" 不在此列）。
    assert!(!payload.contains("\"cacheRead\":0.0"));
    assert!(!payload.contains("\"cacheWrite\":0.0"));
    assert!(!payload.contains("\"input\":0.0"));
    assert!(!payload.contains("\"totalTokens\":120.0"));
    // 两条 assistant 的 usage 都按文件顺序保留。
    let first = payload.find("\"id\":\"a1\"").unwrap();
    let second = payload.find("\"id\":\"a2\"").unwrap();
    assert!(first < second);
}
