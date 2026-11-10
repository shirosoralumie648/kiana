//! `test/harness-read-only.test.ts` 的 Rust 端口：只读 Harness 是另一个进程驱动会话的
//! 观察者。TS 用同一 sqlite 文件的第二个连接；Rust 用同一个 `MemoryStorage` 句柄（等价地
//! 共享同一份会话状态），并用 `ControlledStorage` 计数确认没有写入。

#![allow(clippy::type_complexity)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use kiana_ai::models::Models;
use kiana_durable::Session;
use kiana_durable::harness::harness::{Harness, HarnessImpl};
use kiana_durable::harness::live::LIVE_DOC;
use kiana_durable::harness::types::{
    AgentChange, Conversation as ConversationTrait, ConversationCreateOptions, HarnessOptions,
    ModelRef, ModelsProvider, RegistryReader, RootOptions, Wake,
};
use kiana_durable::index::{ReadOnlySession, UserEntry};
use kiana_durable::types::{
    BoxError, ConversationOwnership, DocTarget, DocumentReader, SessionChange, Storage, Tx,
};

use chat_support::{ChatSetup, all_entries, context, open_chat, unanswered};
use harness_support::text_input;
use support::ControlledStorage;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn storage() -> Arc<ControlledStorage> {
    Arc::new(ControlledStorage::new())
}

/// 已到达 Storage 的非空写入批次数（只读打开不应改变它）。
fn written_batches(storage: &ControlledStorage) -> usize {
    lock(&storage.commits)
        .iter()
        .filter(|batch| !batch.is_empty())
        .count()
}

async fn open_read_only(
    storage: Arc<ControlledStorage>,
    setup: &ChatSetup,
    on_wake: Option<Arc<dyn Fn(&Wake) + Send + Sync>>,
) -> Arc<HarnessImpl> {
    let models: Arc<dyn Models> = setup.models.clone();
    let reports = Arc::clone(&setup.reports);
    let on_report: Arc<
        dyn Fn(&(dyn std::error::Error + Send + Sync), &kiana_durable::types::ReportInfo)
            + Send
            + Sync,
    > = Arc::new(move |error, info| {
        lock(&reports).push((error.to_string(), info.clone()));
    });
    Harness::open(
        storage as Arc<dyn Storage>,
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(models),
            identity: None,
            registry: Arc::clone(&setup.registry) as Arc<dyn RegistryReader>,
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            set_timer: None,
            on_wake,
            on_report: Some(on_report),
            read_only: Some(true),
        },
        &*context(),
    )
    .await
    .expect("the viewer opens")
}

fn assert_read_only<T>(result: Result<T, BoxError>) {
    match result {
        Ok(_) => panic!("expected ReadOnlySession"),
        Err(error) => {
            assert!(
                error.downcast_ref::<ReadOnlySession>().is_some(),
                "expected ReadOnlySession, got {error}"
            );
        }
    }
}

/// 一个不写任何东西的提交。
struct Nothing;

#[async_trait]
impl SessionChange for Nothing {
    async fn run(&self, _tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        Ok(Box::new("nothing"))
    }
}

async fn running_session(
    storage: &Arc<ControlledStorage>,
    setup: &ChatSetup,
) -> (Arc<HarnessImpl>, Arc<dyn ConversationTrait>) {
    let busy = unanswered();
    setup.faux.set_responses(vec![busy.step.clone()]);
    let (driver, root) = open_chat(storage.clone() as Arc<dyn Storage>, setup).await;
    let _ = root
        .submit(text_input("slow").into_draft(), &*context())
        .await
        .expect("the input submission");
    busy.reached().await;
    (driver, root)
}

#[tokio::test(flavor = "multi_thread")]
async fn opens_a_session_with_work_running_elsewhere_without_writing() {
    let storage = storage();
    let setup = ChatSetup::new();
    let (driver, _driver_root) = running_session(&storage, &setup).await;
    let before = written_batches(&storage);
    let wakes: Arc<Mutex<Vec<Wake>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&wakes);
    let viewer = open_read_only(
        Arc::clone(&storage),
        &ChatSetup::new(),
        Some(Arc::new(move |wake: &Wake| lock(&sink).push(*wake))),
    )
    .await;
    let root = viewer
        .root(&*context(), RootOptions::default())
        .await
        .expect("the root");
    let kinds: Vec<String> = all_entries(&root, &*context())
        .await
        .iter()
        .map(|entry| entry.kind.clone())
        .collect();
    assert_eq!(kinds, vec![UserEntry.kind.to_string()]);
    let snapshot = viewer
        .snapshot(&DocTarget::conversation(&LIVE_DOC, root.id()), &*context())
        .await
        .expect("the live doc")
        .expect("the live doc exists");
    let run = snapshot.get("run").expect("the run");
    let task_id: kiana_durable::TaskId =
        serde_json::from_value(run.get("taskId").expect("the task id").clone()).expect("a task id");
    let record = viewer
        .get_task(task_id, &*context())
        .await
        .expect("the task")
        .expect("the task exists");
    assert_eq!(
        record.state.status(),
        kiana_durable::types::TaskStatus::Running
    );
    let inspection = viewer.inspect(&*context()).await.expect("inspect");
    assert_eq!(
        inspection.scheduling,
        kiana_durable::harness::types::SchedulingState::Paused
    );
    assert_eq!(viewer.wake(), Wake::default());
    viewer.close(&*context()).await;

    assert_eq!(written_batches(&storage), before);
    assert!(lock(&wakes).is_empty());
    driver.close(&*context()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_writes_and_scheduling_and_stays_usable() {
    let storage = storage();
    let setup = ChatSetup::new();
    setup.faux.set_responses(vec![chat_support::answer("one")]);
    let (driver, driver_root) = open_chat(Arc::clone(&storage) as Arc<dyn Storage>, &setup).await;
    let submission = driver_root
        .submit(text_input("hi").into_draft(), &*context())
        .await
        .expect("the submission");
    submission.wait(&*context()).await.expect("it settles");
    driver.close(&*context()).await;
    // Rust 的 `MemoryStorage` 把 close 记在共享状态上；`reopen()` 用同一个句柄重新打开，
    // 模拟另一个连接读取同一份存储。
    storage.reopen();
    let before = written_batches(&storage);

    let viewer_setup = ChatSetup::new();
    let viewer = open_read_only(Arc::clone(&storage), &viewer_setup, None).await;
    let root = viewer
        .root(&*context(), RootOptions::default())
        .await
        .expect("the root");

    struct AppendNote {
        conversation_id: kiana_durable::ConversationId,
    }
    #[async_trait]
    impl SessionChange for AppendNote {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let record = tx
                .append_entry(
                    self.conversation_id,
                    kiana_durable::types::EntryDraft {
                        key_order: None,
                        kind: "app.note".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await?;
            Ok(Box::new(record))
        }
    }

    assert_read_only(
        root.commit(
            &AppendNote {
                conversation_id: root.id(),
            },
            &*context(),
            kiana_durable::types::CommitOptions::default(),
        )
        .await,
    );
    assert_read_only(
        root.submit(text_input("again").into_draft(), &*context())
            .await,
    );
    assert_read_only(viewer.resume());
    assert_read_only(viewer.wait_for_idle(&*context()).await);
    assert_read_only(root.abort(&*context(), Default::default()).await);
    assert_read_only(
        viewer
            .create_conversation(
                ConversationCreateOptions {
                    ownership: ConversationOwnership::Ownerless,
                    agent: None,
                    init: None,
                },
                &*context(),
            )
            .await,
    );

    // 仍然健康：读工作，且一个不写入任何东西的提交成功。
    let value = root
        .commit(
            &Nothing,
            &*context(),
            kiana_durable::types::CommitOptions::default(),
        )
        .await
        .expect("the no-write commit");
    assert_eq!(
        *value.downcast::<&'static str>().expect("the value"),
        "nothing"
    );
    assert_eq!(all_entries(&root, &*context()).await.len(), 2);
    viewer.close(&*context()).await;
    assert_eq!(written_batches(&storage), before);
    assert!(viewer_setup.report_messages().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_to_create_the_root_of_an_empty_storage() {
    let storage = storage();
    let viewer = open_read_only(Arc::clone(&storage), &ChatSetup::new(), None).await;
    let result = viewer
        .root(
            &*context(),
            RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    })),
                    ..Default::default()
                }),
                init: None,
            },
        )
        .await;
    assert_read_only(result);
    viewer.close(&*context()).await;
    assert_eq!(written_batches(&storage), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_recovery_to_the_next_writable_open() {
    let storage = storage();
    let setup = ChatSetup::new();
    let (driver, driver_root) = running_session(&storage, &setup).await;
    let snapshot = driver
        .snapshot(
            &DocTarget::conversation(&LIVE_DOC, driver_root.id()),
            &*context(),
        )
        .await
        .expect("the live doc")
        .expect("the live doc exists");
    let task_id: kiana_durable::TaskId = serde_json::from_value(
        snapshot
            .get("run")
            .and_then(|run| run.get("taskId"))
            .expect("the task id")
            .clone(),
    )
    .expect("a task id");

    let viewer = open_read_only(Arc::clone(&storage), &ChatSetup::new(), None).await;
    viewer.close(&*context()).await;
    storage.reopen();

    // 可写打开会恢复一次任务，就像 viewer 从未打开过。
    let (writer, _writer_root) =
        open_chat(Arc::clone(&storage) as Arc<dyn Storage>, &ChatSetup::new()).await;
    let record = writer
        .get_task(task_id, &*context())
        .await
        .expect("the task")
        .expect("the task exists");
    assert_eq!(
        record.state.status(),
        kiana_durable::types::TaskStatus::Pending
    );
    assert_eq!(record.restarts, Some(1));
    writer.close(&*context()).await;
    driver.close(&*context()).await;
}
