//! `utils/event-stream.ts`——事件流本体与工厂函数。
//!
//! TS: `packages/ai/src/utils/event-stream.ts`。
//!
//! TS 里这个文件定义 `EventStream<T, R>`、`AssistantMessageEventStream` 与工厂
//! `createAssistantMessageEventStream()`。Rust 侧的 `EventStream` 语义已经冻结在
//! [`crate::event_stream::AssistantMessageEventStream`]（`ARCHITECTURE.md` §4.9：它属于骨架
//! 固定的跨 crate 契约，实现者不得改签名），因此本模块只做两件事：
//!
//! - 再导出冻结类型，让 `kiana_ai::utils::event_stream::AssistantMessageEventStream` 与 TS 的导入
//!   路径一一对应；
//! - 提供 [`create_assistant_message_event_stream`]（TS 工厂），以及用流的单调耗时给消息补
//!   `durationMs` 的 [`time_assistant_message`]（TS 的 `#time()`）。
//!
//! `EventStream<T, R>` 的泛型形态在 Rust 侧没有第二个使用方（`AssistantMessageEventStream`
//! 是唯一实例），所以不重复实现。
//!
//! # 执行流程
//!
//! 1. 调用方用 [`create_assistant_message_event_stream`] 建流（等价于 `new
//!    AssistantMessageEventStream()`）；
//! 2. 按 `start` → 各块 `*_start`/`*_delta`/`*_end` → `done`|`error` 的顺序 `push`；
//! 3. 底层 `push` 在终态事件上、`end(message)` 在提交结果前都会调用 `time_message`，
//!    因此**正常路径不需要**手动调用 [`time_assistant_message`]；
//! 4. `result()` 等待流结束并返回最终 [`AssistantMessage`]（错误也是以 `error` 事件/消息
//!    表示，而不是 `Err`）。只有在 `done`/`error` 终态事件或 `end(message)` 提交结果后才
//!    返回；仅调用 `end_without_result()`（TS 的无参 `end()`）结束时，若从未提交过结果，
//!    `result()` 会永远 pending，见
//!    [`crate::event_stream::AssistantMessageEventStream::result`] 的说明。
//!
//! # 使用方
//!
//! - 各 provider 适配器（`api/*`）：建流、把服务商事件转成 [`AssistantMessageEvent`] 后
//!   `push`，以 `done`/`error` 终态事件（或 `end(message)`）结束；
//! - 上层（agent / session / extension 运行时）：通过 `recv()`、`take_receiver()` 或
//!   `result()` 消费；
//! - [`crate::index`] 与 crate 根：再导出类型，供下游 crate 直接命名。
//!
//! # 设计原因
//!
//! `AssistantMessageEventStream` 的实现属于 `ARCHITECTURE.md` §4.9 冻结的跨 crate 契约，
//! 唯一实现放在 [`crate::event_stream`]。本文件若也定义一份 `EventStream`，会出现两个
//! 不兼容的类型，且 `kiana_ai::event_stream::*` 与 `kiana_ai::utils::event_stream::*` 的
//! 导入路径会指向不同对象。因此这里只做“按 TS 模块路径再导出 + 两个计时辅助函数”。
//!
//! # 注意事项
//!
//! - 两个计时函数与底层 `time_message` 共享同一条门槛：流已完成、消息已有 `durationMs`、
//!   或消息的 `timestamp` 早于流的创建时刻时，返回 `false` 且**不修改**消息。
//!   `timestamp` 门槛用于“转发别处已开始的响应”（例如稍后取回的 deferred 结果）不重新计时。
//! - [`AssistantMessageEventStream::push`] 按值接收事件，计时写回只发生在流内部保存的副本上；
//!   调用方传入的消息对象不会因 `push` 而改变（与 TS 就地修改的行为不同，见 crate 根
//!   `event_stream` 模块文档）。
//! - 除本文件的单元测试与 [`crate::index`] 的 `export *` 再导出外，这两个辅助函数在仓库内
//!   没有调用方。它们是 TS 私有方法 `#time()` 在 Rust 侧的公开等价入口，不是 TS 导出面的
//!   镜像；本模块的再导出只用于保持文件路径与 TS 模块一一对应（`spec/ai-core.md` §2.2.24：
//!   TS 该文件的导出只有 `EventStream`、`AssistantMessageEventStream` 与
//!   `createAssistantMessageEventStream`）。

pub use crate::event_stream::{
    AssistantMessageEventStream, EVENT_CHANNEL_CAPACITY, StreamEndedWithoutResult, StreamPushError,
};

use crate::types::{AssistantMessage, AssistantMessageEvent};

/// `createAssistantMessageEventStream()`。
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}

/// TS `AssistantMessageEventStream.#time(message)`：给最终消息补 `durationMs`。
///
/// 三个前提（同时满足才写入）：
/// - 流尚未结束（`done === false`）；
/// - 消息还没有 `durationMs`；
/// - 消息的 `timestamp` 不早于流的开始时间（转发别处开始的消息不重新计时）。
///
/// 写入 `durationMs` 后返回 `true`；否则返回 `false` 且不改消息。
///
/// 这是 TS 私有方法 `#time()` 的公开自由函数形式；底层 [`AssistantMessageEventStream`] 的
/// `push`/`end` 也调用同一个 [`AssistantMessageEventStream::time_message`]。因此对从内层流
/// 转发而来的消息再次调用时，若它已带 `durationMs`，会保留首次测量而不是重新计时。
pub fn time_assistant_message(
    stream: &AssistantMessageEventStream,
    message: &mut AssistantMessage,
) -> bool {
    stream.time_message(message)
}

/// 便利函数：按事件类型给 `done`/`error` 事件的消息补 `durationMs`。
///
/// 只匹配 [`AssistantMessageEvent::Done`] 与 [`AssistantMessageEvent::Error`] 两个终态变体，
/// 其余事件（start/delta/end 等）返回 `false` 且原样保留。
///
/// 事件以 `&mut` 传入并在原地修改；`true` 表示确实写入了 `durationMs`，`false` 表示事件
/// 不是终态，或底层门槛（流已完成 / 已有 `durationMs` / `timestamp` 早于流创建时刻）拒绝了
/// 写入。调用方随后再 `push` 时，底层会因 `durationMs` 已存在而不再计时，因此不会双计。
pub fn time_event_message(
    stream: &AssistantMessageEventStream,
    event: &mut AssistantMessageEvent,
) -> bool {
    match event {
        AssistantMessageEvent::Done { message, .. } => time_assistant_message(stream, message),
        AssistantMessageEvent::Error { error, .. } => time_assistant_message(stream, error),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AssistantMessageEvent;

    #[test]
    fn factory_creates_an_empty_stream() {
        let stream = create_assistant_message_event_stream();
        assert!(stream.started_at_unix_ms() > 0);
    }

    #[test]
    fn times_messages_that_started_with_the_stream() {
        let stream = create_assistant_message_event_stream();
        let mut message = AssistantMessage {
            timestamp: stream.started_at_unix_ms(),
            ..Default::default()
        };
        assert!(time_assistant_message(&stream, &mut message));
        assert!(message.duration_ms.is_some());
    }

    #[test]
    fn leaves_messages_alone_when_they_predate_the_stream_or_are_already_timed() {
        let stream = create_assistant_message_event_stream();
        let mut message = AssistantMessage {
            timestamp: stream.started_at_unix_ms() - 10,
            ..Default::default()
        };
        assert!(!time_assistant_message(&stream, &mut message));
        assert!(message.duration_ms.is_none());

        let mut message = AssistantMessage {
            timestamp: stream.started_at_unix_ms(),
            duration_ms: Some(7.0),
            ..Default::default()
        };
        assert!(!time_assistant_message(&stream, &mut message));
        assert_eq!(message.duration_ms, Some(7.0));
    }

    use std::time::Duration;

    use crate::types::{DoneReason, ErrorReason};

    fn started_message(stream: &AssistantMessageEventStream) -> AssistantMessage {
        AssistantMessage {
            timestamp: stream.started_at_unix_ms(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn times_the_final_done_and_error_messages_and_end_results() {
        // TS: "sets durationMs on the final done or error message of a response it saw start"
        let done_stream = create_assistant_message_event_stream();
        let answer = started_message(&done_stream);
        tokio::time::sleep(Duration::from_millis(20)).await;
        done_stream
            .push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(answer.clone()),
            })
            .expect("push done");
        let measured = match done_stream.recv().await.expect("done event") {
            AssistantMessageEvent::Done { message, .. } => {
                message.duration_ms.expect("done message timed")
            }
            other => panic!("unexpected event: {other:?}"),
        };
        assert!(measured >= 15.0, "durationMs = {measured}");
        done_stream.end(answer);
        let result_duration = done_stream
            .result()
            .await
            .expect("result")
            .duration_ms
            .expect("result timed");
        assert!(
            result_duration >= 15.0,
            "result durationMs = {result_duration}"
        );
        // TS 里 `result()` 就是被计时的 done 事件消息对象；Rust 传的是副本，只要求同一量级。
        assert!((result_duration - measured).abs() <= 1.0);

        let failed = create_assistant_message_event_stream();
        let error = started_message(&failed);
        failed
            .push(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: Box::new(error),
            })
            .expect("push error");
        match failed.recv().await.expect("error event") {
            AssistantMessageEvent::Error { error, .. } => {
                assert!(error.duration_ms.is_some_and(|duration| duration >= 0.0));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let ended = create_assistant_message_event_stream();
        ended.end(started_message(&ended));
        assert!(
            ended
                .result()
                .await
                .expect("result")
                .duration_ms
                .is_some_and(|duration| duration >= 0.0)
        );
    }

    #[tokio::test]
    async fn keeps_an_existing_duration_when_a_forwarding_stream_sees_it() {
        // TS: "keeps an existing duration, so a forwarding stream keeps the inner measurement"
        let inner = create_assistant_message_event_stream();
        inner
            .push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(started_message(&inner)),
            })
            .expect("inner push");
        let measured = match inner.recv().await.expect("inner done") {
            AssistantMessageEvent::Done { message, .. } => {
                message.duration_ms.expect("inner timed")
            }
            other => panic!("unexpected event: {other:?}"),
        };
        let outer = create_assistant_message_event_stream();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut forwarded = started_message(&inner);
        forwarded.duration_ms = Some(measured);
        outer
            .push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(forwarded),
            })
            .expect("outer push");
        match outer.recv().await.expect("outer done") {
            AssistantMessageEvent::Done { message, .. } => {
                assert_eq!(message.duration_ms, Some(measured));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(measured < 20.0, "measured = {measured}");

        let mut preset = started_message(&create_assistant_message_event_stream());
        preset.duration_ms = Some(1234.0);
        let mut done = AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(preset),
        };
        assert!(!time_event_message(
            &create_assistant_message_event_stream(),
            &mut done
        ));
        match done {
            AssistantMessageEvent::Done { message, .. } => {
                assert_eq!(message.duration_ms, Some(1234.0));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn leaves_messages_that_predate_the_stream_untimed() {
        // TS: "leaves a message untimed when it started before the stream"
        let stream = create_assistant_message_event_stream();
        let mut fetched = AssistantMessage {
            timestamp: stream.started_at_unix_ms() - 60_000,
            ..Default::default()
        };
        assert!(!time_assistant_message(&stream, &mut fetched));
        assert!(fetched.duration_ms.is_none());
    }

    #[tokio::test]
    async fn does_not_time_messages_after_the_stream_completed() {
        // TS: "does not time a message pushed after the stream completed"
        let stream = create_assistant_message_event_stream();
        stream
            .push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(started_message(&stream)),
            })
            .expect("first done");
        let mut late = started_message(&stream);
        assert!(!stream.time_message(&mut late));
        assert!(late.duration_ms.is_none());
        // TS `EventStream.push` 在 `done` 之后**静默忽略**（不返回错误）。
        assert_eq!(
            stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(late),
            }),
            Ok(())
        );

        let ended = create_assistant_message_event_stream();
        ended.end(started_message(&ended));
        let mut after_end = started_message(&ended);
        assert!(!ended.time_message(&mut after_end));
        assert!(after_end.duration_ms.is_none());
    }

    #[test]
    fn times_done_and_error_events_only() {
        let stream = create_assistant_message_event_stream();
        let message = AssistantMessage {
            timestamp: stream.started_at_unix_ms(),
            ..Default::default()
        };
        let mut done = AssistantMessageEvent::Done {
            reason: crate::types::DoneReason::Stop,
            message: Box::new(message),
        };
        assert!(time_event_message(&stream, &mut done));
        let mut text = AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".into(),
            partial: Box::new(AssistantMessage::default()),
        };
        assert!(!time_event_message(&stream, &mut text));
    }
}
