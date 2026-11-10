//! `test/event-stream.test.ts` 的辅助：构造 `AssistantMessage` 与“睡一会儿”。
//!
//! TS 的 `message(timestamp, durationMs?)` 直接写对象字面量；Rust 侧用
//! [`AssistantMessage::default()`] 再补齐必填字段，避免每个测试重复 20 行。

use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, StopReason, Usage, UsageCost,
};

/// TS `message(timestamp, durationMs?)`：内容为空、`stopReason: "stop"`、零用量。
///
/// TS 的 `timestamp` 取 `Date.now()`；Rust 侧要求调用方显式给出毫秒值，以便测试
/// “早于流创建时刻”的分支（`timestamp < startedAt`）。
pub fn message(timestamp: i64, duration_ms: Option<f64>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: "openai-responses".to_owned(),
        provider: "openai".to_owned(),
        model: "m".to_owned(),
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp,
        duration_ms,
        ..AssistantMessage::default()
    }
}

/// `Date.now()` 的等价物（Unix 毫秒）。
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `await sleep(ms)`。
pub async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// `stream.push({ type: "done", reason: "stop", message })`。
pub fn push_done(
    stream: &AssistantMessageEventStream,
    message: AssistantMessage,
) -> Result<(), kiana_ai::StreamPushError> {
    stream.push(AssistantMessageEvent::Done {
        reason: kiana_ai::types::DoneReason::Stop,
        message: Box::new(message),
    })
}

/// `stream.push({ type: "error", reason: "error", error })`。
pub fn push_error(
    stream: &AssistantMessageEventStream,
    error: AssistantMessage,
) -> Result<(), kiana_ai::StreamPushError> {
    stream.push(AssistantMessageEvent::Error {
        reason: kiana_ai::types::ErrorReason::Error,
        error: Box::new(error),
    })
}

/// 排空事件流（`for await (...)`）：返回按到达顺序的全部事件。
pub async fn drain(stream: &AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    events
}

/// 事件类型序列（`Vec<kind>`），用于断言 “start → text_start → ... → done”。
pub fn kinds(events: &[AssistantMessageEvent]) -> Vec<&'static str> {
    events.iter().map(AssistantMessageEvent::kind).collect()
}
