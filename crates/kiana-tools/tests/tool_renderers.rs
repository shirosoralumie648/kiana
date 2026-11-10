//! 内置工具渲染器的移植测试。
//!
//! TS 参考：`test/tool-execution-component.test.ts`（bash 首个空更新、最终结果
//! truncation footer 去重、耗时文案）、`test/tool-renderer-examples`（grep/find 预览与
//! warning）、以及 `renderers/index.ts` 的 `createAllToolRenderers` /
//! `withBuiltInRenderers` 形状。
//!
//! TS 通过 `ToolExecutionComponent` 驱动；Rust 直接调用 [`ToolRenderers`] 的
//! `render_call` / `render_result`（组件契约相同）。

mod search_shell_test_support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{
    RenderCallFn, ToolRenderContext, ToolRenderResultOptions, ToolRenderers,
};
use kiana_theme::theme::Theme;
use kiana_tools::bash::BashToolOptions;
use kiana_tools::index::ToolName;
use kiana_tools::renderers::{
    create_all_tool_renderers, create_shell_renderers, find_renderers, grep_renderers,
    with_built_in_renderers,
};
use kiana_tui::text::strip_terminal_sequences;
use kiana_tui::tui::Component;
use serde_json::json;
use tempfile::TempDir;

use search_shell_test_support::text_block;

fn theme() -> Arc<Theme> {
    let theme = kiana_theme::get_theme_by_name("dark").expect("builtin dark theme");
    kiana_theme::set_theme_instance(Arc::clone(&theme));
    theme
}

fn context(
    args: JsonValue,
    execution_started: bool,
    is_partial: bool,
    expanded: bool,
) -> ToolRenderContext {
    ToolRenderContext {
        args,
        tool_call_id: "tool-call".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::new(Mutex::new(json!({}))),
        cwd: PathBuf::from("/workspace"),
        execution_started,
        args_complete: true,
        is_partial,
        expanded,
        show_images: false,
        is_error: false,
        duration_ms: None,
        output_pad: 0,
    }
}

fn render(mut component: Box<dyn Component>, width: usize) -> String {
    component
        .render(width)
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_call(renderers: &ToolRenderers, args: &JsonValue, ctx: &ToolRenderContext) -> String {
    let render_call = renderers.render_call.as_ref().expect("render_call");
    render(render_call(args, &theme(), ctx), 200)
}

fn render_result(
    renderers: &ToolRenderers,
    result: &AgentToolResult<JsonValue>,
    options: &ToolRenderResultOptions,
    ctx: &ToolRenderContext,
) -> String {
    let render_result = renderers.render_result.as_ref().expect("render_result");
    render(render_result(result, options, &theme(), ctx), 200)
}

fn result_with_text(text: &str) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![text_block(text)],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn truncation_details() -> JsonValue {
    json!({
        "truncation": {
            "content": "<content omitted>",
            "truncated": true,
            "truncatedBy": "lines",
            "totalLines": 4000,
            "totalBytes": 40000,
            "outputLines": 2000,
            "outputBytes": 19999,
            "lastLinePartial": false,
            "firstLineExceedsLimit": false,
            "maxLines": 2000,
            "maxBytes": 51200
        },
        "fullOutputPath": "/tmp/kiana-bash-abcd.log"
    })
}

#[test]
fn bash_call_renders_prompt_command_and_timeout() {
    let renderers = create_shell_renderers("$");
    let ctx = context(
        json!({ "command": "echo hi", "timeout": 5 }),
        false,
        false,
        false,
    );
    let rendered = render_call(
        &renderers,
        &json!({ "command": "echo hi", "timeout": 5 }),
        &ctx,
    );
    assert!(rendered.contains("$ echo hi"), "rendered: {rendered}");
    assert!(rendered.contains("(timeout 5s)"), "rendered: {rendered}");

    let empty = render_call(&renderers, &json!({ "command": "" }), &ctx);
    assert!(empty.contains("$ ..."), "rendered: {empty}");
}

#[test]
fn powershell_renderer_uses_its_prompt() {
    let renderers = create_shell_renderers("PS>");
    let ctx = context(json!({ "command": "Get-ChildItem" }), false, false, false);
    let rendered = render_call(&renderers, &json!({ "command": "Get-ChildItem" }), &ctx);
    assert!(
        rendered.contains("PS> Get-ChildItem"),
        "rendered: {rendered}"
    );
}

#[test]
fn bash_renderer_does_not_duplicate_final_full_output_truncation_details() {
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "generate output" }), false, false, true);
    let text = "line-2001\nline-4000\n\n[Showing lines 2001-4000 of 4000. Full output: /tmp/kiana-bash-abcd.log]";
    let mut result = result_with_text(text);
    result.details = truncation_details();
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: true,
            is_partial: false,
        },
        &ctx,
    );
    assert_eq!(
        rendered.matches("Full output:").count(),
        1,
        "rendered: {rendered}"
    );
    assert!(rendered.contains("Truncated: showing 2000 of 4000 lines"));
    assert!(!rendered.contains("[Showing lines 2001-4000 of 4000. Full output:"));
}

#[test]
fn bash_collapsed_preview_keeps_the_last_five_visual_lines_and_expand_hint() {
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "many" }), false, false, false);
    let text = (1..=10)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let rendered = render_result(
        &renderers,
        &result_with_text(&text),
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &ctx,
    );
    assert!(rendered.contains("line-10"), "rendered: {rendered}");
    assert!(rendered.contains("5 earlier lines"), "rendered: {rendered}");
    assert!(rendered.contains("to expand"), "rendered: {rendered}");
}

#[test]
fn bash_renderer_formats_elapsed_and_took_from_the_timer_state() {
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "sleep 4" }), true, true, false);
    render_call(&renderers, &json!({ "command": "sleep 4" }), &ctx);
    let running = render_result(
        &renderers,
        &result_with_text("partial"),
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: true,
        },
        &ctx,
    );
    assert!(running.contains("Elapsed "), "rendered: {running}");

    let mut completed_ctx = context(json!({ "command": "sleep 4" }), true, false, false);
    completed_ctx.duration_ms = Some(4_200);
    let completed = render_result(
        &renderers,
        &result_with_text("done"),
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &completed_ctx,
    );
    assert!(completed.contains("Took 4.2s"), "rendered: {completed}");
}

#[test]
fn bash_timer_formats_long_durations_like_ts() {
    for (ms, formatted) in [
        (0u64, "0.0s"),
        (4_200, "4.2s"),
        (59_999, "60.0s"),
        (60_000, "1m 0s"),
        (90_900, "1m 30s"),
        (1_592_200, "26m 32s"),
        (3_600_000, "1h 0m 0s"),
        (7_384_900, "2h 3m 4s"),
    ] {
        let renderers = create_shell_renderers("$");
        let mut ctx = context(json!({ "command": "x" }), true, false, false);
        ctx.duration_ms = Some(ms);
        let rendered = render_result(
            &renderers,
            &result_with_text("done"),
            &ToolRenderResultOptions {
                expanded: false,
                is_partial: false,
            },
            &ctx,
        );
        assert!(
            rendered.contains(&format!("Took {formatted}")),
            "{ms}ms rendered: {rendered}"
        );
    }
}

#[test]
fn grep_call_and_result_preview_match_ts() {
    let renderers = grep_renderers();
    let ctx = context(
        json!({ "pattern": "match", "path": "file.txt", "glob": "*.ts", "limit": 1 }),
        false,
        false,
        false,
    );
    let rendered = render_call(
        &renderers,
        &json!({ "pattern": "match", "path": "file.txt", "glob": "*.ts", "limit": 1 }),
        &ctx,
    );
    assert!(
        rendered.contains("grep /match/ in file.txt"),
        "rendered: {rendered}"
    );
    assert!(rendered.contains("(*.ts)"), "rendered: {rendered}");
    assert!(rendered.contains("limit 1"), "rendered: {rendered}");

    let text = (1..=20)
        .map(|index| format!("file.txt:{index}: match"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut result = result_with_text(&text);
    result.details = json!({
        "matchLimitReached": 1,
        "linesTruncated": true,
        "truncation": {
            "content": "",
            "truncated": true,
            "truncatedBy": "bytes",
            "totalLines": 20,
            "totalBytes": 60000,
            "outputLines": 10,
            "outputBytes": 51200,
            "lastLinePartial": false,
            "firstLineExceedsLimit": false,
            "maxLines": 2000,
            "maxBytes": 51200
        }
    });
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &ctx,
    );
    assert!(rendered.contains("file.txt:1: match"));
    assert!(
        rendered.contains("... (5 more lines"),
        "rendered: {rendered}"
    );
    assert!(rendered.contains("to expand"));
    assert!(
        rendered.contains("[Truncated: 1 matches limit, 50.0KB limit, some lines truncated]"),
        "rendered: {rendered}"
    );
}

#[test]
fn grep_expanded_result_shows_every_line() {
    let renderers = grep_renderers();
    let ctx = context(json!({ "pattern": "x" }), false, false, true);
    let text = (1..=20)
        .map(|index| format!("file.txt:{index}: match"))
        .collect::<Vec<_>>()
        .join("\n");
    let rendered = render_result(
        &renderers,
        &result_with_text(&text),
        &ToolRenderResultOptions {
            expanded: true,
            is_partial: false,
        },
        &ctx,
    );
    assert!(rendered.contains("file.txt:20: match"));
    assert!(!rendered.contains("more lines"));
}

#[test]
fn grep_call_with_non_string_pattern_uses_invalid_arg() {
    let renderers = grep_renderers();
    let ctx = context(json!({ "pattern": 42 }), false, false, false);
    let rendered = render_call(&renderers, &json!({ "pattern": 42 }), &ctx);
    assert!(rendered.contains("[invalid arg]"), "rendered: {rendered}");

    // 缺失的 pattern 在 JS 里是 `undefined`，`str()` 返回空串（不是 invalid arg）。
    let ctx = context(json!({}), false, false, false);
    let rendered = render_call(&renderers, &json!({}), &ctx);
    assert!(rendered.contains("grep // in ."), "rendered: {rendered}");
}

#[test]
fn find_call_and_result_preview_match_ts() {
    let renderers = find_renderers();
    let ctx = context(
        json!({ "pattern": "*.ts", "path": "src", "limit": 5 }),
        false,
        false,
        false,
    );
    let rendered = render_call(
        &renderers,
        &json!({ "pattern": "*.ts", "path": "src", "limit": 5 }),
        &ctx,
    );
    assert!(
        rendered.contains("find *.ts in src"),
        "rendered: {rendered}"
    );
    assert!(rendered.contains("(limit 5)"), "rendered: {rendered}");

    let text = (1..=25)
        .map(|index| format!("file-{index}.ts"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut result = result_with_text(&text);
    result.details = json!({ "resultLimitReached": 2 });
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &ctx,
    );
    assert!(rendered.contains("file-20.ts"));
    assert!(!rendered.contains("file-21.ts"));
    assert!(
        rendered.contains("... (5 more lines"),
        "rendered: {rendered}"
    );
    assert!(
        rendered.contains("[Truncated: 2 results limit]"),
        "rendered: {rendered}"
    );
}

#[test]
fn create_all_tool_renderers_has_every_builtin_in_order() {
    let renderers = create_all_tool_renderers();
    let names: Vec<&str> = renderers.keys().map(|name| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "read",
            "bash",
            "powershell",
            "edit",
            "write",
            "grep",
            "find",
            "ls"
        ]
    );
    for (name, entry) in &renderers {
        assert!(entry.render_call.is_some(), "{name}");
        assert!(entry.render_result.is_some(), "{name}");
    }
    for name in [
        ToolName::Bash,
        ToolName::PowerShell,
        ToolName::Grep,
        ToolName::Find,
    ] {
        assert_eq!(renderers[&name].render_shell, None, "{name}");
    }
}

#[test]
fn with_built_in_renderers_falls_back_and_merges() {
    // 未知工具 + 无定义 → 没有可回退的渲染器。
    assert!(with_built_in_renderers("unknown-tool", None).is_none());

    // 已知工具 + 无定义 → 内置渲染器。
    let bash = with_built_in_renderers("bash", None).expect("bash renderers");
    assert!(bash.render_call.is_some());
    assert!(bash.render_result.is_some());

    // 已知工具 + 只有自定义 call → 保留自定义 call，继承内置 result。
    let custom: RenderCallFn = Arc::new(|_, _, _| {
        Box::new(kiana_tui::components::text_component::Text::with_padding(
            "custom-call",
            0,
            0,
        ))
    });
    let merged = with_built_in_renderers(
        "grep",
        Some(ToolRenderers {
            render_shell: None,
            render_call: Some(Arc::clone(&custom)),
            render_result: None,
        }),
    )
    .expect("merged");
    assert!(Arc::ptr_eq(merged.render_call.as_ref().unwrap(), &custom));
    assert!(merged.render_result.is_some());

    // 未知工具 + 自定义定义 → 原样返回（不会被内置覆盖）。
    let custom_result = with_built_in_renderers(
        "unknown-tool",
        Some(ToolRenderers {
            render_shell: None,
            render_call: Some(Arc::clone(&custom)),
            render_result: None,
        }),
    )
    .expect("custom");
    assert!(Arc::ptr_eq(
        custom_result.render_call.as_ref().unwrap(),
        &custom
    ));
    assert!(custom_result.render_result.is_none());
}

#[test]
fn bash_renderer_state_is_shared_between_call_and_result() {
    // 与 TS 的 `context.state` 相同：render_call 写入 startedAt，render_result 读取它。
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "true" }), true, true, false);
    render_call(&renderers, &json!({ "command": "true" }), &ctx);
    let state = ctx.state.lock().unwrap().clone();
    assert!(state.get("startedAt").is_some(), "state: {state}");
}

#[test]
fn bash_renderer_hides_the_full_output_footer_for_partial_results() {
    // 流式结果保留脚注（脚注只有在最终结果才折叠成 warning）。
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "x" }), false, true, true);
    let text = "line-1\n\n[Showing lines 2001-4000 of 4000. Full output: /tmp/kiana-bash-abcd.log]";
    let mut result = result_with_text(text);
    result.details = truncation_details();
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: true,
            is_partial: true,
        },
        &ctx,
    );
    assert_eq!(
        rendered.matches("Full output:").count(),
        2,
        "rendered: {rendered}"
    );
}

#[test]
fn shell_tool_definition_exposes_renderers() {
    let temp = TempDir::new().expect("temp");
    let definition = kiana_tools::bash::create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    assert!(definition.render_call.is_some());
    assert!(definition.render_result.is_some());
}

#[test]
fn bash_render_update_callback_never_panics_on_empty_partial_output() {
    // `agent-session-dynamic-tools` / `8611` 的渲染侧契约：partial 更新只带 content，
    // details=null 时渲染器仍输出计时行而不是 panic。
    let result = AgentToolResult {
        content: Vec::new(),
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "x" }), true, true, false);
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: true,
        },
        &ctx,
    );
    assert!(!rendered.contains("panic"), "rendered: {rendered}");
}

// ===========================================================================
// 宽松 details（TS 属性访问语义）
// ===========================================================================

#[test]
fn bash_renderer_tolerates_partial_details_like_ts() {
    // 回归：details 来自旧版本 kiana / 扩展时可能缺少 lastLinePartial、
    // firstLineExceedsLimit、maxBytes 等字段。TS 逐个属性读取（`??` 回落默认上限），
    // 仍然去掉与警告行重复的 `Full output:` 脚注并渲染警告；整体反序列化会把这些全丢掉。
    let renderers = create_shell_renderers("$");
    let ctx = context(json!({ "command": "generate output" }), false, false, true);
    let text = "line-1\nline-2000\n\n[Showing lines 1-2000 of 4000. Full output: /tmp/kiana-bash-abcd.log]";
    let mut result = result_with_text(text);
    result.details = json!({
        "truncation": {
            // 缺 content/lastLinePartial/firstLineExceedsLimit/maxBytes。
            "truncated": true,
            "truncatedBy": "bytes",
            "totalLines": 4000,
            "outputLines": 2000
        },
        "fullOutputPath": "/tmp/kiana-bash-abcd.log"
    });
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: true,
            is_partial: false,
        },
        &ctx,
    );
    assert_eq!(
        rendered.matches("Full output:").count(),
        1,
        "rendered: {rendered}"
    );
    assert!(
        rendered.contains("Truncated: 2000 lines shown (50.0KB limit)"),
        "rendered: {rendered}"
    );
    assert!(!rendered.contains("undefined"), "rendered: {rendered}");
}

#[test]
fn find_renderer_tolerates_partial_details_like_ts() {
    let renderers = find_renderers();
    let ctx = context(json!({ "pattern": "x" }), false, false, false);
    let mut result = result_with_text("a\nb");
    result.details = json!({
        "resultLimitReached": 100,
        // 缺 maxBytes -> DEFAULT_MAX_BYTES。
        "truncation": { "truncated": true }
    });
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &ctx,
    );
    assert!(
        rendered.contains("[Truncated: 100 results limit, 50.0KB limit]"),
        "rendered: {rendered}"
    );
}

#[test]
fn grep_renderer_tolerates_partial_details_like_ts() {
    let renderers = grep_renderers();
    let ctx = context(json!({ "pattern": "x" }), false, false, false);
    let mut result = result_with_text("a\nb");
    result.details = json!({
        "matchLimitReached": 5,
        "linesTruncated": true,
        "truncation": { "truncated": true }
    });
    let rendered = render_result(
        &renderers,
        &result,
        &ToolRenderResultOptions {
            expanded: false,
            is_partial: false,
        },
        &ctx,
    );
    assert!(
        rendered.contains("[Truncated: 5 matches limit, 50.0KB limit, some lines truncated]"),
        "rendered: {rendered}"
    );
}
