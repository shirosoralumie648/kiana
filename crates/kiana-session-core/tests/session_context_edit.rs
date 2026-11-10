//! TS: `test/session-context-edit.test.ts`（18 例中的 9 例）。
//!
//! 其余 9 例依赖 `core/compaction` 的 `prepareCompaction` /
//! `estimateProjectedContextTokens`；本 crate 不能反向依赖尚未实现的 `kiana-compaction`
//! （会造成依赖环），已在交付报告里列为 remaining gap。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::Message;
use kiana_session_core::session_manager::{
    ContextEditReplacement, FileEntry, SessionEntry, SessionManager,
};

fn session() -> SessionManager {
    SessionManager::in_memory(&current_cwd(), None, None).unwrap()
}

fn assistant(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(kiana_ai::types::AssistantMessage {
        role: kiana_ai::types::AssistantRole::Assistant,
        content: vec![kiana_ai::types::AssistantContentBlock::Text(text_block(
            text,
        ))],
        api: "faux".to_string(),
        provider: "faux".to_string(),
        model: "faux".to_string(),
        usage: usage_with(10, 1, 11, 0.0),
        stop_reason: kiana_ai::types::StopReason::Stop,
        timestamp: 0,
        ..Default::default()
    }))
}

fn message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            kiana_ai::types::UserContent::Text(text) => text.clone(),
            kiana_ai::types::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect(),
        },
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                kiana_ai::types::AssistantContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect(),
        other => panic!("expected text message, got {other:?}"),
    }
}

fn role(message: &AgentMessage) -> &str {
    message.role()
}

/// TS: "omits a target only from model projection"
#[test]
fn omits_a_target_only_from_model_projection() {
    let mut session = session();
    session.append_message(user_message("request", 0)).unwrap();
    let assistant_id = session.append_message(assistant("partial")).unwrap();
    let mut result = tool_result_message("call-1", "read", "raw output", 0);
    if let AgentMessage::Standard(Message::ToolResult(tool_result)) = &mut result {
        tool_result.details = Some(serde_json::json!({"path": "large.txt"}));
        tool_result.is_error = true;
    }
    let result_id = session.append_message(result).unwrap();
    session.append_context_edit(&assistant_id, None).unwrap();
    session.append_context_edit(&result_id, None).unwrap();

    assert_eq!(
        session
            .get_branch(None)
            .iter()
            .filter(|entry| entry.kind() == "message")
            .count(),
        3
    );
    let projection = session.build_session_projection();
    assert_eq!(
        projection
            .messages
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        ["user"]
    );
    match session.get_entry(&result_id).unwrap() {
        SessionEntry::Message(entry) => {
            let AgentMessage::Standard(Message::ToolResult(tool_result)) = &entry.message else {
                panic!("expected toolResult");
            };
            assert_eq!(
                tool_result.details,
                Some(serde_json::json!({"path": "large.txt"}))
            );
            assert!(tool_result.is_error);
        }
        other => panic!("expected message, got {other:?}"),
    }
}

/// TS: "replaces only content and lets the latest edit win"
#[test]
fn replaces_only_content_and_lets_the_latest_edit_win() {
    let mut session = session();
    let target_id = session.append_message(assistant("original")).unwrap();
    session
        .append_context_edit(
            &target_id,
            Some(ContextEditReplacement {
                content: serde_json::json!([{"type": "text", "text": "first"}]),
            }),
        )
        .unwrap();
    session.append_context_edit(&target_id, None).unwrap();
    session
        .append_context_edit(
            &target_id,
            Some(ContextEditReplacement {
                content: serde_json::json!([{"type": "text", "text": "restored"}]),
            }),
        )
        .unwrap();

    let projected = &session.build_session_projection().messages[0];
    assert_eq!(role(projected), "assistant");
    assert_eq!(message_text(projected), "restored");
    match projected {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            assert_eq!(assistant.usage.total_tokens, 11);
        }
        other => panic!("expected assistant, got {other:?}"),
    }
    match session.get_entry(&target_id).unwrap() {
        SessionEntry::Message(entry) => {
            assert_eq!(message_text(&entry.message), "original");
        }
        other => panic!("expected message, got {other:?}"),
    }
}

/// TS: "normalizes string replacements for array-only assistant and tool-result roles"
#[test]
fn normalizes_string_replacements_for_array_only_assistant_and_tool_result_roles() {
    let mut session = session();
    let assistant_id = session.append_message(assistant("original")).unwrap();
    let result_id = session
        .append_message(tool_result_message("call-1", "read", "original result", 0))
        .unwrap();
    let assistant_edit_id = session
        .append_context_edit(
            &assistant_id,
            Some(ContextEditReplacement {
                content: serde_json::json!("assistant replacement"),
            }),
        )
        .unwrap();
    let result_edit_id = session
        .append_context_edit(
            &result_id,
            Some(ContextEditReplacement {
                content: serde_json::json!("result replacement"),
            }),
        )
        .unwrap();

    match session.get_entry(&assistant_edit_id).unwrap() {
        SessionEntry::ContextEdit(edit) => {
            assert_eq!(
                edit.replacement.as_ref().unwrap().content,
                serde_json::json!([{"type": "text", "text": "assistant replacement"}])
            );
        }
        other => panic!("expected context_edit, got {other:?}"),
    }
    match session.get_entry(&result_edit_id).unwrap() {
        SessionEntry::ContextEdit(edit) => {
            assert_eq!(
                edit.replacement.as_ref().unwrap().content,
                serde_json::json!([{"type": "text", "text": "result replacement"}])
            );
        }
        other => panic!("expected context_edit, got {other:?}"),
    }

    let projected = session.build_session_projection().messages;
    let assistant_json = message_json(&projected[0]);
    assert_eq!(
        assistant_json
            .get("role")
            .and_then(serde_json::Value::as_str),
        Some("assistant")
    );
    assert_eq!(
        assistant_json.get("content"),
        Some(&serde_json::json!([{"type": "text", "text": "assistant replacement"}]))
    );
    let result_json = message_json(&projected[1]);
    assert_eq!(
        result_json.get("role").and_then(serde_json::Value::as_str),
        Some("toolResult")
    );
    assert_eq!(
        result_json.get("content"),
        Some(&serde_json::json!([{"type": "text", "text": "result replacement"}]))
    );
}

/// TS: "normalizes imported string replacements while projecting array-only roles"
#[test]
fn normalizes_imported_string_replacements_while_projecting_array_only_roles() {
    // 导入的条目（context_edit 的 replacement 是字符串）在投影时必须归一化为块数组。
    let entries = vec![
        FileEntry::Header(session_header("imported", Some("/project"), None)),
        FileEntry::Entry(message_entry(
            "assistant-1",
            None,
            "2025-01-01T00:00:00Z",
            assistant("original"),
        )),
        FileEntry::Entry(custom_edit_entry(
            "edit-1",
            Some("assistant-1"),
            "assistant-1",
            Some(serde_json::json!("imported replacement")),
        )),
    ];
    let session = SessionManager::in_memory("/project", None, Some(entries)).unwrap();

    let projected = &session.build_session_projection().messages[0];
    let value = message_json(projected);
    assert_eq!(
        value.get("role").and_then(serde_json::Value::as_str),
        Some("assistant")
    );
    assert_eq!(
        value.get("content"),
        Some(&serde_json::json!([{"type": "text", "text": "imported replacement"}]))
    );
}

fn custom_edit_entry(
    id: &str,
    parent_id: Option<&str>,
    target_id: &str,
    replacement: Option<serde_json::Value>,
) -> SessionEntry {
    SessionEntry::ContextEdit(kiana_session_core::session_manager::ContextEditEntry {
        kind: kiana_session_core::session_manager::ContextEditType::Marker,
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        target_id: target_id.to_string(),
        replacement: replacement.map(|content| ContextEditReplacement { content }),
        raw_entry: None,
    })
}

/// TS: "keeps edits branch-relative"
#[test]
fn keeps_edits_branch_relative() {
    let mut session = session();
    let target_id = session.append_message(user_message("original", 0)).unwrap();
    session
        .append_context_edit(
            &target_id,
            Some(ContextEditReplacement {
                content: serde_json::json!("edited"),
            }),
        )
        .unwrap();
    assert_eq!(
        message_text(&session.build_session_projection().messages[0]),
        "edited"
    );

    session.branch(&target_id).unwrap();
    assert_eq!(
        message_text(&session.build_session_projection().messages[0]),
        "original"
    );
}

/// TS: "uses a self-referencing compaction to retain no preceding entries"
#[test]
fn uses_a_self_referencing_compaction_to_retain_no_preceding_entries() {
    let mut session = session();
    session
        .append_message(user_message("discarded", 0))
        .unwrap();
    let compaction_id = session
        .append_compaction("exact handoff", None, 100.0, None, None, None)
        .unwrap();
    session.append_message(user_message("after", 0)).unwrap();

    match session.get_entry(&compaction_id).unwrap() {
        SessionEntry::Compaction(compaction) => {
            assert_eq!(compaction.first_kept_entry_id, compaction_id);
        }
        other => panic!("expected compaction, got {other:?}"),
    }
    let projection = session.build_session_projection();
    assert_eq!(
        projection
            .messages
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        ["compactionSummary", "user"]
    );
    let summaries: Vec<String> = projection
        .messages
        .iter()
        .map(|message| {
            let value = message_json(message);
            value
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| message_text(message))
        })
        .collect();
    assert_eq!(summaries, ["exact handoff", "after"]);
}

/// TS: "applies post-compaction edits to retained pre-compaction entries"
#[test]
fn applies_post_compaction_edits_to_retained_pre_compaction_entries() {
    let mut session = session();
    session
        .append_message(user_message("summarized", 0))
        .unwrap();
    let retained_id = session
        .append_message(user_message("original retained", 0))
        .unwrap();
    session
        .append_compaction("summary", Some(&retained_id), 100.0, None, None, None)
        .unwrap();
    session
        .append_context_edit(
            &retained_id,
            Some(ContextEditReplacement {
                content: serde_json::json!("edited retained"),
            }),
        )
        .unwrap();

    let projection = session.build_session_projection();
    let texts: Vec<String> = projection
        .messages
        .iter()
        .map(|message| {
            let value = message_json(message);
            value
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| message_text(message))
        })
        .collect();
    assert_eq!(texts, ["summary", "edited retained"]);
}

/// TS: "uses only the newest summary when a repeated compaction retains entries before the older compaction"
#[test]
fn uses_only_the_newest_summary_when_a_repeated_compaction_retains_entries_before_the_older_compaction()
 {
    let mut session = session();
    session
        .append_message(user_message("summarized first", 0))
        .unwrap();
    let retained_id = session.append_message(user_message("retained", 0)).unwrap();
    session
        .append_compaction("first summary", Some(&retained_id), 100.0, None, None, None)
        .unwrap();
    session
        .append_message(assistant("after first compaction"))
        .unwrap();
    session
        .append_compaction("second summary", Some(&retained_id), 80.0, None, None, None)
        .unwrap();
    session
        .append_message(user_message(&"new tail ".repeat(100), 0))
        .unwrap();

    let summaries: Vec<String> = session
        .build_session_projection()
        .messages
        .iter()
        .filter_map(|message| {
            message_json(message)
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(summaries, ["second summary"]);
}

/// TS: "supports repeated retain-none compactions"
#[test]
fn supports_repeated_retain_none_compactions() {
    let mut session = session();
    session
        .append_message(user_message("discarded", 0))
        .unwrap();
    session
        .append_compaction("first handoff", None, 100.0, None, None, None)
        .unwrap();
    session
        .append_message(user_message("also discarded", 0))
        .unwrap();
    let second_id = session
        .append_compaction("second handoff", None, 50.0, None, None, None)
        .unwrap();

    match session.get_entry(&second_id).unwrap() {
        SessionEntry::Compaction(compaction) => {
            assert_eq!(compaction.first_kept_entry_id, second_id);
        }
        other => panic!("expected compaction, got {other:?}"),
    }
    let summaries: Vec<String> = session
        .build_session_projection()
        .messages
        .iter()
        .map(|message| {
            message_json(message)
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(summaries, ["second handoff"]);
}

/// spec §2.2.10：`appendContextEdit` 的校验顺序与逐字错误文本。
#[test]
fn validates_replacements_and_targets_in_typescript_order() {
    let mut session = session();
    let user_id = session.append_message(user_message("request", 0)).unwrap();
    let assistant_id = session.append_message(assistant("partial")).unwrap();
    let custom_id = session.append_custom_entry("ext", None).unwrap();
    let system_id = session
        .append_message(AgentMessage::Standard(Message::System(
            kiana_ai::types::SystemMessage {
                role: kiana_ai::types::SystemRole::System,
                content: kiana_ai::types::SystemContent::Text("prompt".to_string()),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: 0,
            },
        )))
        .unwrap();

    // 1) 替换内容校验先于目标查找（目标不存在也报替换错误）。
    for invalid in [
        serde_json::json!({"bad": true}),
        serde_json::json!(5),
        serde_json::json!(null),
    ] {
        let error = error_text(
            session
                .append_context_edit("missing", Some(ContextEditReplacement { content: invalid })),
        );
        assert_eq!(
            error,
            "Context edit replacement must be null or contain string/array content"
        );
    }
    // 字符串/数组是合法内容；此时才检查目标是否存在。
    let error = error_text(session.append_context_edit(
        "missing",
        Some(ContextEditReplacement {
            content: serde_json::json!("replacement"),
        }),
    ));
    assert_eq!(error, "Entry missing not found");
    let error = error_text(session.append_context_edit("missing", None));
    assert_eq!(error, "Entry missing not found");

    // 2) 目标存在且在活动分支上，但类型不可编辑（custom / system）。
    let error = error_text(session.append_context_edit(&custom_id, None));
    assert_eq!(
        error,
        format!("Entry {custom_id} does not contribute editable model content")
    );
    let error = error_text(session.append_context_edit(&system_id, None));
    assert_eq!(
        error,
        format!("Entry {system_id} does not contribute editable model content")
    );

    // 3) 目标存在但不在活动分支。
    session.branch(&user_id).unwrap();
    session
        .append_message(user_message("another branch", 0))
        .unwrap();
    let error = error_text(session.append_context_edit(&assistant_id, None));
    assert_eq!(
        error,
        format!("Entry {assistant_id} is not on the active branch")
    );
}
