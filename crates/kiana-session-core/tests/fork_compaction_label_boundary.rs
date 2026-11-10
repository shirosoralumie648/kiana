//! TS: `test/suite/regressions/8989-fork-compaction-label-boundary.test.ts`（1 例）。

mod common;

use common::*;
use kiana_session_core::session_manager::{SessionEntry, SessionManager};

/// TS: "preserves compaction context when a fork removes the boundary label"
#[test]
fn preserves_compaction_context_when_a_fork_removes_the_boundary_label() {
    let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
    let old_id = session.append_message(user_message("old", 0)).unwrap();
    // findCutPoint() 可能把压缩边界移到这个对上下文不可见的 label 上。
    let label_id = session
        .append_label_change(&old_id, Some("checkpoint"))
        .unwrap();
    let kept_id = session.append_message(user_message("kept", 0)).unwrap();
    let compaction_id = session
        .append_compaction("summary", Some(&label_id), 100.0, None, None, None)
        .unwrap();
    let leaf_id = session.append_message(user_message("after", 0)).unwrap();

    session.create_branched_session(&leaf_id).unwrap();

    match session.get_entry(&compaction_id).unwrap() {
        SessionEntry::Compaction(compaction) => {
            assert_eq!(compaction.first_kept_entry_id, kept_id);
        }
        other => panic!("expected compaction, got {other:?}"),
    }
    let messages = session.build_session_context().messages;
    assert_eq!(messages.len(), 3);
    let values: Vec<serde_json::Value> = messages.iter().map(message_json).collect();
    assert_eq!(
        values[0].get("role").and_then(serde_json::Value::as_str),
        Some("compactionSummary")
    );
    assert_eq!(
        values[0].get("summary").and_then(serde_json::Value::as_str),
        Some("summary")
    );
    assert_eq!(
        values[1].get("role").and_then(serde_json::Value::as_str),
        Some("user")
    );
    assert_eq!(
        values[1].get("content").and_then(serde_json::Value::as_str),
        Some("kept")
    );
    assert_eq!(
        values[2].get("role").and_then(serde_json::Value::as_str),
        Some("user")
    );
    assert_eq!(
        values[2].get("content").and_then(serde_json::Value::as_str),
        Some("after")
    );
}
