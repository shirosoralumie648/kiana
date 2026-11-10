//! 工具 / bash 执行组件的 TS 对拍测试。
//!
//! 移植自：
//! - `packages/coding-agent/test/tool-execution-component.test.ts`
//! - `packages/coding-agent/test/bash-execution-width.test.ts`
//! - `packages/coding-agent/test/output-pad.test.ts`（bash 与 tool 部分）
//! - `packages/coding-agent/test/edit-tool-no-full-redraw.test.ts`（组件级断言）
//! - `packages/coding-agent/test/read-line-range-display.test.ts`（TUI 断言）
//! - `packages/coding-agent/test/codemode-renderer.test.ts`（组件级断言）

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::types::{JsonValue, UserContentBlock};
use kiana_ext_api::tool_types::{RenderShell, ToolRenderContext, ToolRenderers};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::text_component::Text;
use kiana_tui::terminal_image::{
    ImageProtocol, TerminalCapabilities, reset_capabilities_cache, set_capabilities,
};
use kiana_tui::tui::{Component, Timer, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

use kiana_interactive::components::bash_execution::BashExecutionComponent;
use kiana_interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult, renderers_from_definition,
};

const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDAREAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAVAQEBAAAAAAAAAAAAAAAAAAAGCf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AD3VTB3/2Q==";

fn render_callback() -> RequestRenderCallback {
    Rc::new(|| {})
}

fn request_counter() -> (RequestRenderCallback, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let for_closure = Arc::clone(&counter);
    (
        Rc::new(move || {
            for_closure.fetch_add(1, Ordering::SeqCst);
        }),
        counter,
    )
}

fn mode() -> ToolExecutionOptions {
    ToolExecutionOptions::default()
}

fn text_renderer(label: &'static str) -> kiana_ext_api::tool_types::RenderCallFn {
    Arc::new(move |_args, _theme, _context| Box::new(Text::with_padding(label, 0, 0)))
}

fn result_text_renderer(label: &'static str) -> kiana_ext_api::tool_types::RenderResultFn {
    Arc::new(move |_result, _options, _theme, _context| Box::new(Text::with_padding(label, 0, 0)))
}

fn empty_renderers() -> ToolRenderers {
    ToolRenderers {
        render_shell: None,
        render_call: None,
        render_result: None,
    }
}

fn text_result(text: &str) -> Vec<UserContentBlock> {
    vec![UserContentBlock::Text(kiana_ai::types::TextContent {
        kind_text: kiana_ai::types::TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })]
}

fn golden(key: &str) -> Vec<String> {
    let raw = include_str!("fixtures/render_goldens_dark.json");
    let value: serde_json::Value = serde_json::from_str(raw).expect("golden json");
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing golden {key}"))
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect()
}

fn visible_lines(component: &mut dyn Component, width: usize) -> Vec<String> {
    component
        .render(width)
        .iter()
        .map(|line| kiana_core::ansi::strip_ansi(line))
        .collect()
}

/// 可手动触发的定时器（TS 的 fake timers）。
type PendingCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// 渲染器捕获的 `context.invalidate` 共享槽。
type InvalidateSlot = Arc<std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

struct FakeTimer {
    pending: PendingCallback,
}

impl Timer for FakeTimer {
    fn set(&mut self, _delay_ms: u64, callback: Box<dyn FnOnce()>) {
        *self.pending.borrow_mut() = Some(callback);
    }

    fn cancel(&mut self) {
        *self.pending.borrow_mut() = None;
    }
}

fn truncation_result() -> kiana_tools::truncate::TruncationResult {
    kiana_tools::truncate::TruncationResult {
        content: "a\nb".to_string(),
        truncated: true,
        truncated_by: Some(kiana_tools::truncate::TruncatedBy::Lines),
        total_lines: 2,
        total_bytes: 3,
        output_lines: 2,
        output_bytes: 3,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: 2000,
        max_bytes: 51200,
    }
}

// ============================================================================
// BashExecutionComponent
// ============================================================================

fn bash(request: RequestRenderCallback) -> BashExecutionComponent {
    BashExecutionComponent::new("pwd", Some(request), false, 1)
}

#[test]
fn bash_complete_renders_like_the_ts_component() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = bash(render_callback());
    component.append_output("/tmp\nsecond line\n");
    component.set_complete(Some(0), false, None, None);
    assert_eq!(component.render(80), golden("bash_complete"));
    assert_eq!(component.get_output(), "/tmp\nsecond line\n");
    assert_eq!(component.get_command(), "pwd");
}

#[test]
fn bash_expanded_renders_all_lines() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = BashExecutionComponent::new("echo hi", Some(render_callback()), false, 1);
    component.append_output("hello\n");
    component.set_complete(Some(0), false, None, None);
    component.set_expanded(true);
    assert_eq!(component.render(80), golden("bash_expanded"));
}

#[test]
fn bash_error_and_cancelled_status_lines() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut error = BashExecutionComponent::new("false", Some(render_callback()), false, 1);
    error.append_output("oops\n");
    error.set_complete(Some(1), false, None, None);
    assert_eq!(error.render(80), golden("bash_error"));

    let mut cancelled = BashExecutionComponent::new("sleep", Some(render_callback()), false, 1);
    cancelled.append_output("x");
    cancelled.set_complete(None, true, None, None);
    assert_eq!(cancelled.render(80), golden("bash_cancelled"));
}

#[test]
fn bash_hidden_line_count_and_expand_hint() {
    kiana_theme::init_theme(Some("dark"), false);
    let lines = (1..=30)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<String>>()
        .join("\n");
    let mut collapsed = BashExecutionComponent::new("seq", Some(render_callback()), true, 1);
    collapsed.append_output(&format!("{lines}\n"));
    collapsed.set_complete(Some(0), false, None, None);
    assert_eq!(collapsed.render(80), golden("bash_hidden"));

    let mut expanded = BashExecutionComponent::new("seq", Some(render_callback()), false, 1);
    expanded.append_output(&format!("{lines}\n"));
    expanded.set_complete(Some(0), false, None, None);
    expanded.set_expanded(true);
    assert_eq!(expanded.render(80), golden("bash_hidden_expanded"));
}

#[test]
fn bash_truncation_warning() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = BashExecutionComponent::new("seq", Some(render_callback()), false, 1);
    component.append_output("a\nb\n");
    component.set_complete(
        Some(0),
        false,
        Some(kiana_tools::truncate::TruncationResult {
            content: "a\nb".to_string(),
            truncated: true,
            truncated_by: Some(kiana_tools::truncate::TruncatedBy::Lines),
            total_lines: 2,
            total_bytes: 3,
            output_lines: 2,
            output_bytes: 3,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: 2000,
            max_bytes: 51200,
        }),
        Some("/tmp/full.txt".to_string()),
    );
    assert_eq!(component.render(80), golden("bash_truncated"));
}

#[test]
fn bash_ignores_an_empty_full_output_path() {
    kiana_theme::init_theme(Some("dark"), false);
    // TS: `if (wasTruncated && this.fullOutputPath)` — 空字符串是 falsy，不显示警告。
    let mut empty_path = BashExecutionComponent::new("seq", Some(render_callback()), false, 1);
    empty_path.append_output("a\nb\n");
    empty_path.set_complete(
        Some(0),
        false,
        Some(truncation_result()),
        Some(String::new()),
    );
    let lines = visible_lines(&mut empty_path, 80);
    assert!(
        !lines.iter().any(|line| line.contains("Output truncated")),
        "empty fullOutputPath must not render the warning: {lines:?}"
    );

    // 非空路径仍然显示警告（现有 golden 覆盖完整字节）。
    let mut path = BashExecutionComponent::new("seq", Some(render_callback()), false, 1);
    path.append_output("a\nb\n");
    path.set_complete(
        Some(0),
        false,
        Some(truncation_result()),
        Some("/tmp/full.txt".to_string()),
    );
    assert!(
        visible_lines(&mut path, 80)
            .iter()
            .any(|line| line.contains("Output truncated. Full output: /tmp/full.txt"))
    );
}

#[test]
fn bash_set_timer_drives_the_running_loader_animation() {
    kiana_theme::init_theme(Some("dark"), false);
    let (request, calls) = request_counter();
    let mut component = BashExecutionComponent::new("sleep 1", Some(request), false, 1);
    let pending: PendingCallback = Rc::new(RefCell::new(None));
    component.set_timer(Box::new(FakeTimer {
        pending: Rc::clone(&pending),
    }));
    assert!(
        pending.borrow().is_some(),
        "the loader schedules its animation interval"
    );
    let before = calls.load(Ordering::SeqCst);
    let callback = pending.borrow_mut().take().expect("scheduled");
    callback();
    assert!(
        calls.load(Ordering::SeqCst) > before,
        "each tick requests a render"
    );
    assert!(pending.borrow().is_some(), "the interval re-arms");
}

#[test]
fn bash_output_pad_zero_has_no_leading_space() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = BashExecutionComponent::new("pwd", Some(render_callback()), false, 0);
    component.append_output("/tmp");
    component.set_complete(Some(0), false, None, None);
    assert_eq!(component.render(60), golden("bash_pad0"));
}

#[test]
fn bash_collapsed_preview_lines_respect_render_time_width_not_construction_time_width() {
    kiana_theme::init_theme(Some("dark"), false);
    let wide_width = 200;
    let narrow_width = 80;
    let mut component = BashExecutionComponent::new("pwd", Some(render_callback()), false, 1);
    let long_line = "x".repeat(150);
    component.append_output(&format!("{long_line}\n{long_line}\n"));
    component.set_complete(Some(0), false, None, None);
    let lines = component.render(narrow_width);
    for (index, line) in lines.iter().enumerate() {
        let width = kiana_tui::visible_width(line);
        assert!(
            width <= narrow_width,
            "Line {index} visibleWidth={width} > {narrow_width}"
        );
    }
    let _ = wide_width;
}

#[test]
fn bash_recomputes_lines_when_width_changes_between_renders() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component =
        BashExecutionComponent::new("echo hello", Some(render_callback()), false, 1);
    let long_line = "abcdefghij".repeat(20);
    component.append_output(&format!("{long_line}\n"));
    component.set_complete(Some(0), false, None, None);
    for line in component.render(200) {
        assert!(kiana_tui::visible_width(&line) <= 200);
    }
    for (index, line) in component.render(60).iter().enumerate() {
        assert!(
            kiana_tui::visible_width(line) <= 60,
            "Line {index} too wide"
        );
    }
}

#[test]
fn bash_append_output_continues_the_last_line_and_strips_ansi() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = bash(render_callback());
    component.append_output("\x1b[31mhello");
    component.append_output(" world\r\nnext\rline\r\n");
    assert_eq!(component.get_output(), "hello world\nnext\nline\n");
}

// ============================================================================
// ToolExecutionComponent
// ============================================================================

#[test]
fn stacks_custom_call_and_result_renderers() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(text_renderer("custom call")),
        render_result: Some(result_text_renderer("custom result")),
    };
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-1",
        JsonValue::Null,
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    assert!(
        visible_lines(&mut component, 120)
            .join("\n")
            .contains("custom call")
    );
    component.update_result(
        ToolExecutionResult::new(text_result("done"), JsonValue::Null, false),
        false,
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("custom call"));
    assert!(rendered.contains("custom result"));
}

#[test]
fn self_rendered_empty_tool_rows_take_no_layout_space() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = ToolRenderers {
        render_shell: Some(RenderShell::SelfRender),
        render_call: Some(Arc::new(|_, _, _| {
            Box::new(Text::with_padding("", 0, 0)) as Box<dyn Component>
        })),
        render_result: Some(Arc::new(|_, _, _, _| {
            Box::new(Text::with_padding("", 0, 0)) as Box<dyn Component>
        })),
    };
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-empty-self-render",
        JsonValue::Null,
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    assert!(component.render(120).is_empty());
    component.update_result(
        ToolExecutionResult::new(Vec::new(), JsonValue::Null, false),
        false,
    );
    assert!(component.render(120).is_empty());
}

#[test]
fn uses_built_in_rendering_for_built_in_overrides_without_custom_renderers() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers =
        kiana_tools::renderers::with_built_in_renderers("edit", Some(empty_renderers()))
            .expect("edit has built-in renderers");
    let mut component = ToolExecutionComponent::new(
        "edit",
        "tool-2",
        serde_json::json!({"path": "README.md", "oldText": "before", "newText": "after"}),
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(
            Vec::new(),
            serde_json::json!({"diff": "+1 after", "firstChangedLine": 1}),
            false,
        ),
        false,
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("edit"));
    assert!(rendered.contains("README.md"));
    assert!(!rendered.contains(":1"));
}

#[test]
fn preserves_legacy_file_path_rendering_compatibility_for_built_in_tools() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-3",
        serde_json::json!({"file_path": "README.md"}),
        mode(),
        None,
        render_callback(),
        "/",
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("read"));
    assert!(rendered.contains("README.md"));
}

#[test]
fn read_calls_with_null_offset_and_limit_render_as_full_file_reads() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_read_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-read-null-range",
        serde_json::json!({"path": "src/example.ts", "offset": null, "limit": null}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("read src/example.ts"));
    assert!(!rendered.contains("src/example.ts:"));
}

#[test]
fn inherits_missing_built_in_result_renderer_slot_from_the_built_in_tool() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = kiana_tools::renderers::with_built_in_renderers(
        "read",
        Some(ToolRenderers {
            render_shell: None,
            render_call: Some(text_renderer("override call")),
            render_result: None,
        }),
    )
    .expect("read has built-in renderers");
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-4b",
        serde_json::json!({"path": "notes.txt"}),
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("hello"), JsonValue::Null, false),
        false,
    );
    component.set_expanded(true);
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("override call"));
    assert!(rendered.contains("hello"));
}

#[test]
fn inherits_missing_built_in_call_renderer_slot_from_the_built_in_tool() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = kiana_tools::renderers::with_built_in_renderers(
        "read",
        Some(ToolRenderers {
            render_shell: None,
            render_call: None,
            render_result: Some(result_text_renderer("override result")),
        }),
    )
    .expect("read has built-in renderers");
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-4c",
        serde_json::json!({"path": "README.md"}),
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("hello"), JsonValue::Null, false),
        false,
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("read"));
    assert!(rendered.contains("README.md"));
    assert!(rendered.contains("override result"));
}

#[test]
fn shares_renderer_state_across_custom_call_and_result_slots() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(|_args, _theme, context: &ToolRenderContext| {
            let mut state = context.state.lock().unwrap();
            let object = state.as_object_mut().expect("state object");
            object
                .entry("token".to_string())
                .or_insert_with(|| JsonValue::String("shared-token".to_string()));
            let token = object
                .get("token")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            Box::new(Text::with_padding(format!("custom call {token}"), 0, 0)) as Box<dyn Component>
        })),
        render_result: Some(Arc::new(
            |_result, _options, _theme, context: &ToolRenderContext| {
                let state = context.state.lock().unwrap();
                let token = state
                    .get("token")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string();
                Box::new(Text::with_padding(format!("custom result {token}"), 0, 0))
                    as Box<dyn Component>
            },
        )),
    };
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-5",
        JsonValue::Null,
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("done"), JsonValue::Null, false),
        false,
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("custom call shared-token"));
    assert!(rendered.contains("custom result shared-token"));
}

#[test]
fn exposes_args_in_render_result_context() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(text_renderer("call")),
        render_result: Some(Arc::new(
            |_result, _options, _theme, context: &ToolRenderContext| {
                let foo = context
                    .args
                    .get("foo")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string();
                Box::new(Text::with_padding(format!("arg:{foo}"), 0, 0)) as Box<dyn Component>
            },
        )),
    };
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-5b",
        serde_json::json!({"foo": "bar"}),
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("done"), JsonValue::Null, false),
        false,
    );
    assert!(
        visible_lines(&mut component, 120)
            .join("\n")
            .contains("arg:bar")
    );
}

#[test]
fn shows_arguments_in_the_fallback_call_header() {
    kiana_theme::init_theme(Some("dark"), false);
    let long_value = "x".repeat(200);
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-args",
        serde_json::json!({"query": "kiana", "long": long_value, "text": "line one\nline two"}),
        mode(),
        Some(empty_renderers()),
        render_callback(),
        "/",
    );
    let collapsed = visible_lines(&mut component, 300).join("\n");
    assert!(collapsed.contains("custom_tool query=\"kiana\" long=\"xxx"));
    assert!(collapsed.contains("..."));
    assert!(!collapsed.contains(&long_value));

    component.set_expanded(true);
    let expanded = visible_lines(&mut component, 300).join("\n");
    assert!(expanded.contains("  query: kiana"));
    assert!(expanded.contains(&long_value));
    let expanded_lines: Vec<&str> = expanded.split('\n').map(str::trim_end).collect();
    let text_line = expanded_lines
        .iter()
        .position(|line| line.ends_with("  text: line one"))
        .expect("text arg line");
    assert!(expanded_lines[text_line + 1].contains("    line two"));
}

#[test]
fn collapses_fallback_results_until_expanded() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "tool-6",
        serde_json::json!({"foo": "bar"}),
        mode(),
        Some(empty_renderers()),
        render_callback(),
        "/",
    );
    let output = (1..=15)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<String>>()
        .join("\n");
    component.update_result(
        ToolExecutionResult::new(text_result(&output), JsonValue::Null, false),
        false,
    );
    let collapsed = visible_lines(&mut component, 120).join("\n");
    assert!(collapsed.contains("custom_tool"));
    assert!(collapsed.contains("line-10"));
    assert!(!collapsed.contains("line-11"));
    assert!(collapsed.contains("5 more lines"));
    assert!(collapsed.contains("to expand"));

    component.set_expanded(true);
    let expanded = visible_lines(&mut component, 120).join("\n");
    assert!(expanded.contains("line-15"));
    assert!(!expanded.contains("more lines"));
}

#[test]
fn renders_streamed_partial_arguments_once_per_frame_with_the_latest_arguments() {
    kiana_theme::init_theme(Some("dark"), false);
    let write = kiana_tools::renderers::write_renderers();
    let render_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&render_calls);
    let original = write.render_call.clone().expect("write call renderer");
    let renderers = ToolRenderers {
        render_shell: write.render_shell,
        render_call: Some(Arc::new(move |args, theme, context| {
            counter.fetch_add(1, Ordering::SeqCst);
            original(args, theme, context)
        })),
        render_result: write.render_result.clone(),
    };
    let (request, _requests) = request_counter();
    let mut component = ToolExecutionComponent::new(
        "write",
        "tool-9",
        JsonValue::Null,
        mode(),
        Some(renderers.clone()),
        request,
        ".",
    );
    let lines: Vec<String> = (0..200)
        .map(|index| format!("const line{index} = {index};"))
        .collect();
    let calls_before = render_calls.load(Ordering::SeqCst);
    let mut content = String::new();
    for line in &lines {
        content.push_str(&format!("{line}\n"));
        component.update_args(serde_json::json!({"path": "src/file.ts", "content": content}));
    }
    assert_eq!(render_calls.load(Ordering::SeqCst), calls_before);

    let streamed = visible_lines(&mut component, 120).join("\n");
    assert_eq!(render_calls.load(Ordering::SeqCst) - calls_before, 1);
    let mut direct = ToolExecutionComponent::new(
        "write",
        "tool-10",
        serde_json::json!({"path": "src/file.ts", "content": content}),
        mode(),
        Some(kiana_tools::renderers::write_renderers()),
        render_callback(),
        ".",
    );
    assert_eq!(streamed, visible_lines(&mut direct, 120).join("\n"));

    component.update_args(serde_json::json!({"path": "src/file.ts", "content": format!("{content}const tail = 1;\n")}));
    let final_args = serde_json::json!({"path": "src/file.ts", "content": format!("{content}const tail = 2;\n")});
    component.update_args(final_args.clone());
    let next = visible_lines(&mut component, 120).join("\n");
    assert_eq!(render_calls.load(Ordering::SeqCst) - calls_before, 2);
    assert!(next.contains("201 total"));
    let mut direct = ToolExecutionComponent::new(
        "write",
        "t",
        final_args,
        mode(),
        Some(kiana_tools::renderers::write_renderers()),
        render_callback(),
        ".",
    );
    assert_eq!(next, visible_lines(&mut direct, 120).join("\n"));
    component.render(120);
    assert_eq!(render_calls.load(Ordering::SeqCst) - calls_before, 2);
}

#[test]
fn shows_the_recorded_duration_for_restored_results() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_bash_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "bash",
        "tool-bash-recorded",
        serde_json::json!({"command": "sleep 4"}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(Vec::new(), JsonValue::Null, false).with_duration_ms(Some(4200.0)),
        false,
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    assert!(rendered.contains("Took 4.2s"));
}

#[test]
fn expands_a_collapsed_tool_result_when_clicked() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_read_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-click-expand",
        serde_json::json!({"path": "notes.txt"}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("hidden content"), JsonValue::Null, false),
        false,
    );
    let width = 120;
    let lines = component.render(width);
    let result_row = lines
        .iter()
        .position(|line| kiana_core::ansi::strip_ansi(line).contains("notes.txt"))
        .expect("call row");
    let event = TuiMouseEvent {
        event_type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 2,
        y: result_row as isize,
        screen_x: 2,
        screen_y: result_row as isize,
        width,
        height: lines.len(),
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: Some(1),
    };
    assert_eq!(
        component
            .handle_mouse(&event)
            .and_then(|result| result.handled),
        Some(true)
    );
    assert!(
        visible_lines(&mut component, width)
            .join("\n")
            .contains("hidden content")
    );
}

#[test]
fn collapses_ordinary_read_results_until_expanded() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_read_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-ordinary-read-collapsed",
        serde_json::json!({"path": "notes.txt"}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("hidden content"), JsonValue::Null, false),
        false,
    );
    let collapsed = visible_lines(&mut component, 120).join("\n");
    assert!(collapsed.contains("read"));
    assert!(collapsed.contains("notes.txt"));
    assert!(!collapsed.contains("hidden content"));
    component.set_expanded(true);
    assert!(
        visible_lines(&mut component, 120)
            .join("\n")
            .contains("hidden content")
    );
}

#[test]
fn read_range_display_shows_string_ranges() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_read_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "read",
        "call",
        serde_json::json!({"path": "notes.txt", "offset": "25", "limit": "13"}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    assert!(
        visible_lines(&mut component, 120)
            .join("\n")
            .contains("read notes.txt:25-37")
    );
}

#[test]
fn tool_output_pad_zero_and_one() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = ToolRenderers {
        render_shell: None,
        render_call: Some(text_renderer("custom_tool")),
        render_result: Some(result_text_renderer("ok")),
    };
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "id",
        serde_json::json!({}),
        ToolExecutionOptions {
            output_pad: 0,
            ..ToolExecutionOptions::default()
        },
        Some(renderers),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("ok"), JsonValue::Null, false),
        false,
    );
    let render_lines = |component: &mut ToolExecutionComponent| -> Vec<String> {
        component
            .render(60)
            .iter()
            .map(|line| kiana_core::ansi::strip_ansi(line).trim_end().to_string())
            .filter(|line| {
                line.chars()
                    .any(|ch| ch.is_alphanumeric() || ch == '$' || ch == '(')
            })
            .collect()
    };
    let lines = render_lines(&mut component);
    assert!(!lines.iter().any(|line| line.starts_with(' ')));
    component.set_output_pad(1);
    assert_eq!(
        render_lines(&mut component),
        lines
            .iter()
            .map(|line| format!(" {line}"))
            .collect::<Vec<String>>()
    );
}

#[test]
fn tool_without_definition_uses_content_text_and_click_expand() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = ToolExecutionComponent::new(
        "custom_tool",
        "id",
        serde_json::json!({}),
        ToolExecutionOptions {
            output_pad: 0,
            ..ToolExecutionOptions::default()
        },
        None,
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("ok"), JsonValue::Null, false),
        false,
    );
    let lines = component
        .render(60)
        .iter()
        .map(|line| kiana_core::ansi::strip_ansi(line).trim_end().to_string())
        .filter(|line| {
            line.chars()
                .any(|ch| ch.is_alphanumeric() || ch == '$' || ch == '(')
        })
        .collect::<Vec<String>>();
    assert!(!lines.iter().any(|line| line.starts_with(' ')));
    component.set_output_pad(1);
    let padded = component
        .render(60)
        .iter()
        .map(|line| kiana_core::ansi::strip_ansi(line).trim_end().to_string())
        .filter(|line| {
            line.chars()
                .any(|ch| ch.is_alphanumeric() || ch == '$' || ch == '(')
        })
        .collect::<Vec<String>>();
    assert_eq!(
        padded,
        lines
            .iter()
            .map(|line| format!(" {line}"))
            .collect::<Vec<String>>()
    );
}

#[test]
fn converts_non_png_tool_images_once_the_transcoder_loads() {
    kiana_theme::init_theme(Some("dark"), false);
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    let mut component = ToolExecutionComponent::new(
        "tool",
        "id",
        JsonValue::Null,
        mode(),
        None,
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(
            vec![UserContentBlock::Image(kiana_ai::types::ImageContent {
                kind_image: kiana_ai::types::ImageKind::Image,
                data: "cGFydGlhbA==".to_string(),
                mime_type: "image/jpeg".to_string(),
            })],
            JsonValue::Null,
            false,
        ),
        true,
    );
    component.update_result(
        ToolExecutionResult::new(
            vec![UserContentBlock::Image(kiana_ai::types::ImageContent {
                kind_image: kiana_ai::types::ImageKind::Image,
                data: TINY_JPEG.to_string(),
                mime_type: "image/jpeg".to_string(),
            })],
            JsonValue::Null,
            false,
        ),
        false,
    );
    let rendered = component.render(120).join("\n");
    assert!(rendered.contains(";iVBORw0KGgo"));
    assert!(!rendered.contains("cGFydGlhbA=="));
    let before = rendered;
    component.invalidate();
    assert_eq!(component.render(120).join("\n"), before);
    reset_capabilities_cache();
}

#[test]
fn edit_tool_render_contains_the_diff_and_background_switches_without_rebuild() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_edit_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "edit",
        "tool-call-1",
        serde_json::json!({"path": "notes.txt", "edits": [{"oldText": "a\nb", "newText": "a\nc"}]}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    component.set_args_complete();
    let call_render = visible_lines(&mut component, 80).join("\n");
    assert!(call_render.contains("edit"));
    component.update_result(
        ToolExecutionResult::new(
            text_result("Successfully replaced 1 block(s) in notes.txt."),
            serde_json::json!({"diff": "-1 a\n+1 c", "firstChangedLine": 1}),
            false,
        ),
        false,
    );
    let settled = visible_lines(&mut component, 80).join("\n");
    assert!(settled.contains("edit"));
    assert!(!settled.contains("Successfully replaced"));
}

#[test]
fn codemode_renderers_flow_through_the_component() {
    kiana_theme::init_theme(Some("dark"), false);
    let renderers = kiana_builtin_extensions::codemode::renderer::codemode_renderers();
    let mut component = ToolExecutionComponent::new(
        "codemode",
        "call",
        serde_json::json!({"code": ""}),
        mode(),
        Some(renderers),
        render_callback(),
        "/",
    );
    component.mark_execution_started();
    component.update_result(
        ToolExecutionResult::new(Vec::new(), serde_json::json!({"calls": []}), false),
        true,
    );
    let running = visible_lines(&mut component, 120).join("\n");
    assert!(running.contains("Elapsed"));
    component.update_result(
        ToolExecutionResult::new(Vec::new(), serde_json::json!({"calls": []}), false)
            .with_duration_ms(Some(5100.0)),
        false,
    );
    let completed = visible_lines(&mut component, 120).join("\n");
    assert!(completed.contains("Took 5.1s"));
    assert!(!completed.contains("Elapsed"));
}

#[test]
fn theme_color_helper_matches_theme_tokens() {
    kiana_theme::init_theme(Some("dark"), false);
    assert!(
        kiana_theme::THEME
            .fg(ThemeColor::ToolOutput, "x")
            .expect("theme")
            .contains('x')
    );
}

#[test]
fn self_rendered_edit_result_respects_output_pad() {
    kiana_theme::init_theme(Some("dark"), false);
    let render_lines = |component: &mut ToolExecutionComponent| -> Vec<String> {
        component
            .render(60)
            .iter()
            .map(|line| kiana_core::ansi::strip_ansi(line).trim_end().to_string())
            .filter(|line| {
                line.chars()
                    .any(|ch| ch.is_alphanumeric() || ch == '$' || ch == '(')
            })
            .collect()
    };
    let definition = kiana_tools::create_edit_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "edit",
        "id",
        serde_json::json!({"path": "file.txt", "edits": [{"oldText": "old", "newText": "new"}]}),
        ToolExecutionOptions {
            output_pad: 0,
            ..ToolExecutionOptions::default()
        },
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(
            text_result("Could not find old text"),
            JsonValue::Null,
            true,
        ),
        false,
    );
    let lines = render_lines(&mut component);
    assert!(!lines.iter().any(|line| line.starts_with(' ')));
    component.set_output_pad(1);
    assert_eq!(
        render_lines(&mut component),
        lines
            .iter()
            .map(|line| format!(" {line}"))
            .collect::<Vec<String>>()
    );
}

#[test]
fn does_not_duplicate_built_in_headers_when_passed_the_active_built_in_definition() {
    kiana_theme::init_theme(Some("dark"), false);
    let definition = kiana_tools::create_read_tool_definition("/", None);
    let mut component = ToolExecutionComponent::new(
        "read",
        "tool-4",
        serde_json::json!({"path": "README.md"}),
        mode(),
        Some(renderers_from_definition(&definition)),
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(text_result("hello"), JsonValue::Null, false),
        false,
    );
    let rendered = visible_lines(&mut component, 120).join("\n");
    let occurrences = rendered.matches("read").count();
    assert_eq!(occurrences, 1, "rendered: {rendered}");
}

#[test]
fn bash_running_shows_the_loader_message_until_complete() {
    kiana_theme::init_theme(Some("dark"), false);
    let mut component = BashExecutionComponent::new("sleep 1", Some(render_callback()), false, 1);
    component.append_output("partial");
    let running = visible_lines(&mut component, 80).join("\n");
    assert!(running.contains("Running... ("));
    component.set_complete(Some(0), false, None, None);
    let done = visible_lines(&mut component, 80).join("\n");
    assert!(!done.contains("Running..."));
}

#[test]
fn update_args_requests_a_render_without_rebuilding() {
    kiana_theme::init_theme(Some("dark"), false);
    let (request, requests) = request_counter();
    let mut component = ToolExecutionComponent::new(
        "write",
        "tool-render-request",
        JsonValue::Null,
        mode(),
        Some(kiana_tools::renderers::write_renderers()),
        request,
        ".",
    );
    let before = requests.load(Ordering::SeqCst);
    component.update_args(serde_json::json!({"path": "a.ts", "content": "x"}));
    assert!(requests.load(Ordering::SeqCst) > before);
}

#[test]
fn invalidate_refreshes_reused_image_components() {
    // TS `Container.invalidate()` 递归失效子组件；`updateDisplay` 在来源未变时会复用 Image，
    // 若 ToolExecutionComponent::invalidate 不清除 Image 的缓存行，它永远不会重绘。
    kiana_theme::init_theme(Some("dark"), false);
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    let mut component = ToolExecutionComponent::new(
        "tool",
        "id",
        JsonValue::Null,
        mode(),
        None,
        render_callback(),
        "/",
    );
    component.update_result(
        ToolExecutionResult::new(
            vec![UserContentBlock::Image(kiana_ai::types::ImageContent {
                kind_image: kiana_ai::types::ImageKind::Image,
                data: "cGFydGlhbA==".to_string(),
                mime_type: "image/jpeg".to_string(),
            })],
            JsonValue::Null,
            false,
        ),
        false,
    );
    let fallback = component.render(120).join("\n");
    assert!(fallback.contains("[Image: [image/jpeg]"), "{fallback}");

    // 首次渲染后再注册转码器：只有 Image 缓存被清掉，下一次渲染才会走图片协议。
    const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    kiana_tui::components::image::set_image_transcoder(Some(Arc::new(
        |_data: &str, _mime: &str| Some(ONE_PIXEL_PNG.to_string()),
    )));
    component.invalidate();
    let rendered = component.render(120).join("\n");
    assert!(!rendered.contains("[Image:"), "{rendered}");
    assert!(rendered.contains(ONE_PIXEL_PNG), "{rendered}");
    kiana_tui::components::image::set_image_transcoder(None);
    reset_capabilities_cache();
}

// ============================================================================
// ToolRenderContext 契约：invalidate / lastComponent
// ============================================================================

struct MarkerComponent;

impl Component for MarkerComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["marker".to_string()]
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
}

fn renderers_with_call(render_call: kiana_ext_api::tool_types::RenderCallFn) -> ToolRenderers {
    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: None,
    }
}

#[test]
fn render_context_invalidate_requests_a_repaint_on_the_ui_thread() {
    // TS: `invalidate: () => { this.invalidate(); this.ui.requestRender(); }`。
    kiana_theme::init_theme(Some("dark"), false);
    let (request, calls) = request_counter();
    let slot: InvalidateSlot = Arc::new(std::sync::Mutex::new(None));
    let slot_in_renderer = Arc::clone(&slot);
    let renderer_calls = Arc::new(AtomicUsize::new(0));
    let renderer_calls_in_closure = Arc::clone(&renderer_calls);
    let render_call: kiana_ext_api::tool_types::RenderCallFn =
        Arc::new(move |_args, _theme, context| {
            *slot_in_renderer.lock().unwrap() = Some(Arc::clone(&context.invalidate));
            renderer_calls_in_closure.fetch_add(1, Ordering::SeqCst);
            Box::new(Text::with_padding("call", 0, 0))
        });
    let mut component = ToolExecutionComponent::new(
        "tool",
        "id",
        JsonValue::Null,
        mode(),
        Some(renderers_with_call(render_call)),
        request,
        "/tmp",
    );
    assert_eq!(
        renderer_calls.load(Ordering::SeqCst),
        1,
        "constructor runs the renderer once"
    );

    let invalidate = slot
        .lock()
        .unwrap()
        .clone()
        .expect("renderer captured invalidate");
    let before = calls.load(Ordering::SeqCst);
    invalidate();
    assert!(
        calls.load(Ordering::SeqCst) > before,
        "context.invalidate must request a repaint, not only set a dirty flag"
    );
    component.render(80);
    assert_eq!(
        renderer_calls.load(Ordering::SeqCst),
        2,
        "the invalidate flag rebuilds on the next render"
    );
}

#[test]
fn render_context_invalidate_calls_the_thread_safe_channel_from_another_thread() {
    kiana_theme::init_theme(Some("dark"), false);
    let slot: InvalidateSlot = Arc::new(std::sync::Mutex::new(None));
    let slot_in_renderer = Arc::clone(&slot);
    let render_call: kiana_ext_api::tool_types::RenderCallFn =
        Arc::new(move |_args, _theme, context| {
            *slot_in_renderer.lock().unwrap() = Some(Arc::clone(&context.invalidate));
            Box::new(MarkerComponent)
        });
    let mut component = ToolExecutionComponent::new(
        "tool",
        "id",
        JsonValue::Null,
        mode(),
        Some(renderers_with_call(render_call)),
        render_callback(),
        "/tmp",
    );
    let cross_thread_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&cross_thread_calls);
    component.set_thread_request_render(Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    let invalidate = slot
        .lock()
        .unwrap()
        .clone()
        .expect("renderer captured invalidate");
    std::thread::spawn(move || invalidate()).join().unwrap();
    assert_eq!(
        cross_thread_calls.load(Ordering::SeqCst),
        1,
        "a background invalidate uses the host-provided thread-safe render channel"
    );
}

#[test]
fn renderers_receive_the_component_they_returned_last_frame() {
    // TS 把 `callRendererComponent` / `resultRendererComponent` 作为 `lastComponent`
    // 传给渲染器（write/edit 用作缓存身份）；失败时重置为 undefined。
    kiana_theme::init_theme(Some("dark"), false);
    let seen: Arc<std::sync::Mutex<Vec<bool>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_in_renderer = Arc::clone(&seen);
    let render_call: kiana_ext_api::tool_types::RenderCallFn =
        Arc::new(move |_args, _theme, context| {
            let is_marker = context
                .last_component
                .as_deref()
                .and_then(Component::as_any)
                .is_some_and(|any| any.downcast_ref::<MarkerComponent>().is_some());
            seen_in_renderer.lock().unwrap().push(is_marker);
            Box::new(MarkerComponent)
        });
    let mut component = ToolExecutionComponent::new(
        "tool",
        "id",
        JsonValue::Null,
        mode(),
        Some(renderers_with_call(render_call)),
        render_callback(),
        "/tmp",
    );
    component.render(80);
    component.update_args(JsonValue::String("x".to_string()));
    component.render(80);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![false, true],
        "the previous frame's component is passed on the next render"
    );
}

#[test]
fn failed_renderers_reset_the_cached_last_component() {
    kiana_theme::init_theme(Some("dark"), false);
    let seen: Arc<std::sync::Mutex<Vec<bool>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_in_renderer = Arc::clone(&seen);
    let calls_in_renderer = Arc::clone(&calls);
    let render_call: kiana_ext_api::tool_types::RenderCallFn =
        Arc::new(move |_args, _theme, context| {
            let index = calls_in_renderer.fetch_add(1, Ordering::SeqCst);
            if index == 1 {
                panic!("renderer exploded");
            }
            let is_marker = context
                .last_component
                .as_deref()
                .and_then(Component::as_any)
                .is_some_and(|any| any.downcast_ref::<MarkerComponent>().is_some());
            seen_in_renderer.lock().unwrap().push(is_marker);
            Box::new(MarkerComponent)
        });
    let mut component = ToolExecutionComponent::new(
        "tool",
        "id",
        JsonValue::Null,
        mode(),
        Some(renderers_with_call(render_call)),
        render_callback(),
        "/tmp",
    );
    component.update_args(JsonValue::String("x".to_string()));
    component.render(80); // 第二次调用 panic -> 兜底，缓存清空
    component.update_args(JsonValue::String("y".to_string()));
    component.render(80); // 第三次调用拿到的 lastComponent 必须是 None
    assert_eq!(
        *seen.lock().unwrap(),
        vec![false, false],
        "a failed renderer clears the cached component (TS sets it to undefined)"
    );
}
