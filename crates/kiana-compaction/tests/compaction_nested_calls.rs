//! `compaction-nested-calls.test.ts` 的 Rust 移植。
//!
//! TS: `packages/coding-agent/test/compaction-nested-calls.test.ts`

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{Message, NestedToolCalls, ToolResultMessage, ToolResultRole};
use kiana_compaction::{compute_file_lists, create_file_ops, extract_file_ops_from_message};

/// TS: it("include files touched by nested calls recorded on tool results")
#[test]
fn include_files_touched_by_nested_calls_recorded_on_tool_results() {
    let result = AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "codemode-1".to_string(),
        tool_name: "codemode".to_string(),
        content: Vec::new(),
        details: None,
        usage: None,
        nested_calls: Some(NestedToolCalls {
            calls: vec![
                serde_json::json!({
                    "id": "codemode-1/1",
                    "name": "read",
                    "arguments": { "path": "a.ts" },
                    "status": "ok",
                }),
                serde_json::json!({
                    "id": "codemode-1/2",
                    "name": "edit",
                    "arguments": { "path": "b.ts", "edits": [] },
                    "status": "ok",
                }),
                // `argumentsBytes` 取代 `arguments`：没有 path，忽略。
                serde_json::json!({
                    "id": "codemode-1/3",
                    "name": "write",
                    "argumentsBytes": 40000,
                    "status": "ok",
                }),
            ],
            complete: false,
        }),
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }));

    let mut file_ops = create_file_ops();
    extract_file_ops_from_message(&result, &mut file_ops);
    let lists = compute_file_lists(&file_ops);
    assert_eq!(lists.read_files, vec!["a.ts"]);
    assert_eq!(lists.modified_files, vec!["b.ts"]);
}

/// spec §2.19.1：`addFileOp` 只认 `path` 参数名、只认 read/write/edit，空路径被忽略；
/// toolResult 只读 `nestedCalls`（不再看 content）；非 assistant/toolResult 角色忽略。
#[test]
fn file_operations_follow_the_ts_extraction_rules() {
    fn assistant_tool_call(
        name: &str,
        arguments: indexmap::IndexMap<String, serde_json::Value>,
    ) -> AgentMessage {
        AgentMessage::Standard(Message::Assistant(kiana_ai::types::AssistantMessage {
            role: kiana_ai::types::AssistantRole::Assistant,
            content: vec![kiana_ai::types::AssistantContentBlock::ToolCall(
                kiana_ai::types::ToolCall {
                    kind_tool_call: kiana_ai::types::ToolCallKind::ToolCall,
                    id: "call-1".to_string(),
                    name: name.to_string(),
                    arguments,
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                },
            )],
            stop_reason: kiana_ai::types::StopReason::ToolUse,
            timestamp: 0,
            ..kiana_ai::types::AssistantMessage::default()
        }))
    }

    let args =
        |pairs: &[(&str, serde_json::Value)]| -> indexmap::IndexMap<String, serde_json::Value> {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect()
        };

    let mut file_ops = create_file_ops();

    // `file_path` 不是 `path`：不记录。
    extract_file_ops_from_message(
        &assistant_tool_call("read", args(&[("file_path", serde_json::json!("a.ts"))])),
        &mut file_ops,
    );
    // 未知工具即使有 path 也不记录。
    extract_file_ops_from_message(
        &assistant_tool_call("bash", args(&[("path", serde_json::json!("b.ts"))])),
        &mut file_ops,
    );
    // 空路径是 JS 假值：不记录。
    extract_file_ops_from_message(
        &assistant_tool_call("write", args(&[("path", serde_json::json!(""))])),
        &mut file_ops,
    );
    assert!(file_ops.read.is_empty());
    assert!(file_ops.written.is_empty());
    assert!(file_ops.edited.is_empty());

    // `edit` 与 `write` 分别进入 edited / written。
    extract_file_ops_from_message(
        &assistant_tool_call("edit", args(&[("path", serde_json::json!("c.ts"))])),
        &mut file_ops,
    );
    extract_file_ops_from_message(
        &assistant_tool_call("write", args(&[("path", serde_json::json!("d.ts"))])),
        &mut file_ops,
    );
    assert_eq!(
        file_ops.edited.iter().cloned().collect::<Vec<_>>(),
        vec!["c.ts"]
    );
    assert_eq!(
        file_ops.written.iter().cloned().collect::<Vec<_>>(),
        vec!["d.ts"]
    );

    // toolResult 没有 nestedCalls 时直接返回：即使 content 里带 toolCall 也不看。
    let result = AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call-1".to_string(),
        tool_name: "read".to_string(),
        content: vec![kiana_ai::types::UserContentBlock::Text(
            kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: "ignored".to_string(),
                text_signature: None,
            },
        )],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }));
    let mut result_ops = create_file_ops();
    extract_file_ops_from_message(&result, &mut result_ops);
    assert!(result_ops.read.is_empty());
}
