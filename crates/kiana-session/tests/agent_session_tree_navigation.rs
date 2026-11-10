//! `test/agent-session-tree-navigation.test.ts` 的 Rust 移植。
//!
//! TS 里这些用例 `skipIf(!API_KEY)`，用真实 provider 生成分支摘要；Rust 版按交付要求
//! 改用 faux provider（摘要文本由 faux 响应提供），因此全部用例都会运行。

mod common;

use common::*;
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use kiana_session::agent_session::NavigateTreeOptions;
use kiana_session_core::session_manager::SessionEntry;

fn assistant_entries(harness: &Harness) -> Vec<SessionEntry> {
    harness
        .session_entries()
        .into_iter()
        .filter(|entry| match entry {
            SessionEntry::Message(message) => message.message.role() == "assistant",
            _ => false,
        })
        .collect()
}

fn user_entries(harness: &Harness) -> Vec<SessionEntry> {
    harness
        .session_entries()
        .into_iter()
        .filter(|entry| match entry {
            SessionEntry::Message(message) => message.message.role() == "user",
            _ => false,
        })
        .collect()
}

fn entry_id(entry: &SessionEntry) -> String {
    entry.id().to_string()
}

fn entry_parent_id(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Message(message) => message.parent_id.clone(),
        SessionEntry::BranchSummary(summary) => summary.parent_id.clone(),
        other => other.parent_id().map(str::to_string),
    }
}

/// TS 测试用 `getTree()[0]` 当「根用户消息」；实际 TS/Rust 都会先把首次 run 的
/// system prompt 补丁持久化为第一条 entry，因此这里取第一条用户消息（测试意图不变）。
fn tree_root(harness: &Harness) -> SessionEntry {
    user_entries(harness)
        .into_iter()
        .next()
        .expect("first user entry")
}

fn summary_entry(result: &kiana_session::agent_session::TreeNavigationResult) -> SessionEntry {
    result.summary_entry.clone().expect("summary entry")
}

/// TS: "should navigate to user message and put text in editor"
#[tokio::test]
async fn should_navigate_to_user_message_and_put_text_in_editor() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("ok 1"), faux_text_response("ok 2")]);
        harness
            .session
            .prompt("First message".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("Second message".to_string(), Default::default())
            .await
            .unwrap();
        let tree = harness.session_manager().get_tree();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].entry.kind(), "message");
        let root = user_entries(&harness)
            .into_iter()
            .next()
            .expect("first user entry");
        let result = harness
            .session
            .navigate_tree(
                entry_id(&root),
                NavigateTreeOptions {
                    summarize: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text.as_deref(), Some("First message"));
        // TS 断言 `null`（空会话）；实际第一条 entry 是 system 补丁，leaf 回到用户消息的父节点。
        assert_eq!(
            harness.session_manager().get_leaf_id().map(str::to_string),
            entry_parent_id(&root)
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "should navigate to non-user message without editor text"
#[tokio::test]
async fn should_navigate_to_non_user_message_without_editor_text() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello response")]);
        harness
            .session
            .prompt("Hello".to_string(), Default::default())
            .await
            .unwrap();
        let assistant = assistant_entries(&harness)
            .into_iter()
            .next()
            .expect("assistant");
        let id = entry_id(&assistant);
        let result = harness
            .session
            .navigate_tree(id.clone(), NavigateTreeOptions::default())
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text, None);
        assert_eq!(harness.session_manager().get_leaf_id(), Some(id.as_str()));
        harness.cleanup();
    })
    .await;
}

/// TS: "should create branch summary when navigating with summarize=true"
#[tokio::test]
async fn should_create_branch_summary_when_navigating_with_summarize_true() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![
            faux_text_response("answer 4"),
            faux_text_response("answer 6"),
            faux_text_response("branch summary text"),
        ]);
        harness
            .session
            .prompt("What is 2+2?".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("What is 3+3?".to_string(), Default::default())
            .await
            .unwrap();
        let root = tree_root(&harness);
        let result = harness
            .session
            .navigate_tree(
                entry_id(&root),
                NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text.as_deref(), Some("What is 2+2?"));
        let summary = summary_entry(&result);
        assert_eq!(summary.kind(), "branch_summary");
        if let SessionEntry::BranchSummary(entry) = &summary {
            assert!(!entry.summary.is_empty());
            // TS 断言 `null`（根用户消息）；实际首次 run 的 system 补丁是第一条 entry，
            // 用户消息的父节点就是它，摘要按 TS 规则附加在目标位置。
            assert_eq!(entry.parent_id, entry_parent_id(&root));
        }
        assert_eq!(
            harness.session_manager().get_leaf_id().map(str::to_string),
            Some(entry_id(&summary))
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "should attach summary to correct parent when navigating to nested user message"
#[tokio::test]
async fn should_attach_summary_to_correct_parent_when_navigating_to_nested_user_message() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![
            faux_text_response("answer one"),
            faux_text_response("answer two"),
            faux_text_response("answer three"),
            faux_text_response("nested summary"),
        ]);
        for prompt in ["Message one", "Message two", "Message three"] {
            harness
                .session
                .prompt(prompt.to_string(), Default::default())
                .await
                .unwrap();
        }
        let users = user_entries(&harness);
        assert_eq!(users.len(), 3);
        let u2 = users[1].clone();
        let a1_id = entry_parent_id(&u2).expect("a1");
        let result = harness
            .session
            .navigate_tree(
                entry_id(&u2),
                NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text.as_deref(), Some("Message two"));
        let summary = summary_entry(&result);
        assert_eq!(entry_parent_id(&summary), Some(a1_id.clone()));
        let children = harness
            .session_manager()
            .get_entries()
            .into_iter()
            .filter(|entry| entry_parent_id(entry) == Some(a1_id.clone()))
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 2);
        let kinds: Vec<&str> = children.iter().map(|entry| entry.kind()).collect();
        assert!(kinds.contains(&"branch_summary"));
        assert!(kinds.contains(&"message"));
        harness.cleanup();
    })
    .await;
}

/// TS: "should attach summary to selected node when navigating to assistant message"
#[tokio::test]
async fn should_attach_summary_to_selected_node_when_navigating_to_assistant_message() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![
            faux_text_response("hi"),
            faux_text_response("bye"),
            faux_text_response("selected summary"),
        ]);
        harness
            .session
            .prompt("Hello".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("Goodbye".to_string(), Default::default())
            .await
            .unwrap();
        let a1 = assistant_entries(&harness).into_iter().next().expect("a1");
        let a1_id = entry_id(&a1);
        let result = harness
            .session
            .navigate_tree(
                a1_id.clone(),
                NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text, None);
        let summary = summary_entry(&result);
        assert_eq!(entry_parent_id(&summary), Some(a1_id));
        assert_eq!(
            harness.session_manager().get_leaf_id().map(str::to_string),
            Some(entry_id(&summary))
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "should handle abort during summarization"
#[tokio::test]
async fn should_handle_abort_during_summarization() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let gate = Arc::new(tokio::sync::Notify::new());
        harness.set_responses(vec![
            faux_text_response("first answer"),
            faux_text_response("second answer"),
            FauxResponseStep::message_factory({
                let gate = Arc::clone(&gate);
                move |_input| {
                    let gate = Arc::clone(&gate);
                    async move {
                        // 摘要请求保持挂起，让测试有机会 abortBranchSummary()。
                        gate.notified().await;
                        faux_assistant_message("late summary", Default::default())
                    }
                }
            }),
        ]);
        harness
            .session
            .prompt("Tell me about something".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("Continue".to_string(), Default::default())
            .await
            .unwrap();
        let entries_before = harness.session_entries().len();
        let leaf_before = harness.session_manager().get_leaf_id().map(str::to_string);
        let root = tree_root(&harness);
        let session = harness.session.clone();
        let navigation = tokio::task::spawn_local(async move {
            session
                .navigate_tree(
                    entry_id(&root),
                    NavigateTreeOptions {
                        summarize: Some(true),
                        ..Default::default()
                    },
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(harness.session.is_compacting());
        harness.session.abort_branch_summary();
        gate.notify_waiters();
        let result = navigation.await.unwrap().unwrap();
        assert!(result.cancelled);
        assert_eq!(result.aborted, Some(true));
        assert!(result.summary_entry.is_none());
        assert_eq!(harness.session_entries().len(), entries_before);
        assert_eq!(
            harness.session_manager().get_leaf_id().map(str::to_string),
            leaf_before
        );
        harness.cleanup();
    })
    .await;
}

/// TS: "should not create summary when navigating without summarize option"
#[tokio::test]
async fn should_not_create_summary_when_navigating_without_summarize_option() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("one"), faux_text_response("two")]);
        harness
            .session
            .prompt("First".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("Second".to_string(), Default::default())
            .await
            .unwrap();
        let entries_before = harness.session_entries().len();
        let root = tree_root(&harness);
        harness
            .session
            .navigate_tree(
                entry_id(&root),
                NavigateTreeOptions {
                    summarize: Some(false),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(harness.session_entries().len(), entries_before);
        let summaries = harness
            .session_entries()
            .into_iter()
            .filter(|entry| entry.kind() == "branch_summary")
            .count();
        assert_eq!(summaries, 0);
        harness.cleanup();
    })
    .await;
}

/// TS: "should handle navigation to same position (no-op)"
#[tokio::test]
async fn should_handle_navigation_to_same_position_noop() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello")]);
        harness
            .session
            .prompt("Hello".to_string(), Default::default())
            .await
            .unwrap();
        let leaf_before = harness
            .session_manager()
            .get_leaf_id()
            .map(str::to_string)
            .expect("leaf");
        let entries_before = harness.session_entries().len();
        let result = harness
            .session
            .navigate_tree(leaf_before.clone(), NavigateTreeOptions::default())
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(
            harness.session_manager().get_leaf_id().map(str::to_string),
            Some(leaf_before)
        );
        assert_eq!(harness.session_entries().len(), entries_before);
        harness.cleanup();
    })
    .await;
}

/// TS: "should support custom summarization instructions"
#[tokio::test]
async fn should_support_custom_summarization_instructions() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        harness.set_responses(vec![
            faux_text_response("typescript answer"),
            FauxResponseStep::message_factory({
                let seen = Arc::clone(&seen);
                move |input| {
                    let seen = Arc::clone(&seen);
                    async move {
                        let text = input
                            .context
                            .as_context()
                            .messages
                            .iter()
                            .map(|message| format!("{message:?}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        seen.lock().unwrap().push(text);
                        faux_assistant_message("summary MONKEY MONKEY MONKEY", Default::default())
                    }
                }
            }),
        ]);
        harness.session.prompt("What is TypeScript?".to_string(), Default::default()).await.unwrap();
        let root = tree_root(&harness);
        let result = harness
            .session
            .navigate_tree(
                entry_id(&root),
                NavigateTreeOptions {
                    summarize: Some(true),
                    custom_instructions: Some(
                        "After the summary, you MUST end with exactly: MONKEY MONKEY MONKEY. This is of utmost importance."
                            .to_string(),
                    ),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let summary = summary_entry(&result);
        if let SessionEntry::BranchSummary(entry) = &summary {
            assert!(entry.summary.contains("MONKEY MONKEY MONKEY"));
        }
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("MONKEY MONKEY MONKEY"));
        harness.cleanup();
    })
    .await;
}

/// TS: "should navigate between branches correctly"
#[tokio::test]
async fn should_navigate_between_branches_correctly() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![
            faux_text_response("main response one"),
            faux_text_response("main response two"),
            faux_text_response("branch response"),
            faux_text_response("branch summary"),
        ]);
        harness
            .session
            .prompt("Main branch start".to_string(), Default::default())
            .await
            .unwrap();
        harness
            .session
            .prompt("Main branch continue".to_string(), Default::default())
            .await
            .unwrap();
        let entries_before = harness.session_entries();
        let a1 = entries_before
            .iter()
            .find(|entry| match entry {
                SessionEntry::Message(message) => message.message.role() == "assistant",
                _ => false,
            })
            .expect("a1");
        let a1_id = entry_id(a1);
        let _ = harness
            .session
            .with_session_manager_mut(|manager| manager.branch(&a1_id));
        harness.session.refresh_context();
        harness
            .session
            .prompt("Branch path".to_string(), Default::default())
            .await
            .unwrap();
        let users = user_entries(&harness);
        let u2 = users[1].clone();
        let result = harness
            .session
            .navigate_tree(
                entry_id(&u2),
                NavigateTreeOptions {
                    summarize: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text.as_deref(), Some("Main branch continue"));
        let summary = summary_entry(&result);
        if let SessionEntry::BranchSummary(entry) = &summary {
            assert!(!entry.summary.is_empty());
        }
        harness.cleanup();
    })
    .await;
}

use std::sync::Arc;

/// TS: custom message 的 `editorText` 用 `contentText(content, "")`（拼接文本块，不是 JSON）。
#[tokio::test]
async fn custom_message_editor_text_concatenates_text_blocks() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        harness.set_responses(vec![faux_text_response("hello")]);
        harness
            .session
            .prompt("Hello".to_string(), Default::default())
            .await
            .unwrap();
        let custom_id = harness
            .session
            .with_session_manager_mut(|manager| {
                manager.append_custom_message_entry(
                    "note",
                    serde_json::json!([
                        {"type": "text", "text": "hi"},
                        {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                        {"type": "text", "text": "there"}
                    ]),
                    true,
                    None,
                )
            })
            .unwrap();
        harness.session.refresh_context();
        // 让 leaf 离开目标条目。
        harness.append_session_message(kiana_agent_core::types::AgentMessage::Standard(
            kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text("after".to_string()),
                timestamp: 10,
            }),
        ));
        let result = harness
            .session
            .navigate_tree(custom_id, NavigateTreeOptions::default())
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.editor_text.as_deref(), Some("hithere"));
        harness.cleanup();
    })
    .await;
}
