//! `kiana-durable` 之上的后台任务 journal / notifier 适配。
//!
//! ADR `docs/adr/durable.md` 规定 `kiana-durable` 是后台任务、cron 与跨进程恢复的唯一
//! 事务存储底座。本模块不实现队列，也不维护第二套状态机：它把
//! [`BackgroundTaskRecord`] 投影到 durable 的**会话文档**上，用 durable 的
//! `Session`/`Storage`（默认 JSONL 后端）提供原子读改写，因此：
//!
//! - `save` / `finish` / `record_attempt` / `mark_notified` 都在一个 durable 事务里完成；
//! - `list_recoverable` 从 durable 文档读取，进程崩溃后仍能看到运行中与未通知的记录；
//! - 每个 kiana 会话一个独立的 durable 存储目录（`<root>/<session-key>/`），与
//!   [`crate::execution`] 的 per-session claim 对应，避免跨会话互相覆盖；
//! - terminal 通知在 durable 里留一条 `deliveries` 记录（幂等键
//!   `task_id:terminal_seq`），同时放进进程内待投递队列；父会话在下一轮 run 开始时取走
//!   并作为自定义消息注入。通知失败/进程退出时，启动 sweep 会按
//!   `(task_id, terminal_seq)` 重发同一 key（at-least-once + 幂等 key）。
//!
//! 这里只做 id/状态映射：`BackgroundTaskRecord` 的 JSON 就是 durable 文档里的值，
//! record 的终态转换由 [`BackgroundTaskJournal::finish`] 的 CAS 语义保证只有一次赢家。
//! 没有新增队列，也没有第二份账本。
//!
//! 参考：
//! - `docs/adr/durable.md`（唯一调度/事务底座）；
//! - `docs/history/porting/handoffs/phase-11b.md` §2.2（journal/notifier 注入契约）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use kiana_durable::{
    BoxError, DocDefinition, DocTarget, DocToken, DocumentSemantics, SessionChange, Tx, define_doc,
};
use kiana_durable::{JsonObject, JsonValue};
use serde_json::{Map, Value};

use crate::durable_store::{DurableSessionSlot, DurableSessionStore};

use super::journal::{
    BackgroundTaskError, BackgroundTaskJournal, BackgroundTaskNotification, BackgroundTaskNotifier,
    BackgroundTaskRecord, BackgroundTaskTerminal,
};

/// 后台任务 journal 文档（每个会话存储一个）。
static BACKGROUND_JOURNAL_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new("kiana.background_tasks", 1, DocumentSemantics::SESSION).with_initial(
            |_| {
                let mut value = JsonObject::new();
                value.insert("records".to_owned(), JsonValue::Object(Map::new()));
                value.insert("deliveries".to_owned(), JsonValue::Object(Map::new()));
                value.insert(
                    "consumedDeliveries".to_owned(),
                    JsonValue::Object(Map::new()),
                );
                value
            },
        ),
    )
});

fn journal_target() -> DocTarget {
    DocTarget::session(&BACKGROUND_JOURNAL_DOC)
}

/// 一个提交里要做的操作。
enum JournalOp {
    /// 首次写入。
    Save(BackgroundTaskRecord),
    /// Running -> terminal 的 CAS。
    Finish {
        task_id: String,
        terminal: BackgroundTaskTerminal,
        output_bytes: u64,
        output_truncated: bool,
    },
    /// 运行中任务 attempts +1。
    RecordAttempt { task_id: String },
    /// 标记通知已发出。
    MarkNotified { task_id: String, terminal_seq: u64 },
    /// 记录一份通知投递（幂等键 `task_id:terminal_seq`）。
    RecordDelivery(BackgroundTaskNotification),
    /// 标记投递已被父会话消费；重启后不再重放。
    ConsumeDeliveries { keys: Vec<String> },
}

struct JournalChange {
    op: JournalOp,
}

fn records_of(value: &JsonObject) -> Option<&Map<String, Value>> {
    value.get("records").and_then(JsonValue::as_object)
}

/// 取得 `records` 映射；损坏文档返回错误而不是 panic。
///
/// durable 文档可能被外部损坏（截断、手工编辑）。读路径对同一损坏返回空/报错，
/// 写路径若 `expect` 会让 finish 任务卡在 Running 且没有通知，因此一律 fail-closed。
fn records_mut(value: &mut JsonObject) -> Result<&mut Map<String, Value>, BoxError> {
    if !value.contains_key("records") {
        value.insert(
            "records".to_owned(),
            JsonValue::Object(serde_json::Map::new()),
        );
    }
    value
        .get_mut("records")
        .and_then(JsonValue::as_object_mut)
        .ok_or_else(|| -> BoxError { Box::new(std::io::Error::other(CORRUPT_RECORDS)) })
}

fn deliveries_mut(value: &mut JsonObject) -> Result<&mut Map<String, Value>, BoxError> {
    if !value.contains_key("deliveries") {
        value.insert(
            "deliveries".to_owned(),
            JsonValue::Object(serde_json::Map::new()),
        );
    }
    value
        .get_mut("deliveries")
        .and_then(JsonValue::as_object_mut)
        .ok_or_else(|| -> BoxError { Box::new(std::io::Error::other(CORRUPT_DELIVERIES)) })
}

fn consumed_deliveries_mut(value: &mut JsonObject) -> Result<&mut Map<String, Value>, BoxError> {
    if !value.contains_key("consumedDeliveries") {
        value.insert(
            "consumedDeliveries".to_owned(),
            JsonValue::Object(serde_json::Map::new()),
        );
    }
    value
        .get_mut("consumedDeliveries")
        .and_then(JsonValue::as_object_mut)
        .ok_or_else(|| -> BoxError { Box::new(std::io::Error::other(CORRUPT_CONSUMED)) })
}

const CORRUPT_RECORDS: &str = "background journal records must be an object";
const CORRUPT_DELIVERIES: &str = "background journal deliveries must be an object";
const CORRUPT_CONSUMED: &str = "background journal consumedDeliveries must be an object";

fn decode_record(value: &Value) -> Result<BackgroundTaskRecord, BackgroundTaskError> {
    serde_json::from_value(value.clone())
        .map_err(|error| BackgroundTaskError::Journal(format!("decoding task record: {error}")))
}

#[async_trait]
impl SessionChange for JournalChange {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let mut draft = tx.doc(&journal_target()).await?;
        let value = draft.value_mut();
        match &self.op {
            JournalOp::Save(record) => {
                let encoded = serde_json::to_value(record)
                    .map_err(|error| -> BoxError { Box::new(error) })?;
                records_mut(value)?.insert(record.task_id.clone(), encoded);
                Ok(Box::new(None::<BackgroundTaskRecord>))
            }
            JournalOp::Finish {
                task_id,
                terminal,
                output_bytes,
                output_truncated,
            } => {
                let Some(stored) = records_of(value).and_then(|records| records.get(task_id))
                else {
                    return Ok(Box::new(None::<BackgroundTaskRecord>));
                };
                let mut record = decode_record(stored)?;
                if record.is_terminal() {
                    return Ok(Box::new(None::<BackgroundTaskRecord>));
                }
                record.status = terminal.status;
                record.terminal_seq = record.terminal_seq.saturating_add(1);
                record.terminal = Some(terminal.clone());
                record.output_bytes = *output_bytes;
                record.output_truncated = *output_truncated;
                record.notified = false;
                record.updated_at_ms = now_millis();
                let encoded = serde_json::to_value(&record)
                    .map_err(|error| -> BoxError { Box::new(error) })?;
                records_mut(value)?.insert(task_id.clone(), encoded);
                Ok(Box::new(Some(record)))
            }
            JournalOp::RecordAttempt { task_id } => {
                let Some(stored) = records_of(value).and_then(|records| records.get(task_id))
                else {
                    return Ok(Box::new(None::<BackgroundTaskRecord>));
                };
                let mut record = decode_record(stored)?;
                if record.is_terminal() {
                    return Ok(Box::new(None::<BackgroundTaskRecord>));
                }
                record.attempts = record.attempts.saturating_add(1);
                record.updated_at_ms = now_millis();
                let encoded = serde_json::to_value(&record)
                    .map_err(|error| -> BoxError { Box::new(error) })?;
                records_mut(value)?.insert(task_id.clone(), encoded);
                Ok(Box::new(Some(record)))
            }
            JournalOp::MarkNotified {
                task_id,
                terminal_seq,
            } => {
                if let Some(stored) = records_of(value).and_then(|records| records.get(task_id)) {
                    let mut record = decode_record(stored)?;
                    if record.terminal_seq == *terminal_seq {
                        record.notified = true;
                        record.updated_at_ms = now_millis();
                        let encoded = serde_json::to_value(&record)
                            .map_err(|error| -> BoxError { Box::new(error) })?;
                        records_mut(value)?.insert(task_id.clone(), encoded);
                    }
                }
                Ok(Box::new(()))
            }
            JournalOp::RecordDelivery(notification) => {
                let key = format!("{}:{}", notification.task_id, notification.terminal_seq);
                let encoded = serde_json::to_value(notification)
                    .map_err(|error| -> BoxError { Box::new(error) })?;
                deliveries_mut(value)?.insert(key, encoded);
                Ok(Box::new(()))
            }
            JournalOp::ConsumeDeliveries { keys } => {
                let consumed = consumed_deliveries_mut(value)?;
                for key in keys {
                    consumed.insert(key.clone(), JsonValue::Bool(true));
                }
                Ok(Box::new(()))
            }
        }
    }
}

/// durable 驱动的后台任务 journal + 通知投递队列。
pub struct DurableTaskJournal {
    store: DurableSessionStore,
    /// task id -> session id（避免每次 load 都扫描目录）。
    task_owner: tokio::sync::Mutex<HashMap<String, String>>,
    notifications: Mutex<Vec<BackgroundTaskNotification>>,
}

impl std::fmt::Debug for DurableTaskJournal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableTaskJournal")
            .field("root", &self.store.root())
            .finish_non_exhaustive()
    }
}

impl DurableTaskJournal {
    /// 在 `root` 下为每个会话创建独立的 durable JSONL 存储。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            store: DurableSessionStore::new(root),
            task_owner: tokio::sync::Mutex::new(HashMap::new()),
            notifications: Mutex::new(Vec::new()),
        }
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        self.store.root()
    }

    /// 释放缓存的会话槽位，让出目录租约（干净关停用）。
    ///
    /// 返回 `false` 表示有并发打开正在进行；租约最终由进程退出释放。
    pub(crate) fn release_sessions(&self) -> bool {
        self.store.release_sessions_blocking()
    }

    async fn slot_for(
        &self,
        session_id: &str,
    ) -> Result<Arc<DurableSessionSlot>, BackgroundTaskError> {
        self.store
            .slot_for(session_id)
            .await
            .map_err(BackgroundTaskError::Journal)
    }

    /// 找到 task 所属的会话；内存索引未命中时扫描所有会话目录（只在重启后的第一次
    /// load 发生）。
    async fn slot_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<Arc<DurableSessionSlot>>, BackgroundTaskError> {
        let known = self.task_owner.lock().await.get(task_id).cloned();
        if let Some(session_id) = known {
            return self.slot_for(&session_id).await.map(Some);
        }
        for (_, slot) in self.all_sessions().await? {
            let snapshot = slot
                .snapshot(&journal_target())
                .await
                .map_err(BackgroundTaskError::Journal)?;
            let Some(stored) = snapshot
                .as_ref()
                .and_then(|value| records_of(value))
                .and_then(|records| records.get(task_id))
            else {
                continue;
            };
            // `all_sessions` 返回的是目录名（清洗 + 哈希），不是可再次用于 `slot_for`
            // 的 session id；必须从记录正文取真实的 parentSessionId，否则下一次 load
            // 会按目录名重新哈希并打开一个空存储。
            let record = decode_record(stored)?;
            self.task_owner
                .lock()
                .await
                .insert(task_id.to_owned(), record.parent_session_id);
            return Ok(Some(slot));
        }
        Ok(None)
    }

    /// 需要扫描的会话存储：`Some(id)` 只打开该父会话，`None` 扫描根目录下全部会话。
    async fn sessions_for(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<(String, Arc<DurableSessionSlot>)>, BackgroundTaskError> {
        match session_id {
            Some(session_id) => Ok(vec![(
                session_id.to_owned(),
                self.slot_for(session_id).await?,
            )]),
            None => self.all_sessions().await,
        }
    }

    /// 打开根目录下所有已有的会话存储。
    async fn all_sessions(
        &self,
    ) -> Result<Vec<(String, Arc<DurableSessionSlot>)>, BackgroundTaskError> {
        self.store
            .all_sessions()
            .await
            .map_err(BackgroundTaskError::Journal)
    }

    async fn commit(
        &self,
        session_id: &str,
        op: JournalOp,
    ) -> Result<Box<dyn std::any::Any + Send>, BackgroundTaskError> {
        self.store
            .commit(session_id, &JournalChange { op })
            .await
            .map_err(BackgroundTaskError::Journal)
    }

    /// 取走待投递的 terminal 通知（父会话在下一轮 run 开始时消费）。
    pub fn take_notifications(&self) -> Vec<BackgroundTaskNotification> {
        let mut notifications = self
            .notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *notifications)
    }

    /// 只取走属于指定父会话的 terminal 通知。
    ///
    /// durable journal 的根目录被同一进程里的多个会话共享：某个会话的启动 sweep
    /// 可能替另一个会话落定记录，并把通知放进这个进程级队列。按 `parentSessionId`
    /// 定向 drain，避免把兄弟会话的任务状态注入当前模型（并误 ack 它的投递）。
    pub fn take_notifications_for(
        &self,
        parent_session_id: &str,
    ) -> Vec<BackgroundTaskNotification> {
        let mut notifications = self
            .notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut taken = Vec::new();
        let mut index = 0;
        while index < notifications.len() {
            if notifications[index].parent_session_id == parent_session_id {
                taken.push(notifications.remove(index));
            } else {
                index += 1;
            }
        }
        taken
    }

    /// 当前待投递通知数量。
    pub fn pending_notifications(&self) -> usize {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// 把通知放进进程内待投递队列（按 `task_id:terminal_seq` 去重）。
    fn enqueue_notification(&self, notification: BackgroundTaskNotification) {
        let key = notification_key(&notification);
        let mut notifications = self
            .notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !notifications
            .iter()
            .any(|existing| notification_key(existing) == key)
        {
            notifications.push(notification);
        }
    }

    /// 重新装载“已投递、但父会话尚未消费”的 terminal 通知。
    ///
    /// 进程在 `notify` 之后、父会话 `take_notifications` 之前退出时，通知只存在于
    /// durable 的 `deliveries` 里；这里在启动 sweep 时把它放回进程内队列，避免终态
    /// 事件跨重启丢失。消费后由 [`Self::mark_deliveries_consumed`] 记下 `consumed`
    /// 标记，下一次重启不再重放（消费确认是异步的，崩溃窗口内可能重复投递一次，
    /// 与 `(task_id, terminal_seq)` 幂等键的 at-least-once 语义一致）。
    ///
    /// `session_id` 限定扫描范围：durable journal 根目录被所有会话共享，不按父会话
    /// 过滤会把 A 会话的通知投进 B 会话，然后在 B 的消费路径上把 A 的投递标记为
    /// 已消费。
    pub async fn replay_unconsumed_deliveries(
        &self,
        session_id: Option<&str>,
    ) -> Result<(), BackgroundTaskError> {
        for (_, slot) in self.sessions_for(session_id).await? {
            let snapshot = slot
                .snapshot(&journal_target())
                .await
                .map_err(BackgroundTaskError::Journal)?;
            let Some(value) = snapshot.as_ref() else {
                continue;
            };
            let Some(deliveries) = value.get("deliveries").and_then(JsonValue::as_object) else {
                continue;
            };
            let consumed = value
                .get("consumedDeliveries")
                .and_then(JsonValue::as_object);
            for (key, encoded) in deliveries {
                if consumed.is_some_and(|consumed| consumed.contains_key(key)) {
                    continue;
                }
                let notification: BackgroundTaskNotification =
                    serde_json::from_value(encoded.clone()).map_err(|error| {
                        BackgroundTaskError::Journal(format!(
                            "decoding background notification: {error}"
                        ))
                    })?;
                self.enqueue_notification(notification);
            }
        }
        Ok(())
    }

    /// 确认这些通知已被父会话消费（持久化 `consumed` 标记）。
    pub async fn mark_deliveries_consumed(
        &self,
        notifications: &[BackgroundTaskNotification],
    ) -> Result<(), BackgroundTaskError> {
        let mut by_session: HashMap<String, Vec<String>> = HashMap::new();
        for notification in notifications {
            by_session
                .entry(notification.parent_session_id.clone())
                .or_default()
                .push(notification_key(notification));
        }
        for (session_id, keys) in by_session {
            self.commit(&session_id, JournalOp::ConsumeDeliveries { keys })
                .await?;
        }
        Ok(())
    }
}

fn downcast_record(
    value: Box<dyn std::any::Any + Send>,
) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
    value
        .downcast::<Option<BackgroundTaskRecord>>()
        .map(|boxed| *boxed)
        .map_err(|_| BackgroundTaskError::Journal("unexpected journal commit value".to_string()))
}

#[async_trait]
impl BackgroundTaskJournal for DurableTaskJournal {
    async fn save(&self, record: BackgroundTaskRecord) -> Result<(), BackgroundTaskError> {
        let task_id = record.task_id.clone();
        let session_id = record.parent_session_id.clone();
        self.commit(&session_id, JournalOp::Save(record)).await?;
        self.task_owner.lock().await.insert(task_id, session_id);
        Ok(())
    }

    async fn load(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        let Some(slot) = self.slot_for_task(task_id).await? else {
            return Ok(None);
        };
        let snapshot = slot
            .snapshot(&journal_target())
            .await
            .map_err(BackgroundTaskError::Journal)?;
        let Some(stored) = snapshot
            .as_ref()
            .and_then(|value| records_of(value))
            .and_then(|records| records.get(task_id))
        else {
            return Ok(None);
        };
        decode_record(stored).map(Some)
    }

    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError> {
        // 只读查询不能为了让“没有任务的会话”返回空列表而创建 durable 目录；先看目录
        // 是否存在，缺失等价于空列表（与 `MemoryTaskJournal` 的语义一致）。
        if !self.store.session_path(parent_session_id).exists() {
            return Ok(Vec::new());
        }
        let slot = self.slot_for(parent_session_id).await?;
        let snapshot = slot
            .snapshot(&journal_target())
            .await
            .map_err(BackgroundTaskError::Journal)?;
        let Some(records) = snapshot.as_ref().and_then(|value| records_of(value)) else {
            return Ok(Vec::new());
        };
        let mut records: Vec<BackgroundTaskRecord> = records
            .values()
            .map(decode_record)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|record| record.parent_session_id == parent_session_id)
            .collect();
        records.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        Ok(records)
    }

    async fn list_recoverable(&self) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError> {
        let mut recoverable = Vec::new();
        for (_, slot) in self.all_sessions().await? {
            let snapshot = slot
                .snapshot(&journal_target())
                .await
                .map_err(BackgroundTaskError::Journal)?;
            let Some(records) = snapshot.as_ref().and_then(|value| records_of(value)) else {
                continue;
            };
            for (task_id, stored) in records {
                let record = decode_record(stored)?;
                if !record.is_terminal() || !record.notified {
                    self.task_owner
                        .lock()
                        .await
                        .insert(task_id.clone(), record.parent_session_id.clone());
                    recoverable.push(record);
                }
            }
        }
        Ok(recoverable)
    }

    async fn finish(
        &self,
        task_id: &str,
        terminal: BackgroundTaskTerminal,
        output_bytes: u64,
        output_truncated: bool,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        let Some(record) = self.load(task_id).await? else {
            return Ok(None);
        };
        let session_id = record.parent_session_id.clone();
        let value = self
            .commit(
                &session_id,
                JournalOp::Finish {
                    task_id: task_id.to_owned(),
                    terminal,
                    output_bytes,
                    output_truncated,
                },
            )
            .await?;
        downcast_record(value)
    }

    async fn record_attempt(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        let Some(record) = self.load(task_id).await? else {
            return Ok(None);
        };
        let value = self
            .commit(
                &record.parent_session_id,
                JournalOp::RecordAttempt {
                    task_id: task_id.to_owned(),
                },
            )
            .await?;
        downcast_record(value)
    }

    async fn mark_notified(
        &self,
        task_id: &str,
        terminal_seq: u64,
    ) -> Result<(), BackgroundTaskError> {
        let Some(record) = self.load(task_id).await? else {
            return Ok(());
        };
        self.commit(
            &record.parent_session_id,
            JournalOp::MarkNotified {
                task_id: task_id.to_owned(),
                terminal_seq,
            },
        )
        .await?;
        Ok(())
    }

    async fn replay_pending_deliveries(
        &self,
        session_id: Option<&str>,
    ) -> Result<(), BackgroundTaskError> {
        DurableTaskJournal::replay_unconsumed_deliveries(self, session_id).await
    }
}

#[async_trait]
impl BackgroundTaskNotifier for DurableTaskJournal {
    async fn notify(
        &self,
        notification: &BackgroundTaskNotification,
    ) -> Result<(), BackgroundTaskError> {
        // 先在 durable 里留一条投递记录（幂等键），再把通知放进进程内队列。投递记录是
        // 审计与重启去重依据；队列本身是短生命周期的进程内投递通道。
        self.commit(
            &notification.parent_session_id,
            JournalOp::RecordDelivery(notification.clone()),
        )
        .await?;
        self.enqueue_notification(notification.clone());
        Ok(())
    }
}

fn notification_key(notification: &BackgroundTaskNotification) -> String {
    format!("{}:{}", notification.task_id, notification.terminal_seq)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background::journal::{BackgroundTaskStatus, BackgroundTaskTerminal, TaskUsage};
    use crate::durable_store::{OWNER_FILE, OWNER_LOCK_FILE, read_owner_pid};

    fn record(task_id: &str, session_id: &str) -> BackgroundTaskRecord {
        BackgroundTaskRecord {
            version: 1,
            task_id: task_id.to_string(),
            parent_session_id: session_id.to_string(),
            parent_turn_id: "turn-1".to_string(),
            tool: "bash".to_string(),
            description: None,
            status: BackgroundTaskStatus::Running,
            terminal_seq: 0,
            terminal: None,
            output_path: "/tmp/out".to_string(),
            output_bytes: 0,
            output_truncated: false,
            notified: false,
            attempts: 1,
            created_at_ms: now_millis(),
            updated_at_ms: now_millis(),
            pid: Some(std::process::id()),
            work_backed: false,
            work_id: None,
            run_id: None,
        }
    }

    #[tokio::test]
    async fn load_after_a_restart_scan_reuses_the_records_parent_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = DurableTaskJournal::new(temp.path());
        first
            .save(record("task_scan", "session-scan"))
            .await
            .expect("save");

        // 新实例的 task_owner 为空：第一次 load 走 all_sessions 扫描并把目录名写进
        // 索引；第二次 load 必须仍能命中真实记录，而不是按目录名重新打开空存储。
        let second = DurableTaskJournal::new(temp.path());
        let loaded = second
            .load("task_scan")
            .await
            .expect("first load")
            .expect("record after scan");
        assert_eq!(loaded.parent_session_id, "session-scan");
        let again = second
            .load("task_scan")
            .await
            .expect("second load")
            .expect("record after cached owner");
        assert_eq!(again.parent_session_id, "session-scan");
    }

    #[tokio::test]
    async fn records_survive_a_new_journal_instance_and_finish_is_a_cas() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = DurableTaskJournal::new(temp.path());
        first
            .save(record("task_1", "session-a"))
            .await
            .expect("save");

        // 新的实例（模拟进程重启）仍能读回运行中的记录。
        let second = DurableTaskJournal::new(temp.path());
        let loaded = second.load("task_1").await.expect("load").expect("record");
        assert_eq!(loaded.status, BackgroundTaskStatus::Running);
        let recoverable = second.list_recoverable().await.expect("list");
        assert_eq!(recoverable.len(), 1);

        let terminal = BackgroundTaskTerminal {
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            usage: TaskUsage::default(),
            acceptance: None,
            finished_at_ms: now_millis(),
        };
        let won = second
            .finish("task_1", terminal.clone(), 3, false)
            .await
            .expect("finish")
            .expect("first finish wins");
        assert_eq!(won.terminal_seq, 1);
        let lost = second
            .finish("task_1", terminal, 3, false)
            .await
            .expect("second finish");
        assert!(lost.is_none(), "terminal transition is compare-and-set");

        let third = DurableTaskJournal::new(temp.path());
        let loaded = third.load("task_1").await.expect("load").expect("record");
        assert!(loaded.is_terminal());
        assert_eq!(loaded.terminal_seq, 1);
        // 终态但尚未通知的记录仍会出现在 recoverable 里（重启后 sweep 负责补通知）。
        let pending = third.list_recoverable().await.expect("list");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].is_terminal());
        assert!(!pending[0].notified);
        third
            .mark_notified("task_1", 1)
            .await
            .expect("mark notified");
        assert!(
            DurableTaskJournal::new(temp.path())
                .list_recoverable()
                .await
                .expect("list")
                .is_empty()
        );
    }

    fn notification_fixture(task_id: &str, parent: &str) -> BackgroundTaskNotification {
        BackgroundTaskNotification {
            task_id: task_id.to_string(),
            terminal_seq: 1,
            parent_session_id: parent.to_string(),
            parent_turn_id: "turn-1".to_string(),
            tool: "bash".to_string(),
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            output_path: "/tmp/out".to_string(),
            output_bytes: 0,
            output_truncated: false,
            usage: TaskUsage::default(),
            acceptance: None,
        }
    }

    #[test]
    fn take_notifications_for_drains_only_the_requested_parent_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let journal = DurableTaskJournal::new(temp.path());
        journal.enqueue_notification(notification_fixture("task_a", "session-a"));
        journal.enqueue_notification(notification_fixture("task_b", "session-b"));

        let taken = journal.take_notifications_for("session-a");
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].task_id, "task_a");
        assert_eq!(journal.pending_notifications(), 1);

        let taken = journal.take_notifications_for("session-b");
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].task_id, "task_b");
        assert_eq!(journal.pending_notifications(), 0);
    }

    #[tokio::test]
    async fn unconsumed_deliveries_are_replayed_after_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let terminal = BackgroundTaskTerminal {
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            usage: TaskUsage::default(),
            acceptance: None,
            finished_at_ms: now_millis(),
        };
        let notification = BackgroundTaskNotification {
            task_id: "task_3".to_string(),
            terminal_seq: 1,
            parent_session_id: "session-c".to_string(),
            parent_turn_id: "turn-1".to_string(),
            tool: "bash".to_string(),
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            output_path: "/tmp/out".to_string(),
            output_bytes: 0,
            output_truncated: false,
            usage: TaskUsage::default(),
            acceptance: None,
        };
        {
            let first = DurableTaskJournal::new(temp.path());
            first
                .save(record("task_3", "session-c"))
                .await
                .expect("save");
            first
                .finish("task_3", terminal, 0, false)
                .await
                .expect("finish");
            first.notify(&notification).await.expect("notify");
            first
                .mark_notified("task_3", 1)
                .await
                .expect("mark notified");
            // 进程在父会话取走通知之前退出：进程内队列随进程消失。
            let _ = first.take_notifications();
        }

        let second = DurableTaskJournal::new(temp.path());
        second
            .replay_unconsumed_deliveries(Some("session-c"))
            .await
            .expect("replay");
        let replayed = second.take_notifications();
        assert_eq!(replayed.len(), 1, "delivery survives a process restart");
        assert_eq!(replayed[0].task_id, "task_3");
        second
            .mark_deliveries_consumed(&replayed)
            .await
            .expect("mark consumed");

        let third = DurableTaskJournal::new(temp.path());
        third
            .replay_unconsumed_deliveries(Some("session-c"))
            .await
            .expect("replay");
        assert!(
            third.take_notifications().is_empty(),
            "consumed deliveries must not be replayed"
        );
    }

    #[tokio::test]
    async fn replay_is_scoped_to_the_requested_parent_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let terminal = BackgroundTaskTerminal {
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            usage: TaskUsage::default(),
            acceptance: None,
            finished_at_ms: now_millis(),
        };
        let journal = DurableTaskJournal::new(temp.path());
        for (task_id, session_id) in [("task_a", "session-a"), ("task_b", "session-b")] {
            journal
                .save(record(task_id, session_id))
                .await
                .expect("save");
            journal
                .finish(task_id, terminal.clone(), 0, false)
                .await
                .expect("finish");
            let notification = BackgroundTaskNotification {
                task_id: task_id.to_string(),
                terminal_seq: 1,
                parent_session_id: session_id.to_string(),
                parent_turn_id: "turn-1".to_string(),
                tool: "bash".to_string(),
                status: BackgroundTaskStatus::Completed,
                reason: None,
                exit_code: Some(0),
                output_path: "/tmp/out".to_string(),
                output_bytes: 0,
                output_truncated: false,
                usage: TaskUsage::default(),
                acceptance: None,
            };
            journal.notify(&notification).await.expect("notify");
            journal
                .mark_notified(task_id, 1)
                .await
                .expect("mark notified");
            let _ = journal.take_notifications();
        }

        let second = DurableTaskJournal::new(temp.path());
        second
            .replay_unconsumed_deliveries(Some("session-b"))
            .await
            .expect("replay");
        let replayed = second.take_notifications();
        assert_eq!(replayed.len(), 1, "only the requested session's delivery");
        assert_eq!(replayed[0].task_id, "task_b");
    }

    #[tokio::test]
    async fn notification_delivery_is_idempotent_by_task_and_terminal_seq() {
        let temp = tempfile::tempdir().expect("tempdir");
        let journal = DurableTaskJournal::new(temp.path());
        journal
            .save(record("task_2", "session-b"))
            .await
            .expect("save");
        let terminal = BackgroundTaskTerminal {
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            usage: TaskUsage::default(),
            acceptance: None,
            finished_at_ms: now_millis(),
        };
        journal
            .finish("task_2", terminal, 0, false)
            .await
            .expect("finish");
        let notification = BackgroundTaskNotification {
            task_id: "task_2".to_string(),
            terminal_seq: 1,
            parent_session_id: "session-b".to_string(),
            parent_turn_id: "turn-1".to_string(),
            tool: "bash".to_string(),
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            output_path: "/tmp/out".to_string(),
            output_bytes: 0,
            output_truncated: false,
            usage: TaskUsage::default(),
            acceptance: None,
        };
        journal.notify(&notification).await.expect("notify");
        journal.notify(&notification).await.expect("notify again");
        let delivered = journal.take_notifications();
        assert_eq!(delivered.len(), 1, "in-process queue dedups the key");
        assert!(journal.take_notifications().is_empty());
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[tokio::test]
    async fn all_sessions_skips_a_store_held_by_a_flock_holder() {
        use std::os::unix::io::AsRawFd as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let directory = root.join("foreign-session");
        std::fs::create_dir_all(&directory).expect("directory");
        // 模拟另一个存活进程：真正持有目录锁。`owner.json` 的 pid 不是权威。
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(OWNER_LOCK_FILE))
            .expect("lock file");
        // SAFETY: 传标量；`lock` 在测试期间保持打开。
        let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert!(locked, "the test must hold the directory lock");

        let journal = DurableTaskJournal::new(&root);
        let sessions = journal.all_sessions().await.expect("sessions");
        assert!(
            sessions.is_empty(),
            "another live process's store must not be opened: {}",
            sessions.len()
        );
        drop(lock);
    }

    #[tokio::test]
    async fn all_sessions_claims_unowned_directories_for_this_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let directory = root.join("own-session");
        std::fs::create_dir_all(&directory).expect("directory");

        let journal = DurableTaskJournal::new(&root);
        let sessions = journal.all_sessions().await.expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            Some(std::process::id())
        );
    }
}
