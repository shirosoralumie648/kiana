//! TS: `test/session-manager/tree-traversal.test.ts`（31 例）。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{Message, Usage, UsageCost};
use kiana_session_core::session_manager::{SessionEntry, SessionManager};

fn session() -> SessionManager {
    SessionManager::in_memory(&current_cwd(), None, None).unwrap()
}

fn user(text: &str) -> AgentMessage {
    user_message(text, 0)
}

fn assistant(text: &str) -> AgentMessage {
    assistant_message(text, 0)
}

fn ids(entries: &[SessionEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.id().to_string()).collect()
}

fn test_usage() -> Usage {
    Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 100,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
            total_from_provider: None,
        },
    }
}

mod append_operations {
    use super::*;

    /// TS: "appendMessage creates entry with correct parentId chain"
    #[test]
    fn append_message_creates_entry_with_correct_parent_id_chain() {
        let mut session = session();

        let id1 = session.append_message(user("first")).unwrap();
        let id2 = session.append_message(assistant("second")).unwrap();
        let id3 = session.append_message(user("third")).unwrap();

        let entries = session.get_entries();
        assert_eq!(entries.len(), 3);

        assert_eq!(entries[0].id(), id1);
        assert_eq!(entries[0].parent_id(), None);
        assert_eq!(entries[0].kind(), "message");

        assert_eq!(entries[1].id(), id2);
        assert_eq!(entries[1].parent_id(), Some(id1.as_str()));

        assert_eq!(entries[2].id(), id3);
        assert_eq!(entries[2].parent_id(), Some(id2.as_str()));
    }

    /// TS: "appendThinkingLevelChange integrates into tree"
    #[test]
    fn append_thinking_level_change_integrates_into_tree() {
        let mut session = session();

        let msg_id = session.append_message(user("hello")).unwrap();
        let thinking_id = session.append_thinking_level_change("high").unwrap();
        let _msg2_id = session.append_message(assistant("response")).unwrap();

        let entries = session.get_entries();
        assert_eq!(entries.len(), 3);

        let thinking = entries
            .iter()
            .find(|entry| entry.kind() == "thinking_level_change")
            .expect("thinking entry");
        assert_eq!(thinking.id(), thinking_id);
        assert_eq!(thinking.parent_id(), Some(msg_id.as_str()));

        assert_eq!(entries[2].parent_id(), Some(thinking_id.as_str()));
    }

    /// TS: "appendModelChange integrates into tree"
    #[test]
    fn append_model_change_integrates_into_tree() {
        let mut session = session();

        let msg_id = session.append_message(user("hello")).unwrap();
        let model_id = session.append_model_change("openai", "gpt-4").unwrap();
        let _msg2_id = session.append_message(assistant("response")).unwrap();

        let entries = session.get_entries();
        let model = entries
            .iter()
            .find(|entry| entry.kind() == "model_change")
            .expect("model entry");
        assert_eq!(model.id(), model_id);
        assert_eq!(model.parent_id(), Some(msg_id.as_str()));
        match model {
            SessionEntry::ModelChange(entry) => {
                assert_eq!(entry.provider, "openai");
                assert_eq!(entry.model_id, "gpt-4");
            }
            other => panic!("expected model_change, got {other:?}"),
        }

        assert_eq!(entries[2].parent_id(), Some(model_id.as_str()));
    }

    /// TS: "appendCompaction integrates into tree"
    #[test]
    fn append_compaction_integrates_into_tree() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let usage = test_usage();
        let compaction_id = session
            .append_compaction(
                "summary",
                Some(&id1),
                1000.0,
                None,
                Some(false),
                Some(usage.clone()),
            )
            .unwrap();
        let _id3 = session.append_message(user("3")).unwrap();

        let entries = session.get_entries();
        let compaction = entries
            .iter()
            .find(|entry| entry.kind() == "compaction")
            .expect("compaction entry");
        assert_eq!(compaction.id(), compaction_id);
        assert_eq!(compaction.parent_id(), Some(id2.as_str()));
        match compaction {
            SessionEntry::Compaction(entry) => {
                assert_eq!(entry.summary, "summary");
                assert_eq!(entry.first_kept_entry_id, id1);
                assert_eq!(entry.tokens_before, 1000.0);
                assert_eq!(entry.usage.as_ref(), Some(&usage));
            }
            other => panic!("expected compaction, got {other:?}"),
        }

        assert_eq!(entries[3].parent_id(), Some(compaction_id.as_str()));
    }

    /// TS: "appendCustomEntry integrates into tree"
    #[test]
    fn append_custom_entry_integrates_into_tree() {
        let mut session = session();

        let msg_id = session.append_message(user("hello")).unwrap();
        let custom_id = session
            .append_custom_entry("my_data", Some(serde_json::json!({"key": "value"})))
            .unwrap();
        let _msg2_id = session.append_message(assistant("response")).unwrap();

        let entries = session.get_entries();
        let custom = entries
            .iter()
            .find(|entry| entry.kind() == "custom")
            .expect("custom entry");
        assert_eq!(custom.id(), custom_id);
        assert_eq!(custom.parent_id(), Some(msg_id.as_str()));
        match custom {
            SessionEntry::Custom(entry) => {
                assert_eq!(entry.custom_type, "my_data");
                assert_eq!(entry.data, Some(serde_json::json!({"key": "value"})));
            }
            other => panic!("expected custom, got {other:?}"),
        }

        assert_eq!(entries[2].parent_id(), Some(custom_id.as_str()));
    }

    /// TS: "leaf pointer advances after each append"
    #[test]
    fn leaf_pointer_advances_after_each_append() {
        let mut session = session();

        assert!(session.get_leaf_id().is_none());

        let id1 = session.append_message(user("1")).unwrap();
        assert_eq!(session.get_leaf_id(), Some(id1.as_str()));

        let id2 = session.append_message(assistant("2")).unwrap();
        assert_eq!(session.get_leaf_id(), Some(id2.as_str()));

        let id3 = session.append_thinking_level_change("high").unwrap();
        assert_eq!(session.get_leaf_id(), Some(id3.as_str()));
    }
}

mod get_path {
    use super::*;

    /// TS: "returns empty array for empty session"
    #[test]
    fn returns_empty_array_for_empty_session() {
        let session = session();
        assert!(session.get_branch(None).is_empty());
    }

    /// TS: "returns single entry path"
    #[test]
    fn returns_single_entry_path() {
        let mut session = session();
        let id = session.append_message(user("hello")).unwrap();

        let path = session.get_branch(None);
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].id(), id);
    }

    /// TS: "returns full path from root to leaf"
    #[test]
    fn returns_full_path_from_root_to_leaf() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_thinking_level_change("high").unwrap();
        let id4 = session.append_message(user("3")).unwrap();

        let path = session.get_branch(None);
        assert_eq!(path.len(), 4);
        assert_eq!(ids(&path), [id1, id2, id3, id4]);
    }

    /// TS: "returns path from specified entry to root"
    #[test]
    fn returns_path_from_specified_entry_to_root() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let _id3 = session.append_message(user("3")).unwrap();
        let _id4 = session.append_message(assistant("4")).unwrap();

        let path = session.get_branch(Some(&id2));
        assert_eq!(path.len(), 2);
        assert_eq!(ids(&path), [id1, id2]);
    }
}

mod get_tree {
    use super::*;

    /// TS: "returns empty array for empty session"
    #[test]
    fn returns_empty_array_for_empty_session() {
        let session = session();
        assert!(session.get_tree().is_empty());
    }

    /// TS: "returns single root for linear session"
    #[test]
    fn returns_single_root_for_linear_session() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_message(user("3")).unwrap();

        let tree = session.get_tree();
        assert_eq!(tree.len(), 1);

        let root = &tree[0];
        assert_eq!(root.entry.id(), id1);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].entry.id(), id2);
        assert_eq!(root.children[0].children.len(), 1);
        assert_eq!(root.children[0].children[0].entry.id(), id3);
        assert!(root.children[0].children[0].children.is_empty());
    }

    /// TS: "returns tree with branches after branch"
    #[test]
    fn returns_tree_with_branches_after_branch() {
        let mut session = session();

        // 构建：1 -> 2 -> 3
        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_message(user("3")).unwrap();

        // 从 id2 分支，新增路径：2 -> 4
        session.branch(&id2).unwrap();
        let id4 = session.append_message(user("4-branch")).unwrap();

        let tree = session.get_tree();
        assert_eq!(tree.len(), 1);

        let root = &tree[0];
        assert_eq!(root.entry.id(), id1);
        assert_eq!(root.children.len(), 1);

        let node2 = &root.children[0];
        assert_eq!(node2.entry.id(), id2);
        assert_eq!(node2.children.len(), 2); // id3 与 id4 是兄弟。

        let mut child_ids: Vec<String> = node2
            .children
            .iter()
            .map(|child| child.entry.id().to_string())
            .collect();
        child_ids.sort();
        let mut expected = vec![id3, id4];
        expected.sort();
        assert_eq!(child_ids, expected);
    }

    /// TS: "handles multiple branches at same point"
    #[test]
    fn handles_multiple_branches_at_same_point() {
        let mut session = session();

        let _id1 = session.append_message(user("root")).unwrap();
        let id2 = session.append_message(assistant("response")).unwrap();

        session.branch(&id2).unwrap();
        let id_a = session.append_message(user("branch-A")).unwrap();

        session.branch(&id2).unwrap();
        let id_b = session.append_message(user("branch-B")).unwrap();

        session.branch(&id2).unwrap();
        let id_c = session.append_message(user("branch-C")).unwrap();

        let tree = session.get_tree();
        let node2 = &tree[0].children[0];
        assert_eq!(node2.entry.id(), id2);
        assert_eq!(node2.children.len(), 3);

        let mut branch_ids: Vec<String> = node2
            .children
            .iter()
            .map(|child| child.entry.id().to_string())
            .collect();
        branch_ids.sort();
        let mut expected = vec![id_a, id_b, id_c];
        expected.sort();
        assert_eq!(branch_ids, expected);
    }

    /// TS: "handles deep branching"
    #[test]
    fn handles_deep_branching() {
        let mut session = session();

        // 主路径：1 -> 2 -> 3 -> 4
        let _id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_message(user("3")).unwrap();
        let _id4 = session.append_message(assistant("4")).unwrap();

        // 从 2 分支：2 -> 5 -> 6
        session.branch(&id2).unwrap();
        let id5 = session.append_message(user("5")).unwrap();
        let _id6 = session.append_message(assistant("6")).unwrap();

        // 从 5 分支：5 -> 7
        session.branch(&id5).unwrap();
        let _id7 = session.append_message(user("7")).unwrap();

        let tree = session.get_tree();

        let node2 = &tree[0].children[0];
        assert_eq!(node2.children.len(), 2); // id3 与 id5

        let node5 = node2
            .children
            .iter()
            .find(|child| child.entry.id() == id5)
            .expect("node5");
        assert_eq!(node5.children.len(), 2); // id6 与 id7

        let node3 = node2
            .children
            .iter()
            .find(|child| child.entry.id() == id3)
            .expect("node3");
        assert_eq!(node3.children.len(), 1); // id4
    }
}

mod branch {
    use super::*;

    /// TS: "moves leaf pointer to specified entry"
    #[test]
    fn moves_leaf_pointer_to_specified_entry() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let _id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_message(user("3")).unwrap();

        assert_eq!(session.get_leaf_id(), Some(id3.as_str()));

        session.branch(&id1).unwrap();
        assert_eq!(session.get_leaf_id(), Some(id1.as_str()));
    }

    /// TS: "throws for non-existent entry"
    #[test]
    fn throws_for_non_existent_entry() {
        let mut session = session();
        session.append_message(user("hello")).unwrap();

        let error = error_text(session.branch("nonexistent"));
        assert!(
            error.contains("Entry nonexistent not found"),
            "unexpected error: {error}"
        );
    }

    /// TS: "new appends become children of branch point"
    #[test]
    fn new_appends_become_children_of_branch_point() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let _id2 = session.append_message(assistant("2")).unwrap();

        session.branch(&id1).unwrap();
        let id3 = session.append_message(user("branched")).unwrap();

        let entries = session.get_entries();
        let branched = entries
            .iter()
            .find(|entry| entry.id() == id3)
            .expect("branched");
        assert_eq!(branched.parent_id(), Some(id1.as_str())); // id2 的兄弟。
    }
}

mod branch_with_summary {
    use super::*;

    /// TS: "inserts branch summary with the source and destination and advances leaf"
    #[test]
    fn inserts_branch_summary_with_the_source_and_destination_and_advances_leaf() {
        let mut session = session();

        let id1 = session.append_message(user("1")).unwrap();
        let _id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_message(user("3")).unwrap();

        let usage = test_usage();
        let summary_id = session
            .branch_with_summary(
                Some(&id1),
                "Summary of abandoned work",
                None,
                Some(false),
                Some(usage.clone()),
            )
            .unwrap();

        assert_eq!(session.get_leaf_id(), Some(summary_id.as_str()));

        let entries = session.get_entries();
        let summary = entries
            .iter()
            .find(|entry| entry.kind() == "branch_summary")
            .expect("branch summary entry");
        assert_eq!(summary.parent_id(), Some(id1.as_str()));
        match summary {
            SessionEntry::BranchSummary(entry) => {
                assert_eq!(entry.from_id, id3);
                assert_eq!(entry.summary, "Summary of abandoned work");
                assert_eq!(entry.usage.as_ref(), Some(&usage));
            }
            other => panic!("expected branch_summary, got {other:?}"),
        }
    }

    /// TS: "throws for non-existent entry"
    #[test]
    fn throws_for_non_existent_entry() {
        let mut session = session();
        session.append_message(user("hello")).unwrap();

        let error = error_text(session.branch_with_summary(
            Some("nonexistent"),
            "summary",
            None,
            None,
            None,
        ));
        assert!(
            error.contains("Entry nonexistent not found"),
            "unexpected error: {error}"
        );
    }
}

mod get_leaf_entry {
    use super::*;

    /// TS: "returns undefined for empty session"
    #[test]
    fn returns_undefined_for_empty_session() {
        let session = session();
        assert!(session.get_leaf_entry().is_none());
    }

    /// TS: "returns current leaf entry"
    #[test]
    fn returns_current_leaf_entry() {
        let mut session = session();

        session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();

        let leaf = session.get_leaf_entry().expect("leaf entry");
        assert_eq!(leaf.id(), id2);
    }
}

mod get_entry {
    use super::*;

    /// TS: "returns undefined for non-existent id"
    #[test]
    fn returns_undefined_for_non_existent_id() {
        let session = session();
        assert!(session.get_entry("nonexistent").is_none());
    }

    /// TS: "returns entry by id"
    #[test]
    fn returns_entry_by_id() {
        let mut session = session();

        let id1 = session.append_message(user("first")).unwrap();
        let id2 = session.append_message(assistant("second")).unwrap();

        let entry1 = session.get_entry(&id1).expect("entry1");
        assert_eq!(entry1.kind(), "message");
        match entry1 {
            SessionEntry::Message(entry) => match &entry.message {
                AgentMessage::Standard(Message::User(user)) => match &user.content {
                    kiana_ai::types::UserContent::Text(text) => assert_eq!(text, "first"),
                    other => panic!("expected string content, got {other:?}"),
                },
                other => panic!("expected user message, got {other:?}"),
            },
            other => panic!("expected message, got {other:?}"),
        }

        let entry2 = session.get_entry(&id2).expect("entry2");
        match entry2 {
            SessionEntry::Message(entry) => match &entry.message {
                AgentMessage::Standard(Message::Assistant(assistant)) => {
                    match assistant.content.first() {
                        Some(kiana_ai::types::AssistantContentBlock::Text(text)) => {
                            assert_eq!(text.text, "second")
                        }
                        _ => panic!("expected text block"),
                    }
                }
                other => panic!("expected assistant message, got {other:?}"),
            },
            other => panic!("expected message, got {other:?}"),
        }
    }
}

mod build_session_context_with_branches {
    use super::*;

    /// TS: "returns messages from current branch only"
    #[test]
    fn returns_messages_from_current_branch_only() {
        let mut session = session();

        // 主链：1 -> 2 -> 3
        session.append_message(user("msg1")).unwrap();
        let id2 = session.append_message(assistant("msg2")).unwrap();
        session.append_message(user("msg3")).unwrap();

        // 从 2 分支：2 -> 4
        session.branch(&id2).unwrap();
        session.append_message(assistant("msg4-branch")).unwrap();

        let ctx = session.build_session_context();
        assert_eq!(ctx.messages.len(), 3); // msg1, msg2, msg4-branch（不含 msg3）

        match &ctx.messages[0] {
            AgentMessage::Standard(Message::User(user)) => match &user.content {
                kiana_ai::types::UserContent::Text(text) => assert_eq!(text, "msg1"),
                other => panic!("expected string content, got {other:?}"),
            },
            other => panic!("expected user message, got {other:?}"),
        }
        assert_eq!(assistant_text(&ctx.messages[1]), "msg2");
        assert_eq!(assistant_text(&ctx.messages[2]), "msg4-branch");
    }

    fn assistant_text(message: &AgentMessage) -> &str {
        match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                match assistant.content.first() {
                    Some(kiana_ai::types::AssistantContentBlock::Text(text)) => &text.text,
                    _ => panic!("expected text block"),
                }
            }
            other => panic!("expected assistant message, got {other:?}"),
        }
    }
}

mod create_branched_session {
    use super::*;

    /// TS: "throws for non-existent entry"
    #[test]
    fn throws_for_non_existent_entry() {
        let mut session = session();
        session.append_message(user("hello")).unwrap();

        let error = error_text(session.create_branched_session("nonexistent"));
        assert!(
            error.contains("Entry nonexistent not found"),
            "unexpected error: {error}"
        );
    }

    /// TS: "creates new session with path to specified leaf (in-memory)"
    #[test]
    fn creates_new_session_with_path_to_specified_leaf_in_memory() {
        let mut session = session();

        // 构建：1 -> 2 -> 3 -> 4
        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        let id3 = session.append_message(user("3")).unwrap();
        session.append_message(assistant("4")).unwrap();

        // 从 3 分支：3 -> 5
        session.branch(&id3).unwrap();
        let _id5 = session.append_message(user("5")).unwrap();

        // 从 id2 创建分支会话（只应包含 1 -> 2）。
        let result = session.create_branched_session(&id2).unwrap();
        assert!(result.is_none()); // 内存态返回 None。

        let entries = session.get_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id(), id1);
        assert_eq!(entries[1].id(), id2);
    }

    /// TS: "extracts correct path from branched tree"
    #[test]
    fn extracts_correct_path_from_branched_tree() {
        let mut session = session();

        // 构建：1 -> 2 -> 3
        let id1 = session.append_message(user("1")).unwrap();
        let id2 = session.append_message(assistant("2")).unwrap();
        session.append_message(user("3")).unwrap();

        // 从 2 分支：2 -> 4 -> 5
        session.branch(&id2).unwrap();
        let id4 = session.append_message(user("4")).unwrap();
        let id5 = session.append_message(assistant("5")).unwrap();

        // 从 id5 创建分支会话（应包含 1 -> 2 -> 4 -> 5）。
        session.create_branched_session(&id5).unwrap();

        let entries = session.get_entries();
        assert_eq!(entries.len(), 4);
        assert_eq!(ids(&entries), [id1, id2, id4, id5]);
    }

    /// TS: "does not duplicate entries when forking from before the first user message"
    #[test]
    fn does_not_duplicate_entries_when_forking_from_before_the_first_user_message() {
        let temp = temp_dir("session-fork-dedup-");
        let dir = temp.path().to_string_lossy().into_owned();

        // 创建带两轮对话的持久会话。
        let mut session = SessionManager::create(&dir, Some(&dir), None).unwrap();
        let model_change_id = session
            .append_model_change("anthropic", "claude-sonnet-4-5")
            .unwrap();
        session.append_message(user("first question")).unwrap();
        session.append_message(assistant("first answer")).unwrap();

        // 从 setup 条目 fork（分支路径上没有 user/assistant 消息）。
        let new_file = session
            .create_branched_session(&model_change_id)
            .unwrap()
            .expect("new session file");
        assert!(!std::path::Path::new(&new_file).exists());

        session.append_message(user("new question")).unwrap();
        assert!(std::path::Path::new(&new_file).exists());

        // 模拟扩展在 assistant 之前加条目（类似 preset on turn_start）。
        session
            .append_custom_entry("preset-state", Some(serde_json::json!({"name": "plan"})))
            .unwrap();
        session.append_message(assistant("new answer")).unwrap();

        // 恰好一个 header，每个条目只写一次。
        assert_eq!(
            read_session_file_roles(&new_file),
            ["session", "model_change", "user", "custom", "assistant"]
        );
    }

    /// TS: "preserves tool and summary usage across a file-backed reload"
    #[test]
    fn preserves_tool_and_summary_usage_across_a_file_backed_reload() {
        let temp = temp_dir("session-usage-roundtrip-");
        let dir = temp.path().to_string_lossy().into_owned();

        let mut session = SessionManager::create(&dir, Some(&dir), None).unwrap();
        let root_id = session.append_message(user("question")).unwrap();
        session.append_message(assistant("answer")).unwrap();
        let usage = test_usage();
        let mut tool_result = tool_result_message("call-1", "nested-model", "result", 0);
        if let AgentMessage::Standard(Message::ToolResult(result)) = &mut tool_result {
            result.usage = Some(usage.clone());
        }
        session.append_message(tool_result).unwrap();
        session
            .append_compaction(
                "summary",
                Some(&root_id),
                100.0,
                None,
                Some(false),
                Some(usage.clone()),
            )
            .unwrap();
        session
            .branch_with_summary(
                Some(&root_id),
                "branch summary",
                None,
                Some(false),
                Some(usage.clone()),
            )
            .unwrap();

        let file = session
            .get_session_file()
            .expect("session file")
            .to_string();
        let reopened = SessionManager::open(&file, Some(&dir), None).unwrap();
        let entries = reopened.get_entries();

        let compaction = entries
            .iter()
            .find(|entry| entry.kind() == "compaction")
            .expect("compaction");
        assert_eq!(
            serde_json::to_value(compaction).unwrap().get("usage"),
            Some(&serde_json::to_value(&usage).unwrap())
        );

        let branch_summary = entries
            .iter()
            .find(|entry| entry.kind() == "branch_summary")
            .expect("branch_summary");
        assert_eq!(
            serde_json::to_value(branch_summary).unwrap().get("usage"),
            Some(&serde_json::to_value(&usage).unwrap())
        );

        let tool = entries
            .iter()
            .find(|entry| {
                entry.kind() == "message"
                    && entry
                        .message()
                        .is_some_and(|message| message.role() == "toolResult")
            })
            .expect("tool result");
        let tool_usage = match tool {
            SessionEntry::Message(entry) => match &entry.message {
                AgentMessage::Standard(Message::ToolResult(result)) => {
                    result.usage.as_ref().expect("tool usage")
                }
                other => panic!("expected toolResult message, got {other:?}"),
            },
            other => panic!("expected message entry, got {other:?}"),
        };
        assert_eq!(tool_usage, &usage);
    }

    /// TS: "writes file immediately when forking at a user message"
    #[test]
    fn writes_file_immediately_when_forking_at_a_user_message() {
        let temp = temp_dir("session-fork-with-user-");
        let dir = temp.path().to_string_lossy().into_owned();

        let mut session = SessionManager::create(&dir, Some(&dir), None).unwrap();
        let id1 = session.append_message(user("first question")).unwrap();
        session.append_message(assistant("first answer")).unwrap();

        let new_file = session
            .create_branched_session(&id1)
            .unwrap()
            .expect("new file");
        assert!(std::path::Path::new(&new_file).exists());

        session.append_message(assistant("new answer")).unwrap();
        assert_eq!(
            read_session_file_roles(&new_file),
            ["session", "user", "assistant"]
        );
    }
}

/// spec §2.2.11：`getChildren` 按 `byId` 插入序返回直接子节点；`getEntryCount` 不含 header；
/// `getHeader` 返回会话头。
#[test]
fn get_children_and_entry_count_follow_the_index_order() {
    let mut session = session();
    let root = session.append_message(user("root")).unwrap();
    let left = session.append_message(user("left")).unwrap();
    session.branch(&root).unwrap();
    let right = session.append_message(user("right")).unwrap();
    let right_child = session.append_message(assistant("right child")).unwrap();

    assert_eq!(session.get_entry_count(), 4);
    assert_eq!(
        session.get_header().map(|header| header.id_string()),
        Some(session.get_session_id())
    );
    assert_eq!(
        ids(&session.get_children(&root)),
        [left.clone(), right.clone()]
    );
    assert_eq!(ids(&session.get_children(&right)), vec![right_child]);
    assert!(session.get_children(&left).is_empty());
    assert!(session.get_children("missing").is_empty());
    assert!(!session.uses_default_session_dir());
}
