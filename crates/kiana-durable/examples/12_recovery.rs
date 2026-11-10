//! 移植自 `packages/durable/test/examples/12-recovery.ts`：关闭、重开并从上次的 checkpoint 继续。
//!
//! TS 示例经 Harness 重开驱动恢复；本仓库的 `harness.rs` 尚未实现，因此这里用同一份 `MemoryStorage` 上的两个 `SessionImpl`/`TaskScheduler` 复现同一恢复语义（memo 去重、sleep 与 checkpoint 一起保存）。

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::future::BoxFuture;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{CreateModelsOptions, Models, ModelsImpl};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::scheduler::{
    AgentResolver, InvocationBinding, TaskScheduler, TaskSchedulerOptions, WriteAtClose, erase_task,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, ConversationHandle, Extension, RegistryReader, RegistrySection,
    RegistrySnapshot, RegistryTool, SchedulerOutcome, TaskRuntime, TaskRuntimeChange,
};
use kiana_durable::session::{SessionHooks, SessionImpl, SessionOptions, Transaction};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::timers::host_timers;
use kiana_durable::{
    AdoptFn, BoxError, CommitOptions, ConversationId, ConversationOwnership, ConversationQuery,
    ConversationRecord, CreateConversationOptions, Cursor, DocumentAddress, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, EntryId, EntryQuery, EntryRecord, IdKind,
    JsonValue, Page, Seq, Storage, StorageWrite, StoredDocument, StoredEntry, SubmissionId,
    SubmissionQuery, SubmissionRecord, TaskDefinition, TaskId, TaskOptions, TaskOutcome,
    TaskOwnership, TaskPhase, TaskQuery, TaskRecord, TaskState, Tx,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// 可重开的存储（TS 示例用 SQLite 文件做到这一点）
// ---------------------------------------------------------------------------

/// `MemoryStorage` 的 `close()` 会永久关闭后端，而 TS 示例用 SQLite 文件重开：关闭只结束
/// `Session`，数据仍在文件里。本仓库的 SQLite 后端尚未移植，这里用一层薄包装复现同样的
/// 语义：`close()` 什么也不做，所有读写照旧委托给同一份 `MemoryStorage`。
struct KeepOpenStorage {
    inner: MemoryStorage,
}

impl KeepOpenStorage {
    fn new() -> Self {
        KeepOpenStorage {
            inner: MemoryStorage::new(),
        }
    }
}

#[async_trait]
impl Storage for KeepOpenStorage {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        self.inner.commit(writes, adopt, context).await
    }

    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.inner.mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.inner
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.visible_entry(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.inner
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.inner
            .scan_documents(query, limit, cursor, context)
            .await
    }

    /// TS 示例的 SQLite 文件在关闭后仍然存在；这里保留内存后端，只结束 `Session`。
    async fn close(&self, _context: &dyn Context) {}
}

/// `Harness.writeAtClose` 的对应物：`TaskScheduler::join()` 把要写回的 `pending` 记录交给
/// 这个槽，`Session.close()` 再通过 [`SessionHooks::write_at_close`] 一次性写入。
#[derive(Default)]
struct CloseWrites {
    pending: Mutex<Option<Vec<StorageWrite>>>,
    scheduler: Mutex<Option<TaskScheduler>>,
}

impl CloseWrites {
    fn new() -> Arc<Self> {
        Arc::new(CloseWrites::default())
    }

    /// `Harness.close()` 在关闭监听器（调度器 seal）之后、写回之前 join 调度器。
    fn attach(&self, scheduler: &TaskScheduler) {
        *self.scheduler.lock().unwrap() = Some(scheduler.clone());
    }
}

#[async_trait]
impl SessionHooks for CloseWrites {
    fn write_at_close(&self) -> Vec<StorageWrite> {
        self.pending.lock().unwrap().take().unwrap_or_default()
    }

    async fn before_close(&self) -> Result<(), BoxError> {
        let scheduler = self.scheduler.lock().unwrap().clone();
        if let Some(scheduler) = scheduler {
            scheduler.join().await;
        }
        Ok(())
    }
}

fn close_writes_hook(close: &Arc<CloseWrites>) -> WriteAtClose {
    let close = Arc::clone(close);
    Arc::new(move |writes| {
        *close.pending.lock().unwrap() = Some(writes());
        Box::pin(async { Ok(()) })
    })
}
// ---------------------------------------------------------------------------
// 最小的调度器装配（Harness 层尚未移植）
// ---------------------------------------------------------------------------

struct Snapshot {
    tasks: Vec<AnyTask>,
}

impl RegistrySnapshot for Snapshot {
    fn installed(&self) -> Vec<Arc<dyn Extension>> {
        Vec::new()
    }
    fn extension(&self, _name: &str) -> Option<Arc<dyn Extension>> {
        None
    }
    fn tools(&self) -> Vec<RegistryTool> {
        Vec::new()
    }
    fn sections(&self) -> Vec<RegistrySection> {
        Vec::new()
    }
    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.clone()
    }
    fn task(&self, name: &str) -> Option<AnyTask> {
        self.tasks
            .iter()
            .find(|task| task.definition.name == name)
            .cloned()
    }
}

struct Registry {
    tasks: Vec<AnyTask>,
}

impl RegistryReader for Registry {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(Snapshot {
            tasks: self.tasks.clone(),
        })
    }
    fn subscribe(
        &self,
        _listener: Arc<dyn Fn() + Send + Sync>,
    ) -> kiana_durable::types::Unsubscribe {
        Box::new(|| {})
    }
}

fn scheduler(
    session: SessionImpl,
    storage: Arc<dyn kiana_durable::Storage>,
    tasks: Vec<AnyTask>,
    write_at_close: WriteAtClose,
) -> TaskScheduler {
    let models: Arc<dyn Models> = Arc::new(ModelsImpl::new(CreateModelsOptions::new()));
    TaskScheduler::new(
        TaskSchedulerOptions {
            telemetry: None,
            session: session.clone(),
            storage,
            registry: Arc::new(Registry { tasks }),
            models: Arc::new(move || Arc::clone(&models)),
            agent: Arc::new(
                |_conversation: ConversationId,
                 _snapshot: Arc<dyn RegistrySnapshot>,
                 _context: kiana_chord::context::ContextRef|
                 -> BoxFuture<'static, Result<Arc<Agent>, BoxError>> {
                    Box::pin(async {
                        Ok(Arc::new(Agent {
                            model: None,
                            thinking_level: ModelThinkingLevel::Off,
                            extensions: Vec::new(),
                            tools: Vec::new(),
                            callable: Vec::new(),
                            deferred: Vec::new(),
                            sections: Vec::new(),
                            instructions: None,
                            cwd: None,
                        }))
                    })
                },
            ) as AgentResolver,
            settings: Arc::new(|| Ok(Arc::new(resolve_settings(None)))),
            env: Arc::new(|_, _| Box::pin(async { Ok(None) })),
            now: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis() as i64)
                    .unwrap_or(0)
            }),
            on_wake: None,
            timers: Arc::new(host_timers(None, Arc::new(|_| {}))),
            write_at_close,
            report: Arc::new(|_error, _info, _context| {}),
            settle_outcome: Arc::new(
                |_tx: &Transaction,
                 _record: &TaskRecord,
                 _outcome: &SchedulerOutcome|
                 -> BoxFuture<'_, Result<(), BoxError>> {
                    Box::pin(async { Ok(()) })
                },
            ),
            withdraw_inputs: Arc::new(
                |_tx: &Transaction,
                 _conversation: ConversationId|
                 -> BoxFuture<'_, Result<(), BoxError>> {
                    Box::pin(async { Ok(()) })
                },
            ),
            conversation:
                Arc::new(
                    |_id: ConversationId,
                     _binding: InvocationBinding,
                     _context: kiana_chord::context::ContextRef|
                     -> BoxFuture<
                        'static,
                        Result<Option<Arc<dyn ConversationHandle>>, BoxError>,
                    > { Box::pin(async { Ok(None) }) },
                ),
            context: Arc::clone(&BACKGROUND_CONTEXT),
            read_only: None,
        },
    )
}

// ---------------------------------------------------------------------------
// 与 TS 示例相同的任务定义
// ---------------------------------------------------------------------------

/// 记录每次打印的 tick，测试主流程据此决定何时关掉第一个 Harness。
type Ticks = Arc<Mutex<Vec<i64>>>;

struct Tick {
    ticks: Ticks,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Tick {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let checkpoint = task.state.checkpoint().cloned().unwrap_or(JsonValue::Null);
        let n = checkpoint.get("n").and_then(JsonValue::as_i64).unwrap_or(1);
        // 先保存意图再产生效果：memo 保留同名第一次写入的值，所以进程在打印之后、
        // 保存下一个 checkpoint 之前死掉时，重跑看到 memo，不会重复打印同一个 tick。
        let name = format!("printed-{n}");
        if runtime.memo(&name, context).await?.is_none() {
            runtime.memo_put(&name, json!(true), context).await?;
            println!("tick {n}");
            self.ticks.lock().unwrap().push(n);
        }
        let to = task
            .input
            .get("to")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        if n == to {
            runtime
                .commit(
                    &FinishTick {
                        result: format!("counted to {n}"),
                    },
                    context,
                    CommitOptions::default(),
                )
                .await?;
            return Ok(());
        }
        // sleep 与 checkpoint 一起保存：任务在此不持有任何调用，稍后打开的 Harness 到期后继续。
        runtime
            .sleep_until(
                runtime.now() + 50,
                json!({ "phase": "tick", "n": n + 1 }),
                context,
            )
            .await
    }
}

struct FinishTick {
    result: String,
}
#[async_trait]
impl TaskRuntimeChange for FinishTick {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!(self.result),
            },
        }))
    }
}

struct AbortTicker;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortTicker {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&Cancel, context, CommitOptions::default())
            .await
    }
}

struct Cancel;
#[async_trait]
impl TaskRuntimeChange for Cancel {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Aborted {
                reason: None,
                result: None,
            },
        }))
    }
}

fn ticker_task(ticks: Ticks) -> AnyTask {
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: "example.ticker".to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "tick", "n": 1 })),
        phases: [(
            "tick".to_owned(),
            Arc::new(Tick { ticks }) as Arc<dyn TaskPhase<_, _, _, _>>,
        )]
        .into_iter()
        .collect(),
        abort: Arc::new(AbortTicker),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let storage: Arc<dyn Storage> = Arc::new(KeepOpenStorage::new());
    let ticks: Ticks = Arc::new(Mutex::new(Vec::new()));
    let ticker = ticker_task(Arc::clone(&ticks));

    // 第一次运行：数到 5；打印 tick 2 之后立刻关掉，模拟“效果之后、保存其结果之前”崩溃。
    let close_writes = CloseWrites::new();
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            hooks: Some(close_writes.clone() as Arc<dyn SessionHooks>),
            ..Default::default()
        },
    );
    let create_ticker = ticker.clone();
    let ticker_id = session
        .commit_with(
            move |tx| {
                let ticker = create_ticker.clone();
                Box::pin(async move {
                    let conversation = tx
                        .create_conversation(&CreateConversationOptions {
                            ownership: ConversationOwnership::Ownerless,
                        })
                        .await?;
                    let mut options = TaskOptions::new(TaskOwnership::Conversation);
                    options.conversation_id = Some(conversation.id);
                    tx.create_task(&ticker, json!({ "to": 5 }), options).await
                })
            },
            context,
            Default::default(),
        )
        .await?;
    let first = scheduler(
        session.clone(),
        storage.clone(),
        vec![ticker.clone()],
        close_writes_hook(&close_writes),
    );
    close_writes.attach(&first);
    first.open(context).await?;
    first.resume()?;
    let tick_two = async {
        for _ in 0..600 {
            if ticks.lock().unwrap().contains(&2) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    tick_two.await;
    // 与 TS 的 `firstRun.close(context)` 相同：先 join（把运行中的任务写回 pending），
    // 再关闭 Session（封闭接纳，触发调度器的 seal 并写入 writeAtClose 批次）。
    session.close(context).await;
    drop(first);
    drop(session);

    // 用一个从不 resume 的 Harness 读取保存的记录，什么都不会运行。
    let reader = SessionImpl::new(storage.clone(), SessionOptions::default());
    let saved = reader
        .commit_with(
            move |tx| Box::pin(async move { tx.task(ticker_id).await }),
            context,
            Default::default(),
        )
        .await?;
    if let Some(saved) = saved {
        println!(
            "closed; saved checkpoint: {} memos: {}",
            serde_json::to_string(&saved.state)?,
            serde_json::to_string(&saved.memos)?
        );
    }
    reader.close(context).await;

    // 第二次运行：不需要手工重启任何东西；等待未完成的任务会启用调度并继续它。
    // tick 2 会重跑（它的结果从未保存），但 memo 说它已经打印过了。
    let close_writes = CloseWrites::new();
    let reopened = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            hooks: Some(close_writes.clone() as Arc<dyn SessionHooks>),
            ..Default::default()
        },
    );
    let second = scheduler(
        reopened.clone(),
        storage.clone(),
        vec![ticker],
        close_writes_hook(&close_writes),
    );
    close_writes.attach(&second);
    second.open(context).await?;
    second.resume()?;
    let record = second.wait_for_task(ticker_id, context).await?;
    println!("ticks: {:?}", ticks.lock().unwrap());
    match record.record().state.outcome() {
        Some(TaskOutcome::Completed { result }) => println!("after reopen: {result}"),
        other => println!("after reopen: {other:?}"),
    }
    reopened.close(context).await;
    Ok(())
}
