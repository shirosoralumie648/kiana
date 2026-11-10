//! TS: `test/session-manager/build-context.test.ts`（15 例）。
//!
//! 每个 `it` 的标题原样保留在测试函数文档注释里。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{Message, UserContent};
use kiana_session_core::session_manager::{
    FileEntry, LeafId, ModelRef, SessionContext, SessionEntry, build_context_entries,
    build_session_context, parse_session_entries,
};

fn msg(id: &str, parent_id: Option<&str>, role: &str, text: &str) -> SessionEntry {
    let message = if role == "user" {
        user_message(text, 1)
    } else {
        assistant_message_full(text, 1)
    };
    message_entry(id, parent_id, "2025-01-01T00:00:00Z", message)
}

fn context(entries: &[SessionEntry], leaf_id: LeafId<'_>) -> SessionContext {
    build_session_context(entries, leaf_id, None)
}

fn roles(context: &SessionContext) -> Vec<&str> {
    context.messages.iter().map(AgentMessage::role).collect()
}

fn user_text(message: &AgentMessage) -> &str {
    match message {
        AgentMessage::Standard(Message::User(user)) => match &user.content {
            UserContent::Text(text) => text,
            UserContent::Blocks(_) => panic!("expected string user content"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
}

fn assistant_text(message: &AgentMessage) -> &str {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => match assistant.content.first() {
            Some(kiana_ai::types::AssistantContentBlock::Text(text)) => &text.text,
            _ => panic!("expected text block"),
        },
        other => panic!("expected assistant message, got {other:?}"),
    }
}

fn summary_field(message: &AgentMessage, field: &str) -> String {
    let value = message_json(message);
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("missing field {field} in {value}"))
        .to_string()
}

mod trivial_cases {
    use super::*;

    /// TS: "empty entries returns empty context"
    #[test]
    fn empty_entries_returns_empty_context() {
        let ctx = context(&[], LeafId::Unspecified);
        assert!(ctx.messages.is_empty());
        assert_eq!(ctx.thinking_level, "off");
        assert!(ctx.model.is_none());
    }

    /// TS: "single user message"
    #[test]
    fn single_user_message() {
        let entries = vec![msg("1", None, "user", "hello")];
        let ctx = context(&entries, LeafId::Unspecified);
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role(), "user");
    }

    /// TS: "simple conversation"
    #[test]
    fn simple_conversation() {
        let entries = vec![
            msg("1", None, "user", "hello"),
            msg("2", Some("1"), "assistant", "hi there"),
            msg("3", Some("2"), "user", "how are you"),
            msg("4", Some("3"), "assistant", "great"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);
        assert_eq!(ctx.messages.len(), 4);
        assert_eq!(roles(&ctx), ["user", "assistant", "user", "assistant"]);
    }

    /// TS: "tracks thinking level changes"
    #[test]
    fn tracks_thinking_level_changes() {
        let entries = vec![
            msg("1", None, "user", "hello"),
            thinking_level_entry("2", Some("1"), "high"),
            msg("3", Some("2"), "assistant", "thinking hard"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);
        assert_eq!(ctx.thinking_level, "high");
        assert_eq!(ctx.messages.len(), 2);
    }

    /// TS: "tracks model from assistant message"
    #[test]
    fn tracks_model_from_assistant_message() {
        let entries = vec![
            msg("1", None, "user", "hello"),
            msg("2", Some("1"), "assistant", "hi"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);
        assert_eq!(
            ctx.model,
            Some(ModelRef {
                provider: "anthropic".to_string(),
                model_id: "claude-test".to_string(),
            })
        );
    }

    /// TS: "tracks model from model change entry"
    #[test]
    fn tracks_model_from_model_change_entry() {
        let entries = vec![
            msg("1", None, "user", "hello"),
            model_change_entry("2", Some("1"), "openai", "gpt-4"),
            msg("3", Some("2"), "assistant", "hi"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);
        // assistant 消息覆盖 model_change。
        assert_eq!(
            ctx.model,
            Some(ModelRef {
                provider: "anthropic".to_string(),
                model_id: "claude-test".to_string(),
            })
        );
    }
}

mod with_compaction {
    use super::*;

    /// TS: "includes summary before kept messages"
    #[test]
    fn includes_summary_before_kept_messages() {
        let entries = vec![
            msg("1", None, "user", "first"),
            msg("2", Some("1"), "assistant", "response1"),
            msg("3", Some("2"), "user", "second"),
            msg("4", Some("3"), "assistant", "response2"),
            compaction_entry("5", Some("4"), "Summary of first two turns", "3"),
            msg("6", Some("5"), "user", "third"),
            msg("7", Some("6"), "assistant", "response3"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);

        // summary + 保留的 (3,4) + 之后的 (6,7) = 5 条消息。
        assert_eq!(ctx.messages.len(), 5);
        assert!(summary_field(&ctx.messages[0], "summary").contains("Summary of first two turns"));
        assert_eq!(user_text(&ctx.messages[1]), "second");
        assert_eq!(assistant_text(&ctx.messages[2]), "response2");
        assert_eq!(user_text(&ctx.messages[3]), "third");
        assert_eq!(assistant_text(&ctx.messages[4]), "response3");
    }

    /// TS: "handles compaction keeping from first message"
    #[test]
    fn handles_compaction_keeping_from_first_message() {
        let entries = vec![
            msg("1", None, "user", "first"),
            msg("2", Some("1"), "assistant", "response"),
            compaction_entry("3", Some("2"), "Empty summary", "1"),
            msg("4", Some("3"), "user", "second"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);

        // summary + 全部消息 (1,2,4)。
        assert_eq!(ctx.messages.len(), 4);
        assert!(summary_field(&ctx.messages[0], "summary").contains("Empty summary"));
    }

    /// TS: "multiple compactions uses latest"
    #[test]
    fn multiple_compactions_uses_latest() {
        let entries = vec![
            msg("1", None, "user", "a"),
            msg("2", Some("1"), "assistant", "b"),
            compaction_entry("3", Some("2"), "First summary", "1"),
            msg("4", Some("3"), "user", "c"),
            msg("5", Some("4"), "assistant", "d"),
            compaction_entry("6", Some("5"), "Second summary", "4"),
            msg("7", Some("6"), "user", "e"),
        ];
        let ctx = context(&entries, LeafId::Unspecified);

        // 使用第二个 summary，从 4 开始保留。
        assert_eq!(ctx.messages.len(), 4);
        assert!(summary_field(&ctx.messages[0], "summary").contains("Second summary"));
    }

    /// TS: "buildContextEntries returns compaction-aware entries including custom entries"
    #[test]
    fn build_context_entries_returns_compaction_aware_entries_including_custom_entries() {
        let entries = vec![
            msg("1", None, "user", "first"),
            custom_entry(
                "2",
                Some("1"),
                "old-state",
                Some(serde_json::json!({"hidden": true})),
            ),
            msg("3", Some("2"), "assistant", "response1"),
            custom_entry(
                "4",
                Some("3"),
                "kept-card",
                Some(serde_json::json!({"title": "Kept"})),
            ),
            msg("5", Some("4"), "user", "second"),
            compaction_entry("6", Some("5"), "Summary", "4"),
            custom_entry(
                "7",
                Some("6"),
                "after-card",
                Some(serde_json::json!({"title": "After"})),
            ),
            msg("8", Some("7"), "assistant", "response2"),
        ];

        let ids: Vec<String> = build_context_entries(&entries, LeafId::Unspecified, None)
            .iter()
            .map(|entry| entry.id().to_string())
            .collect();
        assert_eq!(ids, ["6", "4", "5", "7", "8"]);

        let ctx = context(&entries, LeafId::Unspecified);
        assert_eq!(roles(&ctx), ["compactionSummary", "user", "assistant"]);
    }

    /// TS: "keeps settings from the full path after compaction"
    #[test]
    fn keeps_settings_from_the_full_path_after_compaction() {
        let entries = vec![
            msg("1", None, "user", "first"),
            thinking_level_entry("2", Some("1"), "high"),
            msg("3", Some("2"), "assistant", "response1"),
            msg("4", Some("3"), "user", "second"),
            compaction_entry("5", Some("4"), "Summary", "4"),
        ];

        let ctx = context(&entries, LeafId::Unspecified);
        assert_eq!(ctx.thinking_level, "high");
        assert_eq!(roles(&ctx), ["compactionSummary", "user"]);
    }
}

mod with_branches {
    use super::*;

    /// TS: "follows path to specified leaf"
    #[test]
    fn follows_path_to_specified_leaf() {
        let entries = vec![
            msg("1", None, "user", "start"),
            msg("2", Some("1"), "assistant", "response"),
            msg("3", Some("2"), "user", "branch A"),
            msg("4", Some("2"), "user", "branch B"),
        ];

        let ctx_a = context(&entries, LeafId::Id("3"));
        assert_eq!(ctx_a.messages.len(), 3);
        assert_eq!(user_text(&ctx_a.messages[2]), "branch A");

        let ctx_b = context(&entries, LeafId::Id("4"));
        assert_eq!(ctx_b.messages.len(), 3);
        assert_eq!(user_text(&ctx_b.messages[2]), "branch B");
    }

    /// TS: "includes branch summary in path"
    #[test]
    fn includes_branch_summary_in_path() {
        let entries = vec![
            msg("1", None, "user", "start"),
            msg("2", Some("1"), "assistant", "response"),
            msg("3", Some("2"), "user", "abandoned path"),
            branch_summary_entry("4", Some("2"), "Summary of abandoned work", "3"),
            msg("5", Some("4"), "user", "new direction"),
        ];
        let ctx = context(&entries, LeafId::Id("5"));

        assert_eq!(ctx.messages.len(), 4);
        assert!(summary_field(&ctx.messages[2], "summary").contains("Summary of abandoned work"));
        assert_eq!(user_text(&ctx.messages[3]), "new direction");
    }

    /// TS: "complex tree with multiple branches and compaction"
    #[test]
    fn complex_tree_with_multiple_branches_and_compaction() {
        let entries = vec![
            msg("1", None, "user", "start"),
            msg("2", Some("1"), "assistant", "r1"),
            msg("3", Some("2"), "user", "q2"),
            msg("4", Some("3"), "assistant", "r2"),
            compaction_entry("5", Some("4"), "Compacted history", "3"),
            msg("6", Some("5"), "user", "q3"),
            msg("7", Some("6"), "assistant", "r3"),
            msg("8", Some("3"), "user", "wrong path"),
            msg("9", Some("8"), "assistant", "wrong response"),
            branch_summary_entry("10", Some("3"), "Tried wrong approach", "9"),
            msg("11", Some("10"), "user", "better approach"),
        ];

        // 到 7 的主路径：summary + 保留的 (3,4) + 之后的 (6,7)。
        let ctx_main = context(&entries, LeafId::Id("7"));
        assert_eq!(ctx_main.messages.len(), 5);
        assert!(summary_field(&ctx_main.messages[0], "summary").contains("Compacted history"));
        assert_eq!(user_text(&ctx_main.messages[1]), "q2");
        assert_eq!(assistant_text(&ctx_main.messages[2]), "r2");
        assert_eq!(user_text(&ctx_main.messages[3]), "q3");
        assert_eq!(assistant_text(&ctx_main.messages[4]), "r3");

        // 到 11 的分支路径：1,2,3 + branch_summary + 11。
        let ctx_branch = context(&entries, LeafId::Id("11"));
        assert_eq!(ctx_branch.messages.len(), 5);
        assert_eq!(user_text(&ctx_branch.messages[0]), "start");
        assert_eq!(assistant_text(&ctx_branch.messages[1]), "r1");
        assert_eq!(user_text(&ctx_branch.messages[2]), "q2");
        assert!(summary_field(&ctx_branch.messages[3], "summary").contains("Tried wrong approach"));
        assert_eq!(user_text(&ctx_branch.messages[4]), "better approach");
    }
}

mod edge_cases {
    use super::*;

    /// TS: "uses last entry when leafId not found"
    #[test]
    fn uses_last_entry_when_leaf_id_not_found() {
        let entries = vec![
            msg("1", None, "user", "hello"),
            msg("2", Some("1"), "assistant", "hi"),
        ];
        let ctx = context(&entries, LeafId::Id("nonexistent"));
        assert_eq!(ctx.messages.len(), 2);
    }

    /// TS: "handles orphaned entries gracefully"
    #[test]
    fn handles_orphaned_entries_gracefully() {
        let entries = vec![
            msg("1", None, "user", "hello"),
            msg("2", Some("missing"), "assistant", "orphan"),
        ];
        let ctx = context(&entries, LeafId::Id("2"));
        // parent 链断裂时只得到孤儿条目。
        assert_eq!(ctx.messages.len(), 1);
    }
}

/// 回归（评审 round 2 #5）：`getSessionContextSettings` 直接读取原始条目的
/// `thinkingLevel` / `provider` / `modelId`，不做类型校验。非字符串值在 Rust 的 `String`
/// 字段里按 JS `String(value)` 承载（`5` → `"5"`），不再因为类型不符被当成 Unknown 忽略。
///
/// 期望语义由 TS 参考实现确认（原始值为 `5` / `{provider: 7, modelId: true}`）。
#[test]
fn context_settings_read_non_string_thinking_level_and_model_change() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"thinking_level_change","id":"t1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","thinkingLevel":5}"#,
        r#"{"type":"model_change","id":"mc1","parentId":"t1","timestamp":"2025-01-01T00:00:02.000Z","provider":7,"modelId":true}"#,
        r#"{"type":"message","id":"m1","parentId":"mc1","timestamp":"2025-01-01T00:00:03.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#,
    ];
    let entries: Vec<SessionEntry> = parse_session_entries(&lines.join("\n"))
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect();
    let ctx = context(&entries, LeafId::Unspecified);
    assert_eq!(ctx.thinking_level, "5");
    assert_eq!(
        ctx.model,
        Some(ModelRef {
            provider: "7".to_string(),
            model_id: "true".to_string(),
        })
    );
}
