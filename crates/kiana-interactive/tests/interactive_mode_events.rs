//! `interactive-mode.ts` 的事件处理 / 聊天渲染移植（§2.50.5 / §2.50.6）。
//!
//! 覆盖：
//! - `interactive-mode-assistant-diagnostics.test.ts`（thinking drop / cache miss 提示）
//! - `test/suite/regressions/4167-thinking-toggle-pending-tool-render.test.ts`
//! - `test/suite/regressions/8611-thinking-toggle-pending-bash-output.test.ts`
//! - `message_end` 的 aborted/error 文案、`entry_appended` 边界压缩吞事件
//! - `showStatus` 去重、`showError` / `showWarning` 的组件顺序

#![allow(
    clippy::await_holding_lock,
    reason = "主题初始化是进程级状态：同文件的用例必须串行（TS 的 beforeAll(initTheme) 等价）"
)]

mod common;

use std::sync::{Mutex, MutexGuard};

use kiana_agent_core::types::AgentMessage;
use kiana_ai::providers::faux::FauxResponseStep;
use kiana_ai::types::{AssistantMessage, AssistantRole, Message, StopReason, Usage};
use kiana_config::settings::TuiMode;
use kiana_interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use kiana_session::agent_session::AgentSessionEvent;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::json;

static THEME_LOCK: Mutex<()> = Mutex::new(());

fn theme() -> MutexGuard<'static, ()> {
    let guard = THEME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

struct Fixture {
    mode: InteractiveMode,
    _faux: kiana_ai::providers::faux::FauxCore,
    _temp: tempfile::TempDir,
    _engine: common::TestEngine,
    _cwd: String,
}

async fn fixture() -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        engine,
        ..
    } = host;
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Regular),
            ..Default::default()
        },
    );
    Fixture {
        mode,
        _faux: faux,
        _temp: temp_dir,
        _engine: engine,
        _cwd: cwd,
    }
}

async fn fullscreen_fixture() -> Fixture {
    unsafe { std::env::set_var("KIANA_OFFLINE", "1") };
    let host = common::runtime_host(common::RuntimeHostOptions::new()).await;
    let common::RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        engine,
        ..
    } = host;
    let mode = InteractiveMode::new(
        runtime,
        InteractiveModeOptions {
            terminal: Some(Box::new(common::TestTerminal::new())),
            tui_mode: Some(TuiMode::Fullscreen),
            ..Default::default()
        },
    );
    Fixture {
        mode,
        _faux: faux,
        _temp: temp_dir,
        _engine: engine,
        _cwd: cwd,
    }
}

fn chat_text(mode: &InteractiveMode) -> String {
    mode.chat_render_text()
}

fn assistant_with(
    content: Vec<kiana_ai::types::AssistantContentBlock>,
    stop_reason: StopReason,
) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "faux".to_string(),
        provider: "faux".to_string(),
        model: "faux-1".to_string(),
        usage: Usage::default(),
        stop_reason,
        timestamp: 0,
        ..Default::default()
    }))
}

#[tokio::test(flavor = "current_thread")]
async fn show_status_updates_the_previous_line_instead_of_appending() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_status("first status");
        fixture.mode.test_show_status("second status");
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("second status"), "{rendered}");
        assert!(!rendered.contains("first status"), "{rendered}");
        // 只应有一对 Spacer + ThemedText。
        assert_eq!(fixture.mode.chat_child_count(), 2, "{rendered}");

        // 插入其它内容后，新的状态要追加一对。
        fixture.mode.test_show_error("boom");
        fixture.mode.test_show_status("third status");
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("third status"), "{rendered}");
        assert!(rendered.contains("Error: boom"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn show_error_and_warning_add_spacer_first_and_use_exact_prefixes() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_error("boom");
        fixture.mode.test_show_warning("careful");
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("Error: boom"), "{rendered}");
        assert!(rendered.contains("Warning: careful"), "{rendered}");
        assert_eq!(fixture.mode.chat_child_count(), 4);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn render_session_entries_matches_tool_results_by_tool_call_id() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let assistant = json!({
            "type": "message",
            "id": "entry-0",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "toolCall", "id": "tool-1", "name": "slow_tool", "arguments": {"path": "x"}}],
                "api": "faux",
                "provider": "faux",
                "model": "faux-1",
                "usage": {"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
                "stopReason": "toolUse",
                "timestamp": 0
            }
        });
        let result = json!({
            "type": "message",
            "id": "entry-1",
            "parentId": "entry-0",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "tool-1",
                "toolName": "slow_tool",
                "content": [{"type": "text", "text": "HISTORICAL_RESULT"}],
                "isError": false,
                "timestamp": 0
            }
        });
        let entries: Vec<SessionEntry> = vec![
            serde_json::from_value(assistant).expect("assistant entry"),
            serde_json::from_value(result).expect("result entry"),
        ];
        fixture.mode.test_render_entries(&entries);
        assert_eq!(fixture.mode.test_pending_tool_count(), 0);
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("HISTORICAL_RESULT"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn render_session_entries_keeps_unresolved_tool_calls_pending() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        let assistant = json!({
            "type": "message",
            "id": "entry-0",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "toolCall", "id": "tool-4167", "name": "slow_tool", "arguments": {"delayMs": 10000}}],
                "api": "faux",
                "provider": "faux",
                "model": "faux-1",
                "usage": {"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
                "stopReason": "toolUse",
                "timestamp": 0
            }
        });
        let entries: Vec<SessionEntry> =
            vec![serde_json::from_value(assistant).expect("assistant entry")];
        fixture.mode.test_render_entries(&entries);
        assert_eq!(fixture.mode.test_pending_tool_count(), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_cost_notice_is_gated_and_uses_warning_color() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(false);
        fixture.mode.test_add_compaction_cost(12_345, 0.25);
        assert_eq!(fixture.mode.chat_child_count(), 0);

        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(true);
        fixture.mode.test_add_compaction_cost(12_345, 0.25);
        let rendered = chat_text(&fixture.mode);
        assert!(
            rendered.contains("Compaction: 12k tokens billed (~$0.25)"),
            "{rendered}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn thinking_drop_notice_only_when_count_increases() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(true);
        let mut message = AssistantMessage {
            role: AssistantRole::Assistant,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            ..Default::default()
        };
        message.diagnostics = Some(vec![kiana_ai::types::AssistantMessageDiagnostic {
            kind: "anthropic_input_transformations".to_string(),
            timestamp: 0,
            error: None,
            details: Some(
                [(
                    "transformations".to_string(),
                    json!([{"type": "thinking_dropped"}, {"type": "thinking_dropped"}]),
                )]
                .into_iter()
                .collect(),
            ),
        }]);
        fixture.mode.test_maybe_show_thinking_drop(&message);
        let rendered = chat_text(&fixture.mode);
        assert!(
            rendered.contains("Anthropic dropped 2 thinking blocks (details in session)"),
            "{rendered}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn message_end_renders_operation_aborted_and_error_fallbacks() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;

        // assistant 流 → aborted
        let assistant = assistant_with(
            vec![kiana_ai::types::AssistantContentBlock::Text(
                kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "partial".to_string(),
                    text_signature: None,
                },
            )],
            StopReason::Pending,
        );
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::MessageStart {
                message: assistant.clone(),
            })
            .await;
        let mut ended = match assistant {
            AgentMessage::Standard(Message::Assistant(message)) => message,
            _ => unreachable!(),
        };
        ended.stop_reason = StopReason::Aborted;
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::MessageEnd {
                message: AgentMessage::Standard(Message::Assistant(ended)),
            })
            .await;
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("Operation aborted"), "{rendered}");
        assert!(!rendered.contains("Error:"), "{rendered}");

        // assistant 流 → error 且没有 errorMessage → "Error"
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::MessageStart {
                message: assistant_msg("second"),
            })
            .await;
        let mut failed = match assistant_msg("second") {
            AgentMessage::Standard(Message::Assistant(message)) => message,
            _ => unreachable!(),
        };
        failed.stop_reason = StopReason::Error;
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::MessageEnd {
                message: AgentMessage::Standard(Message::Assistant(failed)),
            })
            .await;
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("Error"), "{rendered}");
    })
    .await;
}

fn assistant_msg(text: &str) -> AgentMessage {
    assistant_with(
        vec![kiana_ai::types::AssistantContentBlock::Text(
            kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: text.to_string(),
                text_signature: None,
            },
        )],
        StopReason::Stop,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn entry_appended_swallows_entries_rendered_by_boundary_compaction() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        // 初始化可能因为宿主机缺少 fd/rg 而把离线提示写进聊天；
        // 本用例只关心边界压缩标记的条目不再追加任何组件。
        let before = fixture.mode.chat_child_count();
        fixture.mode.test_mark_boundary_entry("entry-x");
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::EntryAppended {
                entry: serde_json::from_value(json!({
                    "type": "session_info",
                    "id": "entry-x",
                    "timestamp": "2026-01-01T00:00:00.000Z"
                }))
                .expect("entry"),
            })
            .await;
        // 被吞掉的条目不应改变聊天（事件本身也不会渲染 content）。
        assert_eq!(
            fixture.mode.chat_child_count(),
            before,
            "chat:\n{}",
            fixture.mode.chat_render_text()
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tool_execution_events_create_update_and_finish_components() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;

        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::ToolExecutionStart {
                tool_call_id: "tool-9".to_string(),
                tool_name: "slow_tool".to_string(),
                args: json!({"command": "echo first"}),
                parent_tool_call_id: None,
            })
            .await;
        assert_eq!(fixture.mode.test_pending_tool_count(), 1);
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id: "tool-9".to_string(),
                tool_name: "slow_tool".to_string(),
                args: json!({"command": "echo first"}),
                partial_result: json!({"content": [{"type": "text", "text": "PARTIAL_OUTPUT"}], "details": null}),
                parent_tool_call_id: None,
            })
            .await;
        let partial = chat_text(&fixture.mode);
        assert!(partial.contains("PARTIAL_OUTPUT"), "{partial}");

        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: "tool-9".to_string(),
                tool_name: "slow_tool".to_string(),
                result: json!({"content": [{"type": "text", "text": "FINAL_OUTPUT"}], "details": null}),
                is_error: false,
                duration_ms: Some(5.0),
                parent_tool_call_id: None,
            })
            .await;
        assert_eq!(fixture.mode.test_pending_tool_count(), 0);
        let final_rendered = chat_text(&fixture.mode);
        assert!(final_rendered.contains("FINAL_OUTPUT"), "{final_rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn nested_tool_execution_starts_are_ignored() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::ToolExecutionStart {
                tool_call_id: "nested-1".to_string(),
                tool_name: "slow_tool".to_string(),
                args: json!({}),
                parent_tool_call_id: Some("parent-1".to_string()),
            })
            .await;
        assert_eq!(fixture.mode.test_pending_tool_count(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auto_retry_end_failure_uses_attempt_count_in_message() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt: 2,
                final_error: Some("boom".to_string()),
            })
            .await;
        let rendered = chat_text(&fixture.mode);
        assert!(
            rendered.contains("Error: Retry failed after 2 attempts: boom"),
            "{rendered}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_end_aborted_reports_manual_and_auto_messages() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::CompactionEnd {
                reason: kiana_ext_api::events::CompactionReason::Manual,
                result: None,
                aborted: true,
                will_retry: false,
                error_message: None,
            })
            .await;
        let rendered = chat_text(&fixture.mode);
        assert!(
            rendered.contains("Error: Compaction cancelled"),
            "{rendered}"
        );

        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::CompactionEnd {
                reason: kiana_ext_api::events::CompactionReason::Threshold,
                result: None,
                aborted: true,
                will_retry: false,
                error_message: None,
            })
            .await;
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("Auto-compaction cancelled"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_end_with_empty_error_message_renders_no_error_ui() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        let before_children = fixture.mode.chat_child_count();
        let before_text = chat_text(&fixture.mode);

        // TS `else if (event.errorMessage)`：空串是假值，manual 不调用 showError。
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::CompactionEnd {
                reason: kiana_ext_api::events::CompactionReason::Manual,
                result: None,
                aborted: false,
                will_retry: false,
                error_message: Some(String::new()),
            })
            .await;
        assert_eq!(
            fixture.mode.chat_child_count(),
            before_children,
            "manual compaction with an empty errorMessage must not append UI"
        );
        assert_eq!(chat_text(&fixture.mode), before_text);

        // 自动压缩同样不追加 Spacer + 空错误行。
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::CompactionEnd {
                reason: kiana_ext_api::events::CompactionReason::Threshold,
                result: None,
                aborted: false,
                will_retry: false,
                error_message: Some(String::new()),
            })
            .await;
        assert_eq!(
            fixture.mode.chat_child_count(),
            before_children,
            "auto compaction with an empty errorMessage must not append a blank error line"
        );
        assert_eq!(chat_text(&fixture.mode), before_text);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn agent_end_removes_the_streaming_component() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::MessageStart {
                message: assistant_msg("streaming text"),
            })
            .await;
        let during = chat_text(&fixture.mode);
        assert!(during.contains("streaming text"), "{during}");
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::AgentEnd {
                messages: Vec::new(),
                will_retry: false,
            })
            .await;
        let after = chat_text(&fixture.mode);
        assert!(!after.contains("streaming text"), "{after}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_and_branch_summary_costs_use_exact_labels() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(true);
        // 10 + 20 + 30 + 40 = 100 tokens；$0.125 → ~$0.13。
        fixture
            .mode
            .test_add_compaction_cost_kind("compaction", 100, 0.125);
        fixture
            .mode
            .test_add_compaction_cost_kind("branch_summary", 100, 0.125);
        let rendered = chat_text(&fixture.mode);
        assert!(
            rendered.contains("Compaction: 100 tokens billed (~$0.13)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Branch summary: 100 tokens billed (~$0.13)"),
            "{rendered}"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_entries_render_summary_then_cost_in_order() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.settings_manager().set_show_cache_miss_notices(true);
        let entries: Vec<SessionEntry> = vec![
            serde_json::from_value(json!({
                "type": "compaction",
                "id": "current",
                "parentId": "previous",
                "timestamp": "2025-01-02T00:00:00Z",
                "summary": "current summary",
                "firstKeptEntryId": "kept",
                "tokensBefore": 200,
                "usage": {"input":10,"output":20,"cacheRead":30,"cacheWrite":40,"totalTokens":100,"cost":{"input":0.01,"output":0.02,"cacheRead":0.03,"cacheWrite":0.04,"total":0.1}}
            }))
            .expect("current compaction"),
            serde_json::from_value(json!({
                "type": "compaction",
                "id": "previous",
                "parentId": null,
                "timestamp": "2025-01-01T00:00:00Z",
                "summary": "previous summary",
                "firstKeptEntryId": "kept",
                "tokensBefore": 100,
                "usage": {"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"totalTokens":10,"cost":{"input":0.001,"output":0.002,"cacheRead":0.003,"cacheWrite":0.004,"total":0.01}}
            }))
            .expect("previous compaction"),
        ];
        fixture.mode.test_render_entries(&entries);
        let rendered = chat_text(&fixture.mode);
        let current_summary = rendered
            .find("Compacted from 200 tokens")
            .expect("current summary");
        let current_cost = rendered.find("Compaction: 100 tokens billed").expect("current cost");
        let previous_summary = rendered
            .find("Compacted from 100 tokens")
            .expect("previous summary");
        let previous_cost = rendered.find("Compaction: 10 tokens billed").expect("previous cost");
        assert!(current_summary < current_cost, "{rendered}");
        assert!(current_cost < previous_summary, "{rendered}");
        assert!(previous_summary < previous_cost, "{rendered}");
    })
    .await;
}

/// `flushCompactionQueue` 先启动首轮 prompt（不等待完成），再把剩余消息按 steer/followUp
/// 排进仍在运行的 run（review finding：阻塞 await 会把 steer 变成后续独立 turn）。
#[tokio::test(flavor = "current_thread")]
async fn flush_compaction_queue_queues_the_rest_while_the_first_run_is_active() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture._faux.set_responses(vec![
            kiana_ai::providers::faux::FauxResponseStep::message_factory(|_input| async {
                std::future::pending::<kiana_ai::types::AssistantMessage>().await
            }),
        ]);
        fixture
            .mode
            .test_queue_compaction_message("change direction", false);
        fixture
            .mode
            .test_queue_compaction_message("second steer", false);

        let session = fixture.mode.session();
        let completed = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.mode.test_flush_compaction_queue(false),
        )
        .await;
        assert!(
            completed.is_ok(),
            "flush must not await the first prompt to finish"
        );
        assert!(fixture.mode.test_compaction_queue_texts().is_empty());

        common::wait_until(|| {
            session
                .get_steering_messages()
                .iter()
                .any(|message| message == "second steer")
        })
        .await;
        assert!(session.is_streaming());
        session.request_abort();
    })
    .await;
}

// ---------------------------------------------------------------------------
// 第二轮审阅补齐的 TS 用例（interactive-mode-status / compaction / diagnostics /
// interactive-tui 的 transcript rebuild）
// ---------------------------------------------------------------------------

use kiana_interactive::components::status_indicator::StatusIndicatorKind;

#[tokio::test(flavor = "current_thread")]
async fn managed_tool_status_renders_updates_as_one_contiguous_group() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.test_show_managed_tool_status(
            kiana_core::tools_manager::ToolStatusKind::Info,
            "fd downloading",
        );
        fixture.mode.test_show_managed_tool_status(
            kiana_core::tools_manager::ToolStatusKind::Info,
            "rg downloading",
        );
        fixture.mode.test_show_managed_tool_status(
            kiana_core::tools_manager::ToolStatusKind::Warning,
            "rg failed",
        );
        assert_eq!(
            fixture.mode.chat_child_count(),
            4,
            "one spacer + three lines"
        );
        let rendered = chat_text(&fixture.mode);
        assert!(rendered.contains("fd downloading"), "{rendered}");
        assert!(rendered.contains(" rg downloading"), "{rendered}");
        assert!(rendered.contains(" Warning: rg failed"), "{rendered}");
        // 组内只有一条 Spacer：三行连续出现（中间没有空行）。
        let lines: Vec<&str> = rendered.lines().collect();
        let fd = lines
            .iter()
            .position(|line| line.contains("fd downloading"))
            .unwrap();
        assert!(lines[fd + 1].contains("rg downloading"), "{rendered}");
        assert!(lines[fd + 2].contains("Warning: rg failed"), "{rendered}");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn turn_start_restores_the_working_state_after_compaction() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .settings_manager()
            .set_show_terminal_progress(true)
            .expect("settings");

        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::TurnStart)
            .await;
        assert_eq!(
            fixture.mode.test_active_status_kind(),
            Some(StatusIndicatorKind::Working)
        );

        // 同一次运行里再次 turn_start：不会重复创建（already working）。
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::TurnStart)
            .await;
        assert_eq!(
            fixture.mode.test_active_status_kind(),
            Some(StatusIndicatorKind::Working)
        );

        // workingVisible=false：清掉工作指示器。
        fixture.mode.test_set_working_visible(false);
        fixture
            .mode
            .test_handle_event(&AgentSessionEvent::TurnStart)
            .await;
        assert_eq!(fixture.mode.test_active_status_kind(), None);
    })
    .await;
}

/// `compaction_end` 成功路径：清空聊天、渲染保留条目、追加摘要消息与费用提示。
#[tokio::test(flavor = "current_thread")]
async fn compaction_end_success_renders_retained_entries_and_cost() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(true);
        let session = fixture.mode.session();
        session
            .with_session_manager_mut(|manager| {
                manager.append_compaction("previous summary", None, 100.0, None, None, None)
            })
            .expect("append compaction");
        fixture.mode.test_show_status("BEFORE_COMPACTION");

        let event: AgentSessionEvent = serde_json::from_value(json!({
            "type": "compaction_end",
            "reason": "manual",
            "result": {
                "summary": "summarized context",
                "firstKeptEntryId": "",
                "tokensBefore": 123,
                "usage": {
                    "input": 10,
                    "output": 20,
                    "cacheRead": 30,
                    "cacheWrite": 40,
                    "totalTokens": 100,
                    "cost": {
                        "input": 0.01,
                        "output": 0.02,
                        "cacheRead": 0.03,
                        "cacheWrite": 0.065,
                        "total": 0.125
                    }
                }
            },
            "aborted": false,
            "willRetry": false
        }))
        .expect("compaction_end event");
        fixture.mode.test_handle_event(&event).await;
        let rendered = chat_text(&fixture.mode);
        assert!(!rendered.contains("BEFORE_COMPACTION"), "{rendered}");
        assert!(rendered.contains("[compaction]"), "{rendered}");
        assert!(rendered.contains("Compacted from 123 tokens"), "{rendered}");
        assert!(
            rendered.contains("Compaction: 100 tokens billed (~$0.13)"),
            "{rendered}"
        );
    })
    .await;
}

/// `restoreQueuedMessagesToEditor({ abort: true })` 通过 AgentSession 中止响应。
#[tokio::test(flavor = "current_thread")]
async fn interactive_response_aborts_route_through_agent_session() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            ._faux
            .set_responses(vec![FauxResponseStep::message_factory(|_input| async {
                std::future::pending::<AssistantMessage>().await
            })]);
        let session = fixture.mode.session();
        let prompt_session = session.clone();
        let prompt = tokio::task::spawn_local(async move {
            prompt_session
                .prompt("hi".to_string(), Default::default())
                .await
        });
        common::wait_until(|| session.is_streaming()).await;

        fixture.mode.test_restore_queued_messages_to_editor(true);
        common::wait_until(|| !session.is_streaming()).await;
        let _ = prompt.await;
    })
    .await;
}

/// 未变化的 thinking drop 计数不重复提示；关闭 cache miss 提示时完全静默。
#[tokio::test(flavor = "current_thread")]
async fn unchanged_and_disabled_thinking_drop_notices_stay_silent() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fixture().await;
        fixture.mode.init().await;
        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(true);
        // 初始化可能因为宿主机缺少 fd/rg 而写离线提示；这里比较事件前后的增量。
        let before = fixture.mode.chat_child_count();

        let message = thinking_drop_message(3, 1);
        // 分支里已有 3 次 drop 的历史消息 → 不重复提示。
        fixture
            .mode
            .session()
            .with_session_manager_mut(|manager| {
                manager.append_message(AgentMessage::Standard(Message::Assistant(
                    thinking_drop_message(3, 1),
                )))
            })
            .expect("append");

        fixture.mode.test_maybe_show_thinking_drop(&message);
        assert_eq!(fixture.mode.chat_child_count(), before);

        fixture
            .mode
            .settings_manager()
            .set_show_cache_miss_notices(false);
        fixture
            .mode
            .test_maybe_show_thinking_drop(&thinking_drop_message(5, 2));
        assert_eq!(fixture.mode.chat_child_count(), before);
    })
    .await;
}

fn thinking_drop_message(dropped: usize, timestamp: i64) -> AssistantMessage {
    let transformations: Vec<serde_json::Value> = (0..dropped)
        .map(|index| json!({"type": "thinking_dropped", "path": format!("messages.{index}.content.0")}))
        .collect();
    AssistantMessage {
        role: AssistantRole::Assistant,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        timestamp,
        diagnostics: Some(vec![kiana_ai::types::AssistantMessageDiagnostic {
            kind: "anthropic_input_transformations".to_string(),
            timestamp,
            error: None,
            details: Some(
                [("transformations".to_string(), json!(transformations))]
                    .into_iter()
                    .collect(),
            ),
        }]),
        ..Default::default()
    }
}

/// 重建会话条目时丢弃全屏文本选择（regression #9311）。
#[tokio::test(flavor = "current_thread")]
async fn rerendering_session_entries_drops_the_fullscreen_selection() {
    let _theme = theme();
    common::run_local(async {
        let mut fixture = fullscreen_fixture().await;
        fixture.mode.init().await;
        fixture.mode.test_ui_add_child(Box::new(
            kiana_tui::components::text_component::Text::with_padding(
                "alpha\nbeta\ngamma\ndelta",
                0,
                0,
            ),
        ));
        // 先在聊天里放一行可见文本（fullscreen 渲染 layoutRoot 才会显示）。
        fixture.mode.test_show_status("alpha");
        fixture.mode.test_show_status("beta");
        fixture.mode.test_ui_render_now();
        let raw_lines = fixture.mode.test_ui_screen_lines();
        let lines: Vec<String> = raw_lines
            .iter()
            .map(|line| kiana_tui::text::strip_terminal_sequences(line))
            .collect();
        let mut visible: Vec<(usize, usize)> = lines
            .iter()
            .enumerate()
            .filter_map(|(row, line)| {
                let mut characters = line.char_indices();
                let (col, character) = characters.find(|(_, character)| {
                    !character.is_whitespace() && character.is_alphanumeric()
                })?;
                let _ = character;
                Some((row, col))
            })
            .collect();
        assert!(visible.len() >= 2, "need selectable rows: {lines:?}");
        let (start_row, start_col) = visible.remove(0);
        let (end_row, end_col) = visible.remove(visible.len() - 1);
        fixture
            .mode
            .test_ui_handle_input(&format!("\x1b[<0;{};{}M", start_col + 1, start_row + 1));
        fixture
            .mode
            .test_ui_handle_input(&format!("\x1b[<32;{};{}M", end_col + 1, end_row + 1));
        fixture
            .mode
            .test_ui_handle_input(&format!("\x1b[<0;{};{}m", end_col + 1, end_row + 1));
        fixture.mode.test_ui_render_now();
        assert!(
            fixture.mode.test_ui_has_active_selection(),
            "mouse drag must create a selection: {lines:?}"
        );

        fixture.mode.test_render_entries(&[]);
        assert!(!fixture.mode.test_ui_has_active_selection());
    })
    .await;
}
