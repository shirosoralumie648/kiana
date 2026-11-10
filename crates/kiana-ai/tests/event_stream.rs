//! `packages/ai/test/event-stream.test.ts` 的 Rust 移植。
//!
//! TS 的 `EventStream<T, R>` 是**泛型**队列（`FifoQueue` + 等待者队列）；Rust 侧冻结的
//! [`AssistantMessageEventStream`] 只针对 `AssistantMessageEvent`/`AssistantMessage`
//! （`ARCHITECTURE.md` §4.9），没有泛型版本。因此下面用 `text_delta` 事件表达“编号事件”、
//! 用 `Done`/`End` 表达“完成事件 + 最终结果”。映射与差异逐条写在测试的文档里：
//!
//! | TS | Rust |
//! |---|---|
//! | `stream.push(1)` | `push(TextDelta{delta:"1"})` |
//! | `EventStream(ev => ev === 3, ev => ev)` 的“完成事件携带结果” | `push(Done{message})` |
//! | `stream.end(result)` | `end(result)` |
//! | `stream.end()`（无结果） | `end_without_result()`（`result()` 继续 pending） |
//! | `iterator.next()` → `{value, done}` | `recv()` → `Option<event>` |
//! | 多个并发迭代器各得一个事件（注册序） | 多个并发 `recv()` 各注册一个等待者，按注册序各得一个事件 |

mod common;

use common::event_stream::{drain, kinds, message, now_ms, push_done, push_error, sleep};
use kiana_ai::event_stream::{
    AssistantMessageEventStream, EVENT_CHANNEL_CAPACITY, StreamEndedWithoutResult, StreamPushError,
};
use kiana_ai::types::{AssistantMessage, AssistantMessageEvent as Event};
use kiana_ai::types::{AssistantMessageEvent, DoneReason, ErrorReason, StopReason};

/// `push(TextDelta{delta})` 的简写（TS 的 `stream.push(1)`）。
fn push_delta(stream: &AssistantMessageEventStream, delta: &str) -> Result<(), StreamPushError> {
    stream.push(Event::TextDelta {
        content_index: 0,
        delta: delta.to_owned(),
        partial: Box::new(AssistantMessage::default()),
    })
}

/// 取 `TextDelta` 的 `delta`（断言用）。
fn delta_of(event: &AssistantMessageEvent) -> Option<&str> {
    match event {
        AssistantMessageEvent::TextDelta { delta, .. } => Some(delta),
        _ => None,
    }
}

/// 断言两条消息除 `durationMs` 外相同。
///
/// TS 的 `#time()` 直接改调用方传进来的对象（`answer.durationMs` 在 push 后可见）；冻结的 Rust API
/// 按值接收 `AssistantMessage`，写回只发生在流内部持有的副本上，因此调用方观察不到——测试只能比较
/// “除 `durationMs` 以外的字段”，并单独断言 `result()`/事件里的 `durationMs`（见 `deviations`）。
fn assert_same_except_duration(left: &AssistantMessage, right: &AssistantMessage) {
    let mut left = left.clone();
    let mut right = right.clone();
    left.duration_ms = None;
    right.duration_ms = None;
    assert_eq!(left, right);
}

// ---------------------------------------------------------------------------
// EventStream：队列语义（TS 的 5 条）
// ---------------------------------------------------------------------------

/// TS: "drains buffered events in order and ignores events pushed after completion"。
#[tokio::test]
async fn drains_buffered_events_in_order_and_ignores_events_pushed_after_completion() {
    let stream = AssistantMessageEventStream::new();
    push_delta(&stream, "1").expect("buffered push");
    push_delta(&stream, "2").expect("buffered push");
    let final_message = message(now_ms(), None);
    push_done(&stream, final_message.clone()).expect("done push");
    // TS 的 `push(4)` 在 `done` 之后被静默忽略（不排队、不报错）。
    assert_eq!(push_delta(&stream, "4"), Ok(()));

    // TS 的 `await stream.result()` == 完成事件携带的结果。
    let result = stream.result().await.expect("result resolves");
    assert_same_except_duration(&result, &final_message);
    assert!(result.duration_ms.is_some(), "the result is timed");

    // TS 的 `for await (...)` 排空：编号事件按顺序，随后是 `done`（`push(4)` 不在其中）。
    let events = drain(&stream).await;
    let deltas: Vec<&str> = events.iter().filter_map(delta_of).collect();
    assert_eq!(deltas, ["1", "2"]);
    assert_eq!(kinds(&events), ["text_delta", "text_delta", "done"]);
}

/// TS: "preserves order when events arrive after buffered draining starts"。
#[tokio::test]
async fn preserves_order_when_events_arrive_after_buffered_draining_starts() {
    let stream = AssistantMessageEventStream::new();
    push_delta(&stream, "1").expect("buffered push");
    push_delta(&stream, "2").expect("buffered push");

    // `iterator.next()` 三次。
    let first = stream.recv().await.expect("first event");
    assert_eq!(delta_of(&first), Some("1"));
    push_delta(&stream, "3").expect("late push");
    let second = stream.recv().await.expect("second event");
    assert_eq!(delta_of(&second), Some("2"));
    let third = stream.recv().await.expect("third event");
    assert_eq!(delta_of(&third), Some("3"));

    // `stream.end(3)` 之后迭代结束（`{value: undefined, done: true}`）。
    let result = message(now_ms(), None);
    stream.end(result.clone());
    assert!(stream.recv().await.is_none(), "iterator ends after end()");
    assert_same_except_duration(&stream.result().await.expect("result resolves"), &result);
}

/// TS: "delivers events to waiting consumers in registration order"。
///
/// 两个独立的 `recv()` future 各自注册一个等待者（与 TS 的两个 `AsyncIterator.next()` 相同）；
/// `push(1)` 交给最早注册的、`push(2)` 交给第二个。
#[tokio::test]
async fn delivers_events_to_waiting_consumers_in_registration_order() {
    let stream = AssistantMessageEventStream::new();
    let first = stream.recv();
    let second = stream.recv();
    // 两个 future 必须先把等待者压进队列（TS 里 `next()` 调用即注册）。
    let (first, second, ()) = futures::join!(first, second, async {
        tokio::task::yield_now().await;
        push_delta(&stream, "1").expect("push 1");
        push_delta(&stream, "2").expect("push 2");
    });
    assert_eq!(delta_of(&first.expect("first consumer")), Some("1"));
    assert_eq!(delta_of(&second.expect("second consumer")), Some("2"));
}

/// 回归：被取消的 `recv()` 不能吞掉事件。
///
/// 旧实现把 oneshot sender 留在 `waiting` 里；`select!`/超时取消一次 `recv()` 后，
/// 下一次 `push` 会弹出这个死 sender、投递失败，事件被丢弃。
#[tokio::test]
async fn cancelling_a_waiting_recv_does_not_lose_the_next_event() {
    let stream = AssistantMessageEventStream::new();

    // 注册等待者，然后取消它（任务 abort ≈ select! 输掉）。
    let cancelled = {
        let stream = stream.clone();
        tokio::spawn(async move { stream.recv().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    cancelled.abort();
    let _ = cancelled.await;

    push_delta(&stream, "1").expect("push after cancelled recv");
    let event = stream
        .recv()
        .await
        .expect("the event must survive a cancelled receiver");
    assert_eq!(delta_of(&event), Some("1"));
}

/// 回归：事件已经投递给等待者（`send` 成功）、但 `recv()` 在再次 poll 之前被取消时，
/// 事件必须回到队列，而不是随 future 一起被丢弃。
#[tokio::test]
async fn dropping_a_woken_recv_requeues_its_event() {
    let stream = AssistantMessageEventStream::new();
    let mut first = Box::pin(stream.recv());
    assert!(futures::poll!(first.as_mut()).is_pending());
    // push 选中这个等待者：事件写入它的槽位并唤醒它。
    push_delta(&stream, "1").expect("push to the pending waiter");
    // 唤醒已经发生，但 future 还没被再次 poll 就被 `select!` 丢弃。
    drop(first);
    let event = stream.recv().await.expect("the event must be requeued");
    assert_eq!(delta_of(&event), Some("1"));
}

/// 回归：取消两个等待者中的第一个时，被回收的事件要立即交给第二个等待者，
/// 而不是留在队列里等下一次 `push`（否则第二个消费者会先收到更晚的事件）。
#[tokio::test]
async fn cancelling_the_first_of_two_waiters_delivers_its_event_to_the_second() {
    let stream = AssistantMessageEventStream::new();
    let mut first = Box::pin(stream.recv());
    assert!(futures::poll!(first.as_mut()).is_pending());
    let second = {
        let stream = stream.clone();
        tokio::spawn(async move { stream.recv().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    push_delta(&stream, "1").expect("push to the first waiter");
    // 第一个 future 已经收到唤醒，但此刻被取消；它的事件必须转交给第二个等待者。
    drop(first);

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), second)
        .await
        .expect("the second waiter must be woken immediately")
        .expect("survivor task");
    assert_eq!(delta_of(&event.expect("event is requeued")), Some("1"));
}

/// 回归：取消最早的等待者后，事件要交给后面仍然存活的等待者，而不是滞留在队列里。
#[tokio::test]
async fn cancelling_the_first_waiter_delivers_to_the_next_one() {
    let stream = AssistantMessageEventStream::new();

    let first = {
        let stream = stream.clone();
        tokio::spawn(async move { stream.recv().await })
    };
    let second = {
        let stream = stream.clone();
        tokio::spawn(async move { stream.recv().await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    first.abort();
    let _ = first.await;

    push_delta(&stream, "1").expect("push after aborting the first waiter");
    let event = second
        .await
        .expect("the survivor task must finish")
        .expect("the survivor receives the event");
    assert_eq!(delta_of(&event), Some("1"));
}

/// 补充：一个已在等待的消费者会被后续 `push` 唤醒，且事件不丢不重。
#[tokio::test]
async fn delivers_events_to_a_waiting_consumer() {
    let stream = AssistantMessageEventStream::new();
    // `recv()` 先进入等待（等价 TS 的 `iterator.next()` 先注册），20ms 后再 push。
    let (first, ()) = futures::join!(stream.recv(), async {
        sleep(20).await;
        push_delta(&stream, "1").expect("push 1");
    });
    assert_eq!(delta_of(&first.expect("waiting consumer wakes")), Some("1"));

    // 第二个事件按顺序交给下一次 `recv()`（不重不丢）。
    push_delta(&stream, "2").expect("push 2");
    assert_eq!(
        delta_of(&stream.recv().await.expect("second event")),
        Some("2")
    );
}

/// 回归（review finding 10）：`recv()`/`result()` 的 future 必须 `Send`（等待期间不持锁），
/// 因此可以 `tokio::spawn` 到多线程运行时上并发消费——TS 里多个迭代器同时 `next()` 同理。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recv_and_result_futures_are_send_and_work_across_tasks() {
    fn assert_send<T: Send>(_: &T) {}
    let stream = AssistantMessageEventStream::new();
    let recv_future = stream.recv();
    assert_send(&recv_future);
    drop(recv_future);
    let result_future = stream.result();
    assert_send(&result_future);
    drop(result_future);

    let reader = {
        let stream = stream.clone();
        tokio::spawn(async move {
            // 队列里已有的事件（先入队再 spawn，保证顺序）。
            let mut deltas = Vec::new();
            while let Some(event) = stream.recv().await {
                match event {
                    Event::TextDelta { delta, .. } => deltas.push(delta),
                    _ => break,
                }
            }
            deltas
        })
    };
    push_delta(&stream, "1").expect("push 1");
    push_delta(&stream, "2").expect("push 2");
    sleep(10).await;
    stream.end(message(now_ms(), None));
    let deltas = reader.await.expect("reader task joins");
    assert_eq!(deltas, ["1", "2"]);
}

/// TS: "drains buffered events after end and resolves the explicit result"。
#[tokio::test]
async fn drains_buffered_events_after_end_and_resolves_the_explicit_result() {
    let stream = AssistantMessageEventStream::new();
    push_delta(&stream, "1").expect("buffered push");
    push_delta(&stream, "2").expect("buffered push");
    let result = message(now_ms(), None);
    stream.end(result.clone());

    assert_same_except_duration(&stream.result().await.expect("result resolves"), &result);
    let events = drain(&stream).await;
    let deltas: Vec<&str> = events.iter().filter_map(delta_of).collect();
    assert_eq!(deltas, ["1", "2"]);
}

/// TS: "wakes all waiting consumers when ended without a result"。
///
/// `end_without_result()` 是所有等待者以 `{done: true}` 醒来的无参 `end()`；
/// 它同时**不**提交结果（`result()` 继续 pending，见本测试 (b1)）。
#[tokio::test]
async fn wakes_all_waiting_consumers_when_ended_without_a_result() {
    let stream = AssistantMessageEventStream::new();
    let first = stream.recv();
    let second = stream.recv();
    let (first, second, ()) = futures::join!(first, second, async {
        tokio::task::yield_now().await;
        stream.end_without_result();
    });
    assert!(first.is_none(), "first consumer wakes with None");
    assert!(second.is_none(), "second consumer wakes with None");
}

/// `end(message)` 也会唤醒等待中的消费者（TS 的 `end(result)` 先 resolve 再唤醒）。
#[tokio::test]
async fn wakes_a_waiting_consumer_when_the_stream_ends() {
    let stream = AssistantMessageEventStream::new();
    let (received, ()) = futures::join!(stream.recv(), async {
        sleep(20).await;
        stream.end(message(now_ms(), None));
    });
    assert!(received.is_none(), "waiting consumer wakes with None");

    // (b1) 没有调用 `end` 时 TS 的 `result()` 永远 pending；Rust 侧只要还有句柄存活，
    // 结果通道就不会关闭，因此 `result()` 同样不 resolve。
    let pending = AssistantMessageEventStream::new();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), pending.result())
            .await
            .is_err(),
        "result() stays pending without end()"
    );

    // (b2) `result()` 是同一个 Promise：第一次调用超时不会“消耗”它，再调一次仍然 pending
    // （TS 的 `finalResultPromise` 也从不 reject；只有 `end()` 无参且从未 resolve 时永远 pending）。
    let reused = AssistantMessageEventStream::new();
    let first = tokio::time::timeout(std::time::Duration::from_millis(10), reused.result()).await;
    assert!(first.is_err(), "first result() waits");
    let second = tokio::time::timeout(std::time::Duration::from_millis(10), reused.result()).await;
    assert!(second.is_err(), "result() stays pending and is reusable");
    // 结果提交后两次调用都拿到同一个值。
    reused.end(message(now_ms(), None));
    let a = reused.result().await.expect("result resolves");
    let b = reused.result().await.expect("result re-read");
    assert_eq!(a, b);
}

/// 冻结签名里的 `StreamEndedWithoutResult` 文本（`ARCHITECTURE.md` §4.9 的错误契约）。
#[test]
fn stream_ended_without_result_message_is_stable() {
    assert_eq!(
        StreamEndedWithoutResult.to_string(),
        "assistant message event stream ended without a result"
    );
}

// ---------------------------------------------------------------------------
// AssistantMessageEventStream 的计时（TS 的 4 条）
// ---------------------------------------------------------------------------

/// TS: "sets durationMs on the final done or error message of a response it saw start"。
#[tokio::test]
async fn sets_duration_ms_on_the_final_done_or_error_message_of_a_response_it_saw_start() {
    let done = AssistantMessageEventStream::new();
    let answer = message(now_ms(), None);
    sleep(20).await;
    push_done(&done, answer.clone()).expect("done push");
    let result = done.result().await.expect("result resolves");
    let duration = result.duration_ms.expect("durationMs is set");
    assert!(duration >= 15.0, "durationMs {duration} >= 15");
    // `done` 事件携带的消息与 `result()` 是同一份测量值。
    let events = drain(&done).await;
    let AssistantMessageEvent::Done {
        message: event_message,
        ..
    } = events.last().expect("done event")
    else {
        panic!("last event is done");
    };
    assert_eq!(event_message.duration_ms, Some(duration));

    let failed = AssistantMessageEventStream::new();
    let error = message(now_ms(), None);
    push_error(&failed, error.clone()).expect("error push");
    let error_result = failed.result().await.expect("error result");
    assert_eq!(error_result.stop_reason, StopReason::Stop);
    assert!(
        error_result
            .duration_ms
            .is_some_and(|duration| duration >= 0.0)
    );

    let ended = AssistantMessageEventStream::new();
    let ended_result = message(now_ms(), None);
    ended.end(ended_result.clone());
    let result = ended.result().await.expect("end result");
    assert!(result.duration_ms.is_some_and(|duration| duration >= 0.0));
}

/// TS: "keeps an existing duration, so a forwarding stream keeps the inner measurement"。
#[tokio::test]
async fn keeps_an_existing_duration_so_a_forwarding_stream_keeps_the_inner_measurement() {
    let outer = AssistantMessageEventStream::new();
    sleep(20).await;
    let inner = AssistantMessageEventStream::new();
    push_done(&inner, message(now_ms(), None)).expect("inner done");
    let measured = inner
        .result()
        .await
        .expect("inner result")
        .duration_ms
        .expect("inner measured");
    assert!(measured < 20.0, "inner measurement {measured} < 20");

    // 转发同一个（已带 durationMs 的）消息：外层不得覆盖。
    push_done(&outer, message(now_ms(), Some(measured))).expect("outer done");
    assert_eq!(
        outer.result().await.expect("outer result").duration_ms,
        Some(measured)
    );

    // 预设值同样保留。
    let preset = AssistantMessageEventStream::new();
    push_done(&preset, message(now_ms(), Some(1234.0))).expect("preset done");
    assert_eq!(
        preset.result().await.expect("preset result").duration_ms,
        Some(1234.0)
    );
}

/// TS: "leaves a message untimed when it started before the stream, such as a fetched deferred result"。
#[tokio::test]
async fn leaves_a_message_untimed_when_it_started_before_the_stream() {
    let fetched = message(now_ms() - 60_000, None);
    let stream = AssistantMessageEventStream::new();
    push_done(&stream, fetched).expect("done push");
    assert_eq!(
        stream.result().await.expect("result resolves").duration_ms,
        None
    );
}

/// TS: "does not time a message pushed after the stream completed"。
#[tokio::test]
async fn does_not_time_a_message_pushed_after_the_stream_completed() {
    let stream = AssistantMessageEventStream::new();
    let first = message(now_ms(), None);
    push_done(&stream, first.clone()).expect("first done");
    let late = message(now_ms(), None);
    assert_eq!(push_done(&stream, late), Ok(()));
    // 已完成的结果不被后来者替换（TS 里 `late.durationMs` 保持 undefined）。
    let result = stream.result().await.expect("result resolves");
    assert_same_except_duration(&result, &first);
    assert!(result.duration_ms.is_some(), "the first result stays timed");
}

/// 补充：`started_at_unix_ms`/`set_started_at_unix_ms`/`elapsed_ms` 与 TS 的
/// `#startedAt`/`performance.now()` 对应（`event-stream.ts` 的 `#time()` 用到两者）。
#[tokio::test]
async fn started_at_and_elapsed_ms_track_the_ts_clock_pair() {
    let stream = AssistantMessageEventStream::new();
    let wall = stream.started_at_unix_ms();
    assert!((now_ms() - wall).abs() < 5_000, "startedAt is Date.now()");
    sleep(10).await;
    assert!(stream.elapsed_ms() >= 5.0, "elapsed_ms is monotonic");

    // `set_started_at_unix_ms` 对应测试里 mock `Date.now`：之后 timestamp 早于它的消息不计时。
    let stream = AssistantMessageEventStream::new();
    stream.set_started_at_unix_ms(now_ms());
    push_done(&stream, message(0, None)).expect("done push");
    assert_eq!(
        stream.result().await.expect("result resolves").duration_ms,
        None
    );
}

/// 补充：冻结实现的 `end()` 可重复调用，但只有第一次的结果生效
/// （TS 的 `end(result)` 也是 `resolveFinalResult` 幂等）。
#[tokio::test]
async fn end_is_idempotent_and_keeps_the_first_result() {
    let stream = AssistantMessageEventStream::new();
    let first = message(now_ms(), None);
    stream.end(first.clone());
    stream.end(message(now_ms(), None));
    assert_same_except_duration(&stream.result().await.expect("result resolves"), &first);
}

/// 补充：没有事件消费者时 `push` 也会静默入队（TS 里“不消费迭代器”是合法用法）。
#[test]
fn push_without_a_consumer_queues_silently() {
    let stream = AssistantMessageEventStream::new();
    assert_eq!(push_delta(&stream, "1"), Ok(()));
    assert_eq!(push_done(&stream, message(now_ms(), None)), Ok(()));
}

/// 补充：`take_receiver` 把消费权交给 mpsc 接收端；转发任务保持 TS 的迭代器语义。
#[tokio::test]
async fn take_receiver_forwards_events_in_order() {
    use tokio::sync::mpsc;
    let stream = AssistantMessageEventStream::new();
    let mut receiver: mpsc::Receiver<Event> = stream.take_receiver().expect("runtime is available");
    assert!(stream.take_receiver().is_none(), "only one receiver");
    push_delta(&stream, "1").expect("push 1");
    push_delta(&stream, "2").expect("push 2");
    let final_message = message(now_ms(), None);
    push_done(&stream, final_message.clone()).expect("done push");
    let mut kinds = Vec::new();
    while let Some(event) = receiver.recv().await {
        kinds.push(event.kind());
    }
    // `end()` 让转发任务看到迭代结束，通道随之关闭（否则这里永远等不到 `None`）。
    assert_eq!(kinds, ["text_delta", "text_delta", "done"]);
    assert_same_except_duration(
        &stream.result().await.expect("result resolves"),
        &final_message,
    );
}

/// 补充：`end_without_result()` 同样关闭 `take_receiver` 的通道（迭代器以 `{done:true}` 结束）。
#[tokio::test]
async fn take_receiver_closes_on_end_without_result() {
    let stream = AssistantMessageEventStream::new();
    let mut receiver = stream.take_receiver().expect("runtime is available");
    push_delta(&stream, "1").expect("push 1");
    stream.end_without_result();
    let mut kinds = Vec::new();
    while let Some(event) = receiver.recv().await {
        kinds.push(event.kind());
    }
    assert_eq!(kinds, ["text_delta"]);
}

/// 补充：`end()` 之后再 `push` 是静默 no-op，转发通道不会因此重开（TS 的 `done` 丢弃）。
#[tokio::test]
async fn push_after_end_is_dropped_for_the_forwarded_channel() {
    let stream = AssistantMessageEventStream::new();
    let mut receiver = stream.take_receiver().expect("runtime is available");
    push_done(&stream, message(now_ms(), None)).expect("done push");
    assert_eq!(push_delta(&stream, "late"), Ok(()));
    let mut kinds = Vec::new();
    while let Some(event) = receiver.recv().await {
        kinds.push(event.kind());
    }
    assert_eq!(kinds, ["done"]);
}

/// 回归（review finding 12）：缓冲超过 `EVENT_CHANNEL_CAPACITY` 时终态事件仍然送达。
///
/// 旧实现把 `push` 直接写进容量 4096 的 mpsc 并丢弃 `try_send` 失败的事件，于是第 4097 个
/// 事件（可能正好是 `done`）消失；TS 的 `FifoQueue` 无上限、终态事件一定进入事件序列。
#[tokio::test]
async fn terminal_event_survives_a_buffer_larger_than_the_channel_capacity() {
    let stream = AssistantMessageEventStream::new();
    let total = EVENT_CHANNEL_CAPACITY + 5;
    for index in 0..total {
        push_delta(&stream, &index.to_string()).expect("buffered push");
    }
    push_done(&stream, message(now_ms(), None)).expect("done push");
    let events = drain(&stream).await;
    assert_eq!(events.len(), total + 1, "no event is dropped");
    assert_eq!(events.last().expect("last event").kind(), "done");
}

/// 补充：`result()` 可重复调用（TS 的 `finalResultPromise` 是同一个 Promise）。
#[tokio::test]
async fn result_is_reusable() {
    let stream = AssistantMessageEventStream::new();
    let final_message = message(now_ms(), None);
    stream.end(final_message.clone());
    let first = stream.result().await.expect("first result");
    let second = stream.result().await.expect("second result");
    assert_eq!(first, second);
    assert_same_except_duration(&second, &final_message);
}

/// 补充：`DoneReason`/`ErrorReason` 只会被终态事件使用（TS 的 `isComplete`）。
#[test]
fn terminal_event_kinds_are_done_and_error() {
    let message = message(1, None);
    let done = Event::Done {
        reason: DoneReason::Stop,
        message: Box::new(message.clone()),
    };
    assert_eq!(done.kind(), "done");
    let error = Event::Error {
        reason: ErrorReason::Error,
        error: Box::new(message),
    };
    assert_eq!(error.kind(), "error");
}

/// 补充：`TextDelta` 的 `partial` 是快照（`Box<AssistantMessage>`），`kind()` 与 TS 的
/// `event.type` 字面量一致。
#[test]
fn delta_partial_is_a_boxed_snapshot() {
    let delta = Event::TextDelta {
        content_index: 0,
        delta: "x".to_owned(),
        partial: Box::new(AssistantMessage::default()),
    };
    assert_eq!(delta_of(&delta), Some("x"));
    assert_eq!(delta.kind(), "text_delta");
    assert!(delta.partial().is_some());
}

/// 回归：终态事件与 `done` 标记必须原子投递——并发的 `recv()` 不能丢掉 `done`。
///
/// 修复前 `push` 先 `done.store(true)` 再取 `state` 锁：消费者若恰好在两者之间取锁，会看到
/// 「`done` 为真且队列为空」而返回 `None`，随后才入队/交付的终态事件就永远不会被消费。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_concurrent_recv_never_loses_a_terminal_event() {
    for iteration in 0..2000 {
        let stream = AssistantMessageEventStream::new();
        let producer = {
            let stream = stream.clone();
            tokio::spawn(async move {
                push_delta(&stream, "x").expect("delta push");
                push_done(&stream, message(now_ms(), None)).expect("done push");
                stream.end(message(now_ms(), None));
            })
        };
        let mut saw_done = false;
        while let Some(event) = stream.recv().await {
            if event.kind() == "done" {
                saw_done = true;
            }
        }
        producer.await.expect("producer task");
        assert!(saw_done, "terminal event lost on iteration {iteration}");
    }
}
