//! 乐观用户消息队列。
//!
//! # 为什么需要
//!
//! 用户按下回车后，UI 必须立刻显示自己的消息；服务端通过事件流回显同一条消息时，不能
//! 再显示第二遍。网络中断/服务端崩溃时，这条“发送中”的气泡也不能永久挂着。
//!
//! # 确认规则
//!
//! * **trimmed 内容匹配**：比较 `content.trim()`（服务端收到的完整内容，可能带附件说明）。
//! * **服务端时间戳围栏**：入队时记录当前已知的最新服务端事件时间 `after_timestamp`；
//!   回显事件的时间戳必须 **严格大于** 该围栏，才允许消费。浏览器/本机时钟不能用来
//!   判断服务端事件顺序。
//! * **确认 id 去重**：消费成功时记录 `conversation_id:event_id`；同一确认事件被重放
//!   （断线重连、durable 重发）不会再次消费队列里的消息。
//! * **优先消费 sending**：先找 `sending` 的匹配项；找不到再允许 `error` 项被迟到的
//!   回显消费。
//! * **150s watchdog**：`sending` 超过 [`PENDING_MESSAGE_TIMEOUT_MS`] 仍未被确认时
//!   转 `error`（可重试）。重试会重置围栏并重新开始计时。
//!
//! # 与参考实现的关系
//!
//! 行为取自 OpenHands `src/stores/optimistic-user-message-store.ts`。时钟由调用方传入
//! （`now_ms`），因此测试不需要 sleep，也不会受真实时间影响。

use std::collections::{BTreeMap, HashSet};

/// 发送中的 watchdog 超时（150 秒）。
pub const PENDING_MESSAGE_TIMEOUT_MS: i64 = 150_000;

/// pending 消息状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingStatus {
    /// 已发送，等待服务端回显。
    Sending,
    /// 发送失败 / 超时；可重试。
    Error {
        /// 错误文本。
        message: String,
    },
}

/// 一条乐观用户消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingUserMessage {
    /// 本地 id。
    pub id: String,
    /// 会话 id（切换会话时不串台）。
    pub conversation_id: String,
    /// 气泡显示文本（用户输入）。
    pub text: String,
    /// 实际发送给服务端的完整内容（匹配键）。
    pub content: String,
    /// 状态。
    pub status: PendingStatus,
    /// 入队本地时间（毫秒）。
    pub timestamp_ms: i64,
    /// 服务端时间戳围栏；`None` 表示未建立围栏（任何时间戳都可匹配）。
    pub after_timestamp_ms: Option<i64>,
    /// 附件 URL。
    pub image_urls: Vec<String>,
    /// 文件 URL。
    pub file_urls: Vec<String>,
}

impl PendingUserMessage {
    /// 是否可重试（错误状态）。
    pub fn retryable(&self) -> bool {
        matches!(self.status, PendingStatus::Error { .. })
    }
}

/// 消费回显的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumedPending {
    /// 被消费的消息。
    pub message: PendingUserMessage,
    /// 确认 id（`conversation:event`），可用于日志。
    pub confirmation_id: String,
}

/// 队列。
#[derive(Debug, Default)]
pub struct PendingUserQueue {
    messages: Vec<PendingUserMessage>,
    confirmed_ids: HashSet<String>,
    next_id: u64,
    /// 每个会话最近一次观察到的服务端时间戳（围栏按会话隔离）。
    server_timestamps: BTreeMap<String, i64>,
}

impl PendingUserQueue {
    /// 新建空队列。
    pub fn new() -> Self {
        Self::default()
    }

    /// 所有 pending 消息（按入队顺序）。
    pub fn messages(&self) -> &[PendingUserMessage] {
        &self.messages
    }

    /// 某个会话的 pending 消息。
    pub fn messages_for(&self, conversation_id: &str) -> Vec<&PendingUserMessage> {
        self.messages
            .iter()
            .filter(|message| message.conversation_id == conversation_id)
            .collect()
    }

    /// 已确认的事件 id 数。
    pub fn confirmed_count(&self) -> usize {
        self.confirmed_ids.len()
    }

    /// 记录某个会话已知的最新服务端事件时间戳（作为该会话下一次入队的围栏）。
    pub fn observe_server_timestamp(&mut self, conversation_id: &str, timestamp_ms: Option<i64>) {
        if let Some(timestamp) = timestamp_ms {
            self.server_timestamps
                .entry(conversation_id.to_string())
                .and_modify(|current| *current = (*current).max(timestamp))
                .or_insert(timestamp);
        }
    }

    /// 某个会话当前的围栏。
    pub fn fence_for(&self, conversation_id: &str) -> Option<i64> {
        self.server_timestamps.get(conversation_id).copied()
    }

    /// 入队一条乐观消息，返回本地 id。
    pub fn enqueue(
        &mut self,
        conversation_id: &str,
        text: &str,
        content: Option<&str>,
        image_urls: Vec<String>,
        file_urls: Vec<String>,
        now_ms: i64,
    ) -> String {
        self.next_id += 1;
        let id = format!("pending-{}-{}", now_ms, self.next_id);
        let message = PendingUserMessage {
            id: id.clone(),
            conversation_id: conversation_id.to_string(),
            text: text.to_string(),
            content: content.unwrap_or(text).to_string(),
            status: PendingStatus::Sending,
            timestamp_ms: now_ms,
            after_timestamp_ms: self.fence_for(conversation_id),
            image_urls,
            file_urls,
        };
        self.messages.push(message);
        id
    }

    /// 标记为失败（API 拒绝等）。
    pub fn mark_error(&mut self, id: &str, message: &str) {
        if let Some(pending) = self.messages.iter_mut().find(|pending| pending.id == id) {
            pending.status = PendingStatus::Error {
                message: message.to_string(),
            };
        }
    }

    /// 重试：回到 `Sending`，刷新围栏并重新开始 watchdog。
    ///
    /// 返回是否找到该消息。
    pub fn mark_sending(&mut self, id: &str, now_ms: i64) -> bool {
        let Some(pending) = self.messages.iter_mut().find(|pending| pending.id == id) else {
            return false;
        };
        pending.status = PendingStatus::Sending;
        pending.after_timestamp_ms = self
            .server_timestamps
            .get(&pending.conversation_id)
            .copied();
        pending.timestamp_ms = now_ms;
        true
    }

    /// 从队列移除。
    pub fn remove(&mut self, id: &str) -> Option<PendingUserMessage> {
        let index = self.messages.iter().position(|pending| pending.id == id)?;
        Some(self.messages.remove(index))
    }

    /// 清空（切换会话等）。
    pub fn clear(&mut self) {
        self.messages.clear();
        self.confirmed_ids.clear();
    }

    /// 把临时会话 id（如 `task-`）迁移到真实会话 id；返回迁移条数。
    pub fn reassign_conversation(&mut self, from: &str, to: &str) -> usize {
        let mut moved = 0usize;
        for pending in &mut self.messages {
            if pending.conversation_id == from {
                pending.conversation_id = to.to_string();
                moved += 1;
            }
        }
        moved
    }

    /// watchdog：把所有超时仍为 `Sending` 的消息转成 `Error`，返回转换条数。
    pub fn tick(&mut self, now_ms: i64) -> usize {
        let mut changed = 0usize;
        for pending in &mut self.messages {
            if pending.status == PendingStatus::Sending
                && now_ms.saturating_sub(pending.timestamp_ms) >= PENDING_MESSAGE_TIMEOUT_MS
            {
                pending.status = PendingStatus::Error {
                    message: "Send timed out".to_string(),
                };
                changed += 1;
            }
        }
        changed
    }

    /// 用服务端回显确认一条 pending 消息。
    ///
    /// 返回被消费的消息；不匹配或重复确认返回 `None`。
    pub fn consume_matching(
        &mut self,
        conversation_id: &str,
        content: &str,
        event_id: &str,
        event_timestamp_ms: i64,
    ) -> Option<ConsumedPending> {
        let confirmation_id = format!("{conversation_id}:{event_id}");
        if self.confirmed_ids.contains(&confirmation_id) {
            return None;
        }
        let matches = |pending: &PendingUserMessage| {
            pending.conversation_id == conversation_id
                && pending
                    .after_timestamp_ms
                    .is_none_or(|after| event_timestamp_ms > after)
                && pending.content.trim() == content.trim()
        };
        let target = self
            .messages
            .iter()
            .position(|pending| pending.status == PendingStatus::Sending && matches(pending))
            .or_else(|| self.messages.iter().position(matches))?;
        let message = self.messages.remove(target);
        self.confirmed_ids.insert(confirmation_id.clone());
        Some(ConsumedPending {
            message,
            confirmation_id,
        })
    }
}
