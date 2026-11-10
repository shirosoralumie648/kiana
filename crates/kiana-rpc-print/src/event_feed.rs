//! 多客户端事件流：全局 typed 流 → 编码一次 → 每连接 dropping queue。
//!
//! 设计参照：
//! - `reference_project/deer-flow/backend/packages/harness/deerflow/runtime/events/store/*`
//!   的 append-only durable 事件与 `after_seq` 补放；
//! - `reference_project/opencode/packages/client/src/shared-events.ts` 的 SSE/WS 单帧；
//! - `reference_project/qwen-code/packages/cli/src/serve/**` 的慢客户端隔离。
//!
//! # 语义
//!
//! - 每次 [`EventFeed::publish`] 只序列化一次 payload；[`FeedEvent::json`] 是 WS 的完整
//!   单帧，[`FeedEvent::to_sse_frame`] 是 SSE 的单条 `data:` 帧（两者共用同一份 JSON，
//!   不会为每个订阅者重新编码）。
//! - 每个订阅者有一条容量 [`DEFAULT_QUEUE_CAPACITY`] 的队列。发布时若队列满，只把
//!   **该订阅**标记 failed 并从 feed 移除；队列被丢弃，其它订阅者与进程内存不受影响。
//! - [`EventChannel::Durable`] 事件进入有界 replay log；[`EventChannel::Volatile`] 不重放。
//!   [`EventFeed::subscribe`] 的 `since` 只补 `seq > since` 的 durable 事件，因此断线重连
//!   不会重复投递已确认的事件。
//! - 新订阅的第一条事件固定是 `server.connected`；调用方按
//!   [`HEARTBEAT_INTERVAL_SECS`]（15s）调用 [`EventFeed::publish_heartbeat`]。
//! - REST 最新页用 [`EventFeed::latest_page`]；把返回的 `since` 交给
//!   [`EventFeed::subscribe`] 就完成 REST → WS 的交接，中间不丢不重。
//! - [`SeqCursor`] 用“最高连续 seq + ahead-set”处理乱序到达，ahead-set 上限
//!   [`MAX_AHEAD`]。
//!
//! # 不做什么
//!
//! 本模块不绑定具体 HTTP/WebSocket 服务器：它只产出帧与队列。`kiana-rpc-print` 现有的
//! stdin/stdout RPC 协议不受影响；daemon 接线层（本波不新增 crate）负责把
//! [`Subscription::try_recv`] 的帧写到 SSE/WS。

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// 每个连接默认队列容量。
pub const DEFAULT_QUEUE_CAPACITY: usize = 4096;
/// durable replay log 默认容量。
pub const DEFAULT_DURABLE_CAPACITY: usize = 4096;
/// `SeqCursor` ahead-set 上限。
pub const MAX_AHEAD: usize = 1024;
/// 心跳间隔（秒）。
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;
/// 新订阅的第一条事件类型。
pub const CONNECTED_EVENT: &str = "server.connected";
/// 心跳事件类型。
pub const HEARTBEAT_EVENT: &str = "server.heartbeat";

/// 事件通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventChannel {
    /// 只发给当前在线订阅者，不重放。
    Volatile,
    /// 进入 replay log，可按 `since` 补放。
    Durable,
}

/// 编码后的事件。
#[derive(Debug)]
pub struct FeedEvent {
    seq: u64,
    channel: EventChannel,
    event_type: String,
    json: Arc<str>,
}

impl FeedEvent {
    /// 全局单调序号。
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// 通道。
    pub fn channel(&self) -> EventChannel {
        self.channel
    }

    /// 事件类型（SSE 的 `event:` 字段）。
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    /// 预编码 JSON（WS 单帧）。
    pub fn json(&self) -> &str {
        &self.json
    }

    /// 解析后的 payload（测试/嵌入方读取用；不参与发送路径）。
    pub fn payload(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.json)
    }

    /// 一条完整的 SSE 帧；只有一个 `data:` 行。
    pub fn to_sse_frame(&self) -> String {
        format!(
            "id: {}\nevent: {}\ndata: {}\n\n",
            self.seq, self.event_type, self.json
        )
    }

    /// WS 单帧就是预编码 JSON。
    pub fn to_ws_frame(&self) -> &str {
        &self.json
    }
}

/// EventFeed 错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FeedError {
    /// 事件无法序列化。
    #[error("failed to encode event: {0}")]
    Encode(String),
}

/// 订阅队列状态（feed 与订阅者共享）。
struct SubscriptionState {
    id: u64,
    capacity: usize,
    queue: Mutex<VecDeque<Arc<FeedEvent>>>,
    failed: AtomicBool,
}

impl SubscriptionState {
    fn push(&self, event: Arc<FeedEvent>) -> bool {
        let mut queue = self.queue.lock().expect("subscription queue");
        if queue.len() >= self.capacity {
            self.failed.store(true, Ordering::SeqCst);
            queue.clear();
            return false;
        }
        queue.push_back(event);
        true
    }
}

/// 一个客户端订阅。
pub struct Subscription {
    state: Arc<SubscriptionState>,
    feed: Weak<Mutex<FeedInner>>,
}

impl Subscription {
    /// 订阅 id。
    pub fn id(&self) -> u64 {
        self.state.id
    }

    /// 取下一条事件；队列空返回 `None`。
    pub fn try_recv(&self) -> Option<Arc<FeedEvent>> {
        self.state
            .queue
            .lock()
            .expect("subscription queue")
            .pop_front()
    }

    /// 取出当前全部事件（保持顺序）。
    pub fn drain(&self) -> Vec<Arc<FeedEvent>> {
        self.state
            .queue
            .lock()
            .expect("subscription queue")
            .drain(..)
            .collect()
    }

    /// 队列是否因溢出被标记失败。
    pub fn is_failed(&self) -> bool {
        self.state.failed.load(Ordering::SeqCst)
    }

    /// 关闭订阅并从 feed 注销；幂等。
    pub fn close(&self) {
        self.state.failed.store(true, Ordering::SeqCst);
        if let Some(feed) = self.feed.upgrade() {
            feed.lock()
                .expect("feed")
                .subscribers
                .remove(&self.state.id);
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(feed) = self.feed.upgrade() {
            feed.lock()
                .expect("feed")
                .subscribers
                .remove(&self.state.id);
        }
    }
}

struct FeedInner {
    next_seq: u64,
    next_subscriber_id: u64,
    queue_capacity: usize,
    durable_capacity: usize,
    subscribers: HashMap<u64, Arc<SubscriptionState>>,
    durable: VecDeque<Arc<FeedEvent>>,
}

/// 订阅结果。
pub struct SubscribeOutcome {
    /// 订阅；队列已包含 `server.connected` 与需要补放的 durable 事件。
    pub subscription: Subscription,
    /// `since` 早于 replay log 起点时的最早可补序号；否则 `None`。
    pub gap: Option<u64>,
}

/// REST 最新页。
#[derive(Debug, Clone)]
pub struct LatestPage {
    /// 最新 durable 事件（按 seq 升序）。
    pub events: Vec<Arc<FeedEvent>>,
    /// 交给 WS `since` 的游标（本页最后一条的 seq；空页为当前最新 seq）。
    pub since: u64,
    /// 是否还有更早的事件。
    pub has_more: bool,
}

#[derive(Serialize)]
struct WireEvent<'a, T: Serialize> {
    seq: u64,
    channel: EventChannel,
    #[serde(rename = "type")]
    event_type: &'a str,
    data: &'a T,
}

/// 全局事件流；clone 共享同一状态。
#[derive(Clone)]
pub struct EventFeed {
    inner: Arc<Mutex<FeedInner>>,
}

impl Default for EventFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl EventFeed {
    /// 默认容量（4096 / 4096）。
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_QUEUE_CAPACITY, DEFAULT_DURABLE_CAPACITY)
    }

    /// 自定义容量。
    pub fn with_limits(queue_capacity: usize, durable_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FeedInner {
                next_seq: 1,
                next_subscriber_id: 1,
                queue_capacity: queue_capacity.max(2),
                durable_capacity: durable_capacity.max(1),
                subscribers: HashMap::new(),
                durable: VecDeque::new(),
            })),
        }
    }

    /// 发布一个 typed 事件；只序列化一次。失败时不消耗 seq。
    pub fn publish<T: Serialize>(
        &self,
        channel: EventChannel,
        event_type: &str,
        data: &T,
    ) -> Result<Arc<FeedEvent>, FeedError> {
        let mut inner = self.inner.lock().expect("feed");
        let seq = inner.next_seq;
        let wire = WireEvent {
            seq,
            channel,
            event_type,
            data,
        };
        let json =
            serde_json::to_string(&wire).map_err(|error| FeedError::Encode(error.to_string()))?;
        let event = Arc::new(FeedEvent {
            seq,
            channel,
            event_type: event_type.to_string(),
            json: Arc::from(json),
        });
        inner.next_seq = inner.next_seq.saturating_add(1);
        if channel == EventChannel::Durable {
            inner.durable.push_back(Arc::clone(&event));
            while inner.durable.len() > inner.durable_capacity {
                inner.durable.pop_front();
            }
        }
        let mut failed = Vec::new();
        for (id, subscriber) in &inner.subscribers {
            if !subscriber.push(Arc::clone(&event)) {
                failed.push(*id);
            }
        }
        for id in failed {
            inner.subscribers.remove(&id);
        }
        Ok(event)
    }

    /// 发布 heartbeat（volatile）。
    pub fn publish_heartbeat(&self, now_ms: u128) -> Result<Arc<FeedEvent>, FeedError> {
        self.publish(
            EventChannel::Volatile,
            HEARTBEAT_EVENT,
            &serde_json::json!({"at": now_ms}),
        )
    }

    /// 订阅；`since` 只补放 `seq > since` 的 durable 事件。
    ///
    /// 返回时订阅队列的顺序是：`server.connected` → 补放的 durable 事件 → 之后的 live
    /// 事件（发布在锁外发生，因此 live 事件不会插到补放之前）。
    pub fn subscribe(&self, since: Option<u64>) -> SubscribeOutcome {
        let mut inner = self.inner.lock().expect("feed");
        let id = inner.next_subscriber_id;
        inner.next_subscriber_id = inner.next_subscriber_id.saturating_add(1);
        let state = Arc::new(SubscriptionState {
            id,
            capacity: inner.queue_capacity,
            queue: Mutex::new(VecDeque::new()),
            failed: AtomicBool::new(false),
        });

        // `server.connected` 不占全局 seq；用当前最新序号作为它的 SSE id。
        let connected_seq = inner.next_seq.saturating_sub(1);
        let connected = Arc::new(FeedEvent {
            seq: connected_seq,
            channel: EventChannel::Volatile,
            event_type: CONNECTED_EVENT.to_string(),
            json: Arc::from(
                serde_json::json!({
                    "seq": connected_seq,
                    "type": CONNECTED_EVENT,
                    "data": {
                        "heartbeatIntervalSeconds": HEARTBEAT_INTERVAL_SECS,
                    }
                })
                .to_string(),
            ),
        });
        let _ = state.push(connected);

        let mut gap = None;
        if let Some(since) = since {
            let earliest = inner
                .durable
                .front()
                .map(|event| event.seq)
                .unwrap_or(inner.next_seq);
            if earliest > since.saturating_add(1) {
                gap = Some(earliest);
            }
            let mut replay: Vec<Arc<FeedEvent>> = inner
                .durable
                .iter()
                .filter(|event| event.seq > since)
                .cloned()
                .collect();
            // `server.connected` 已占一个位置；补放不能把队列挤爆。
            let capacity = inner.queue_capacity.saturating_sub(1);
            if replay.len() > capacity {
                let drop_count = replay.len() - capacity;
                replay.drain(0..drop_count);
                if let Some(first) = replay.first() {
                    gap = Some(first.seq);
                }
            }
            for event in replay {
                let _ = state.push(event);
            }
        }

        inner.subscribers.insert(id, Arc::clone(&state));
        SubscribeOutcome {
            subscription: Subscription {
                state,
                feed: Arc::downgrade(&self.inner),
            },
            gap,
        }
    }

    /// REST 最新一页 durable 事件。
    pub fn latest_page(&self, limit: usize) -> LatestPage {
        let inner = self.inner.lock().expect("feed");
        let limit = limit.max(1);
        let total = inner.durable.len();
        let take = total.min(limit);
        let events: Vec<Arc<FeedEvent>> =
            inner.durable.iter().skip(total - take).cloned().collect();
        let since = events
            .last()
            .map(|event| event.seq)
            .unwrap_or_else(|| inner.next_seq.saturating_sub(1));
        LatestPage {
            events,
            since,
            has_more: total > take,
        }
    }

    /// 当前订阅者数量。
    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().expect("feed").subscribers.len()
    }

    /// 下一个将被分配的 seq。
    pub fn next_seq(&self) -> u64 {
        self.inner.lock().expect("feed").next_seq
    }
}

/// `SeqCursor` 错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SeqCursorError {
    /// ahead-set 超过上限：调用方应重新走 REST 最新页。
    #[error("sequence gap too large: {ahead} observations ahead of {expected}, max {max}")]
    GapTooLarge {
        /// 期望的下一个 seq。
        expected: u64,
        /// ahead-set 当前大小。
        ahead: usize,
        /// 上限。
        max: usize,
    },
}

/// 断线重连的序号游标：最高连续 seq + 乱序 ahead-set。
#[derive(Debug, Clone)]
pub struct SeqCursor {
    next_expected: u64,
    ahead: BTreeSet<u64>,
    max_ahead: usize,
}

impl SeqCursor {
    /// `after` 是调用方已确认的最后一个 seq。
    pub fn new(after: u64) -> Self {
        Self::with_max_ahead(after, MAX_AHEAD)
    }

    /// 自定义 ahead 上限（测试用）。
    pub fn with_max_ahead(after: u64, max_ahead: usize) -> Self {
        Self {
            next_expected: after.saturating_add(1),
            ahead: BTreeSet::new(),
            max_ahead: max_ahead.max(1),
        }
    }

    /// 观察一个 seq；返回本次新确认的连续 seq 列表。
    ///
    /// 重复/过旧的 seq 返回空列表（幂等）。
    pub fn observe(&mut self, seq: u64) -> Result<Vec<u64>, SeqCursorError> {
        if seq < self.next_expected {
            return Ok(Vec::new());
        }
        if seq == self.next_expected {
            let mut confirmed = vec![seq];
            self.next_expected = self.next_expected.saturating_add(1);
            while self.ahead.remove(&self.next_expected) {
                confirmed.push(self.next_expected);
                self.next_expected = self.next_expected.saturating_add(1);
            }
            return Ok(confirmed);
        }
        self.ahead.insert(seq);
        if self.ahead.len() > self.max_ahead {
            return Err(SeqCursorError::GapTooLarge {
                expected: self.next_expected,
                ahead: self.ahead.len(),
                max: self.max_ahead,
            });
        }
        Ok(Vec::new())
    }

    /// 下一个期望的 seq。
    pub fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// 最高连续 seq（即最后一个已确认的 seq）。
    pub fn highest_contiguous(&self) -> u64 {
        self.next_expected.saturating_sub(1)
    }

    /// ahead-set 大小。
    pub fn ahead_len(&self) -> usize {
        self.ahead.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_once_and_emits_single_sse_or_ws_frame() {
        let feed = EventFeed::new();
        let event = feed
            .publish(
                EventChannel::Durable,
                "message",
                &serde_json::json!({"text": "a \"quoted\" line\nsecond"}),
            )
            .expect("publish");
        let frame = event.to_sse_frame();
        assert_eq!(frame.matches("data:").count(), 1);
        assert!(frame.starts_with(&format!("id: {}\nevent: message\ndata: ", event.seq())));
        assert!(frame.ends_with("\n\n"));
        assert_eq!(event.to_ws_frame(), event.json());
        assert!(event.json().contains("quoted"));
    }

    #[test]
    fn slow_subscriber_is_failed_without_affecting_fast_subscriber() {
        let feed = EventFeed::with_limits(2, 16);
        let slow = feed.subscribe(None).subscription;
        let fast = feed.subscribe(None).subscription;
        // 消化掉 server.connected。
        assert!(slow.try_recv().is_some());
        assert!(fast.try_recv().is_some());

        feed.publish(EventChannel::Volatile, "e1", &1).expect("e1");
        assert!(fast.try_recv().is_some());
        feed.publish(EventChannel::Volatile, "e2", &2).expect("e2");
        assert!(fast.try_recv().is_some());
        feed.publish(EventChannel::Volatile, "e3", &3).expect("e3");

        assert!(slow.is_failed(), "slow queue must be failed");
        assert_eq!(feed.subscriber_count(), 1, "only slow was removed");
        assert_eq!(fast.try_recv().expect("fast e3").seq(), 3);
    }

    #[test]
    fn reconnect_since_replays_only_missing_durable_events() {
        let feed = EventFeed::new();
        let a = feed.publish(EventChannel::Durable, "a", &1).expect("a");
        let b = feed.publish(EventChannel::Durable, "b", &2).expect("b");
        feed.publish(EventChannel::Volatile, "volatile", &99)
            .expect("v");
        let c = feed.publish(EventChannel::Durable, "c", &3).expect("c");

        let outcome = feed.subscribe(Some(a.seq()));
        assert!(outcome.gap.is_none());
        let events = outcome.subscription.drain();
        let types: Vec<&str> = events.iter().map(|event| event.event_type()).collect();
        assert_eq!(types, vec![CONNECTED_EVENT, "b", "c"]);
        let seqs: Vec<u64> = events
            .iter()
            .filter(|event| event.channel() == EventChannel::Durable)
            .map(|event| event.seq())
            .collect();
        assert_eq!(seqs, vec![b.seq(), c.seq()]);

        let d = feed.publish(EventChannel::Durable, "d", &4).expect("d");
        let live = outcome.subscription.try_recv().expect("live d");
        assert_eq!(live.seq(), d.seq());
    }

    #[test]
    fn subscribing_after_latest_replays_nothing() {
        let feed = EventFeed::new();
        feed.publish(EventChannel::Durable, "a", &1).expect("a");
        let last = feed.publish(EventChannel::Durable, "b", &2).expect("b");
        let outcome = feed.subscribe(Some(last.seq()));
        let events = outcome.subscription.drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), CONNECTED_EVENT);
    }

    #[test]
    fn latest_page_hands_off_to_ws_since_without_duplicates() {
        let feed = EventFeed::new();
        for n in 0..5 {
            feed.publish(EventChannel::Durable, "n", &n).expect("n");
        }
        let page = feed.latest_page(2);
        assert_eq!(page.events.len(), 2);
        assert!(page.has_more);
        assert_eq!(page.since, page.events.last().expect("last").seq());

        let outcome = feed.subscribe(Some(page.since));
        let replay = outcome.subscription.drain();
        assert_eq!(replay.len(), 1, "only connected, no duplicated page");
        let next = feed
            .publish(EventChannel::Durable, "next", &5)
            .expect("next");
        assert_eq!(
            outcome.subscription.try_recv().expect("live").seq(),
            next.seq()
        );
    }

    #[test]
    fn bounded_durable_log_reports_gap() {
        let feed = EventFeed::with_limits(64, 4);
        for n in 0..10 {
            feed.publish(EventChannel::Durable, "n", &n).expect("n");
        }
        let outcome = feed.subscribe(Some(1));
        assert_eq!(outcome.gap, Some(7), "earliest retained seq");
        let durable: Vec<u64> = outcome
            .subscription
            .drain()
            .into_iter()
            .filter(|event| event.channel() == EventChannel::Durable)
            .map(|event| event.seq())
            .collect();
        assert_eq!(durable, vec![7, 8, 9, 10]);
    }

    #[test]
    fn seq_cursor_handles_out_of_order_and_bounds_ahead_set() {
        let mut cursor = SeqCursor::new(0);
        assert!(cursor.observe(3).expect("ahead").is_empty());
        assert_eq!(cursor.ahead_len(), 1);
        assert_eq!(cursor.observe(1).expect("contiguous"), vec![1]);
        assert_eq!(cursor.observe(2).expect("drains ahead"), vec![2, 3]);
        assert_eq!(cursor.highest_contiguous(), 3);
        assert_eq!(cursor.observe(2).expect("duplicate"), Vec::<u64>::new());

        let mut cursor = SeqCursor::with_max_ahead(0, 4);
        for seq in 100..104 {
            assert!(cursor.observe(seq).expect("within bound").is_empty());
        }
        let error = cursor.observe(104).expect_err("over bound");
        assert!(matches!(error, SeqCursorError::GapTooLarge { .. }));
    }

    #[test]
    fn connected_and_heartbeat_events_are_emitted() {
        let feed = EventFeed::new();
        let subscription = feed.subscribe(None).subscription;
        let connected = subscription.try_recv().expect("connected");
        assert_eq!(connected.event_type(), CONNECTED_EVENT);

        let heartbeat = feed.publish_heartbeat(12_345).expect("heartbeat");
        let received = subscription.try_recv().expect("heartbeat");
        assert_eq!(received.event_type(), HEARTBEAT_EVENT);
        assert_eq!(received.seq(), heartbeat.seq());
        assert_eq!(received.payload().expect("payload")["data"]["at"], 12_345);
        assert_eq!(HEARTBEAT_INTERVAL_SECS, 15);
    }

    #[test]
    fn close_is_idempotent_and_removes_subscription() {
        let feed = EventFeed::new();
        let subscription = feed.subscribe(None).subscription;
        assert_eq!(feed.subscriber_count(), 1);
        subscription.close();
        assert_eq!(feed.subscriber_count(), 0);
        subscription.close();
        feed.publish(EventChannel::Volatile, "e", &1)
            .expect("publish");
        assert!(subscription.is_failed());
    }

    #[test]
    fn encoding_failure_does_not_consume_a_sequence() {
        #[derive(Serialize)]
        struct BadKey {
            map: HashMap<(i32, i32), i32>,
        }
        let feed = EventFeed::new();
        let before = feed.next_seq();
        let bad = BadKey {
            map: HashMap::from([((1, 2), 3)]),
        };
        let error = feed
            .publish(EventChannel::Durable, "bad", &bad)
            .expect_err("non-string key");
        assert!(matches!(error, FeedError::Encode(_)));
        assert_eq!(feed.next_seq(), before);
        assert_eq!(feed.latest_page(10).events.len(), 0);
    }
}
