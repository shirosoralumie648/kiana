//! TS: `test/suite/lax-message-content.test.ts`（6 例中的 3 例）。
//!
//! 前 3 例经由 extension harness / 会话运行时（其它 unit），这里无法构造；纯
//! `sessionEntryToContextMessages` 的 3 例完整移植。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_session_core::session_manager::{
    SessionEntry, SessionManager, parse_session_entries, session_entry_to_context_messages,
};

fn message_entry(message: serde_json::Value) -> SessionEntry {
    let message: AgentMessage = serde_json::from_value(message).expect("lenient message");
    let line = serde_json::json!({
        "type": "message",
        "id": "entry-1",
        "parentId": null,
        "timestamp": "2025-01-01T00:00:00.000Z",
        "message": message,
    });
    match parse_session_entries(&line.to_string()).into_iter().next() {
        Some(kiana_session_core::session_manager::FileEntry::Entry(entry)) => entry,
        other => panic!("expected message entry, got {other:?}"),
    }
}

/// TS: "normalizes null or missing content when loading session message entries"
#[test]
fn normalizes_null_or_missing_content_when_loading_session_message_entries() {
    let bad_messages = vec![
        serde_json::json!({"role": "user", "content": null, "timestamp": 0}),
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "api": "openai-completions",
            "provider": "openai",
            "model": "test-model",
            "usage": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
                "totalTokens": 0,
                "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
            },
            "stopReason": "stop",
            "timestamp": 0,
        }),
        serde_json::json!({
            "role": "toolResult",
            "toolCallId": "call_1",
            "toolName": "web_search",
            "isError": false,
            "timestamp": 0,
        }),
    ];

    for bad_message in bad_messages {
        let role = bad_message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string();
        let messages = session_entry_to_context_messages(&message_entry(bad_message));
        assert_eq!(messages.len(), 1);
        let value = message_json(&messages[0]);
        assert_eq!(
            value.get("role").and_then(serde_json::Value::as_str),
            Some(role.as_str())
        );
        assert_eq!(value.get("content"), Some(&serde_json::json!([])));
    }
}

/// TS: "normalizes null content when loading custom message entries"
#[test]
fn normalizes_null_content_when_loading_custom_message_entries() {
    let line = serde_json::json!({
        "type": "custom_message",
        "id": "entry-1",
        "parentId": null,
        "timestamp": "2025-01-01T00:00:00.000Z",
        "customType": "test",
        "content": null,
        "display": false,
    });
    let entry = match parse_session_entries(&line.to_string()).into_iter().next() {
        Some(kiana_session_core::session_manager::FileEntry::Entry(entry)) => entry,
        other => panic!("expected custom message entry, got {other:?}"),
    };

    let messages = session_entry_to_context_messages(&entry);
    assert_eq!(messages.len(), 1);
    let value = message_json(&messages[0]);
    assert_eq!(
        value.get("role").and_then(serde_json::Value::as_str),
        Some("custom")
    );
    assert_eq!(value.get("content"), Some(&serde_json::json!([])));
}

/// TS: "keeps valid message content untouched when loading session entries"
#[test]
fn keeps_valid_message_content_untouched_when_loading_session_entries() {
    let messages = session_entry_to_context_messages(&message_entry(
        serde_json::json!({"role": "user", "content": "hello", "timestamp": 0}),
    ));
    assert_eq!(messages.len(), 1);
    let value = message_json(&messages[0]);
    assert_eq!(
        value.get("role").and_then(serde_json::Value::as_str),
        Some("user")
    );
    assert_eq!(
        value.get("content").and_then(serde_json::Value::as_str),
        Some("hello")
    );
}

/// 回归（评审 #3）：`buildContextEntries` 按原始 `message.role === "system"` 跳过保留区内的
/// system 消息；宽松解析成 `AgentMessage::Custom` 的 `content: null` system 消息同样要跳过。
///
/// TS 参考输出（vitest probe）：
/// `[compactionSummary, user("keep me"), user("after")]`。
#[test]
fn compaction_projection_skips_lenient_system_messages() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"sys1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"system","content":null,"timestamp":0}}"#,
        r#"{"type":"message","id":"u1","parentId":"sys1","timestamp":"2025-01-01T00:00:02.000Z","message":{"role":"user","content":"keep me","timestamp":1}}"#,
        r#"{"type":"compaction","id":"c1","parentId":"u1","timestamp":"2025-01-01T00:00:03.000Z","summary":"sum","firstKeptEntryId":"sys1","tokensBefore":10}"#,
        r#"{"type":"message","id":"u2","parentId":"c1","timestamp":"2025-01-01T00:00:04.000Z","message":{"role":"user","content":"after","timestamp":2}}"#,
    ];
    let entries = parse_session_entries(&lines.join("\n"));
    let manager = SessionManager::in_memory("/tmp", None, Some(entries)).unwrap();

    let ids: Vec<String> = manager
        .build_context_entries()
        .iter()
        .map(|entry| entry.id().to_string())
        .collect();
    assert_eq!(ids, vec!["c1", "u1", "u2"]);

    let projection: Vec<serde_json::Value> = manager
        .build_session_projection()
        .messages
        .iter()
        .map(message_json)
        .collect();
    let expected: Vec<serde_json::Value> = serde_json::from_str(
        r#"[{"role":"compactionSummary","summary":"sum","tokensBefore":10,"timestamp":1735689603000},{"role":"user","content":"keep me","timestamp":1},{"role":"user","content":"after","timestamp":2}]"#,
    )
    .unwrap();
    assert_eq!(projection, expected);
}

/// 回归（评审 #4）：context_edit 的字符串替换对 assistant/toolResult 消息一律转成
/// `[{type:"text",text}]`，包括宽松解析成 `AgentMessage::Custom` 的消息。
///
/// TS 参考输出（vitest probe）：`[{role:"assistant",content:[{type:"text",text:"edited"}],timestamp:0}]`。
#[test]
fn context_edit_string_replacement_on_lenient_assistant_messages() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"assistant","content":"orig","timestamp":0}}"#,
        r#"{"type":"context_edit","id":"e1","parentId":"a1","timestamp":"2025-01-01T00:00:02.000Z","targetId":"a1","replacement":{"content":"edited"}}"#,
    ];
    let entries = parse_session_entries(&lines.join("\n"));
    let manager = SessionManager::in_memory("/tmp", None, Some(entries)).unwrap();
    let projection: Vec<serde_json::Value> = manager
        .build_session_projection()
        .messages
        .iter()
        .map(message_json)
        .collect();
    assert_eq!(
        projection,
        vec![serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "edited"}],
            "timestamp": 0,
        })]
    );
}

/// 回归（评审 #5）：`getSessionContextSettings` 读取原始 `message.role === "assistant"`；
/// 宽松解析成 `AgentMessage::Custom` 的 assistant 消息同样推导出模型。
#[test]
fn context_settings_reads_models_from_lenient_assistant_messages() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"assistant","content":null,"provider":"p","model":"m","timestamp":5}}"#,
    ];
    let entries = parse_session_entries(&lines.join("\n"));
    let manager = SessionManager::in_memory("/tmp", None, Some(entries)).unwrap();
    let context = manager.build_session_context();
    assert_eq!(context.thinking_level, "off");
    let model = context.model.expect("model");
    assert_eq!(model.provider, "p");
    assert_eq!(model.model_id, "m");
}

/// 回归（评审 round 2 #6）：TS 的 `sessionEntryToContextMessages` 原样返回
/// `entry.message`，标准消息对象上的额外键会被保留。Rust 对从文件加载、带额外键的标准
/// 消息回放原始对象（`AgentMessage::Custom`），没有额外键时仍返回 typed `Standard`。
#[test]
fn context_messages_preserve_extra_keys_of_file_loaded_standard_messages() {
    // 不能用 `message_entry` 助手：它会把消息重新序列化成 `AgentMessage`（额外键已丢失）。
    let parse_raw_entry = |message: serde_json::Value| -> SessionEntry {
        let line = serde_json::json!({
            "type": "message",
            "id": "entry-1",
            "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z",
            "message": message,
        });
        match parse_session_entries(&line.to_string()).into_iter().next() {
            Some(kiana_session_core::session_manager::FileEntry::Entry(entry)) => entry,
            other => panic!("expected message entry, got {other:?}"),
        }
    };
    let entry = parse_raw_entry(serde_json::json!({
        "role": "user", "content": "hi", "timestamp": 1, "extra": true,
    }));
    let messages = session_entry_to_context_messages(&entry);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        message_json(&messages[0]),
        serde_json::json!({"role": "user", "content": "hi", "timestamp": 1, "extra": true})
    );

    // 没有额外键：保持 typed Standard（下游可以直接按类型读取）。
    let entry = message_entry(serde_json::json!({
        "role": "user", "content": "hi", "timestamp": 1,
    }));
    let messages = session_entry_to_context_messages(&entry);
    assert!(matches!(&messages[0], AgentMessage::Standard(_)));

    // assistant 的额外键同样保留。
    let entry = parse_raw_entry(serde_json::json!({
        "role": "assistant", "content": [], "api": "a", "provider": "p", "model": "m",
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                  "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": 2, "extra": 9,
    }));
    let value = message_json(&session_entry_to_context_messages(&entry)[0]);
    assert_eq!(value.get("extra"), Some(&serde_json::json!(9)));
    assert_eq!(
        value.get("provider").and_then(|value| value.as_str()),
        Some("p")
    );
}
