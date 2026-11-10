//! `export-jsonl-share.test.ts` 的移植：`exportSessionForShare` 在不改变会话 id / 链接的
//! 前提下追加 `kiana.share` 展示数据。

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{AssistantContentBlock, Message, StopReason, ToolCall, ToolCallKind};
use kiana_interactive::session_share::export_session_for_share;
use kiana_session_core::session_manager::SessionManager;
use serde_json::Value;

use crate::common::{
    FauxSessionOptions, assistant_msg, faux_session, run_local, simple_tool_definition,
    tool_result_msg, user_msg,
};

mod common;

fn read_records(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("read jsonl")
        .trim()
        .split('\n')
        .map(|line| serde_json::from_str(line).expect("json record"))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn adds_presentation_data_without_changing_conversation_ids_or_links() {
    run_local(async {
        let fixture = faux_session(FauxSessionOptions {
            custom_tools: vec![simple_tool_definition(
                "share_tool",
                "Render a value for sharing",
                None,
            )],
            tools: Some(vec!["share_tool".to_string()]),
            ..Default::default()
        })
        .await;
        let temp_dir = tempfile::tempdir().expect("temp dir");

        let (user_id, assistant_id, result_id, original_entry_ids) =
            fixture.session.with_session_manager_mut(|manager| {
                let user_id = manager
                    .append_message(user_msg("hello"))
                    .expect("append user");
                let mut assistant = assistant_msg("");
                let AgentMessage::Standard(Message::Assistant(ref mut message)) = assistant else {
                    unreachable!("assistant_msg")
                };
                message.content = vec![AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: ToolCallKind::ToolCall,
                    id: "call-1".to_string(),
                    name: "share_tool".to_string(),
                    arguments: serde_json::json!({ "value": "example" })
                        .as_object()
                        .expect("object")
                        .clone()
                        .into_iter()
                        .collect(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                })];
                message.stop_reason = StopReason::ToolUse;
                message.error_message = None;
                let assistant_id = manager.append_message(assistant).expect("append assistant");
                let result_id = manager
                    .append_message(tool_result_msg("call-1", "share_tool", "done"))
                    .expect("append result");
                let ids: Vec<String> = manager
                    .get_branch(None)
                    .iter()
                    .map(|entry| entry.id().to_string())
                    .collect();
                (user_id, assistant_id, result_id, ids)
            });

        let normal_path = temp_dir.path().join("normal.jsonl");
        fixture
            .session
            .export_to_jsonl(Some(normal_path.to_string_lossy().into_owned()))
            .expect("normal export");
        let normal_records = read_records(&normal_path);
        assert!(
            !normal_records.iter().any(|record| {
                record.get("type").and_then(Value::as_str) == Some("custom")
                    && record.get("customType").and_then(Value::as_str) == Some("kiana.share")
            }),
            "normal export must not contain kiana.share"
        );

        let share_path = temp_dir.path().join("share.jsonl");
        export_session_for_share(&share_path, &fixture.session).expect("share export");
        let records = read_records(&share_path);
        let conversation = &records[1..records.len() - 1];
        let ids: Vec<&str> = conversation
            .iter()
            .map(|record| record.get("id").and_then(Value::as_str).expect("id"))
            .collect();
        assert_eq!(ids, original_entry_ids);
        let parent_ids: Vec<Value> = conversation
            .iter()
            .map(|record| record.get("parentId").cloned().expect("parentId"))
            .collect();
        let mut expected_parents = vec![Value::Null];
        expected_parents.extend(
            original_entry_ids
                .iter()
                .take(original_entry_ids.len() - 1)
                .map(|id| Value::String(id.clone())),
        );
        assert_eq!(parent_ids, expected_parents);
        let last_three: Vec<&str> = conversation
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|record| record.get("id").and_then(Value::as_str).expect("id"))
            .collect();
        assert_eq!(
            last_three,
            vec![user_id.as_str(), assistant_id.as_str(), result_id.as_str()]
        );

        let share_entry = records.last().expect("share entry");
        assert_eq!(
            share_entry.get("type").and_then(Value::as_str),
            Some("custom")
        );
        assert_eq!(
            share_entry.get("customType").and_then(Value::as_str),
            Some("kiana.share")
        );
        assert_eq!(
            share_entry.get("parentId").and_then(Value::as_str),
            Some(result_id.as_str())
        );
        assert!(
            share_entry
                .get("timestamp")
                .and_then(Value::as_str)
                .is_some()
        );
        let share_id = share_entry
            .get("id")
            .and_then(Value::as_str)
            .expect("share id");
        assert_eq!(share_id.chars().count(), 8, "id is the first 8 UUID chars");

        let data = share_entry.get("data").expect("data");
        assert_eq!(
            data.get("systemPrompt").and_then(Value::as_str),
            Some(fixture.session.state().system_prompt.as_str())
        );
        let tools = data.get("tools").and_then(Value::as_array).expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].get("name").and_then(Value::as_str),
            Some("share_tool")
        );
        assert_eq!(
            tools[0].get("description").and_then(Value::as_str),
            Some("Render a value for sharing")
        );
        let expected_parameters = fixture.session.state().tools[0].parameters.clone();
        assert_eq!(tools[0].get("parameters"), Some(&expected_parameters));
        for absent in ["renderedTools", "theme", "version"] {
            assert!(data.get(absent).is_none(), "data must not contain {absent}");
        }

        let imported = SessionManager::open(&share_path.to_string_lossy(), None, None)
            .expect("open share file");
        assert_eq!(imported.get_leaf_id(), Some(share_id));
        let context = imported.build_session_context();
        let roles: Vec<&str> = context
            .messages
            .iter()
            .map(|message| message.role())
            .collect();
        assert_eq!(roles, vec!["user", "assistant", "toolResult"]);

        fixture.session.dispose();
    })
    .await;
}
