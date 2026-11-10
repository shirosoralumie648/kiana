//! 第 11 周后半：会话恢复（`recovery.rs`）的契约测试。
//!
//! 覆盖 DoD：
//! - 在 tool_calls 与 tool_result 之间 kill：分类为 `InterruptedTurn`，每个悬挂调用
//!   合成一个 failed tool result，稳定 id、错误限长、不重复执行、turn 有标记；
//! - 在 user prompt 与 assistant 之间 kill：分类为 `InterruptedPrompt`，重放尾部 user
//!   一次且 canonical 不新增 user entry；
//! - 孤儿 tool result 只从请求视图移除，canonical transcript 保留；
//! - 分类先于修复，分类/计划不修改 transcript；
//! - 非尾部悬挂调用只在请求视图补结果。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, StopReason, ToolCall,
    ToolCallKind, UserContent,
};
use kiana_session_core::recovery::{
    DEFAULT_RECOVERY_ERROR_MAX_CHARS, DegradedIssueKind, INTERRUPTED_TURN_CUSTOM_TYPE,
    RecoveryClass, RecoveryContinuation, RequestViewRepair, ToolCallRef, classify_recovery,
    plan_recovery, repair_request_view, synthetic_failed_tool_result,
};
use kiana_session_core::session_manager::SessionManager;

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: name.to_string(),
        arguments: arguments
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    })
}

fn assistant_with_calls(calls: &[(&str, &str)], timestamp: i64) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: calls
            .iter()
            .map(|(id, name)| tool_call(id, name, serde_json::json!({"command": "echo hi"})))
            .collect(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "test".to_string(),
        usage: usage_with(1, 1, 2, 0.0),
        stop_reason: StopReason::ToolUse,
        timestamp,
        ..Default::default()
    }))
}

fn manager_with(messages: Vec<AgentMessage>) -> SessionManager {
    let mut manager =
        SessionManager::in_memory("/tmp/kiana-recovery", None, None).expect("in-memory session");
    for message in messages {
        manager.append_message(message).expect("append");
    }
    manager
}

fn entry_count(manager: &SessionManager) -> usize {
    manager.get_branch(None).len()
}

#[test]
fn kill_between_tool_calls_and_results_synthesizes_failed_results_without_reexecution() {
    let mut manager = manager_with(vec![
        user_message("run both checks", 1),
        assistant_with_calls(&[("call_a", "bash"), ("call_b", "write")], 2),
        tool_result_message("call_a", "bash", "ok", 3),
    ]);

    let plan = plan_recovery(&manager.get_branch(None));
    assert_eq!(plan.classification.class, RecoveryClass::InterruptedTurn);
    let turn = plan
        .classification
        .interrupted_turn
        .as_ref()
        .expect("interrupted turn detail");
    assert_eq!(turn.dangling_calls.len(), 1);
    assert_eq!(turn.dangling_calls[0].call_id, "call_b");
    assert_eq!(turn.dangling_calls[0].tool_name, "write");
    // 尾部的悬挂调用只由 InterruptedTurn 处理，不在 degraded 里重复出现。
    assert!(plan.classification.degraded.iter().all(|issue| {
        issue.kind != DegradedIssueKind::DanglingToolCall || issue.tool_call_id != "call_b"
    }));
    assert_eq!(
        plan.request_view_repairs
            .iter()
            .filter(|repair| matches!(
                repair,
                RequestViewRepair::InsertSyntheticToolResult { tool_call_id, .. }
                    if tool_call_id == "call_b"
            ))
            .count(),
        1
    );

    let before = entry_count(&manager);
    let applied = plan.apply(&mut manager).expect("apply recovery");

    // 只给没有结果的调用补一个结果，再写一个 turn 标记。
    assert_eq!(applied.appended_tool_results, vec!["call_b".to_string()]);
    assert_eq!(applied.appended_entry_ids.len(), 2);
    assert_eq!(entry_count(&manager), before + 2);

    let branch = manager.get_branch(None);
    let synthetic = branch
        .iter()
        .filter_map(|entry| entry.message())
        .find_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result))
                if result.tool_call_id == "call_b" =>
            {
                Some(result.clone())
            }
            _ => None,
        })
        .expect("synthetic result for call_b");
    assert!(synthetic.is_error);
    assert_eq!(synthetic.tool_name, "write");
    let text = match &synthetic.content[0] {
        kiana_ai::types::UserContentBlock::Text(text) => text.text.clone(),
        _ => panic!("expected text"),
    };
    assert!(text.chars().count() <= DEFAULT_RECOVERY_ERROR_MAX_CHARS);
    assert!(text.contains("interrupted"));

    // 标记条目存在且不进入 LLM 上下文。
    assert!(branch.iter().any(|entry| {
        matches!(entry, kiana_session_core::session_manager::SessionEntry::Custom(custom)
            if custom.custom_type == INTERRUPTED_TURN_CUSTOM_TYPE)
    }));

    // 幂等：再次应用不会重复追加，也绝不会重新执行工具（本模块没有任何执行入口）。
    let applied_again = plan.apply(&mut manager).expect("second apply");
    assert!(applied_again.appended_tool_results.is_empty());
    assert!(applied_again.appended_entry_ids.is_empty());
    assert_eq!(entry_count(&manager), before + 2);

    // 修复后重新分类应为 Clean。
    let after = classify_recovery(&manager.get_branch(None));
    assert_eq!(after.class, RecoveryClass::Clean, "{after:?}");
}

#[test]
fn kill_between_user_prompt_and_assistant_replays_tail_user_once_without_new_entry() {
    let mut manager = manager_with(vec![
        user_message("first", 1),
        assistant_message("done", 2),
        user_message("second prompt", 3),
    ]);
    let before = entry_count(&manager);

    let plan = plan_recovery(&manager.get_branch(None));
    assert_eq!(plan.classification.class, RecoveryClass::InterruptedPrompt);
    let prompt = plan
        .classification
        .interrupted_prompt
        .as_ref()
        .expect("interrupted prompt detail");
    assert_eq!(
        prompt.entry_id,
        manager.get_leaf_id().expect("leaf").to_string()
    );
    assert!(matches!(
        &prompt.message,
        AgentMessage::Standard(Message::User(user))
            if matches!(&user.content, UserContent::Text(text) if text == "second prompt")
    ));

    match plan.continuation().expect("continuation") {
        RecoveryContinuation::ReplayUser { entry_id, message } => {
            assert_eq!(entry_id, prompt.entry_id);
            assert!(matches!(message, AgentMessage::Standard(Message::User(_))));
        }
        other => panic!("unexpected continuation: {other:?}"),
    }

    // 重放不写 canonical：条目数量不变，且尾部仍然只有一条未回复的 user。
    plan.apply(&mut manager).expect("apply");
    assert_eq!(entry_count(&manager), before);
    let branch = manager.get_branch(None);
    assert_eq!(
        branch
            .iter()
            .filter(|entry| entry
                .message()
                .is_some_and(|message| message.role() == "user"))
            .count(),
        2
    );
    let last_message = branch
        .iter()
        .rev()
        .find_map(|entry| entry.message())
        .expect("last message");
    assert_eq!(last_message.role(), "user");

    // 同一个 plan 再取 continuation 仍是同一个 entry id（调用方按 id 去重）。
    match plan.continuation().expect("continuation again") {
        RecoveryContinuation::ReplayUser { entry_id, .. } => assert_eq!(entry_id, prompt.entry_id),
        other => panic!("unexpected continuation: {other:?}"),
    }
}

#[test]
fn orphan_tool_result_is_removed_from_request_view_but_kept_in_canonical_transcript() {
    let mut manager = manager_with(vec![
        user_message("start", 1),
        tool_result_message("call_orphan", "bash", "stale", 2),
        assistant_message("ok", 3),
    ]);
    let canonical_before = manager.get_branch(None);

    let plan = plan_recovery(&canonical_before);
    assert_eq!(plan.classification.class, RecoveryClass::DegradedHistory);
    assert!(plan.classification.degraded.iter().any(|issue| {
        issue.kind == DegradedIssueKind::OrphanToolResult && issue.tool_call_id == "call_orphan"
    }));
    assert!(plan.canonical_repairs.is_empty());

    let projection = manager.build_session_projection();
    let repaired = repair_request_view(&projection.messages, &plan);
    assert_eq!(repaired.removed_orphan_results, vec!["call_orphan"]);
    assert!(!repaired.messages.iter().any(|message| matches!(
        message,
        AgentMessage::Standard(Message::ToolResult(result))
            if result.tool_call_id == "call_orphan"
    )));

    // canonical transcript 保留孤儿结果，且 apply 不修改它。
    let before_count = entry_count(&manager);
    plan.apply(&mut manager).expect("apply");
    assert_eq!(entry_count(&manager), before_count);
    assert!(manager.get_branch(None).iter().any(|entry| matches!(
        entry.message(),
        Some(AgentMessage::Standard(Message::ToolResult(result)))
            if result.tool_call_id == "call_orphan"
    )));
}

#[test]
fn classification_and_plan_are_read_only() {
    let manager = manager_with(vec![
        user_message("start", 1),
        assistant_with_calls(&[("call_a", "bash")], 2),
    ]);
    let before = manager.get_branch(None);
    let _ = classify_recovery(&before);
    let plan = plan_recovery(&before);
    assert_eq!(plan.classification.class, RecoveryClass::InterruptedTurn);
    assert_eq!(manager.get_branch(None), before);
}

#[test]
fn historical_dangling_call_is_closed_in_request_view_only() {
    let mut manager = manager_with(vec![
        user_message("first", 1),
        assistant_with_calls(&[("call_old", "bash")], 2),
        user_message("second", 3),
        assistant_message("still here", 4),
    ]);
    let canonical_before = manager.get_branch(None);
    assert_eq!(
        classify_recovery(&canonical_before).class,
        RecoveryClass::DegradedHistory
    );

    let plan = plan_recovery(&canonical_before);
    assert!(plan.canonical_repairs.is_empty());
    assert!(plan.request_view_repairs.iter().any(|repair| matches!(
        repair,
        RequestViewRepair::InsertSyntheticToolResult { tool_call_id, .. }
            if tool_call_id == "call_old"
    )));

    let projection = manager.build_session_projection();
    let repaired = repair_request_view(&projection.messages, &plan);
    assert_eq!(repaired.synthesized_tool_results, vec!["call_old"]);
    // 合成结果紧跟 assistant，且在后续 user 之前。
    let positions: Vec<(usize, &str)> = repaired
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| match message {
            AgentMessage::Standard(Message::Assistant(_)) => Some((index, "assistant")),
            AgentMessage::Standard(Message::ToolResult(result)) => {
                Some((index, result.tool_call_id.as_str()))
            }
            AgentMessage::Standard(Message::User(_)) => Some((index, "user")),
            _ => None,
        })
        .collect();
    let assistant_at = positions
        .iter()
        .position(|(_, kind)| *kind == "assistant")
        .expect("assistant");
    assert_eq!(positions[assistant_at + 1].1, "call_old");
    assert_eq!(positions[assistant_at + 2].1, "user");

    // canonical 不变。
    plan.apply(&mut manager).expect("apply");
    assert_eq!(manager.get_branch(None), canonical_before);
}

#[test]
fn synthetic_result_id_and_error_length_are_stable_and_bounded() {
    let call = ToolCallRef {
        call_id: "call_stable".to_string(),
        tool_name: "bash".to_string(),
    };
    let first = synthetic_failed_tool_result(&call, 40);
    let second = synthetic_failed_tool_result(&call, 40);
    let AgentMessage::Standard(Message::ToolResult(first)) = &first else {
        panic!("expected tool result");
    };
    let AgentMessage::Standard(Message::ToolResult(second)) = &second else {
        panic!("expected tool result");
    };
    assert_eq!(first.tool_call_id, "call_stable");
    assert_eq!(second.tool_call_id, first.tool_call_id);
    let text = match &first.content[0] {
        kiana_ai::types::UserContentBlock::Text(text) => &text.text,
        _ => panic!("expected text"),
    };
    assert!(text.chars().count() <= 40, "{}", text.chars().count());
    assert!(text.ends_with("..."));
}

#[test]
fn clean_tail_with_assistant_text_is_not_treated_as_interrupted() {
    let manager = manager_with(vec![user_message("q", 1), assistant_message("a", 2)]);
    let classification = classify_recovery(&manager.get_branch(None));
    assert_eq!(classification.class, RecoveryClass::Clean);
    assert!(classification.interrupted_prompt.is_none());
    assert!(classification.interrupted_turn.is_none());
}

#[test]
fn repair_request_view_keeps_real_results_and_synthesizes_only_missing_ones() {
    let manager = manager_with(vec![
        user_message("run both", 1),
        assistant_with_calls(&[("call_a", "bash"), ("call_b", "write")], 2),
        tool_result_message("call_a", "bash", "ok", 3),
    ]);
    let plan = plan_recovery(&manager.get_branch(None));
    assert_eq!(plan.classification.class, RecoveryClass::InterruptedTurn);

    let projection = manager.build_session_projection();
    let repaired = repair_request_view(&projection.messages, &plan);
    assert_eq!(
        repaired.synthesized_tool_results,
        vec!["call_b".to_string()],
        "only the unanswered call may be synthesized"
    );
    assert!(repaired.removed_orphan_results.is_empty());
    // 真实结果 call_a 必须保留；call_b 只出现一次（合成），两者都在请求视图里。
    let result_ids: Vec<String> = repaired
        .messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::ToolResult(result)) => {
                Some(result.tool_call_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(result_ids.len(), 2);
    assert!(result_ids.contains(&"call_a".to_string()));
    assert!(result_ids.contains(&"call_b".to_string()));
}

#[test]
fn repair_request_view_of_a_clean_conversation_is_identity() {
    let manager = manager_with(vec![
        user_message("run", 1),
        assistant_with_calls(&[("call_a", "bash")], 2),
        tool_result_message("call_a", "bash", "ok", 3),
        assistant_message("done", 4),
    ]);
    let plan = plan_recovery(&manager.get_branch(None));
    assert_eq!(plan.classification.class, RecoveryClass::Clean);
    let projection = manager.build_session_projection();
    let repaired = repair_request_view(&projection.messages, &plan);
    assert_eq!(repaired.messages, projection.messages);
}
