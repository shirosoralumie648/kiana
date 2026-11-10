//! 压缩提交事务（第 6 周）。
//!
//! # 解决什么问题
//!
//! `SessionManager::append_compaction` 先更新内存（`file_entries` / `by_id` / leaf），
//! 再写盘；写盘失败时内存已经变了。第 6 周要求“摘要失败时历史逐字节不变”，
//! 同时提交边界也不能留下半替换历史。本模块提供：
//!
//! - [`HistorySnapshot`]：提交前的字节级历史快照（每条一行 JSON）；
//! - [`append_compaction_transactional`]：先校验历史未被并发修改，再追加压缩条目；
//!   追加失败时回滚内存状态并返回原始错误。
//!
//! # 边界
//!
//! 磁盘上已经写出的半行无法安全回滚（进程崩溃/IO 错误）。本模块承诺的是**内存状态
//! 与提交前一致**；落盘失败后调用方应停止继续写入该会话并报告错误。正常路径下
//! `append_entry` 的整行一次 `write_all`，只有底层 IO 部分失败才可能留下残行。
//!
//! TS 没有对应实现：TS 的 `_persist` 在 throw 后同样残留内存状态；本模块是 Rust 侧
//! 为 all-or-nothing 要求新增的提交边界。

use kiana_ai::types::JsonValue;

use crate::session_manager::{LeafPointer, SessionEntry, SessionManager, SessionManagerError};

/// 提交前的历史快照。
#[derive(Debug, Clone, PartialEq)]
pub struct HistorySnapshot {
    entries: Vec<SessionEntry>,
    bytes: Vec<u8>,
}

impl HistorySnapshot {
    /// 捕获当前历史。
    pub fn capture(manager: &SessionManager) -> Self {
        let entries = manager.get_entries();
        let bytes = serialize_entries(&entries);
        Self { entries, bytes }
    }

    /// 快照字节（每条一行 JSON，顺序与 `get_entries` 一致）。
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 快照中的条目。
    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    /// 当前历史是否与快照逐字节一致。
    pub fn matches(&self, manager: &SessionManager) -> bool {
        serialize_entries(&manager.get_entries()) == self.bytes
    }
}

fn serialize_entries(entries: &[SessionEntry]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        if let Ok(line) = serde_json::to_vec(entry) {
            bytes.extend_from_slice(&line);
        }
        bytes.push(b'\n');
    }
    bytes
}

/// 事务提交错误。
#[derive(Debug, thiserror::Error)]
pub enum CompactionCommitError {
    /// 提交前历史已经被并发修改（树导航、另一条压缩、外部追加）。
    #[error("session history changed while compaction was in progress; refusing to commit")]
    ConcurrentModification,
    /// 底层追加失败；内存已回滚。
    #[error(transparent)]
    Manager(#[from] SessionManagerError),
}

/// 多步提交事务：把“prune 的 context_edit + 压缩条目”作为一个整体回滚。
///
/// 用法：
///
/// ```ignore
/// let snapshot = HistorySnapshot::capture(&manager);
/// let mut transaction = CommitTransaction::begin(&manager);
/// let edit_id = manager.append_context_edit(target, Some(replacement))?;
/// transaction.record(&edit_id);
/// let compaction_id = append_compaction_transactional(...)?;
/// transaction.record(&compaction_id);
/// // 任一步返回 Err 时：transaction.rollback(&mut manager)
/// ```
///
/// 成功路径不需要 `commit()`：事务只是回滚能力的持有者，drop 即结束。
pub struct CommitTransaction {
    marker: (usize, LeafPointer),
    appended_ids: Vec<String>,
}

impl CommitTransaction {
    /// 在第一次 append 之前开始事务。
    pub fn begin(manager: &SessionManager) -> Self {
        Self {
            marker: manager.append_transaction_marker(),
            appended_ids: Vec::new(),
        }
    }

    /// 记录一个成功 append 的条目 id。
    pub fn record(&mut self, id: &str) {
        self.appended_ids.push(id.to_string());
    }

    /// 回滚本事务内的所有 append（按逆序移除 id，再截断条目）。
    pub fn rollback(self, manager: &mut SessionManager) {
        for id in self.appended_ids.iter().rev() {
            manager.remove_appended_entry(id);
        }
        manager.rollback_append_transaction(self.marker, None);
    }
}

/// 在历史未被修改的前提下追加压缩条目。
///
/// 参数与 [`SessionManager::append_compaction`] 相同。返回新条目 id；
/// 失败时内存状态回滚到调用前。
#[allow(clippy::too_many_arguments)]
pub fn append_compaction_transactional(
    manager: &mut SessionManager,
    snapshot: &HistorySnapshot,
    summary: &str,
    first_kept_entry_id: Option<&str>,
    tokens_before: f64,
    details: Option<JsonValue>,
    from_hook: Option<bool>,
    usage: Option<kiana_ai::types::Usage>,
) -> Result<String, CompactionCommitError> {
    if !snapshot.matches(manager) {
        return Err(CompactionCommitError::ConcurrentModification);
    }
    let marker = manager.append_transaction_marker();
    match manager.append_compaction(
        summary,
        first_kept_entry_id,
        tokens_before,
        details,
        from_hook,
        usage,
    ) {
        Ok(id) => Ok(id),
        Err(error) => {
            let appended_id = if manager.entry_count() > marker.0 {
                manager.appended_entry_id(marker.0)
            } else {
                None
            };
            manager.rollback_append_transaction(marker, appended_id.as_deref());
            Err(CompactionCommitError::Manager(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::SessionManager;

    fn manager() -> SessionManager {
        SessionManager::in_memory("/tmp/work", None, None).expect("in-memory manager")
    }

    #[test]
    fn snapshot_tracks_appends() {
        let mut manager = manager();
        let snapshot = HistorySnapshot::capture(&manager);
        assert!(snapshot.matches(&manager));
        manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        assert!(!snapshot.matches(&manager));
    }

    #[test]
    fn concurrent_modification_is_rejected() {
        let mut manager = manager();
        manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        let snapshot = HistorySnapshot::capture(&manager);
        manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("other".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        let before = manager.get_entries().len();
        let error = append_compaction_transactional(
            &mut manager,
            &snapshot,
            "summary",
            None,
            100.0,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CompactionCommitError::ConcurrentModification
        ));
        assert_eq!(manager.get_entries().len(), before);
    }

    #[test]
    fn successful_commit_appends_exactly_one_entry() {
        let mut manager = manager();
        manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        let snapshot = HistorySnapshot::capture(&manager);
        let before = manager.get_entries().len();
        let id = append_compaction_transactional(
            &mut manager,
            &snapshot,
            "summary text",
            None,
            100.0,
            None,
            None,
            None,
        )
        .expect("commit");
        let after = manager.get_entries();
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().unwrap().id(), id);
        assert_eq!(after.last().unwrap().kind(), "compaction");
    }

    #[test]
    fn rollback_helper_restores_entries_and_leaf() {
        let mut manager = manager();
        let marker = manager.append_transaction_marker();
        let marker_len = marker.0;
        let id = manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        assert!(manager.get_entry(&id).is_some());
        manager.rollback_append_transaction(marker, Some(&id));
        assert!(manager.get_entry(&id).is_none());
        assert_eq!(manager.entry_count(), marker_len);
    }

    #[test]
    fn multi_append_transaction_rolls_back_everything() {
        let mut manager = manager();
        manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("hi".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        let before = manager.get_entries().len();
        let mut transaction = CommitTransaction::begin(&manager);
        let first = manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("one".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        transaction.record(&first);
        let second = manager
            .append_message(kiana_agent_core::types::AgentMessage::Standard(
                kiana_ai::types::Message::User(kiana_ai::types::UserMessage {
                    role: kiana_ai::types::UserRole::User,
                    content: kiana_ai::types::UserContent::Text("two".to_string()),
                    timestamp: 0,
                }),
            ))
            .unwrap();
        transaction.record(&second);
        transaction.rollback(&mut manager);
        assert_eq!(manager.get_entries().len(), before);
        assert!(manager.get_entry(&first).is_none());
        assert!(manager.get_entry(&second).is_none());
    }
}
