//! 移植 `packages/coding-agent/test/codemode-renderer.test.ts`（5 个 `it`）。
//!
//! TS 通过 `ToolExecutionComponent` 渲染；本 crate 不能依赖 `kiana-interactive`（阶段 7），
//! 因此直接调用 [`codemode_renderers`] 的 `render_result`，断言去 ANSI 后的行（宽度 200、
//! 每行 trimEnd、最后整体 trim），与 TS 测试的 `render()` 辅助函数一致。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, TextKind, UserContentBlock};
use kiana_builtin_extensions::codemode::renderer::codemode_renderers;
use kiana_ext_api::tool_types::{RenderState, ToolRenderContext, ToolRenderResultOptions};
use kiana_theme::theme::{Theme, init_theme};
use kiana_tui::tui::Component;
use serde_json::{Map, Value, json};

fn ensure_theme() -> Arc<Theme> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| init_theme(Some("dark"), false));
    kiana_theme::theme::current_theme().expect("theme must be initialized")
}

fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(kiana_ai::types::TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn result(content: Vec<UserContentBlock>, details: JsonValue) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content,
        details,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn base_context(expanded: bool, is_error: bool) -> ToolRenderContext {
    ToolRenderContext {
        args: json!({"code": ""}),
        tool_call_id: "call".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::new(Mutex::new(JsonValue::Object(Map::new()))) as RenderState,
        cwd: PathBuf::from("/"),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded,
        show_images: false,
        is_error,
        duration_ms: None,
        output_pad: 1,
    }
}

fn strip(component: &mut Box<dyn Component>, width: usize) -> String {
    let lines = component.render(width);
    let text = kiana_core::ansi::strip_ansi(&lines.join("\n"));
    let trimmed: Vec<&str> = text.split('\n').map(str::trim_end).collect::<Vec<_>>();
    kiana_json::js_string::js_trim(&trimmed.join("\n")).to_string()
}

#[allow(clippy::type_complexity)] // 与渲染器签名一致的临时借用，拆别名只会分散。
fn render(
    result: AgentToolResult<JsonValue>,
    is_error: bool,
    expanded: bool,
    width: usize,
) -> String {
    let theme = ensure_theme();
    let context = base_context(expanded, is_error);
    let renderers = codemode_renderers();
    let render_result = renderers.render_result.expect("renderResult");
    let function: &dyn Fn(
        &AgentToolResult<JsonValue>,
        &ToolRenderResultOptions,
        &Theme,
        &ToolRenderContext,
    ) -> Box<dyn Component> = render_result.as_ref();
    let mut component = function(
        &result,
        &ToolRenderResultOptions {
            expanded,
            is_partial: false,
        },
        &theme,
        &context,
    );
    strip(&mut component, width)
}

#[test]
fn hides_the_script_header_and_shows_the_output() {
    let text = render(
        result(
            vec![
                text_block("Script completed\nWall time 0.1 seconds\nOutput:\n"),
                text_block("hello"),
            ],
            json!({"calls": [{"id": "call/1", "name": "read", "args": "{\"path\":\"a\"}", "status": "ok", "durationMs": 5}]}),
        ),
        false,
        true,
        200,
    );
    assert_eq!(text, "✓ read {\"path\":\"a\"} 5ms\n\nhello");
}

#[test]
fn shows_the_cost_of_model_calls_and_their_total() {
    let text = render(
        result(
            vec![text_block(
                "Script completed\nWall time 0.1 seconds\nOutput:\n",
            )],
            json!({"calls": [
                {"id": "call/models.classify/1", "name": "models.classify", "args": "scorer/judge", "status": "ok", "durationMs": 5, "cost": 0.000012936},
                {"id": "call/models.classify/2", "name": "models.classify", "args": "scorer/judge", "status": "ok", "durationMs": 5, "cost": 0.02},
                {"id": "call/models.classify/3", "name": "models.classify", "args": "scorer/judge", "status": "ok", "durationMs": 5}
            ]}),
        ),
        false,
        true,
        200,
    );
    assert_eq!(
        text,
        [
            "✓ models.classify scorer/judge 5ms $0.000013",
            "✓ models.classify scorer/judge 5ms $0.02",
            "✓ models.classify scorer/judge 5ms",
            "Model calls: $0.02",
        ]
        .join("\n")
    );
}

#[test]
fn shows_results_without_a_header_such_as_rejected_options() {
    let text = render(
        result(
            vec![text_block(
                "The @options line must be followed by JavaScript source",
            )],
            Value::Null,
        ),
        true,
        true,
        200,
    );
    assert_eq!(
        text,
        "The @options line must be followed by JavaScript source"
    );
}

#[test]
fn limits_collapsed_output_to_wrapped_lines_not_logical_lines() {
    let text = render(
        result(
            vec![
                text_block("Script completed\nWall time 0.1 seconds\nOutput:\n"),
                text_block(&"x".repeat(1000)),
            ],
            json!({"calls": [], "fullOutputPath": "/tmp/out.txt"}),
        ),
        false,
        false,
        50,
    );
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 7);
    assert_eq!(
        lines[..5],
        [
            "x".repeat(50),
            "x".repeat(50),
            "x".repeat(50),
            "x".repeat(50),
            "x".repeat(50)
        ]
    );
    // 测试进程没有配置键位，`keyHint` 的 dim 段为空，因此逗号后是提示描述前的空格。
    assert_eq!(lines[5], "... (15 more lines,  to expand)");
    assert_eq!(lines[6], "Full output: /tmp/out.txt");
}

#[test]
fn collapsed_code_preview_hint_has_no_extra_closing_parenthesis() {
    let theme = ensure_theme();
    let renderers = codemode_renderers();
    let render_call = renderers.render_call.expect("renderCall");
    let context = base_context(false, false);
    let code = (1..=20)
        .map(|index| format!("let x{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut component = render_call(&json!({"code": code}), &theme, &context);
    let text = strip(&mut component, 50);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 12, "{text}");
    assert_eq!(lines[0], "codemode");
    assert_eq!(lines[11], "... (10 more lines,  to expand)");
}

fn now_millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

#[test]
#[allow(clippy::type_complexity)] // 同上。
fn shows_the_elapsed_time_while_the_script_runs_and_the_total_once_it_finished() {
    // TS 通过 `ToolExecutionComponent` + fake timers 驱动；Rust 侧用渲染状态里的
    // `startedAt` 与最终的 `duration_ms` 模拟同一个时钟。
    let theme = ensure_theme();
    let state: RenderState = Arc::new(Mutex::new(json!({"startedAt": now_millis() - 3000.0})));
    let renderers = codemode_renderers();
    let render_result = renderers.render_result.expect("renderResult");
    let function: &dyn Fn(
        &AgentToolResult<JsonValue>,
        &ToolRenderResultOptions,
        &Theme,
        &ToolRenderContext,
    ) -> Box<dyn Component> = render_result.as_ref();
    let partial_result = result(vec![], json!({"calls": []}));
    let partial_context = ToolRenderContext {
        args: json!({"code": ""}),
        tool_call_id: "call".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::clone(&state),
        cwd: PathBuf::from("/"),
        execution_started: true,
        args_complete: true,
        is_partial: true,
        expanded: false,
        show_images: false,
        is_error: false,
        duration_ms: None,
        output_pad: 1,
    };
    let mut partial = function(
        &partial_result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: true,
        },
        &theme,
        &partial_context,
    );
    let text = assert_once(&mut partial, 120);
    assert!(text.contains("Elapsed 3.0s"), "{text}");

    let final_result = result(vec![], json!({"calls": []}));
    let final_context = ToolRenderContext {
        args: json!({"code": ""}),
        tool_call_id: "call".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::clone(&state),
        cwd: PathBuf::from("/"),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded: false,
        show_images: false,
        is_error: false,
        duration_ms: Some(5100),
        output_pad: 1,
    };
    let mut finished = function(
        &final_result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &theme,
        &final_context,
    );
    let text = assert_once(&mut finished, 120);
    assert!(text.contains("Took 5.1s"), "{text}");
    assert!(!text.contains("Elapsed"), "{text}");
}

fn assert_once(component: &mut Box<dyn Component>, width: usize) -> String {
    let lines = component.render(width);
    kiana_core::ansi::strip_ansi(&lines.join("\n"))
}
