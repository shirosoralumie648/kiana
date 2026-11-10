//! `test/suite/regressions/6324-branch-summary-ambient-auth.test.ts` 的 Rust 移植。

mod common;

use std::sync::Arc;

use common::*;
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::providers::faux::faux_assistant_message;
use kiana_ai::types::{AssistantMessageEvent, DoneReason, Usage, UsageCost};
use kiana_session::agent_session::NavigateTreeOptions;
use kiana_session_core::session_manager::SessionEntry;

fn usage_with_cost(input: u64, output: u64, total_cost: f64) -> Usage {
    Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: total_cost,
            total_from_provider: None,
        },
    }
}

/// TS: "summarizes tree branches when request auth has no API key"
///
/// 评审 finding #3：自定义流函数（构造后替换）走 `getAuth` 的可选回退分支，
/// 没有 API key 时用裸模型继续，而不是抛 `No API key found`。
#[tokio::test]
async fn summarizes_tree_branches_when_request_auth_has_no_api_key() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;

        let stream_call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        harness
            .agent
            .set_stream_function(branch_summary_stream_fn(Arc::clone(&stream_call_count)));

        let target_id = harness.append_session_message(user_msg("first branch"));
        harness.append_session_message(assistant_msg("first reply"));
        harness.append_session_message(user_msg("abandoned branch work"));
        harness.append_session_message(assistant_msg("abandoned reply"));

        let result = harness
            .session
            .navigate_tree(
                target_id,
                NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(!result.cancelled);
        assert_eq!(
            stream_call_count.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let summary = result.summary_entry.expect("summary entry");
        let SessionEntry::BranchSummary(entry) = &summary else {
            panic!("expected branch summary entry");
        };
        assert!(entry.summary.contains("branch summary text"));
        let usage = entry.usage.clone().expect("usage");
        assert_eq!(usage.cost.total, 0.25);
        harness.cleanup();
    })
    .await;
}

fn assistant_msg(text: &str) -> kiana_agent_core::types::AgentMessage {
    kiana_agent_core::types::AgentMessage::Standard(kiana_ai::types::Message::Assistant(
        faux_assistant_message(text, Default::default()),
    ))
}

/// 自定义流函数：记录调用次数并断言 ambient auth 下没有 API key，返回固定摘要。
fn branch_summary_stream_fn(
    calls: Arc<std::sync::atomic::AtomicUsize>,
) -> kiana_agent_core::types::StreamFn {
    Arc::new(move |model, _context, options| {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(
                options
                    .as_ref()
                    .and_then(|options| options.base.api_key.clone())
                    .is_none(),
                "ambient auth must leave apiKey undefined"
            );
            let mut message = faux_assistant_message("branch summary text", Default::default());
            message.api = model.api.clone();
            message.provider = model.provider.clone();
            message.model = model.id.clone();
            message.usage = usage_with_cost(1, 1, 0.25);
            message.stop_reason = kiana_ai::types::StopReason::Stop;
            let stream = create_assistant_message_event_stream();
            let _ = stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(message),
            });
            Ok(stream)
        })
    })
}

/// 回归（评审 finding #3）：**构造期**安装的自定义流函数也走可选鉴权回退分支。
///
/// TS 用 `agent.streamFunction === streamSimple` 判断；本 port 在构造时记录原始函数是否
/// 为规范实例，避免 `AgentSession` 的包装函数让判断恒真。
#[tokio::test]
async fn custom_stream_function_installed_at_construction_uses_ambient_auth() {
    run(async {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            stream_fn: Some(branch_summary_stream_fn(Arc::clone(&calls))),
            ..Default::default()
        })
        .await;
        let target_id = harness.append_session_message(user_msg("first branch"));
        harness.append_session_message(assistant_msg("first reply"));
        harness.append_session_message(user_msg("abandoned branch work"));
        harness.append_session_message(assistant_msg("abandoned reply"));

        let result = harness
            .session
            .navigate_tree(
                target_id,
                NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let summary = result.summary_entry.expect("summary entry");
        let SessionEntry::BranchSummary(entry) = &summary else {
            panic!("expected branch summary entry");
        };
        assert!(entry.summary.contains("branch summary text"));
        harness.cleanup();
    })
    .await;
}
