//! 压缩流水线的 all-or-nothing / fallback / split-turn 集成测试。
//!
//! 覆盖 DoD：
//! - summarizer 失败时历史逐字节不变；
//! - no-LLM fallback 产出严格更小的上下文；
//! - split-turn 不切断 tool pair，产出 turn-prefix 摘要；
//! - PTL 重试头部丢最旧消息最多 3 次、至少留一组；
//! - provider overflow 只 compact+retry 一次，第二次明确报错。

use std::sync::Arc;

use kiana_agent_core::types::AgentMessage;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::create_assistant_message_event_stream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, DoneReason,
    Message, Model, StopReason, TextContent, TextKind, ToolCall, ToolCallKind, ToolResultMessage,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_compaction::CompactionSettings;
use kiana_compaction::fallback::FallbackConfig;
use kiana_compaction::pipeline::{
    CompactModelOptions, PipelineError, PipelineOptions, PipelineOutcome, check_split_turn_pairing,
    run_compaction_pipeline,
};
use kiana_compaction::pruning::PruneConfig;
use kiana_compaction::ptl::{
    MAX_PTL_RETRIES, OverflowAction, OverflowRecovery, drop_oldest_groups, plan_ptl_retries,
};
use kiana_session_core::session_manager::{SessionEntry, SessionMessageEntry, SessionMessageType};

fn user(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    }))
}

fn assistant(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 0,
        ..Default::default()
    }))
}

fn assistant_call(id: &str) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::default(),
            id: id.to_string(),
            name: "read".to_string(),
            arguments: Default::default(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        })],
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        timestamp: 0,
        ..Default::default()
    }))
}

fn tool_result(id: &str) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: id.to_string(),
        tool_name: "read".to_string(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "ok".to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }))
}

fn entries(messages: Vec<AgentMessage>) -> Vec<SessionEntry> {
    messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            SessionEntry::Message(SessionMessageEntry {
                kind: SessionMessageType::Marker,
                id: format!("entry-{index}"),
                parent_id: (index > 0).then(|| format!("entry-{}", index - 1)),
                timestamp: String::new(),
                message,
                raw_message: None,
            })
        })
        .collect()
}

fn serialized(entries: &[SessionEntry]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend_from_slice(&serde_json::to_vec(entry).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

fn failing_stream() -> kiana_agent_core::types::StreamFn {
    Arc::new(|_model, _context, _options| {
        Box::pin(async move { Err(AiError::new(AiErrorKind::Transport, "synthetic failure")) })
    })
}

fn options_with_fallback(stream: kiana_agent_core::types::StreamFn) -> PipelineOptions {
    PipelineOptions {
        prune: PruneConfig::default(),
        fallback: Some(FallbackConfig::default()),
        model: CompactModelOptions {
            model: Model::default(),
            stream_fn: Some(stream),
            retry: None,
            ..CompactModelOptions::default()
        },
    }
}

fn large_history() -> Vec<SessionEntry> {
    let mut messages = Vec::new();
    for index in 0..40 {
        messages.push(user(&format!(
            "user turn {index}: {}",
            "context ".repeat(300)
        )));
        messages.push(assistant(&format!(
            "assistant turn {index}: {}",
            "answer ".repeat(300)
        )));
    }
    entries(messages)
}

#[tokio::test]
async fn summarizer_failure_leaves_history_byte_identical_and_falls_back() {
    let history = large_history();
    let before = serialized(&history);
    let options = options_with_fallback(failing_stream());
    let outcome = run_compaction_pipeline(&history, &CompactionSettings::default(), &options)
        .await
        .expect("pipeline");
    let fallback = match &outcome {
        PipelineOutcome::Fallback { result, .. } => result.clone(),
        other => panic!("expected fallback, got {other:?}"),
    };
    assert!(outcome.history_unchanged());
    assert_eq!(
        serialized(&history),
        before,
        "input history must not change"
    );
    assert_eq!(serialized(outcome.entries()), serialized(&history));
    assert!(fallback.summary.contains("## Objective"));
    assert!(fallback.usage.is_none());
    // fallback 必须严格更小。
    let input_tokens: u64 = {
        let preparation =
            kiana_compaction::prepare_compaction(&history, &CompactionSettings::default())
                .expect("preparation");
        preparation
            .messages_to_summarize
            .iter()
            .map(kiana_compaction::estimate_tokens)
            .sum()
    };
    let summary_tokens = kiana_compaction::fallback::estimate_text_tokens(&fallback.summary);
    assert!(summary_tokens < input_tokens);
}

#[tokio::test]
async fn without_fallback_failure_returns_original_history() {
    let history = large_history();
    let before = serialized(&history);
    let mut options = options_with_fallback(failing_stream());
    options.fallback = None;
    let outcome = run_compaction_pipeline(&history, &CompactionSettings::default(), &options)
        .await
        .expect("pipeline");
    match outcome {
        PipelineOutcome::Failed {
            entries,
            error,
            history_unchanged,
        } => {
            assert!(matches!(
                error,
                kiana_compaction::CompactionError::StreamFailed { .. }
            ));
            assert!(history_unchanged);
            assert_eq!(serialized(&entries), before);
        }
        other => panic!("expected failure, got {other:?}"),
    }
    assert_eq!(serialized(&history), before);
}

#[tokio::test]
async fn split_turn_preserves_tool_pair_and_emits_turn_prefix() {
    let messages = vec![
        user("do the task"),
        assistant_call("call-1"),
        tool_result("call-1"),
        user("now refine it"),
        assistant("working on it"),
    ];
    let history = entries(messages);
    let split_settings = CompactionSettings {
        keep_recent_tokens: 0,
        ..CompactionSettings::default()
    };
    let preparation =
        kiana_compaction::prepare_compaction(&history, &split_settings).expect("preparation");
    assert!(preparation.is_split_turn);
    assert!(!preparation.turn_prefix_messages.is_empty());
    check_split_turn_pairing(&preparation).expect("pairing intact");

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let stream: kiana_agent_core::types::StreamFn = Arc::new(move |_model, _ctx, _opts| {
        let sequence = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            // 只返回普通 assistant 文本：compact 会把它拼成 split-turn 摘要。
            let message = AssistantMessage {
                role: AssistantRole::Assistant,
                content: vec![AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("summary-{sequence}"),
                    text_signature: None,
                })],
                usage: Default::default(),
                stop_reason: StopReason::Stop,
                timestamp: 0,
                ..Default::default()
            };
            let stream = create_assistant_message_event_stream();
            let _ = stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(message),
            });
            Ok(stream)
        })
    });
    let options = PipelineOptions {
        prune: PruneConfig::default(),
        fallback: None,
        model: CompactModelOptions {
            model: Model::default(),
            stream_fn: Some(stream),
            ..CompactModelOptions::default()
        },
    };
    // 直接调用 compact 验证 turn-prefix 拼接（prepare 的 split-turn 分支）。
    let result = kiana_compaction::compact(
        &preparation,
        &Model::default(),
        None,
        None,
        None,
        None,
        None,
        Some(options.model.stream_fn.unwrap()),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("compact");
    assert!(result.summary.contains("**Turn Context (split turn):**"));
    assert!(result.summary.contains("summary-0"));
    assert!(result.summary.contains("summary-1"));
}

#[test]
fn pairing_violation_is_rejected_before_model_call() {
    let preparation = kiana_compaction::CompactionPreparation {
        first_kept_entry_id: "x".to_string(),
        messages_to_summarize: vec![tool_result("missing-call")],
        turn_prefix_messages: Vec::new(),
        is_split_turn: false,
        tokens_before: 100,
        previous_summary: None,
        file_ops: Default::default(),
        settings: Default::default(),
    };
    let error = check_split_turn_pairing(&preparation).unwrap_err();
    assert!(matches!(error, PipelineError::PairingBroken(_)));
}

#[test]
fn ptl_plans_drop_at_most_three_groups_and_keep_one() {
    let mut messages = Vec::new();
    for index in 0..6 {
        messages.push(user(&format!("u{index}")));
        messages.push(assistant_call(&format!("c{index}")));
        messages.push(tool_result(&format!("c{index}")));
    }
    let plans = plan_ptl_retries(&messages);
    assert_eq!(plans.len(), MAX_PTL_RETRIES);
    assert_eq!(plans.last().unwrap().len(), messages.len() - 3 * 3);
    let (kept, dropped) = drop_oldest_groups(&messages, 100);
    assert_eq!(dropped, 5);
    assert_eq!(kept.len(), 3);

    let mut recovery = OverflowRecovery::new();
    assert_eq!(recovery.on_overflow(), OverflowAction::CompactAndRetry);
    assert!(matches!(
        recovery.on_overflow(),
        OverflowAction::Fail { .. }
    ));
}
