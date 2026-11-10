//! 移植 `test/read-line-range-display.test.ts` 的 `formatReadLineRange` 用例与
//! `test/tool-execution-component.test.ts` 的 fallback 头部 / duration 用例。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{
    ImageContent, ImageKind, JsonValue, TextContent, TextKind, UserContentBlock,
};
use kiana_core::ansi::strip_ansi;
use kiana_ext_api::tool_types::ToolRenderContext;
use kiana_theme::theme::Theme;
use kiana_tools::render_utils::{
    RenderToolPathOptions, format_read_line_range, format_tool_call_with_args,
    format_tool_duration, get_text_output, invalid_arg_text, link_path, normalize_display_text,
    render_tool_path, replace_tabs, shorten_path, start_tool_timer, str, tool_timer_text,
};
use serde_json::json;

fn theme() -> Arc<Theme> {
    kiana_theme::theme::get_theme_by_name("dark").expect("bundled dark theme")
}

#[test]
fn formats_raw_read_arguments() {
    let cases: Vec<(Option<JsonValue>, Option<JsonValue>, &str)> = vec![
        (Some(json!(25)), Some(json!(13)), ":25-37"),
        (Some(json!("25")), Some(json!("13")), ":25-37"),
        (Some(JsonValue::Null), Some(json!("13")), ":1-13"),
        (Some(json!("25")), Some(JsonValue::Null), ":25"),
        (None, None, ""),
        (Some(json!("abc")), Some(json!(13)), ""),
        (Some(json!(25)), Some(json!("")), ""),
        (Some(json!(25)), Some(json!("Infinity")), ""),
    ];
    for (offset, limit, expected) in cases {
        assert_eq!(
            format_read_line_range(offset.as_ref(), limit.as_ref()),
            expected,
            "{offset:?}, {limit:?}"
        );
    }
}

#[test]
fn formats_durations_like_the_tool_execution_component() {
    let cases = [
        (0.0, "0.0s"),
        (4_200.0, "4.2s"),
        // `toFixed(1)` 的半数远离零：250ms -> 0.25 -> "0.3s"（Rust `{:.1}` 会给 "0.2s"）。
        (250.0, "0.3s"),
        (750.0, "0.8s"),
        (1_250.0, "1.3s"),
        (2_250.0, "2.3s"),
        (2_750.0, "2.8s"),
        (1_999.0, "2.0s"),
        (59_900.0, "59.9s"),
        (59_999.0, "60.0s"),
        (60_000.0, "1m 0s"),
        (90_900.0, "1m 30s"),
        (1_592_200.0, "26m 32s"),
        (3_599_999.0, "59m 59s"),
        (3_600_000.0, "1h 0m 0s"),
        (7_384_900.0, "2h 3m 4s"),
        (1_250.0, "1.3s"),
    ];
    for (ms, expected) in cases {
        assert_eq!(format_tool_duration(ms), expected, "{ms}ms");
    }
}

#[test]
fn str_distinguishes_type_errors_from_nullish_values() {
    assert_eq!(str(None), Some(String::new()));
    assert_eq!(str(Some(&JsonValue::Null)), Some(String::new()));
    assert_eq!(str(Some(&json!("hello"))), Some("hello".to_string()));
    assert_eq!(str(Some(&json!(3))), None);
    assert_eq!(str(Some(&json!({"a": 1}))), None);
}

#[test]
fn text_helpers_normalize_tabs_and_carriage_returns() {
    assert_eq!(replace_tabs("a\tb"), "a   b");
    assert_eq!(normalize_display_text("a\r\nb\r"), "a\nb");
    assert_eq!(strip_ansi(&invalid_arg_text(&theme())), "[invalid arg]");
}

#[test]
fn get_text_output_strips_ansi_and_carriage_returns() {
    let result = AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "\u{1b}[31mred\u{1b}[0m\r\nline2".to_string(),
            text_signature: None,
        })],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    assert_eq!(get_text_output(Some(&result), false), "red\nline2");
    assert_eq!(get_text_output(None, false), "");
}

#[test]
fn get_text_output_appends_image_fallbacks_without_image_support() {
    let result = AgentToolResult {
        content: vec![
            UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "hello".to_string(),
                text_signature: None,
            }),
            UserContentBlock::Image(ImageContent {
                kind_image: ImageKind::Image,
                data: String::new(),
                mime_type: "image/png".to_string(),
            }),
        ],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    let output = get_text_output(Some(&result), false);
    // 测试环境的 `getCapabilities().images` 通常为空（无终端），因此追加占位行。
    if kiana_tui::get_capabilities().images.is_none() {
        // 终端不支持图片时，`showImages` 不影响占位行（TS：`!caps.images || !showImages`）。
        assert_eq!(output, "hello\n[Image: [image/png]]");
        assert_eq!(
            get_text_output(Some(&result), true),
            "hello\n[Image: [image/png]]"
        );
    } else {
        assert_eq!(output, "hello");
        assert_eq!(get_text_output(Some(&result), true), "hello");
        assert_eq!(
            get_text_output(Some(&result), false),
            "hello\n[Image: [image/png]]"
        );
    }
}

#[test]
fn fallback_call_header_matches_the_ts_expectations() {
    let theme = theme();
    let long_value = "x".repeat(200);
    let args = json!({ "query": "kiana", "long": long_value, "text": "line one\nline two" });

    let collapsed = strip_ansi(&format_tool_call_with_args(
        "custom_tool",
        Some(&args),
        &theme,
        false,
    ));
    assert!(collapsed.contains("custom_tool query=\"kiana\" long=\"xxx"));
    assert!(collapsed.contains("..."));
    assert!(!collapsed.contains(&long_value));

    let expanded = strip_ansi(&format_tool_call_with_args(
        "custom_tool",
        Some(&args),
        &theme,
        true,
    ));
    assert!(expanded.contains("  query: kiana"));
    assert!(expanded.contains(&long_value));
    let lines: Vec<&str> = expanded.split('\n').collect();
    let text_line = lines
        .iter()
        .position(|line| line.trim_end().ends_with("  text: line one"))
        .expect("text line");
    assert_eq!(lines[text_line + 1], "    line two");

    // 无参数 / null / 空对象都只返回标题。
    assert_eq!(
        strip_ansi(&format_tool_call_with_args("t", None, &theme, false)),
        "t"
    );
    assert_eq!(
        strip_ansi(&format_tool_call_with_args(
            "t",
            Some(&JsonValue::Null),
            &theme,
            false
        )),
        "t"
    );
    assert_eq!(
        strip_ansi(&format_tool_call_with_args(
            "t",
            Some(&json!({})),
            &theme,
            false
        )),
        "t"
    );
    // 数字/嵌套值走 JS `JSON.stringify` 语义（`1e+21`、`1`、`-0 -> 0`）。
    let numbers = json!({"n": 1.5, "big": 1e21, "small": 1e-7, "zero": 1.0, "neg": -0.0});
    let collapsed = strip_ansi(&format_tool_call_with_args(
        "t",
        Some(&numbers),
        &theme,
        false,
    ));
    assert!(
        collapsed.contains("n=1.5 big=1e+21 small=1e-7 zero=1 neg=0"),
        "{collapsed}"
    );
    let nested = json!({"nested": {"x": 1, "y": [1, 2, 3]}});
    let expanded = strip_ansi(&format_tool_call_with_args(
        "t",
        Some(&nested),
        &theme,
        true,
    ));
    assert!(expanded.contains("  nested: {\n      \"x\": 1,\n      \"y\": [\n        1,\n        2,\n        3\n      ]\n    }"), "{expanded}");

    // 非对象参数按 `args=` 呈现。
    assert!(
        strip_ansi(&format_tool_call_with_args(
            "t",
            Some(&json!("v")),
            &theme,
            false
        ))
        .contains("t args=\"v\"")
    );
}

const RENDER_UTILS_GOLDEN: &str = include_str!("fixtures/primitives/render_utils.golden.json");

#[test]
fn fallback_headers_match_the_ts_golden_samples() {
    let theme = theme();
    // 黄金值由 TS `formatToolCallWithArgs` 实测生成（vitest 探针）：覆盖 `Object.entries`
    // 的整数索引键序、展开分支的 `JSON.stringify(value, null, 2)`（JS 数字格式 / 嵌套键序）以及
    // 数组 / 标量参数。
    let cases: Vec<serde_json::Value> = serde_json::from_str(RENDER_UTILS_GOLDEN).expect("fixture");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let title = case["title"].as_str().expect("title");
        let args: JsonValue =
            serde_json::from_str(case["args"].as_str().expect("args")).expect("args json");
        assert_eq!(
            strip_ansi(&format_tool_call_with_args(
                title,
                Some(&args),
                &theme,
                false
            )),
            case["collapsed"].as_str().expect("collapsed"),
            "{name} collapsed"
        );
        assert_eq!(
            strip_ansi(&format_tool_call_with_args(
                title,
                Some(&args),
                &theme,
                true
            )),
            case["expanded"].as_str().expect("expanded"),
            "{name} expanded"
        );
    }
}

#[test]
fn link_path_encodes_urls_like_node_path_to_file_url() {
    // 黄金值来自 Node 26 `pathToFileURL(p).href`（见 TS 探针输出）。
    kiana_tui::terminal_image::set_capabilities(kiana_tui::terminal_image::TerminalCapabilities {
        hyperlinks: true,
        ..kiana_tui::terminal_image::TerminalCapabilities::default()
    });
    let cases = [
        ("/tmp/a~b", "file:///tmp/a%7Eb"),
        ("/tmp/a[b]", "file:///tmp/a%5Bb%5D"),
        ("/tmp/a|b", "file:///tmp/a%7Cb"),
        ("/tmp/a^b", "file:///tmp/a%5Eb"),
        ("/tmp/a b", "file:///tmp/a%20b"),
        ("/tmp/a#b", "file:///tmp/a%23b"),
        ("/tmp/a?b", "file:///tmp/a%3Fb"),
        ("/tmp/a%b", "file:///tmp/a%25b"),
    ];
    for (path, url) in cases {
        let linked = link_path("text", path, "/");
        let expected = format!("\u{1b}]8;;{url}\u{1b}\\text\u{1b}]8;;\u{1b}\\");
        assert_eq!(linked, expected, "{path}");
    }
    kiana_tui::terminal_image::reset_capabilities_cache();
}

#[test]
fn shorten_path_replaces_the_home_prefix() {
    let home = kiana_tui::terminal_image::home_dir().expect("home");
    let home = home.to_string_lossy().into_owned();
    let inside = format!("{home}/projects/file.txt");
    assert_eq!(shorten_path(Some(&json!(inside))), "~/projects/file.txt");
    assert_eq!(shorten_path(Some(&json!("/tmp/file.txt"))), "/tmp/file.txt");
    assert_eq!(shorten_path(Some(&json!(3))), "");
    assert_eq!(shorten_path(None), "");
}

#[test]
fn render_tool_path_handles_null_empty_and_values() {
    let theme = theme();
    assert_eq!(
        strip_ansi(&render_tool_path(None, &theme, "/tmp", None)),
        "[invalid arg]"
    );
    assert_eq!(
        strip_ansi(&render_tool_path(Some(""), &theme, "/tmp", None)),
        "..."
    );
    let options = RenderToolPathOptions {
        empty_fallback: Some("fallback".to_string()),
    };
    assert!(
        strip_ansi(&render_tool_path(Some(""), &theme, "/tmp", Some(&options)))
            .contains("fallback")
    );
    assert!(
        strip_ansi(&render_tool_path(Some("src/main.rs"), &theme, "/tmp", None))
            .contains("src/main.rs")
    );
}

#[test]
fn link_path_is_plain_text_without_hyperlink_capability() {
    // 测试环境通常没有 hyperlink 能力；有的话 URL 也会出现在 OSC 8 载荷里。
    let linked = link_path("text", "/tmp/x", "/tmp");
    if kiana_tui::get_capabilities().hyperlinks {
        assert!(linked.contains("\u{1b}]8;;file://"));
    } else {
        assert_eq!(linked, "text");
    }
}

fn render_context(state: JsonValue, started: bool, is_error: bool) -> ToolRenderContext {
    ToolRenderContext {
        args: JsonValue::Null,
        tool_call_id: "tool-1".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::new(Mutex::new(state)),
        cwd: std::path::PathBuf::from("/tmp"),
        execution_started: started,
        args_complete: true,
        is_partial: false,
        expanded: false,
        show_images: false,
        is_error,
        duration_ms: None,
        output_pad: 1,
    }
}

#[test]
fn tool_timer_starts_on_execution_and_reports_elapsed_then_took() {
    let mut context = render_context(JsonValue::Null, true, false);
    start_tool_timer(&context);
    {
        let state = context.state.lock().expect("lock");
        assert!(state.get("startedAt").is_some());
    }
    let elapsed = tool_timer_text(&context, true).expect("elapsed");
    assert!(elapsed.starts_with("Elapsed "), "{elapsed}");

    context.is_error = false;
    let took = tool_timer_text(&context, false).expect("took");
    assert!(took.starts_with("Took "), "{took}");

    // 无开始时间且没有 durationMs -> 无文本。
    let idle = render_context(JsonValue::Null, false, false);
    assert_eq!(tool_timer_text(&idle, false), None);
}

#[test]
fn tool_timer_invalidates_the_row_every_second_while_partial() {
    // TS: `state.startedAt !== undefined && isPartial && !state.interval` ->
    // `setInterval(() => context.invalidate(), 1000)`；终结/出错时 `clearInterval`。
    let (sender, receiver) = std::sync::mpsc::channel::<()>();
    let mut context = render_context(JsonValue::Null, true, false);
    context.invalidate = Arc::new(move || {
        let _ = sender.send(());
    });

    start_tool_timer(&context);
    assert!(
        tool_timer_text(&context, true)
            .expect("elapsed")
            .starts_with("Elapsed ")
    );

    // 第一次 tick 在 1 秒后；给慢 CI 留足余量。
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("the running tool row must request a redraw every second");

    // 终结后清除 interval：线程退出，不再请求重绘。
    assert!(
        tool_timer_text(&context, false)
            .expect("took")
            .starts_with("Took ")
    );
    // 先等线程观察到标记被移除（最多一个 tick），排空已在途的请求，再确认完全安静。
    std::thread::sleep(Duration::from_millis(1300));
    while receiver.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(1300));
    assert!(
        receiver.try_recv().is_err(),
        "tool timer kept invalidating after the result finished"
    );
}

#[test]
fn tool_timer_clears_the_interval_on_error() {
    let (sender, receiver) = std::sync::mpsc::channel::<()>();
    let mut context = render_context(JsonValue::Null, true, false);
    context.invalidate = Arc::new(move || {
        let _ = sender.send(());
    });

    start_tool_timer(&context);
    let _ = tool_timer_text(&context, true);
    context.is_error = true;
    let text = tool_timer_text(&context, true).expect("elapsed on error");
    assert!(text.starts_with("Elapsed "), "{text}");
    {
        let state = context.state.lock().expect("lock");
        assert!(state.get("interval").is_none());
    }
    // 出错后间隔已清除；即使结果仍标记为 partial 也不再重绘。
    std::thread::sleep(Duration::from_millis(1300));
    while receiver.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(1300));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn recorded_duration_wins_over_the_renderer_clock() {
    let mut context = render_context(json!({"startedAt": 0.0}), false, false);
    context.duration_ms = Some(4_200);
    assert_eq!(
        tool_timer_text(&context, false).as_deref(),
        Some("Took 4.2s")
    );
}
