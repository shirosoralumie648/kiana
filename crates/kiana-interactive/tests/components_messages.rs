//! 消息组件的 TS 对拍测试。
//!
//! 移植自：
//! - `packages/coding-agent/test/assistant-message.test.ts`
//! - `packages/coding-agent/test/user-message.test.ts`
//! - `packages/coding-agent/test/collapsible-message-components.test.ts`
//! - `packages/coding-agent/test/custom-message.test.ts`

mod common;

use std::sync::{Arc, Mutex};

use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonObject, StopReason, TextContent,
    TextKind, ThinkingContent, ThinkingKind, ToolCall, ToolCallKind, Usage,
};
use kiana_core::ansi::strip_ansi;
use kiana_session_core::messages::{
    BranchSummaryMessage, BranchSummaryRole, CompactionSummaryMessage, CompactionSummaryRole,
    CustomMessage, CustomRole,
};
use kiana_session_core::session_manager::{CustomEntry, CustomEntryType};
use kiana_theme::theme::get_markdown_theme;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

use kiana_interactive::components::assistant_message::AssistantMessageComponent;
use kiana_interactive::components::branch_summary_message::BranchSummaryMessageComponent;
use kiana_interactive::components::compaction_summary_message::CompactionSummaryMessageComponent;
use kiana_interactive::components::custom_entry::CustomEntryComponent;
use kiana_interactive::components::custom_message::CustomMessageComponent;
use kiana_interactive::components::skill_invocation_message::SkillInvocationMessageComponent;
use kiana_interactive::components::user_message::UserMessageComponent;

const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";
const BG_RESET: &str = "\x1b[49m";

fn init_dark() {
    kiana_theme::init_theme(Some("dark"), false);
}

fn text(value: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: value.to_string(),
        text_signature: None,
    })
}

fn thinking(value: &str) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(ThinkingContent {
        kind_thinking: ThinkingKind::Thinking,
        thinking: value.to_string(),
        thinking_signature: None,
        redacted: None,
    })
}

fn tool_call(id: &str, name: &str) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: JsonObject::new(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })
}

fn assistant_message(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
    error_message: Option<&str>,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "gpt-4o-mini".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        deferred: None,
        error_message: error_message.map(str::to_string),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

fn assistant(content: Vec<AssistantContentBlock>) -> AssistantMessage {
    assistant_message(content, StopReason::Stop, None)
}

fn click_event(y: isize, width: usize, height: usize) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x: 1,
        y,
        screen_x: 1,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: Some(1),
    }
}

fn golden() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/message_renders_dark.json"))
        .expect("golden fixture is valid JSON")
}

fn golden_lines(key: &str) -> Vec<String> {
    golden()[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing golden key {key}"))
        .iter()
        .map(|value| value.as_str().expect("golden line is a string").to_string())
        .collect()
}

fn assert_golden(key: &str, actual: Vec<String>) {
    assert_eq!(actual, golden_lines(key), "golden mismatch for {key}");
}

#[test]
fn adds_osc_133_zone_markers_to_assistant_messages_without_tool_calls() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![text("hello")])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let lines = component.render(40);
    assert!(!lines.is_empty());
    assert!(lines[0].contains(OSC133_ZONE_START));
    assert!(lines[lines.len() - 1].starts_with(&format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}")));
    assert_golden("assistant_text", lines);
}

#[test]
fn does_not_add_osc_133_zone_markers_when_assistant_message_contains_tool_calls() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![
            text("calling tool"),
            tool_call("tool-1", "read"),
        ])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let rendered = component.render(60).join("\n");
    assert!(!rendered.contains(OSC133_ZONE_START));
    assert!(!rendered.contains(OSC133_ZONE_END));
    assert!(!rendered.contains(OSC133_ZONE_FINAL));
    assert_golden("assistant_tool", component.render(60));
}

#[test]
fn renders_length_stops_with_neutral_truncation_wording() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant_message(
            vec![thinking("private reasoning")],
            StopReason::Length,
            None,
        )),
        true,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let rendered = component.render(80).join("\n");
    assert!(rendered.contains("Thinking..."));
    assert!(rendered.contains("Response was truncated before completion."));
    assert_golden("assistant_length", component.render(80));
}

#[test]
fn renders_aborted_and_error_stops() {
    init_dark();
    for (key, message, expected) in [
        (
            "assistant_aborted",
            assistant_message(vec![text("partial")], StopReason::Aborted, None),
            "Operation aborted",
        ),
        (
            "assistant_aborted_custom",
            assistant_message(
                vec![text("partial")],
                StopReason::Aborted,
                Some("Stop requested"),
            ),
            "Stop requested",
        ),
        (
            "assistant_error",
            assistant_message(vec![text("partial")], StopReason::Error, None),
            "Error: Unknown error",
        ),
        (
            "assistant_error_msg",
            assistant_message(vec![text("partial")], StopReason::Error, Some("boom")),
            "Error: boom",
        ),
    ] {
        let mut component = AssistantMessageComponent::new(
            Some(message),
            false,
            get_markdown_theme(),
            "Thinking...",
            1,
            Vec::new(),
            None,
            None,
        );
        let lines = component.render(80);
        assert!(
            strip_ansi(&lines.join("\n")).contains(expected),
            "{key}: {expected}"
        );
        assert_golden(key, lines);
    }
}

#[test]
fn coalesces_adjacent_thinking_blocks_into_one_hidden_thinking_label() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![
            thinking("first thought"),
            thinking(""),
            thinking("second thought"),
            text("answer"),
        ])),
        true,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let rendered = strip_ansi(&component.render(80).join("\n"));
    assert_eq!(rendered.matches("Thinking...").count(), 1);
    assert!(rendered.contains("answer"));
    assert_golden("assistant_hidden_thinking", component.render(80));
}

#[test]
fn renders_visible_thinking_with_theme_style() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![thinking("reasoning"), text("answer")])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let lines = component.render(80);
    assert!(strip_ansi(&lines.join("\n")).contains("reasoning"));
    assert_golden("assistant_visible_thinking", lines);
}

#[test]
fn collapses_individual_thinking_runs_when_clicked() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![
            thinking("first reasoning"),
            text("answer"),
            thinking("second reasoning"),
        ])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let width = 80;
    let lines = component.render(width);
    let first_thinking_row = lines
        .iter()
        .position(|line| strip_ansi(line).contains("first reasoning"))
        .expect("first thinking run is visible");
    let event = click_event(first_thinking_row as isize, width, lines.len());
    assert_eq!(
        component
            .handle_mouse(&event)
            .and_then(|result| result.handled),
        Some(true)
    );

    let collapsed = strip_ansi(&component.render(width).join("\n"));
    assert!(!collapsed.contains("first reasoning"));
    assert!(collapsed.contains("Thinking..."));
    assert!(collapsed.contains("second reasoning"));
}

#[test]
fn uses_configured_output_padding_for_text_and_thinking() {
    init_dark();
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![text("hello"), thinking("reasoning")])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        Vec::new(),
        None,
        None,
    );
    let lines: Vec<String> = component
        .render(80)
        .iter()
        .map(|line| strip_ansi(line))
        .collect();
    assert!(lines.iter().any(|line| line.contains(" hello")));
    assert!(lines.iter().any(|line| line.contains(" reasoning")));

    component.set_output_pad(0);
    let updated: Vec<String> = component
        .render(80)
        .iter()
        .map(|line| strip_ansi(line))
        .collect();
    assert!(updated.iter().any(|line| line.starts_with("hello")));
    assert!(updated.iter().any(|line| line.starts_with("reasoning")));
}

#[test]
fn chains_markdown_transformers_in_registration_order() {
    init_dark();
    let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let calls_a = Arc::clone(&calls);
    let calls_b = Arc::clone(&calls);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> = vec![
        Arc::new(move |markdown: &str, context| {
            calls_a.lock().unwrap().push("formula");
            assert_eq!(
                context,
                &kiana_ext_api::types::MarkdownTransformContext {
                    message_type: kiana_ext_api::types::MarkdownMessageType::Assistant,
                    is_streaming: false,
                    available_width: 78,
                    message_id: None,
                    timestamp: None,
                }
            );
            markdown.replace("$x^2$", "x²")
        }),
        Arc::new(move |markdown: &str, _| {
            calls_b.lock().unwrap().push("suffix");
            format!("{markdown} Done.")
        }),
    ];
    let message = assistant(vec![text("The result is $x^2$.")]);
    let mut component = AssistantMessageComponent::new(
        Some(message),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        transformers,
        None,
        None,
    );
    assert!(strip_ansi(&component.render(80).join("\n")).contains("The result is x². Done."));
    assert_eq!(*calls.lock().unwrap(), vec!["formula", "suffix"]);
}

#[test]
fn identifies_partial_assistant_markdown_as_streaming() {
    init_dark();
    let streaming_states = Arc::new(Mutex::new(Vec::<bool>::new()));
    let states = Arc::clone(&streaming_states);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> =
        vec![Arc::new(move |markdown: &str, context| {
            states.lock().unwrap().push(context.is_streaming);
            if context.is_streaming {
                markdown.to_string()
            } else {
                format!("{markdown} transformed")
            }
        })];
    let message = assistant(vec![text("partial")]);
    let mut component = AssistantMessageComponent::new(
        None,
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        transformers,
        None,
        None,
    );
    component.update_content_streaming(&message, true);
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("transformed"));

    component.update_content_streaming(&message, false);
    assert!(strip_ansi(&component.render(80).join("\n")).contains("partial transformed"));
    assert_eq!(*streaming_states.lock().unwrap(), vec![true, false]);
}

#[test]
fn reapplies_markdown_transformers_when_available_width_changes() {
    init_dark();
    let available_widths = Arc::new(Mutex::new(Vec::<usize>::new()));
    let widths = Arc::clone(&available_widths);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> =
        vec![Arc::new(move |markdown: &str, context| {
            widths.lock().unwrap().push(context.available_width);
            format!("{markdown} ({})", context.available_width)
        })];
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![text("answer")])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        transformers,
        None,
        None,
    );
    assert!(strip_ansi(&component.render(80).join("\n")).contains("answer (78)"));
    component.render(80);
    assert!(strip_ansi(&component.render(60).join("\n")).contains("answer (58)"));
    assert_eq!(*available_widths.lock().unwrap(), vec![78, 58]);
}

#[test]
fn continues_the_markdown_transformer_chain_when_a_transformer_throws() {
    init_dark();
    let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let calls_a = Arc::clone(&calls);
    let calls_b = Arc::clone(&calls);
    let calls_c = Arc::clone(&calls);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> = vec![
        Arc::new(move |markdown: &str, _| {
            calls_a.lock().unwrap().push("first");
            markdown.replace("still", "remains")
        }),
        Arc::new(move |_: &str, _| {
            calls_b.lock().unwrap().push("throw");
            panic!("broken transformer");
        }),
        Arc::new(move |markdown: &str, _| {
            calls_c.lock().unwrap().push("last");
            format!("{markdown} after error")
        }),
    ];
    let mut component = AssistantMessageComponent::new(
        Some(assistant(vec![text("still visible")])),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        transformers,
        None,
        None,
    );
    assert!(strip_ansi(&component.render(80).join("\n")).contains("remains visible after error"));
    assert_eq!(*calls.lock().unwrap(), vec!["first", "throw", "last"]);
}

#[test]
fn transforms_text_and_thinking_markdown_without_mutating_the_original_message() {
    init_dark();
    let message = assistant(vec![text("answer"), thinking("reasoning")]);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> =
        vec![Arc::new(move |markdown: &str, context| {
            format!("{:?}:{markdown}", context.message_type)
        })];
    let mut component = AssistantMessageComponent::new(
        Some(message.clone()),
        false,
        get_markdown_theme(),
        "Thinking...",
        1,
        transformers,
        None,
        None,
    );
    let rendered = strip_ansi(&component.render(80).join("\n"));
    assert!(rendered.contains("Assistant:answer"));
    assert!(rendered.contains("AssistantThinking:reasoning"));
    assert_eq!(message.content.len(), 2);
}

#[test]
fn user_message_keeps_height_stable_while_moving_closing_osc_markers_off_line_end() {
    init_dark();
    let mut component =
        UserMessageComponent::new("hello", get_markdown_theme(), 1, Vec::new(), None, None);
    let lines = component.render(20);
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains(OSC133_ZONE_START));
    assert!(lines[0].ends_with(BG_RESET));
    assert!(!lines[0].contains(OSC133_ZONE_END));
    assert!(lines[1].contains("hello"));
    assert!(lines[2].starts_with(&format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}")));
    assert!(lines[2].ends_with(BG_RESET));
    assert_golden("user", lines);
}

#[test]
fn user_message_chains_transformers_with_user_context() {
    init_dark();
    let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let calls_a = Arc::clone(&calls);
    let calls_b = Arc::clone(&calls);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> = vec![
        Arc::new(move |markdown: &str, context| {
            calls_a.lock().unwrap().push("formula");
            assert_eq!(
                context,
                &kiana_ext_api::types::MarkdownTransformContext {
                    message_type: kiana_ext_api::types::MarkdownMessageType::User,
                    is_streaming: false,
                    available_width: 78,
                    message_id: None,
                    timestamp: None,
                }
            );
            markdown.replace("$x^2$", "x²")
        }),
        Arc::new(move |markdown: &str, _| {
            calls_b.lock().unwrap().push("suffix");
            format!("{markdown} Done.")
        }),
    ];
    let mut component = UserMessageComponent::new(
        "The input is $x^2$.",
        get_markdown_theme(),
        1,
        transformers,
        None,
        None,
    );
    assert!(strip_ansi(&component.render(80).join("\n")).contains("The input is x². Done."));
    assert_eq!(*calls.lock().unwrap(), vec!["formula", "suffix"]);
}

#[test]
fn user_message_reapplies_transformers_when_invalidated() {
    init_dark();
    let suffix = Arc::new(Mutex::new("before"));
    let suffix_for_transformer = Arc::clone(&suffix);
    let transformers: Vec<kiana_ext_api::types::MarkdownTransformer> =
        vec![Arc::new(move |markdown: &str, _| {
            format!("{markdown} {}", suffix_for_transformer.lock().unwrap())
        })];
    let mut component =
        UserMessageComponent::new("Message", get_markdown_theme(), 1, transformers, None, None);
    assert!(strip_ansi(&component.render(80).join("\n")).contains("Message before"));

    *suffix.lock().unwrap() = "after";
    component.invalidate();
    assert!(strip_ansi(&component.render(80).join("\n")).contains("Message after"));
}

#[test]
fn user_message_passes_persisted_entry_identity_and_live_leaves_it_undefined() {
    init_dark();
    let captured = Arc::new(Mutex::new(Vec::<(Option<String>, Option<String>)>::new()));
    let slot = Arc::clone(&captured);
    let capture: kiana_ext_api::types::MarkdownTransformer =
        Arc::new(move |markdown: &str, context| {
            slot.lock()
                .unwrap()
                .push((context.message_id.clone(), context.timestamp.clone()));
            markdown.to_string()
        });
    let mut restored = UserMessageComponent::new(
        "restored",
        get_markdown_theme(),
        1,
        vec![Arc::clone(&capture)],
        Some("entry-abc".to_string()),
        Some("2025-01-15T10:30:00.000Z".to_string()),
    );
    restored.render(80);
    let mut live =
        UserMessageComponent::new("live", get_markdown_theme(), 1, vec![capture], None, None);
    live.render(80);
    assert_eq!(
        *captured.lock().unwrap(),
        vec![
            (
                Some("entry-abc".to_string()),
                Some("2025-01-15T10:30:00.000Z".to_string())
            ),
            (None, None),
        ]
    );
}

#[test]
fn uses_configured_output_padding_for_user_messages() {
    init_dark();
    let mut padded =
        UserMessageComponent::new("hello", get_markdown_theme(), 1, Vec::new(), None, None);
    assert!(
        padded
            .render(40)
            .iter()
            .map(|line| strip_ansi(line))
            .any(|line| line.starts_with(" hello"))
    );
    let mut unpadded =
        UserMessageComponent::new("hello", get_markdown_theme(), 0, Vec::new(), None, None);
    let lines: Vec<String> = unpadded
        .render(40)
        .iter()
        .map(|line| strip_ansi(line))
        .collect();
    assert!(lines.iter().any(|line| line.starts_with("hello")));
    unpadded.set_output_pad(0);
    assert!(
        unpadded
            .render(40)
            .iter()
            .map(|line| strip_ansi(line))
            .any(|line| line.starts_with("hello"))
    );
    assert_golden("user_pad0", unpadded.render(20));
}

fn toggle_by_click(component: &mut dyn Component, marker: &str) -> bool {
    let width = 80;
    let lines = component.render(width);
    let Some(row) = lines
        .iter()
        .position(|line| strip_ansi(line).contains(marker))
    else {
        return false;
    };
    let event = click_event(row as isize, width, lines.len());
    component
        .handle_mouse(&event)
        .and_then(|result| result.handled)
        .unwrap_or(false)
}

#[test]
fn toggles_a_compaction_summary_when_clicked() {
    init_dark();
    let mut component = CompactionSummaryMessageComponent::new(
        CompactionSummaryMessage {
            role: CompactionSummaryRole::CompactionSummary,
            summary: "compaction details".to_string(),
            tokens_before: 1234.0,
            timestamp: 0.0,
        },
        get_markdown_theme(),
        1,
    );
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("compaction details"));
    assert!(toggle_by_click(&mut component, "[compaction]"));
    assert!(strip_ansi(&component.render(80).join("\n")).contains("compaction details"));
    assert!(toggle_by_click(&mut component, "[compaction]"));
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("compaction details"));
    assert_golden("compaction_expanded", {
        component.set_expanded(true);
        component.render(80)
    });
    let mut collapsed = CompactionSummaryMessageComponent::new(
        CompactionSummaryMessage {
            role: CompactionSummaryRole::CompactionSummary,
            summary: "compaction details".to_string(),
            tokens_before: 1234.0,
            timestamp: 0.0,
        },
        get_markdown_theme(),
        1,
    );
    assert_golden("compaction", collapsed.render(80));
}

#[test]
fn toggles_a_branch_summary_when_clicked() {
    init_dark();
    let mut component = BranchSummaryMessageComponent::new(
        BranchSummaryMessage {
            role: BranchSummaryRole::BranchSummary,
            summary: "branch details".to_string(),
            from_id: Some("entry-1".to_string()),
            timestamp: 0.0,
        },
        get_markdown_theme(),
        1,
    );
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("branch details"));
    assert!(toggle_by_click(&mut component, "[branch]"));
    assert!(strip_ansi(&component.render(80).join("\n")).contains("branch details"));
    assert!(toggle_by_click(&mut component, "[branch]"));
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("branch details"));
    assert_golden("branch", component.render(80));
    component.set_expanded(true);
    assert_golden("branch_expanded", component.render(80));
}

#[test]
fn toggles_a_skill_invocation_when_clicked() {
    init_dark();
    let block = kiana_session::agent_session::ParsedSkillBlock {
        name: "example-skill".to_string(),
        location: "/tmp/example-skill.md".to_string(),
        content: "skill details".to_string(),
        user_message: None,
    };
    let mut component = SkillInvocationMessageComponent::new(block, get_markdown_theme(), 1);
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("skill details"));
    assert!(toggle_by_click(&mut component, "[skill]"));
    assert!(strip_ansi(&component.render(80).join("\n")).contains("skill details"));
    assert!(toggle_by_click(&mut component, "[skill]"));
    assert!(!strip_ansi(&component.render(80).join("\n")).contains("skill details"));
    assert_golden("skill", component.render(80));
    component.set_expanded(true);
    assert_golden("skill_expanded", component.render(80));
}

#[test]
fn compaction_summary_formats_tokens_as_locale_string() {
    use kiana_interactive::components::compaction_summary_message::js_to_locale_string;
    assert_eq!(js_to_locale_string(1234.0), "1,234");
    assert_eq!(js_to_locale_string(1234567.0), "1,234,567");
    assert_eq!(js_to_locale_string(1234.5), "1,234.5");
}

#[test]
fn locale_string_matches_intl_default_rounding() {
    // 期望值来自 node：`v.toLocaleString()`（默认 locale 下 Intl.NumberFormat 的
    // maximumFractionDigits=3、half-expand、去掉小数尾零、指数展开、-0 保留符号）。
    use kiana_interactive::components::compaction_summary_message::js_to_locale_string;
    let cases: [(f64, &str); 19] = [
        (1234.5678, "1,234.568"),
        (0.0001234, "0"),
        (999.9999, "1,000"),
        (1.0005, "1.001"),
        (2.0005, "2.001"),
        (0.0005, "0.001"),
        (0.00025, "0"),
        (-0.0, "-0"),
        (-0.0004, "-0"),
        (1e21, "1,000,000,000,000,000,000,000"),
        (1.5e21, "1,500,000,000,000,000,000,000"),
        (1.2345678901234568e21, "1,234,567,890,123,456,800,000"),
        (1e-7, "0"),
        (0.1, "0.1"),
        (1.5, "1.5"),
        (1234567.0, "1,234,567"),
        (f64::NAN, "NaN"),
        (f64::INFINITY, "∞"),
        (f64::NEG_INFINITY, "-∞"),
    ];
    for (value, expected) in cases {
        assert_eq!(js_to_locale_string(value), expected, "value {value}");
    }
}

#[test]
fn provides_output_padding_to_custom_renderers_and_updates_it() {
    init_dark();
    let options_seen = Arc::new(Mutex::new(
        Vec::<kiana_ext_api::types::MessageRenderOptions>::new(),
    ));
    let slot = Arc::clone(&options_seen);
    let renderer: kiana_ext_api::types::MessageRenderer =
        Arc::new(move |_message, options, _theme| {
            slot.lock().unwrap().push(*options);
            Some(
                Box::new(Text::with_padding("custom", options.output_pad, 0)) as Box<dyn Component>,
            )
        });
    let message = CustomMessage {
        role: CustomRole::Custom,
        custom_type: "test".to_string(),
        content: serde_json::Value::String("custom".to_string()),
        display: true,
        details: None,
        timestamp: 0.0,
    };
    let mut component =
        CustomMessageComponent::new(message, Some(renderer), get_markdown_theme(), 1);
    assert_eq!(
        *options_seen.lock().unwrap(),
        vec![kiana_ext_api::types::MessageRenderOptions {
            expanded: false,
            output_pad: 1
        }]
    );
    assert!(
        component
            .render(40)
            .iter()
            .map(|line| strip_ansi(line))
            .any(|line| line.starts_with(" custom"))
    );

    component.set_output_pad(0);
    assert_eq!(
        options_seen.lock().unwrap().last().copied(),
        Some(kiana_ext_api::types::MessageRenderOptions {
            expanded: false,
            output_pad: 0
        })
    );
    assert!(
        component
            .render(40)
            .iter()
            .map(|line| strip_ansi(line))
            .any(|line| line.starts_with("custom"))
    );
}

#[test]
fn custom_message_default_rendering_matches_goldens() {
    init_dark();
    let message = CustomMessage {
        role: CustomRole::Custom,
        custom_type: "test".to_string(),
        content: serde_json::Value::String("custom".to_string()),
        display: true,
        details: None,
        timestamp: 0.0,
    };
    let mut component = CustomMessageComponent::new(message.clone(), None, get_markdown_theme(), 1);
    assert_golden("custom_default", component.render(40));

    let mut unpadded = CustomMessageComponent::new(message, None, get_markdown_theme(), 0);
    assert_golden("custom_default_pad0", unpadded.render(40));
}

#[test]
fn custom_message_renders_arrays_of_text_blocks() {
    init_dark();
    let message = CustomMessage {
        role: CustomRole::Custom,
        custom_type: "multi".to_string(),
        content: serde_json::json!([
            {"type": "text", "text": "one"},
            {"type": "image", "data": "AAAA", "mimeType": "image/png"},
            {"type": "text", "text": "two"}
        ]),
        display: true,
        details: None,
        timestamp: 0.0,
    };
    let mut component = CustomMessageComponent::new(message, None, get_markdown_theme(), 1);
    let rendered = strip_ansi(&component.render(40).join("\n"));
    assert!(rendered.contains("one"));
    assert!(rendered.contains("two"));
}

fn custom_entry() -> CustomEntry {
    CustomEntry {
        kind: CustomEntryType::Marker,
        custom_type: "ext".to_string(),
        data: Some(serde_json::json!({"x": 1})),
        id: "e1".to_string(),
        parent_id: None,
        timestamp: "2025-01-01T00:00:00.000Z".to_string(),
    }
}

#[test]
fn custom_entry_renders_renderer_output_and_tracks_content() {
    init_dark();
    let renderer: kiana_ext_api::types::EntryRenderer = Arc::new(|_entry, options, _theme| {
        Some(Box::new(Text::with_padding(
            format!("renderer {}", options.expanded),
            1,
            0,
        )) as Box<dyn Component>)
    });
    let mut component = CustomEntryComponent::new(custom_entry(), renderer, 1);
    assert!(component.has_content());
    assert!(strip_ansi(&component.render(40).join("\n")).contains("renderer false"));
    assert_golden("entry_default", component.render(40));
}

#[test]
fn custom_entry_without_component_renders_nothing() {
    init_dark();
    let renderer: kiana_ext_api::types::EntryRenderer = Arc::new(|_, _, _| None);
    let mut component = CustomEntryComponent::new(custom_entry(), renderer, 1);
    assert!(!component.has_content());
    assert!(component.render(40).is_empty());
    assert_golden("entry_empty", component.render(40));
}

#[test]
fn custom_entry_renders_a_failure_box_when_the_renderer_throws() {
    init_dark();
    let renderer: kiana_ext_api::types::EntryRenderer = Arc::new(|_, _, _| panic!("kaboom"));
    let mut component = CustomEntryComponent::new(custom_entry(), renderer, 1);
    assert!(component.has_content());
    let rendered = strip_ansi(&component.render(40).join("\n"));
    assert!(rendered.contains("[ext] renderer failed: kaboom"));
    assert_golden("entry_error", component.render(40));

    let renderer: kiana_ext_api::types::EntryRenderer = Arc::new(|_, _, _| panic!("strfail"));
    let mut component = CustomEntryComponent::new(custom_entry(), renderer, 1);
    assert_golden("entry_error_nostack", component.render(40));
}

#[test]
fn custom_message_passes_output_padding_of_zero_to_the_default_box() {
    // TS 断言 renderer 收到的 options 精确等于 `{ expanded: false, outputPad }`；上面已覆盖。
    // 这里补一条默认渲染的 0 缩进检查（Box.setPaddingX 生效）。
    init_dark();
    let message = CustomMessage {
        role: CustomRole::Custom,
        custom_type: "test".to_string(),
        content: serde_json::Value::String("custom".to_string()),
        display: true,
        details: None,
        timestamp: 0.0,
    };
    let mut component = CustomMessageComponent::new(message, None, get_markdown_theme(), 1);
    component.set_output_pad(0);
    let lines: Vec<String> = component
        .render(40)
        .iter()
        .map(|line| strip_ansi(line))
        .collect();
    assert!(lines.iter().any(|line| line.starts_with("[test]")));
}

#[test]
fn compaction_summary_respects_output_pad_zero_and_one() {
    init_dark();
    let message = CompactionSummaryMessage {
        role: CompactionSummaryRole::CompactionSummary,
        summary: "summary".to_string(),
        tokens_before: 10.0,
        timestamp: 0.0,
    };
    let render_lines = |component: &mut CompactionSummaryMessageComponent| -> Vec<String> {
        component
            .render(60)
            .iter()
            .map(|line| strip_ansi(line).trim_end().to_string())
            .filter(|line| {
                line.chars()
                    .any(|ch| ch.is_alphanumeric() || ch == '$' || ch == '(')
            })
            .collect()
    };
    let mut component = CompactionSummaryMessageComponent::new(message, get_markdown_theme(), 0);
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
