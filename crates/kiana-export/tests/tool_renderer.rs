//! `createToolHtmlRenderer` 的行为测试（TS 源没有独立测试文件；覆盖 §8.4 的
//! 宽度 100、`isPartial:false`、`showImages:false`、`outputPad:1`、首尾空行裁剪与
//! panic → `None`）。
//!
//! TS 的 `renderCall` / `renderResult` 用 `try/catch` 吞掉一切异常；Rust 用
//! `catch_unwind` 在渲染器边界复刻。

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kiana_ai::types::JsonValue;
use kiana_export::tool_renderer::{
    DefaultToolHtmlRenderer, ToolHtmlRenderer, ToolHtmlRendererDeps, create_tool_html_renderer,
};
use kiana_ext_api::AgentToolResult;
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_tui::tui::Component;

fn dark_theme() -> Arc<Theme> {
    kiana_theme::get_theme_by_name("dark").expect("dark theme")
}

/// 按脚本返回固定行的组件，并记录每次 `render` 的宽度。
struct ScriptedComponent {
    script: Arc<Mutex<VecDeque<Vec<String>>>>,
    widths: Arc<Mutex<Vec<usize>>>,
}

impl Component for ScriptedComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.widths.lock().expect("widths").push(width);
        self.script
            .lock()
            .expect("script")
            .pop_front()
            .unwrap_or_default()
    }
}

struct PanickingComponent;

impl Component for PanickingComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        panic!("component render exploded");
    }
}

fn scripted_renderer(scripts: Vec<Vec<&str>>, widths: Arc<Mutex<Vec<usize>>>) -> RenderCallFn {
    let script: VecDeque<Vec<String>> = scripts
        .into_iter()
        .map(|lines| lines.into_iter().map(str::to_string).collect())
        .collect();
    let script = Arc::new(Mutex::new(script));
    Arc::new(
        move |_args: &JsonValue, _theme: &Theme, _context: &ToolRenderContext| {
            Box::new(ScriptedComponent {
                script: script.clone(),
                widths: widths.clone(),
            }) as Box<dyn Component>
        },
    )
}

fn renderer_with(
    call: Option<RenderCallFn>,
    result: Option<RenderResultFn>,
) -> DefaultToolHtmlRenderer {
    create_tool_html_renderer(ToolHtmlRendererDeps {
        get_tool_renderers: Arc::new(move |name: &str| {
            if name != "custom" {
                return None;
            }
            Some(ToolRenderers {
                render_shell: None,
                render_call: call.clone(),
                render_result: result.clone(),
            })
        }),
        theme: dark_theme(),
        cwd: "/tmp".to_string(),
        width: None,
    })
}

fn silence_panics<T>(body: impl FnOnce() -> T) -> T {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(body));
    panic::set_hook(previous);
    match outcome {
        Ok(value) => value,
        Err(payload) => panic::resume_unwind(payload),
    }
}

#[test]
fn renders_calls_with_width_100_and_ansi_html() {
    let widths = Arc::new(Mutex::new(Vec::new()));
    let mut renderer = renderer_with(
        Some(scripted_renderer(vec![vec!["one", "two"]], widths.clone())),
        None,
    );

    let html = renderer.render_call("id", "custom", &serde_json::json!({"a": 1}));
    assert_eq!(
        html,
        Some("<div class=\"ansi-line\">one</div><div class=\"ansi-line\">two</div>".to_string())
    );
    assert_eq!(*widths.lock().unwrap(), vec![100]);
}

#[test]
fn render_width_defaults_to_100_and_honors_explicit_width() {
    let widths = Arc::new(Mutex::new(Vec::new()));
    let renderer = create_tool_html_renderer(ToolHtmlRendererDeps {
        get_tool_renderers: Arc::new({
            let scripts = scripted_renderer(vec![vec!["x"], vec!["y"]], widths.clone());
            move |_name: &str| {
                Some(ToolRenderers {
                    render_shell: None,
                    render_call: Some(scripts.clone()),
                    render_result: None,
                })
            }
        }),
        theme: dark_theme(),
        cwd: "/tmp".to_string(),
        width: Some(42),
    });
    let mut renderer = renderer;
    assert!(
        renderer
            .render_call("id", "custom", &JsonValue::Null)
            .is_some()
    );
    assert_eq!(*widths.lock().unwrap(), vec![42]);
}

#[test]
fn missing_renderer_returns_none() {
    let mut renderer = renderer_with(None, None);
    assert_eq!(renderer.render_call("id", "custom", &JsonValue::Null), None);
    assert_eq!(
        renderer.render_result("id", "custom", &[], &JsonValue::Null, false),
        None
    );
    // 未知工具名。
    assert_eq!(renderer.render_call("id", "other", &JsonValue::Null), None);
    assert_eq!(
        renderer.render_result("id", "other", &[], &JsonValue::Null, false),
        None
    );
}

#[test]
fn trims_blank_lines_at_both_ends_only() {
    let widths = Arc::new(Mutex::new(Vec::new()));
    let result_lines = ["", "\u{1b}[31mone\u{1b}[0m", "two", ""];
    let render: RenderResultFn = Arc::new({
        let script: Arc<Mutex<VecDeque<Vec<String>>>> = Arc::new(Mutex::new(VecDeque::from(vec![
            result_lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            result_lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
        ])));
        let widths = widths.clone();
        move |_result: &AgentToolResult<JsonValue>,
              _options: &ToolRenderResultOptions,
              _theme: &Theme,
              _context: &ToolRenderContext| {
            Box::new(ScriptedComponent {
                script: script.clone(),
                widths: widths.clone(),
            }) as Box<dyn Component>
        }
    });
    let mut renderer = renderer_with(
        Some(scripted_renderer(vec![], Arc::new(Mutex::new(Vec::new())))),
        Some(render),
    );

    let rendered = renderer
        .render_result("id", "custom", &[], &JsonValue::Null, false)
        .expect("rendered");
    // 折叠与展开内容相同 → 只有 expanded。
    assert_eq!(rendered.collapsed, None);
    assert_eq!(
        rendered.expanded,
        "<div class=\"ansi-line\"><span style=\"color:#800000\">one</span></div><div class=\"ansi-line\">two</div>"
    );
    assert_eq!(*widths.lock().unwrap(), vec![100, 100]);
}

#[test]
fn blank_line_trimming_uses_js_trim_whitespace_set() {
    // JS `String.prototype.trim()` 不把 U+0085 NEL 当空白，但把 U+FEFF 当空白；
    // Rust 的 `str::trim()` 恰好相反。首尾判定必须按 JS 集合。
    let result_lines = ["\u{0085}", "one", "\u{feff}"];
    let render: RenderResultFn = Arc::new({
        let script: Arc<Mutex<VecDeque<Vec<String>>>> = Arc::new(Mutex::new(VecDeque::from(vec![
            result_lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            result_lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
        ])));
        move |_result: &AgentToolResult<JsonValue>,
              _options: &ToolRenderResultOptions,
              _theme: &Theme,
              _context: &ToolRenderContext| {
            Box::new(ScriptedComponent {
                script: script.clone(),
                widths: Arc::new(Mutex::new(Vec::new())),
            }) as Box<dyn Component>
        }
    });
    let mut renderer = renderer_with(None, Some(render));
    let rendered = renderer
        .render_result("id", "custom", &[], &JsonValue::Null, false)
        .expect("rendered");
    // U+0085 行保留（JS 非空白），U+FEFF 行被裁掉（JS 空白）。
    assert_eq!(
        rendered.expanded,
        "<div class=\"ansi-line\">\u{0085}</div><div class=\"ansi-line\">one</div>"
    );
}

#[test]
fn returns_collapsed_only_when_non_empty_and_different() {
    let widths = Arc::new(Mutex::new(Vec::new()));
    let render: RenderResultFn = Arc::new({
        let script: Arc<Mutex<VecDeque<Vec<String>>>> = Arc::new(Mutex::new(VecDeque::from(vec![
            vec!["collapsed".to_string()],
            vec!["expanded".to_string()],
        ])));
        let widths = widths.clone();
        move |_result: &AgentToolResult<JsonValue>,
              _options: &ToolRenderResultOptions,
              _theme: &Theme,
              _context: &ToolRenderContext| {
            Box::new(ScriptedComponent {
                script: script.clone(),
                widths: widths.clone(),
            }) as Box<dyn Component>
        }
    });
    let mut renderer = renderer_with(None, Some(render));
    let rendered = renderer
        .render_result("id", "custom", &[], &JsonValue::Null, false)
        .expect("rendered");
    assert_eq!(
        rendered.collapsed.as_deref(),
        Some("<div class=\"ansi-line\">collapsed</div>")
    );
    assert_eq!(rendered.expanded, "<div class=\"ansi-line\">expanded</div>");
}

#[test]
fn empty_expanded_yields_empty_string() {
    let render: RenderResultFn = Arc::new(|_result, _options, _theme, _context| {
        Box::new(EmptyComponent) as Box<dyn Component>
    });
    let mut renderer = renderer_with(None, Some(render));
    let rendered = renderer
        .render_result("id", "custom", &[], &JsonValue::Null, false)
        .expect("rendered");
    assert_eq!(rendered.collapsed, None);
    assert_eq!(rendered.expanded, "");
}

struct EmptyComponent;

impl Component for EmptyComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

#[test]
fn render_context_fields_match_ts() {
    #[derive(Debug, Clone)]
    struct Snapshot {
        tool_call_id: String,
        args: JsonValue,
        state_calls: i64,
        expanded: bool,
        is_partial: bool,
        is_error: bool,
        show_images: bool,
        execution_started: bool,
        args_complete: bool,
        duration_ms: Option<u64>,
        output_pad: usize,
        cwd: PathBuf,
    }

    fn snapshot(context: &ToolRenderContext) -> Snapshot {
        let state = context.state.lock().expect("state");
        Snapshot {
            tool_call_id: context.tool_call_id.clone(),
            args: context.args.clone(),
            state_calls: state.get("calls").and_then(JsonValue::as_i64).unwrap_or(0),
            expanded: context.expanded,
            is_partial: context.is_partial,
            is_error: context.is_error,
            show_images: context.show_images,
            execution_started: context.execution_started,
            args_complete: context.args_complete,
            duration_ms: context.duration_ms,
            output_pad: context.output_pad,
            cwd: context.cwd.clone(),
        }
    }

    let call_snapshots: Arc<Mutex<Vec<Snapshot>>> = Arc::new(Mutex::new(Vec::new()));
    let result_snapshots: Arc<Mutex<Vec<Snapshot>>> = Arc::new(Mutex::new(Vec::new()));

    let call: RenderCallFn = Arc::new({
        let snapshots = call_snapshots.clone();
        move |_args: &JsonValue, _theme: &Theme, context: &ToolRenderContext| {
            {
                let mut state = context.state.lock().expect("state");
                let calls = state.get("calls").and_then(JsonValue::as_i64).unwrap_or(0);
                state["calls"] = serde_json::json!(calls + 1);
            }
            snapshots.lock().unwrap().push(snapshot(context));
            Box::new(EmptyComponent) as Box<dyn Component>
        }
    });
    let render_result: RenderResultFn = Arc::new({
        let snapshots = result_snapshots.clone();
        move |_result: &AgentToolResult<JsonValue>,
              options: &ToolRenderResultOptions,
              _theme: &Theme,
              context: &ToolRenderContext| {
            snapshots.lock().unwrap().push(snapshot(context));
            assert!(!options.is_partial);
            Box::new(EmptyComponent) as Box<dyn Component>
        }
    });

    let mut renderer = renderer_with(Some(call), Some(render_result));
    let args = serde_json::json!({"a": 1});
    assert!(renderer.render_call("id", "custom", &args).is_some());
    assert!(
        renderer
            .render_result("id", "custom", &[], &serde_json::json!({"d": true}), true)
            .is_some()
    );

    let calls = call_snapshots.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.tool_call_id, "id");
    assert_eq!(call.args, args);
    assert_eq!(call.state_calls, 1);
    assert!(!call.expanded);
    assert!(call.is_partial);
    assert!(!call.is_error);
    assert!(!call.show_images);
    assert!(call.execution_started);
    assert!(call.args_complete);
    assert_eq!(call.duration_ms, None);
    assert_eq!(call.output_pad, 1);
    assert_eq!(call.cwd, PathBuf::from("/tmp"));

    let results = result_snapshots.lock().unwrap();
    assert_eq!(results.len(), 2, "collapsed + expanded");
    assert_eq!(results[0].tool_call_id, "id");
    assert_eq!(results[0].args, args);
    assert_eq!(results[0].state_calls, 1, "state shared per tool call id");
    assert!(!results[0].expanded);
    assert!(!results[0].is_partial);
    assert!(results[0].is_error);
    assert!(results[1].expanded);
    assert!(!results[1].is_partial);
    assert!(results[1].is_error);
}

#[test]
fn args_are_null_before_render_call() {
    let snapshots: Arc<Mutex<Vec<JsonValue>>> = Arc::new(Mutex::new(Vec::new()));
    let render_result: RenderResultFn = Arc::new({
        let snapshots = snapshots.clone();
        move |_result, _options, _theme, context| {
            snapshots.lock().unwrap().push(context.args.clone());
            Box::new(EmptyComponent) as Box<dyn Component>
        }
    });
    let mut renderer = renderer_with(None, Some(render_result));
    assert!(
        renderer
            .render_result("id", "custom", &[], &JsonValue::Null, false)
            .is_some()
    );
    // render_result 会调用渲染器两次（collapsed + expanded），两次的 args 都是 Null。
    assert_eq!(
        *snapshots.lock().unwrap(),
        vec![JsonValue::Null, JsonValue::Null]
    );
}

#[test]
fn panicking_renderer_returns_none() {
    silence_panics(|| {
        let call: RenderCallFn = Arc::new(|_args, _theme, _context| panic!("call exploded"));
        let render_result: RenderResultFn =
            Arc::new(|_result, _options, _theme, _context| panic!("result exploded"));
        let mut renderer = renderer_with(Some(call), Some(render_result));
        assert_eq!(renderer.render_call("id", "custom", &JsonValue::Null), None);
        assert_eq!(
            renderer.render_result("id", "custom", &[], &JsonValue::Null, false),
            None
        );
    });
}

#[test]
fn panicking_component_render_returns_none() {
    silence_panics(|| {
        let call: RenderCallFn =
            Arc::new(|_args, _theme, _context| Box::new(PanickingComponent) as Box<dyn Component>);
        let render_result: RenderResultFn = Arc::new(|_result, _options, _theme, _context| {
            Box::new(PanickingComponent) as Box<dyn Component>
        });
        let mut renderer = renderer_with(Some(call), Some(render_result));
        assert_eq!(renderer.render_call("id", "custom", &JsonValue::Null), None);
        assert_eq!(
            renderer.render_result("id", "custom", &[], &JsonValue::Null, false),
            None
        );
    });
}

/// 渲染器 panic 后，同一 toolCallId 的共享状态 Mutex 不能被 poison 卡死。
///
/// TS 的 `state` 是普通对象，异常不会留下任何锁定状态；Rust 必须在 catch 之后清除
/// poison，否则后续渲染 `.lock().unwrap()` 会再次 panic。
#[test]
fn panicking_renderer_does_not_poison_shared_state() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let first_call = Arc::new(AtomicBool::new(true));
    let call: RenderCallFn = Arc::new({
        let first_call = first_call.clone();
        move |_args: &JsonValue, _theme: &Theme, context: &ToolRenderContext| {
            // poison 未清除时 `.expect` 会 panic（第二个调用拿不到锁）。
            let mut state = context.state.lock().expect("state must not be poisoned");
            state["rendered"] = JsonValue::Bool(true);
            if first_call.swap(false, Ordering::SeqCst) {
                panic!("first render explodes");
            }
            Box::new(EmptyComponent) as Box<dyn Component>
        }
    });
    let mut renderer = renderer_with(Some(call), None);
    assert_eq!(renderer.render_call("id", "custom", &JsonValue::Null), None);
    assert!(
        renderer
            .render_call("id", "custom", &JsonValue::Null)
            .is_some(),
        "same toolCallId must render again after a panic"
    );
}

/// TS 的 `try/catch` 不产生任何 stderr；`catch_unwind` 默认会经 panic hook 打印。
///
/// 在子进程里只跑这一条测试，断言 stderr 不含 `panicked`。
#[test]
fn panicking_renderer_produces_no_stderr() {
    const CHILD_ENV: &str = "KIANA_EXPORT_PANIC_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let call: RenderCallFn = Arc::new(|_args, _theme, _context| panic!("renderer exploded"));
        let mut renderer = renderer_with(Some(call), None);
        assert_eq!(renderer.render_call("id", "custom", &JsonValue::Null), None);
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("current exe"))
        .args([
            "--exact",
            "panicking_renderer_produces_no_stderr",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn child test");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "child failed: {stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !stderr.contains("panicked"),
        "panic output leaked to stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// index::preRenderCustomTools 与渲染器的集成（TEMPLATE_RENDERED_TOOLS 过滤、
// 合并顺序、renderedTools 的序列化形状）。
// ---------------------------------------------------------------------------

use base64::Engine as _;
use kiana_export::index::{ExportOptions, export_session_to_html};
use kiana_export::tool_renderer::RenderedToolResult;
use kiana_session_core::session_manager::SessionManager;

const TOOL_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/tool-calls-session.jsonl"
);

#[derive(Default)]
struct RecordingRenderer {
    calls: Vec<(String, String)>,
    results: Vec<(String, String)>,
}

impl ToolHtmlRenderer for RecordingRenderer {
    fn render_call(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        args: &JsonValue,
    ) -> Option<String> {
        self.calls
            .push((tool_call_id.to_string(), tool_name.to_string()));
        Some(format!("CALL:{tool_call_id}:{}", args["x"]))
    }

    fn render_result(
        &mut self,
        tool_call_id: &str,
        tool_name: &str,
        _result: &[kiana_ai::types::UserContentBlock],
        _details: &JsonValue,
        is_error: bool,
    ) -> Option<RenderedToolResult> {
        self.results
            .push((tool_call_id.to_string(), tool_name.to_string()));
        assert!(!is_error);
        Some(RenderedToolResult {
            collapsed: Some(format!("C:{tool_call_id}")),
            expanded: format!("E:{tool_call_id}"),
        })
    }
}

fn decode_payload(path: &str) -> String {
    let html = std::fs::read_to_string(path).expect("read html");
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
fn pre_render_skips_template_rendered_tools_and_merges_results() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manager = SessionManager::open(TOOL_FIXTURE, None, None).expect("open");
    let output = dir.path().join("tools.html");
    let mut renderer = RecordingRenderer::default();
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

    // bash 由模板内联渲染：call 与 result 都不调用渲染器。
    assert_eq!(
        renderer.calls,
        vec![("call-custom".to_string(), "custom".to_string())]
    );
    assert_eq!(
        renderer.results,
        vec![("call-custom".to_string(), "custom".to_string())]
    );

    let payload = decode_payload(&path);
    assert!(payload.contains(
        "\"renderedTools\":{\"call-custom\":{\"callHtml\":\"CALL:call-custom:1\",\"resultHtmlCollapsed\":\"C:call-custom\",\"resultHtmlExpanded\":\"E:call-custom\"}}"
    ));
    assert!(!payload.contains("\"call-bash\":{"));
    // 插入序：renderedTools 位于 leafId 之后、作为最后一个键。
    assert!(
        payload.find("\"leafId\":\"t2\"").unwrap() < payload.find("\"renderedTools\"").unwrap()
    );
}

#[test]
fn empty_rendered_tools_key_is_omitted() {
    struct NothingRenderer;
    impl ToolHtmlRenderer for NothingRenderer {
        fn render_call(&mut self, _id: &str, _name: &str, _args: &JsonValue) -> Option<String> {
            None
        }
        fn render_result(
            &mut self,
            _id: &str,
            _name: &str,
            _result: &[kiana_ai::types::UserContentBlock],
            _details: &JsonValue,
            _is_error: bool,
        ) -> Option<RenderedToolResult> {
            None
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let manager = SessionManager::open(TOOL_FIXTURE, None, None).expect("open");
    let output = dir.path().join("nothing.html");
    let mut renderer = NothingRenderer;
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
    let payload = decode_payload(&path);
    assert!(!payload.contains("\"renderedTools\""));
}

#[test]
fn empty_call_html_is_not_recorded() {
    struct EmptyCallRenderer;
    impl ToolHtmlRenderer for EmptyCallRenderer {
        fn render_call(&mut self, _id: &str, _name: &str, _args: &JsonValue) -> Option<String> {
            Some(String::new())
        }
        fn render_result(
            &mut self,
            _id: &str,
            _name: &str,
            _result: &[kiana_ai::types::UserContentBlock],
            _details: &JsonValue,
            _is_error: bool,
        ) -> Option<RenderedToolResult> {
            None
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let manager = SessionManager::open(TOOL_FIXTURE, None, None).expect("open");
    let output = dir.path().join("empty-call.html");
    let mut renderer = EmptyCallRenderer;
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
    let payload = decode_payload(&path);
    assert!(!payload.contains("\"renderedTools\""));
}

/// TS 的 `renderedTools` 是普通对象：读取 `Object.prototype` 的成员得到真值
/// （`existing` 成立），而给 `__proto__` 赋值只改原型、不产生自有可枚举键。
/// 用伪造的 toolCallId 复刻该行为（真实 provider 的 id 不会与这些名字冲突）。
#[test]
fn rendered_tools_follow_js_object_prototype_semantics() {
    fn assistant_entry(
        entry_id: &str,
        parent_id: Option<&str>,
        call_id: &str,
        tool_name: &str,
    ) -> String {
        let parent = parent_id.map_or_else(|| "null".to_string(), |parent| format!("\"{parent}\""));
        format!(
            "{{\"type\":\"message\",\"id\":\"{entry_id}\",\"parentId\":{parent},\"timestamp\":\"1970-01-01T00:00:00.001Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"toolCall\",\"id\":\"{call_id}\",\"name\":\"{tool_name}\",\"arguments\":{{\"x\":1}}}}],\"api\":\"anthropic-messages\",\"provider\":\"anthropic\",\"model\":\"test\",\"usage\":{{\"input\":1,\"output\":2,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":3,\"cost\":{{\"input\":0.001,\"output\":0.002,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0.003}}}},\"stopReason\":\"toolUse\",\"timestamp\":1}}}}"
        )
    }

    fn tool_result_entry(
        entry_id: &str,
        parent_id: &str,
        call_id: &str,
        tool_name: &str,
    ) -> String {
        format!(
            "{{\"type\":\"message\",\"id\":\"{entry_id}\",\"parentId\":\"{parent_id}\",\"timestamp\":\"1970-01-01T00:00:00.002Z\",\"message\":{{\"role\":\"toolResult\",\"toolCallId\":\"{call_id}\",\"toolName\":\"{tool_name}\",\"content\":[{{\"type\":\"text\",\"text\":\"res\"}}],\"isError\":false,\"timestamp\":2}}}}"
        )
    }

    let cases: [(&str, &str); 7] = [
        ("hasOwnProperty", "bash"),
        ("constructor", "custom"),
        ("__proto__", "custom"),
        ("toString", "bash"),
        ("valueOf", "custom"),
        ("toLocaleString", "custom"),
        ("normal", "custom"),
    ];
    let mut lines = vec![
        "{\"type\":\"session\",\"version\":3,\"id\":\"proto-test\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp/kiana-proto\"}".to_string(),
    ];
    let mut parent_id: Option<String> = None;
    for (index, (call_id, tool_name)) in cases.iter().enumerate() {
        let assistant_id = format!("a{}", index + 1);
        let result_id = format!("t{}", index + 1);
        lines.push(assistant_entry(
            &assistant_id,
            parent_id.as_deref(),
            call_id,
            tool_name,
        ));
        lines.push(tool_result_entry(
            &result_id,
            &assistant_id,
            call_id,
            tool_name,
        ));
        parent_id = Some(result_id);
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let session_path = dir.path().join("proto-session.jsonl");
    std::fs::write(&session_path, lines.join("\n") + "\n").expect("write fixture");
    let manager = SessionManager::open(session_path.to_str().unwrap(), None, None).expect("open");
    let output = dir.path().join("proto.html");
    let mut renderer = RecordingRenderer::default();
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

    // 模板内联渲染的工具（bash）在 toolCall 阶段跳过；但 `hasOwnProperty`/`toString`
    // 命中 Object.prototype，算作真值 existing，toolResult 阶段仍调用 renderResult。
    assert_eq!(
        renderer.calls,
        vec![
            ("constructor".to_string(), "custom".to_string()),
            ("__proto__".to_string(), "custom".to_string()),
            ("valueOf".to_string(), "custom".to_string()),
            ("toLocaleString".to_string(), "custom".to_string()),
            ("normal".to_string(), "custom".to_string()),
        ]
    );
    assert_eq!(
        renderer.results,
        vec![
            ("hasOwnProperty".to_string(), "bash".to_string()),
            ("constructor".to_string(), "custom".to_string()),
            ("__proto__".to_string(), "custom".to_string()),
            ("toString".to_string(), "bash".to_string()),
            ("valueOf".to_string(), "custom".to_string()),
            ("toLocaleString".to_string(), "custom".to_string()),
            ("normal".to_string(), "custom".to_string()),
        ]
    );

    let payload = decode_payload(&path);
    let rendered = &payload[payload.find("\"renderedTools\":").expect("renderedTools")..];
    assert_eq!(
        rendered,
        concat!(
            "\"renderedTools\":{",
            "\"hasOwnProperty\":{\"resultHtmlCollapsed\":\"C:hasOwnProperty\",\"resultHtmlExpanded\":\"E:hasOwnProperty\"},",
            "\"constructor\":{\"callHtml\":\"CALL:constructor:1\",\"resultHtmlCollapsed\":\"C:constructor\",\"resultHtmlExpanded\":\"E:constructor\"},",
            "\"toString\":{\"resultHtmlCollapsed\":\"C:toString\",\"resultHtmlExpanded\":\"E:toString\"},",
            "\"valueOf\":{\"callHtml\":\"CALL:valueOf:1\",\"resultHtmlCollapsed\":\"C:valueOf\",\"resultHtmlExpanded\":\"E:valueOf\"},",
            "\"toLocaleString\":{\"callHtml\":\"CALL:toLocaleString:1\",\"resultHtmlCollapsed\":\"C:toLocaleString\",\"resultHtmlExpanded\":\"E:toLocaleString\"},",
            "\"normal\":{\"callHtml\":\"CALL:normal:1\",\"resultHtmlCollapsed\":\"C:normal\",\"resultHtmlExpanded\":\"E:normal\"}",
            "}}"
        )
    );
    // `__proto__` 赋值只改原型：不出现在自有键里。
    assert!(!rendered.contains("__proto__"));
}
