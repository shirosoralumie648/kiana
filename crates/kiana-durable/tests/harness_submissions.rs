//! `test/harness-submissions.test.ts` 的移植（阶段级子集）。
//!
//! TS 通过完整 `Harness` 驱动提交；这里直接使用 [`Submissions`] 与真实 `SessionImpl`，覆盖不依赖
//! 调度器的行为：
//! - 时钟与队列模式在 Session 线上求值（TS 在 `commitWith` 回调里读 `this.#now()` /
//!   `this.#queueModes()`），排队在别的提交之后的 `submit()` 不会用陈旧的值；
//! - 空闲输入被放置、写提交直接落定。
//!
//! 依赖完整 Harness 的用例（忙碌/steering/follow-up、run 推进、等待与关闭竞态）见交付报告。

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::types::UserContent;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::inbox::QueueModes;
use kiana_durable::harness::submissions::Submissions;
use kiana_durable::harness::types::{SubmissionDraft, SubmissionDraftKind};
use kiana_durable::session::ContextSource;
use kiana_durable::types::{CommitOptions, EntryQuery, JsonValue, SubmissionStatus, Tx};

use support::{commit, create_conversation, open_test_session};

fn ctx() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn input_draft(text: &str) -> SubmissionDraft {
    SubmissionDraft {
        request_id: None,
        kind: SubmissionDraftKind::Input {
            content: UserContent::Text(text.to_owned()),
            when_busy: None,
            entry: None,
        },
    }
}

/// 计数的 `now`/`queueModes`（TS `HarnessOptions.now` 与设置里的队列模式）。
struct Clock {
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    modes: Arc<dyn Fn() -> QueueModes + Send + Sync>,
    now_calls: Arc<AtomicUsize>,
    mode_calls: Arc<AtomicUsize>,
}

fn clock() -> Clock {
    let settings = resolve_settings(None);
    let now_calls = Arc::new(AtomicUsize::new(0));
    let mode_calls = Arc::new(AtomicUsize::new(0));
    let now_counter = Arc::clone(&now_calls);
    let mode_counter = Arc::clone(&mode_calls);
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || {
        now_counter.fetch_add(1, Ordering::SeqCst);
        42
    });
    let modes: Arc<dyn Fn() -> QueueModes + Send + Sync> = Arc::new(move || {
        mode_counter.fetch_add(1, Ordering::SeqCst);
        QueueModes::from_settings(&settings)
    });
    Clock {
        now,
        modes,
        now_calls,
        mode_calls,
    }
}

#[tokio::test]
async fn a_queued_submit_reads_its_clock_and_queue_modes_on_the_session_line() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let clock = clock();
    let submissions = Submissions::new(
        session.clone(),
        Arc::clone(&clock.now),
        Arc::clone(&clock.modes),
        Arc::new(|| {}),
        None,
    )
    .expect("submissions open");

    // 先占住 Session 线：一个在变更回调里等待信号的提交。
    let gate = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let hold_gate = Arc::clone(&gate);
    let hold_entered = Arc::clone(&entered);
    let holder_context = arc_context();
    let holder = tokio::spawn(async move {
        session
            .commit_with(
                move |_tx| {
                    let gate = Arc::clone(&hold_gate);
                    let entered = Arc::clone(&hold_entered);
                    Box::pin(async move {
                        entered.notify_one();
                        gate.notified().await;
                        Ok(())
                    })
                },
                holder_context,
                CommitOptions::default(),
            )
            .await
    });
    entered.notified().await;

    // 这一次提交排在线的后面：接纳尚未运行。
    let submitter = submissions.clone();
    let join = tokio::spawn(async move {
        submitter
            .submit(conversation_id, input_draft("go"), &**BACKGROUND_CONTEXT)
            .await
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        clock.now_calls.load(Ordering::SeqCst),
        0,
        "the clock is read when admission runs, not when submit() is called"
    );
    assert_eq!(clock.mode_calls.load(Ordering::SeqCst), 0);

    gate.notify_one();
    holder.await.expect("holder joins").expect("holder commit");
    let submission = join
        .await
        .expect("submitter joins")
        .expect("the input is admitted");
    assert_eq!(clock.now_calls.load(Ordering::SeqCst), 1);
    assert_eq!(clock.mode_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        submission.status(ctx()).await.expect("status").status,
        SubmissionStatus::Placed
    );
}

#[tokio::test]
async fn an_idle_write_settles_done_without_a_run() {
    let (_storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let submissions = Submissions::new(
        session.clone(),
        Arc::new(|| 1),
        Arc::new(|| QueueModes::from_settings(&resolve_settings(None))),
        Arc::new(|| {}),
        None,
    )
    .expect("submissions open");
    let draft = SubmissionDraft {
        request_id: Some("w1".to_owned()),
        kind: SubmissionDraftKind::Write {
            entry: kiana_durable::EntryDraft {
                key_order: None,
                kind: "note".to_owned(),
                model: None,
                data: Some(JsonValue::String("x".to_owned())),
                head: None,
                edits: None,
            },
        },
    };
    let submission = submissions
        .submit(conversation_id, draft, ctx())
        .await
        .expect("the write is admitted");
    let settled = submission.wait(ctx()).await.expect("the write settles");
    assert_eq!(settled.status, SubmissionStatus::Done);
    assert!(settled.entry.is_some());
    // 没有 generation 任务，也没有 run。
    let entries = commit(&session, move |tx| {
        Box::pin(async move {
            Ok(Tx::scan_entries(
                tx,
                &EntryQuery {
                    conversation_id,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                10,
                None,
            )
            .await?
            .items)
        })
    })
    .await
    .expect("entries scan");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, "note");
}

/// `commit_with` 需要 `ContextSource`；用背景上下文包一层。
fn arc_context() -> kiana_chord::context::ContextRef {
    BACKGROUND_CONTEXT.context_ref()
}
