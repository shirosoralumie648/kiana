//! Reviewer 回归测试：export-html 与 TS 参考实现的逐字差异。
//!
//! 每个 fixture 的期望载荷都由 Node 26 运行 `/home/ctyun/kiana-work/upstream-reference` 的
//! `core/export-html/index.ts` 生成（`tests/golden/*.payload.json`）。覆盖：
//!
//! 1. `leafId` 保存最后一条条目的原始 `id`（非字符串也按原类型写出；缺省则省略键）；
//! 2. `preRenderCustomTools` 遍历原始 JSON 条目（未知内容块/缺省字段不跳过），
//!    并把 `renderedTools` 的普通对象原型语义（`__proto__` 赋值、原型链读取、
//!    字符串展开）一并复刻；
//! 3. 读侧 fs 错误按 Node 文案（目录 `EISDIR … , read`、不可读 `EACCES … , open '<path>'`）；
//! 4. 写侧 fs 错误区分 `open` / `write`（`/dev/full` → `ENOSPC … , write`）；
//! 5. 会话行解析走 `JSON.parse` 语义（`1e400` → `null`；孤立代理项 → U+FFFD，
//!    ARCHITECTURE §8 决定 31/32）；
//! 6. 深嵌套条目不经过递归转换（`serde_json::to_value` 在约 7,000 层就栈溢出；
//!    TS 的 `JSON.stringify` 到 30 万层仍成功）；
//! 7. `preRenderCustomTools` 对畸形条目的无防护解引用（TS 抛 `TypeError` 并中止导出、
//!    不写文件）。

use std::path::Path;

use base64::Engine as _;
use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_export::index::{
    ExportOptions, SessionData, export_from_file, export_session_to_html, generate_html,
};
use kiana_export::tool_renderer::{RenderedToolResult, ToolHtmlRenderer};
use kiana_session_core::session_manager::SessionManager;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> String {
    format!("{FIXTURES}/{name}")
}

fn decode_payload(html: &str) -> String {
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

fn export_payload(input: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.html");
    let path = export_from_file(
        input,
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect("export");
    decode_payload(&std::fs::read_to_string(path).expect("read html"))
}

// ---------------------------------------------------------------------------
// 1. leafId 原始 id
// ---------------------------------------------------------------------------

#[test]
fn leaf_id_keeps_raw_non_string_ids_like_ts() {
    let cases: [(&str, &str); 6] = [
        (
            "leaf-id-42.jsonl",
            include_str!("golden/leaf-id-42.payload.json"),
        ),
        (
            "leaf-id-false.jsonl",
            include_str!("golden/leaf-id-false.payload.json"),
        ),
        (
            "leaf-id-object.jsonl",
            include_str!("golden/leaf-id-object.payload.json"),
        ),
        (
            "leaf-id-array.jsonl",
            include_str!("golden/leaf-id-array.payload.json"),
        ),
        (
            "leaf-id-null.jsonl",
            include_str!("golden/leaf-id-null.payload.json"),
        ),
        (
            "leaf-id-missing.jsonl",
            include_str!("golden/leaf-id-missing.payload.json"),
        ),
    ];
    for (name, expected) in cases {
        let payload = export_payload(&fixture(name));
        assert_eq!(payload, expected, "fixture {name}");
    }
}

// ---------------------------------------------------------------------------
// 2. preRenderCustomTools over raw entries (+ renderedTools 原型语义)
// ---------------------------------------------------------------------------

/// 与 TS golden 生成脚本里完全一样的假渲染器：输出只依赖 id / name / isError。
struct FakeRenderer {
    calls: Vec<RecordedCall>,
}

#[derive(Debug, Clone, PartialEq)]
enum RecordedCall {
    Call {
        id: String,
        name: String,
        args: JsonValue,
    },
    Result {
        id: String,
        name: String,
        content: Vec<UserContentBlock>,
        details: JsonValue,
        is_error: bool,
    },
}

impl ToolHtmlRenderer for FakeRenderer {
    fn render_call(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        args: &JsonValue,
    ) -> Option<String> {
        self.calls.push(RecordedCall::Call {
            id: tool_call_id.to_string(),
            name: tool_name.to_string(),
            args: args.clone(),
        });
        Some(format!("call|{tool_call_id}|{tool_name}"))
    }

    fn render_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        result: &[UserContentBlock],
        details: &JsonValue,
        is_error: bool,
    ) -> Option<RenderedToolResult> {
        self.calls.push(RecordedCall::Result {
            id: tool_call_id.to_string(),
            name: tool_name.to_string(),
            content: result.to_vec(),
            details: details.clone(),
            is_error,
        });
        Some(RenderedToolResult {
            collapsed: Some(format!("rc|{tool_call_id}|{tool_name}")),
            expanded: format!("re|{tool_call_id}|{tool_name}|{is_error}"),
        })
    }
}

fn export_prerender(fixture_file: &str) -> (String, FakeRenderer) {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("out.html");
    let manager = SessionManager::open(&fixture(fixture_file), None, None).expect("open");
    let mut renderer = FakeRenderer { calls: Vec::new() };
    let path = export_session_to_html(
        &manager,
        None,
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: Some(&mut renderer),
        }),
    )
    .expect("export");
    let html = std::fs::read_to_string(path).expect("read html");
    (decode_payload(&html), renderer)
}

#[test]
fn prerender_custom_tools_payload_matches_ts() {
    let (payload, _renderer) = export_prerender("prerender-session.jsonl");
    assert_eq!(
        payload,
        include_str!("golden/prerender.payload.json"),
        "renderedTools 必须与 TS 逐字节相同"
    );
    // `__proto__` 赋值后的字符串展开：整数索引键排在命名键之前。
    assert!(payload.contains("\"callHtml\":{\"0\":\"c\",\"1\":\"a\","));
}

#[test]
fn prerender_custom_tools_passes_raw_inputs() {
    let (payload, renderer) = export_prerender("prerender-session.jsonl");

    // 未知 assistant 内容块不影响 toolCall（TS 遍历原始 JSON）。
    assert!(payload.contains("\"call1\":{\"callHtml\":\"call|call1|custom\""));
    // 缺省 `arguments` → 渲染器收到 null（Rust 接口没有 undefined，见报告）。
    let call2 = renderer
        .calls
        .iter()
        .find(|call| matches!(call, RecordedCall::Call { id, .. } if id == "call2"))
        .expect("call2 rendered");
    assert_eq!(
        call2,
        &RecordedCall::Call {
            id: "call2".to_string(),
            name: "custom".to_string(),
            args: JsonValue::Null,
        }
    );
    // 未知 toolResult 内容块仍会调用 renderResult；`isError` 为 true 时透传。
    let call9 = renderer
        .calls
        .iter()
        .find(|call| matches!(call, RecordedCall::Result { id, .. } if id == "call9"))
        .expect("call9 result rendered");
    match call9 {
        RecordedCall::Result {
            name,
            details,
            is_error,
            ..
        } => {
            assert_eq!(name, "custom");
            assert_eq!(details, &JsonValue::Null);
            assert!(is_error);
        }
        _ => unreachable!(),
    }
    // 缺省 `toolName` → 空串；缺省 `isError` → false。
    let call2_result = renderer
        .calls
        .iter()
        .find(|call| matches!(call, RecordedCall::Result { id, .. } if id == "call2"))
        .expect("call2 result rendered");
    match call2_result {
        RecordedCall::Result { name, is_error, .. } => {
            assert_eq!(name, "");
            assert!(!is_error);
        }
        _ => unreachable!(),
    }
    // 模板内联渲染的工具（bash）在没有已渲染 call 时跳过。
    assert!(!renderer.calls.iter().any(|call| matches!(
        call,
        RecordedCall::Result { id, .. } if id == "bash"
    )));
    // `Object.prototype` 成员名作为 toolCallId 时算作已有条目 → 渲染并建自有键。
    assert!(payload.contains("\"toString\":{\"resultHtmlCollapsed\":\"rc|toString|bash\""));
}

// ---------------------------------------------------------------------------
// 3/4. fs 错误文案
// ---------------------------------------------------------------------------

#[test]
fn read_directory_matches_node_eisdir() {
    let input_dir = tempfile::tempdir().expect("tempdir");
    let output_dir = tempfile::tempdir().expect("tempdir");
    let error = export_from_file(
        input_dir.path().to_str().expect("utf8"),
        Some(ExportOptions {
            output_path: Some(
                output_dir
                    .path()
                    .join("out.html")
                    .to_string_lossy()
                    .into_owned(),
            ),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect_err("directory must fail");
    assert_eq!(
        error.to_string(),
        "EISDIR: illegal operation on a directory, read"
    );
}

#[cfg(unix)]
#[test]
fn read_unreadable_file_matches_node_eacces() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let input = dir.path().join("session.jsonl");
    std::fs::write(
        &input,
        "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}\n",
    )
    .expect("write fixture");
    let mut permissions = std::fs::metadata(&input).expect("metadata").permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(&input, permissions).expect("chmod");

    // root 不受权限位限制：无法复现时跳过（不影响断言）。
    let unreadable = std::fs::File::open(&input).is_err();
    let result = if unreadable {
        export_from_file(
            input.to_str().expect("utf8"),
            Some(ExportOptions {
                output_path: Some(dir.path().join("out.html").to_string_lossy().into_owned()),
                theme_name: Some("dark".to_string()),
                tool_renderer: None,
            }),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    } else {
        Ok(())
    };

    let mut permissions = std::fs::metadata(&input).expect("metadata").permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(&input, permissions).expect("restore chmod");

    if let Err(message) = result {
        assert_eq!(
            message,
            format!("EACCES: permission denied, open '{}'", input.display())
        );
    }
}

#[test]
fn write_missing_directory_matches_node_enoent_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("missing").join("out.html");
    let error = export_from_file(
        &fixture("golden-session.jsonl"),
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect_err("missing output dir must fail");
    assert_eq!(
        error.to_string(),
        format!(
            "ENOENT: no such file or directory, open '{}'",
            output.display()
        )
    );
}

#[test]
fn write_directory_matches_node_eisdir_open() {
    let output_dir = tempfile::tempdir().expect("tempdir");
    let error = export_from_file(
        &fixture("golden-session.jsonl"),
        Some(ExportOptions {
            output_path: Some(output_dir.path().to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect_err("directory output must fail");
    assert_eq!(
        error.to_string(),
        format!(
            "EISDIR: illegal operation on a directory, open '{}'",
            output_dir.path().display()
        )
    );
}

#[cfg(target_os = "linux")]
#[test]
fn write_full_device_matches_node_enospc_write() {
    if !Path::new("/dev/full").exists() {
        return;
    }
    let error = export_from_file(
        &fixture("golden-session.jsonl"),
        Some(ExportOptions {
            output_path: Some("/dev/full".to_string()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect_err("writing to /dev/full must fail");
    assert_eq!(error.to_string(), "ENOSPC: no space left on device, write");
}

// ---------------------------------------------------------------------------
// 5. JSON.parse 语义
// ---------------------------------------------------------------------------

#[test]
fn out_of_range_number_parses_as_null_like_json_parse() {
    let payload = export_payload(&fixture("big-number-session.jsonl"));
    assert_eq!(payload, include_str!("golden/big-number.payload.json"));
    assert!(payload.contains("\"n\":null"));
}

#[test]
fn lone_surrogate_escape_keeps_the_entry() {
    let payload = export_payload(&fixture("surrogate-session.jsonl"));
    // 三条消息都保留（JSON.parse 接受孤立代理项；§8 决定 31：U+FFFD 替换）。
    assert!(payload.contains("\"id\":\"e1\""));
    assert!(payload.contains("\"id\":\"e2\""));
    assert!(payload.contains("\"id\":\"e3\""));
    assert!(payload.contains("\"leafId\":\"e3\""));
    assert!(payload.contains("\"content\":\"x\u{fffd}y\""));
    assert!(!payload.contains("\\ud800"));
}

// ---------------------------------------------------------------------------
// 6. 深嵌套条目：载荷序列化必须是迭代实现
// ---------------------------------------------------------------------------

/// 9,000 层嵌套（TS 侧到 30 万层仍成功）。
///
/// `generate_html` 曾用 `serde_json::to_value(&SessionData)` 序列化载荷：它会递归
/// 遍历每条条目，debug 构建在约 7,000 层（8 MB 栈）就 `SIGABRT`。这里在同样 8 MB
/// 栈的线程里（复刻进程主线程）构造 9,000 层 `toolCall.arguments`，断言载荷逐层
/// 完整写出。`SessionData` 的 `Drop` 仍是递归的（§8 决定 33 的残留限制，约 10 万层
/// 起），因此测试结束前 `mem::forget` 掉这棵树。
#[test]
fn generate_html_serializes_deeply_nested_entries_without_recursion() {
    const DEPTH: usize = 9_000;
    let html = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut arguments = JsonValue::Null;
            for _ in 0..DEPTH {
                arguments = JsonValue::Array(vec![arguments]);
            }
            let mut block = serde_json::Map::new();
            block.insert("type".to_string(), JsonValue::String("toolCall".into()));
            block.insert("id".to_string(), JsonValue::String("c1".into()));
            block.insert("name".to_string(), JsonValue::String("custom".into()));
            block.insert("arguments".to_string(), arguments);
            let mut message = serde_json::Map::new();
            message.insert("role".to_string(), JsonValue::String("assistant".into()));
            message.insert(
                "content".to_string(),
                JsonValue::Array(vec![JsonValue::Object(block)]),
            );
            let mut entry = serde_json::Map::new();
            entry.insert("type".to_string(), JsonValue::String("message".into()));
            entry.insert("id".to_string(), JsonValue::String("e1".into()));
            entry.insert("message".to_string(), JsonValue::Object(message));

            let data = SessionData {
                header: JsonValue::Null,
                entries: vec![JsonValue::Object(entry)],
                leaf_id: None,
                system_prompt: None,
                tools: None,
                rendered_tools: None,
            };
            let html = generate_html(&data, Some("dark")).expect("generate html");
            // 递归 Drop 超出本测试的栈预算（§8 决定 33 残留）。
            std::mem::forget(data);
            html
        })
        .expect("spawn deep export thread")
        .join()
        .expect("deep export thread must not abort");

    let payload = decode_payload(&html);
    assert!(payload.starts_with("{\"header\":null,\"entries\":[{\"type\":\"message\""));
    assert!(
        payload.contains(&format!(
            "\"arguments\":{}null{}",
            "[".repeat(DEPTH),
            "]".repeat(DEPTH)
        )),
        "deep arguments must be serialized layer by layer"
    );
}

// ---------------------------------------------------------------------------
// 7. preRenderCustomTools 对畸形条目的 TypeError
// ---------------------------------------------------------------------------

const SESSION_HEADER: &str = "{\"type\":\"session\",\"version\":3,\"id\":\"h\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}";

fn write_session(dir: &Path, name: &str, lines: &[&str]) -> String {
    let path = dir.join(name);
    let mut text = String::from(SESSION_HEADER);
    text.push('\n');
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(&path, text).expect("write session");
    path.to_string_lossy().into_owned()
}

/// 与 TS golden 生成脚本同一行文的畸形条目：`message` 缺失/为 `null`、`null` 块。
///
/// 返回 `TempDir` 以保活输入/输出路径（断言输出文件是否存在）。
fn export_malformed_with_renderer(
    name: &str,
    lines: &[&str],
) -> (Result<String, String>, FakeRenderer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_session(dir.path(), name, lines);
    let output = dir.path().join("out.html");
    let manager = SessionManager::open(&input, None, None).expect("open");
    let mut renderer = FakeRenderer { calls: Vec::new() };
    let result = export_session_to_html(
        &manager,
        None,
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: Some(&mut renderer),
        }),
    )
    .map_err(|error| error.to_string());
    (result, renderer, dir)
}

#[test]
fn prerender_throws_like_ts_when_message_is_missing_or_null() {
    let call_entry = "{\"type\":\"message\",\"id\":\"e1\",\"parentId\":null,\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"custom\",\"arguments\":{}}]}}";
    let cases = [
        (
            "nomsg.jsonl",
            "{\"type\":\"message\",\"id\":\"e2\",\"parentId\":\"e1\",\"timestamp\":\"1970-01-01T00:00:00.000Z\"}",
            "Cannot read properties of undefined (reading 'role')",
        ),
        (
            "nullmsg.jsonl",
            "{\"type\":\"message\",\"id\":\"e2\",\"parentId\":\"e1\",\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":null}",
            "Cannot read properties of null (reading 'role')",
        ),
    ];
    for (name, malformed, expected) in cases {
        // TS 先处理 e1（渲染 call），随后在畸形条目上抛 TypeError。
        let (result, renderer, output) =
            export_malformed_with_renderer(name, &[call_entry, malformed]);
        assert_eq!(result.expect_err("must throw"), expected, "fixture {name}");
        assert!(
            !output.path().join("out.html").exists(),
            "TS 在写文件前中止：{name}"
        );
        assert_eq!(renderer.calls.len(), 1, "e1 的 call 已渲染：{name}");
    }
}

#[test]
fn prerender_throws_like_ts_on_null_content_block() {
    let content =
        "[{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"custom\",\"arguments\":{}},null]";
    let malformed = format!(
        "{{\"type\":\"message\",\"id\":\"e1\",\"parentId\":null,\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":{content}}}}}"
    );
    let (result, renderer, output) =
        export_malformed_with_renderer("nullblock.jsonl", &[&malformed]);
    assert_eq!(
        result.expect_err("must throw"),
        "Cannot read properties of null (reading 'type')"
    );
    assert!(!output.path().join("out.html").exists());
    // null 块之前的 toolCall 已经进入渲染器（TS 的遍历顺序）。
    assert_eq!(renderer.calls.len(), 1);
}

#[test]
fn prerender_skips_non_null_primitives_like_ts() {
    // 原始值/数组的属性访问只得到 `undefined`，TS 不抛，只是不匹配分支。
    let lines = [
        "{\"type\":\"message\",\"id\":\"e1\",\"parentId\":null,\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":\"hello\"}",
        "{\"type\":\"message\",\"id\":\"e2\",\"parentId\":\"e1\",\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":42}",
        "{\"type\":\"message\",\"id\":\"e3\",\"parentId\":\"e2\",\"timestamp\":\"1970-01-01T00:00:00.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[5,\"x\",true,[],{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"custom\",\"arguments\":{}}]}}",
    ];
    let (result, renderer, output) = export_malformed_with_renderer("primitives.jsonl", &lines);
    let path = result.expect("primitive fields must not throw");
    assert_eq!(
        path,
        output.path().join("out.html").to_string_lossy(),
        "export written"
    );
    assert_eq!(renderer.calls.len(), 1, "只有真正的 toolCall 被渲染");
}

#[test]
fn malformed_messages_without_renderer_are_ignored_like_ts() {
    // 不提供 toolRenderer 时 preRenderCustomTools 不运行：同样的畸形条目正常导出。
    let dir = tempfile::tempdir().expect("tempdir");
    let input = write_session(
        dir.path(),
        "nomsg.jsonl",
        &[
            "{\"type\":\"message\",\"id\":\"e1\",\"parentId\":null,\"timestamp\":\"1970-01-01T00:00:00.000Z\"}",
        ],
    );
    let output = dir.path().join("out.html");
    let manager = SessionManager::open(&input, None, None).expect("open");
    let path = export_session_to_html(
        &manager,
        None,
        Some(ExportOptions {
            output_path: Some(output.to_string_lossy().into_owned()),
            theme_name: Some("dark".to_string()),
            tool_renderer: None,
        }),
    )
    .expect("no renderer -> no throw");
    assert!(std::path::Path::new(&path).exists());
}
