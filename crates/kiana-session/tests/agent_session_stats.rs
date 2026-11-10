//! `test/agent-session-stats.test.ts` 的 Rust 移植。

mod common;

use common::*;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, TextContent,
    TextKind, ToolResultMessage, ToolResultRole, Usage, UsageCost, UserContentBlock, UserMessage,
    UserRole,
};
use kiana_session_core::usage_totals::get_usage_cost_breakdown;

fn create_usage(total_tokens: u64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens,
        cost: UsageCost::default(),
        ..Default::default()
    }
}

fn paid_usage(input: u64, output: u64, cache_read: u64, cache_write: u64, total: f64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total,
            total_from_provider: None,
        },
        ..Default::default()
    }
}

fn assistant_message(harness: &Harness, text: &str, usage: Usage, timestamp: i64) -> AgentMessage {
    let model = harness.get_model(None).expect("model");
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage,
        stop_reason: StopReason::Stop,
        timestamp,
        ..Default::default()
    }))
}

fn user_message(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.to_string()),
        timestamp,
    }))
}

fn tool_result_message(usage: Usage) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "tool-call-1".to_string(),
        tool_name: "test_tool".to_string(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "tool result".to_string(),
            text_signature: None,
        })],
        details: None,
        usage: Some(usage),
        nested_calls: None,
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    }))
}

fn context_window(harness: &Harness) -> u64 {
    harness
        .get_model(None)
        .and_then(|model| model.context_window)
        .unwrap_or(0)
}

/// TS: "exposes the current context usage alongside token totals"
#[tokio::test]
async fn exposes_the_current_context_usage_alongside_token_totals() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.append_session_message(user_message("hello", 1));
        harness.append_session_message(assistant_message(&harness, "hi", create_usage(200), 2));
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.context_usage, harness.session.get_context_usage());
        let usage = stats.context_usage.clone().expect("context usage");
        let window = context_window(&harness);
        assert_eq!(usage.tokens, Some(200));
        assert_eq!(usage.context_window, window);
        assert_eq!(usage.percent, Some(200.0 / window as f64 * 100.0));
        harness.cleanup();
    })
    .await;
}

/// TS: "reports unknown current context usage immediately after compaction"
#[tokio::test]
async fn reports_unknown_current_context_usage_immediately_after_compaction() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.append_session_message(user_message("first", 1));
        harness.append_session_message(assistant_message(
            &harness,
            "response1",
            create_usage(180_000),
            2,
        ));
        let kept = harness.append_session_message(user_message("second", 3));
        harness.append_session_message(assistant_message(
            &harness,
            "response2",
            create_usage(195_000),
            4,
        ));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction("summary", Some(&kept), 195_000.0, None, None, None)
            })
            .unwrap();
        harness.session.refresh_context();
        harness.append_session_message(user_message("third", 5));
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.tokens.input, 375_000.0);
        let usage = stats.context_usage.expect("context usage");
        assert_eq!(usage.tokens, None);
        assert_eq!(usage.percent, None);
        harness.cleanup();
    })
    .await;
}

/// TS: "uses post-compaction usage for current context instead of stale kept usage"
#[tokio::test]
async fn uses_post_compaction_usage_for_current_context() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.append_session_message(user_message("first", 1));
        harness.append_session_message(assistant_message(
            &harness,
            "response1",
            create_usage(180_000),
            2,
        ));
        let kept = harness.append_session_message(user_message("second", 3));
        harness.append_session_message(assistant_message(
            &harness,
            "response2",
            create_usage(195_000),
            4,
        ));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction("summary", Some(&kept), 195_000.0, None, None, None)
            })
            .unwrap();
        harness.session.refresh_context();
        harness.append_session_message(user_message("third", 5));
        harness.append_session_message(assistant_message(
            &harness,
            "response3",
            create_usage(25_000),
            6,
        ));
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.tokens.input, 400_000.0);
        let usage = stats.context_usage.expect("context usage");
        let window = context_window(&harness);
        assert_eq!(usage.tokens, Some(25_000));
        assert_eq!(usage.percent, Some(25_000.0 / window as f64 * 100.0));
        harness.cleanup();
    })
    .await;
}

/// TS: "includes branch summary usage in session totals"
#[tokio::test]
async fn includes_branch_summary_usage_in_session_totals() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.branch_with_summary(
                    None,
                    "summary",
                    None,
                    None,
                    Some(paid_usage(10, 20, 30, 40, 1.0)),
                )
            })
            .unwrap();
        harness.session.refresh_context();
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.tokens.input, 10.0);
        assert_eq!(stats.tokens.output, 20.0);
        assert_eq!(stats.tokens.cache_read, 30.0);
        assert_eq!(stats.tokens.cache_write, 40.0);
        assert_eq!(stats.tokens.total, 100.0);
        assert_eq!(stats.cost, 1.0);
        harness.cleanup();
    })
    .await;
}

/// TS: "includes compaction usage in session totals"
#[tokio::test]
async fn includes_compaction_usage_in_session_totals() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let first = harness.append_session_message(user_message("hello", 1));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction(
                    "summary",
                    Some(&first),
                    100.0,
                    None,
                    None,
                    Some(paid_usage(10, 20, 30, 40, 1.0)),
                )
            })
            .unwrap();
        harness.session.refresh_context();
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.tokens.input, 10.0);
        assert_eq!(stats.tokens.output, 20.0);
        assert_eq!(stats.tokens.cache_read, 30.0);
        assert_eq!(stats.tokens.cache_write, 40.0);
        assert_eq!(stats.tokens.total, 100.0);
        assert_eq!(stats.cost, 1.0);
        harness.cleanup();
    })
    .await;
}

/// TS: "includes cache-warming usage exactly once without adding messages"
#[tokio::test]
async fn includes_cache_warming_usage_exactly_once_without_adding_messages() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let model = harness.get_model(None).expect("model");
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_usage(
                    "cache_warm",
                    &model.provider,
                    &model.id,
                    Usage {
                        input: 2,
                        output: 1,
                        cache_read: 97,
                        cache_write: 0,
                        total_tokens: 100,
                        cost: UsageCost {
                            input: 0.001,
                            output: 0.002,
                            cache_read: 0.007,
                            cache_write: 0.0,
                            total: 0.01,
                            total_from_provider: None,
                        },
                        ..Default::default()
                    },
                    Some("extension override"),
                )
            })
            .unwrap();
        harness.session.refresh_context();
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.tokens.input, 2.0);
        assert_eq!(stats.tokens.output, 1.0);
        assert_eq!(stats.tokens.cache_read, 97.0);
        assert_eq!(stats.tokens.cache_write, 0.0);
        assert_eq!(stats.tokens.total, 100.0);
        assert_eq!(stats.total_messages, 0);
        assert!(harness.session.messages().is_empty());
        let breakdown = get_usage_cost_breakdown(&harness.session_entries());
        assert_eq!(breakdown.len(), 1);
        assert_eq!(breakdown[0].key, format!("{}/{}", model.provider, model.id));
        assert_eq!(breakdown[0].cost, 0.01);
        assert_eq!(breakdown[0].tokens, 100.0);
        harness.cleanup();
    })
    .await;
}

/// TS: "includes tool result usage in session totals"
#[tokio::test]
async fn includes_tool_result_usage_in_session_totals() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.append_session_message(tool_result_message(paid_usage(10, 20, 30, 40, 1.0)));
        let stats = harness.session.get_session_stats();
        assert_eq!(stats.tokens.input, 10.0);
        assert_eq!(stats.tokens.output, 20.0);
        assert_eq!(stats.tokens.cache_read, 30.0);
        assert_eq!(stats.tokens.cache_write, 40.0);
        assert_eq!(stats.tokens.total, 100.0);
        assert_eq!(stats.cost, 1.0);
        harness.cleanup();
    })
    .await;
}

/// TS: "groups tool and summary usage separately from model-attributed usage"
#[tokio::test]
async fn groups_tool_and_summary_usage_separately_from_model_attributed_usage() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let model = harness.get_model(None).expect("model");
        let root = harness.append_session_message(user_message("hello", 1));
        let mut assistant_usage = create_usage(100);
        assistant_usage.cost.total = 0.5;
        harness.append_session_message(assistant_message(&harness, "response", assistant_usage, 2));
        let mut tool_usage = create_usage(100);
        tool_usage.cost.total = 1.0;
        harness.append_session_message(tool_result_message(tool_usage));
        let mut compaction_usage = create_usage(100);
        compaction_usage.cost.total = 2.0;
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction(
                    "summary",
                    Some(&root),
                    100.0,
                    None,
                    None,
                    Some(compaction_usage),
                )
            })
            .unwrap();
        let mut branch_usage = create_usage(100);
        branch_usage.cost.total = 3.0;
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.branch_with_summary(None, "branch summary", None, None, Some(branch_usage))
            })
            .unwrap();
        let breakdown = get_usage_cost_breakdown(&harness.session_entries());
        assert_eq!(breakdown.len(), 2);
        assert_eq!(breakdown[0].key, "Tools/summaries");
        assert_eq!(breakdown[0].cost, 6.0);
        assert_eq!(breakdown[0].tokens, 300.0);
        assert_eq!(breakdown[1].key, format!("{}/{}", model.provider, model.id));
        assert_eq!(breakdown[1].cost, 0.5);
        assert_eq!(breakdown[1].tokens, 100.0);
        harness.cleanup();
    })
    .await;
}

/// TS: "ignores zero-usage messages when checking for post-compaction context usage"
#[tokio::test]
async fn ignores_zero_usage_messages_when_checking_for_post_compaction_context_usage() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.append_session_message(user_message("first", 1));
        harness.append_session_message(assistant_message(
            &harness,
            "response1",
            create_usage(180_000),
            2,
        ));
        let kept = harness.append_session_message(user_message("second", 3));
        harness.append_session_message(assistant_message(
            &harness,
            "response2",
            create_usage(195_000),
            4,
        ));
        harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_compaction("summary", Some(&kept), 195_000.0, None, None, None)
            })
            .unwrap();
        harness.session.refresh_context();
        harness.append_session_message(user_message("third", 5));
        harness.append_session_message(assistant_message(
            &harness,
            "response3",
            create_usage(25_000),
            6,
        ));
        harness.append_session_message(user_message("continue", 7));
        harness.append_session_message(assistant_message(&harness, "partial", create_usage(0), 8));
        let stats = harness.session.get_session_stats();
        let usage = stats.context_usage.expect("context usage");
        let tokens = usage.tokens.expect("tokens");
        assert!(tokens > 25_000);
        harness.cleanup();
    })
    .await;
}

/// `exportToHtml` 必须导出**实时**会话管理器（带会话文件），而不是重建的内存会话。
#[tokio::test]
async fn export_to_html_exports_the_live_file_backed_session() {
    run(async {
        let temp = tempfile::tempdir().expect("temp dir");
        let session_dir = temp.path().join("sessions");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mut manager = kiana_session_core::session_manager::SessionManager::create(
            &temp.path().to_string_lossy(),
            Some(&session_dir.to_string_lossy()),
            None,
        )
        .expect("session manager");
        manager
            .append_message(user_message("hello export", 1))
            .expect("append");
        let harness = create_harness(HarnessOptions {
            session_manager: Some(manager),
            ..Default::default()
        })
        .await;
        let output = temp.path().join("export.html");
        let path = harness
            .session
            .export_to_html(Some(output.to_string_lossy().into_owned()), None)
            .expect("export");
        assert!(std::path::Path::new(&path).exists());
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("<html") || html.contains("<!doctype"));
        assert!(html.len() > 1000);
        harness.cleanup();
    })
    .await;
}

/// TS: 内存会话导出报 `Cannot export in-memory session to HTML`。
#[tokio::test]
async fn export_to_html_rejects_in_memory_sessions() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let error = harness.session.export_to_html(None, None).unwrap_err();
        assert_eq!(error.to_string(), "Cannot export in-memory session to HTML");
        harness.cleanup();
    })
    .await;
}

/// 回归（评审 finding #6）：`sessionManager` 是**实时**只读视图。
///
/// 旧实现每次脏读都把整份会话复制成一个新的 `SessionManager` 并 `Box::leak`，UI 每轮
/// 读取一次就泄漏 O(条目数)。现在读取直接访问会话管理器，只按 id 缓存条目的不可变引用。
#[tokio::test]
async fn session_manager_view_is_live_and_reference_stable() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let view = harness.session.session_manager();
        let id = harness.append_session_message(user_message("hello", 1));
        let entry = view.get_entry(&id).expect("entry visible");
        assert_eq!(entry.id(), id);
        // 同一个条目在后续变更后返回同一地址（条目不可变，按 id 只缓存一次）。
        let first: *const kiana_session_core::session_manager::SessionEntry =
            view.get_entry(&id).expect("entry");
        harness.append_session_message(user_message("again", 2));
        let second: *const kiana_session_core::session_manager::SessionEntry =
            view.get_entry(&id).expect("entry");
        assert_eq!(first, second);
        assert_eq!(view.get_entries().len(), harness.session_entries().len());
        assert!(view.get_leaf_id().is_some());
        let branch = view.get_branch(None);
        assert_eq!(
            branch.last().map(|entry| entry.id().to_string()),
            view.get_leaf_id().map(str::to_string)
        );
        harness.cleanup();
    })
    .await;
}
