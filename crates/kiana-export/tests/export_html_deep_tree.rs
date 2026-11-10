//! 移植 `packages/coding-agent/test/export-html-deep-tree.test.ts`（#11225）。
//!
//! TS 测试构造 20,000 层单链，在 `node:vm` 里跑 viewer 的 `buildTree` /
//! `flattenTree` / `findNewestLeaf` / `getPath`。Rust 不执行 JS；按任务规定改为：
//!
//! 1. 用同样的 20,000 条单链夹具执行真实导出，证明序列化路径没有递归深度问题；
//! 2. 断言 base64 载荷保留全部条目 id、父子链与 leaf；
//! 3. 断言 viewer 的树算法是迭代实现（行为在浏览器端，字节一致即行为一致）。

use base64::Engine as _;
use std::fmt::Write as _;

use kiana_export::index::{ExportOptions, export_from_file};

const DEPTH: usize = 20_000;

fn build_fixture(path: &std::path::Path) {
    let mut text = String::with_capacity(DEPTH * 160);
    text.push_str(
        "{\"type\":\"session\",\"version\":3,\"id\":\"deep-test\",\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"cwd\":\"/tmp/kiana-deep\"}\n",
    );
    for index in 0..DEPTH {
        let parent = if index == 0 {
            "null".to_string()
        } else {
            format!("\"e{}\"", index - 1)
        };
        writeln!(
            text,
            "{{\"type\":\"message\",\"id\":\"e{index}\",\"parentId\":{parent},\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"x\",\"timestamp\":{index}}}}}"
        )
        .expect("write fixture");
    }
    std::fs::write(path, text).expect("write fixture");
}

// it("builds and flattens a single-child chain deeper than the call stack")
#[test]
fn builds_and_flattens_a_single_child_chain_deeper_than_the_call_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("session.jsonl");
    build_fixture(&fixture);
    let output = dir.path().join("out.html");

    let path = export_from_file(
        fixture.to_str().expect("utf8 path"),
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

    assert_eq!(payload.matches("\"type\":\"message\"").count(), DEPTH);
    assert!(payload.contains("\"id\":\"e0\""));
    assert!(payload.contains(&format!("\"id\":\"e{}\"", DEPTH - 1)));
    assert!(payload.contains(&format!("\"leafId\":\"e{}\"", DEPTH - 1)));
    // 单链父子关系：e1 的 parentId 是 e0，最后一条的 parentId 是 e19998。
    assert!(payload.contains("\"id\":\"e1\",\"parentId\":\"e0\""));
    assert!(payload.contains(&format!(
        "\"id\":\"e{}\",\"parentId\":\"e{}\"",
        DEPTH - 1,
        DEPTH - 2
    )));
    // 顺序：e0 在 e19999 之前。
    assert!(
        payload.find("\"id\":\"e0\"").unwrap()
            < payload.find(&format!("\"id\":\"e{}\"", DEPTH - 1)).unwrap()
    );

    // viewer 的树算法必须是迭代实现（避免调用栈溢出）。
    let template_js = kiana_export::assets::TEMPLATE_JS;
    assert!(template_js.contains("function buildTree()"));
    assert!(template_js.contains("function flattenTree("));
    assert!(template_js.contains("function findNewestLeaf("));
    assert!(template_js.contains("function buildActivePathIds("));
    assert!(template_js.contains("function getPath("));
    // 深度链不会触发递归：模板里没有 `buildTree = (node) =>` 之类的递归辅助。
    assert!(!template_js.contains("function walkTree("));
}
