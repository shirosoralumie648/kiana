//! 移植自 `packages/durable/test/examples/30-reload-and-restart.ts`：运行中的调用在扩展重载后
//! 仍用它启动时的代码完成；存储里的选择是名字，重启后会绑定到新进程安装的定义。
//!
//! TS 示例经 Harness 的工具调用与 `agent()` 驱动；`harness.rs` 尚未移植，因此这里用调度器任务
//! 复现同样的两段语义：定义按名字与版本存储、运行中的调用绑定它启动时的定义、重启后由迁移函数
//! 把旧记录接到新定义上。

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::future::BoxFuture;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{CreateModelsOptions, Models, ModelsImpl};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::scheduler::{
    AgentResolver, InvocationBinding, TaskScheduler, TaskSchedulerOptions, WriteAtClose,
    blocked_subject, erase_task,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, BlockedReason, ConversationHandle, Extension, RegistryReader, RegistrySection,
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
    SubmissionQuery, SubmissionRecord, TaskDefinition, TaskId, TaskMigrate, TaskOptions,
    TaskOutcome, TaskOwnership, TaskPhase, TaskQuery, TaskRecord, TaskState, Tx,
};
use serde_json::json;

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

/// 可替换任务定义的注册表：模拟扩展文件在进程运行期间变了一套代码。
struct Registry {
    tasks: Mutex<Vec<AnyTask>>,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl Registry {
    fn new(tasks: Vec<AnyTask>) -> Arc<Self> {
        Arc::new(Registry {
            tasks: Mutex::new(tasks),
            listeners: Mutex::new(Vec::new()),
        })
    }

    /// 安装新代码并通知调度器（TS `registry.install()`）。
    fn install(&self, tasks: Vec<AnyTask>) {
        *self.tasks.lock().unwrap() = tasks;
        let listeners = self.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    }
}

impl RegistryReader for Registry {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(Snapshot {
            tasks: self.tasks.lock().unwrap().clone(),
        })
    }
    fn subscribe(
        &self,
        listener: Arc<dyn Fn() + Send + Sync>,
    ) -> kiana_durable::types::Unsubscribe {
        self.listeners.lock().unwrap().push(listener);
        Box::new(|| {})
    }
}

fn scheduler(
    session: SessionImpl,
    storage: Arc<dyn Storage>,
    registry: Arc<Registry>,
    write_at_close: WriteAtClose,
) -> TaskScheduler {
    let models: Arc<dyn Models> = Arc::new(ModelsImpl::new(CreateModelsOptions::new()));
    TaskScheduler::new(
        TaskSchedulerOptions {
            telemetry: None,
            session: session.clone(),
            storage,
            registry,
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
}

impl CloseWrites {
    fn new() -> Arc<Self> {
        Arc::new(CloseWrites::default())
    }
}

#[async_trait]
impl SessionHooks for CloseWrites {
    fn write_at_close(&self) -> Vec<StorageWrite> {
        self.pending.lock().unwrap().take().unwrap_or_default()
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
// 与 TS 示例相同的版本化定义
// ---------------------------------------------------------------------------

/// v1 的 `call`：`hold` 为真时模拟一次仍在运行、被 gate 扣住的调用；否则把进度存进
/// `finish` checkpoint 后睡一会儿，模拟进程在调用中途停止。
struct CallV1 {
    started: Arc<tokio::sync::Semaphore>,
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for CallV1 {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let hold = task
            .input
            .get("hold")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if !hold {
            return runtime
                .sleep_until(runtime.now() + 200, json!({ "phase": "finish" }), context)
                .await;
        }
        self.started.add_permits(1);
        let permit = self
            .gate
            .acquire()
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        permit.forget();
        runtime
            .commit(
                &VersionedResult { version: "v1" },
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct FinishV1;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for FinishV1 {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(
                &VersionedResult { version: "v1" },
                context,
                CommitOptions::default(),
            )
            .await
    }
}

/// v2 的 `call`：新任务直接跑 v2；重启后迁移过来的旧记录也从这里继续。
struct CallV2;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for CallV2 {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(
                &VersionedResult { version: "v2" },
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct VersionedResult {
    version: &'static str,
}

#[async_trait]
impl TaskRuntimeChange for VersionedResult {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!(self.version),
            },
        }))
    }
}

struct AbortVersioned;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortVersioned {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&AbortVersionedChange, context, CommitOptions::default())
            .await
    }
}

struct AbortVersionedChange;

#[async_trait]
impl TaskRuntimeChange for AbortVersionedChange {
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

fn versioned_v1(
    started: Arc<tokio::sync::Semaphore>,
    gate: Arc<tokio::sync::Semaphore>,
) -> AnyTask {
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: "example.versioned".to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "call" })),
        phases: [
            (
                "call".to_owned(),
                Arc::new(CallV1 { started, gate }) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
            (
                "finish".to_owned(),
                Arc::new(FinishV1) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
        ]
        .into_iter()
        .collect(),
        abort: Arc::new(AbortVersioned),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

fn versioned_v2() -> AnyTask {
    let migrate: TaskMigrate<JsonValue, JsonValue> = Arc::new(|input, _checkpoint, from| {
        Ok((input, json!({ "phase": "call", "migratedFrom": from })))
    });
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: "example.versioned".to_owned(),
        version: 2,
        initial: Arc::new(|_| json!({ "phase": "call" })),
        phases: [(
            "call".to_owned(),
            Arc::new(CallV2) as Arc<dyn TaskPhase<_, _, _, _>>,
        )]
        .into_iter()
        .collect(),
        abort: Arc::new(AbortVersioned),
        migrate: Some(migrate),
        hooks: None,
    };
    erase_task(&definition)
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

fn result_text(record: &TaskRecord) -> String {
    match record.state.outcome() {
        Some(TaskOutcome::Completed { result }) => result
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| result.to_string()),
        Some(other) => format!("{other:?}"),
        None => "active".to_owned(),
    }
}

fn blocked_name(reason: BlockedReason) -> &'static str {
    match reason {
        BlockedReason::MissingTask => "missing task",
        BlockedReason::TaskTooOld => "task too old",
        BlockedReason::MigrationFailed => "migration failed",
        BlockedReason::RestartLimit => "restart limit",
    }
}

async fn create_conversation(session: &SessionImpl) -> Result<ConversationId, BoxError> {
    session
        .commit_with(
            |tx| {
                Box::pin(async move {
                    let record = tx
                        .create_conversation(&CreateConversationOptions {
                            ownership: ConversationOwnership::Ownerless,
                        })
                        .await?;
                    Ok(record.id)
                })
            },
            &**BACKGROUND_CONTEXT,
            Default::default(),
        )
        .await
}

async fn create_task(
    session: &SessionImpl,
    conversation_id: ConversationId,
    task: &AnyTask,
    input: JsonValue,
) -> Result<TaskId, BoxError> {
    let task = task.clone();
    session
        .commit_with(
            move |tx| {
                let task = task.clone();
                let input = input.clone();
                Box::pin(async move {
                    let mut options = TaskOptions::new(TaskOwnership::Conversation);
                    options.conversation_id = Some(conversation_id);
                    tx.create_task(&task, input, options).await
                })
            },
            &**BACKGROUND_CONTEXT,
            Default::default(),
        )
        .await
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let storage: Arc<dyn Storage> = Arc::new(KeepOpenStorage::new());
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let v1 = versioned_v1(Arc::clone(&started), Arc::clone(&gate));
    let v2 = versioned_v2();

    // 第一个进程。
    let close_writes = CloseWrites::new();
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            hooks: Some(close_writes.clone() as Arc<dyn SessionHooks>),
            ..Default::default()
        },
    );
    let registry = Registry::new(vec![v1.clone()]);
    let first = scheduler(
        session.clone(),
        storage.clone(),
        registry.clone(),
        close_writes_hook(&close_writes),
    );
    first.open(context).await?;
    let conversation_id = create_conversation(&session).await?;

    // 文件在调用运行期间改变：运行中的调用用 v1 完成，下一次调用使用 v2。
    let running = create_task(&session, conversation_id, &v1, json!({ "hold": true })).await?;
    first.resume()?;
    let permit = started
        .acquire()
        .await
        .map_err(|error| Box::new(error) as BoxError)?;
    permit.forget();
    registry.install(vec![v2.clone()]);
    gate.add_permits(1);
    let settled = first.wait_for_task(running, context).await?;
    println!(
        "call running during the reload: {}",
        result_text(settled.record())
    );

    let next = create_task(&session, conversation_id, &v2, json!({ "hold": false })).await?;
    first.resume()?;
    let settled = first.wait_for_task(next, context).await?;
    println!("next call: {}", result_text(settled.record()));

    // 重启前留下一个仍在进行中的 v1 任务：它把进度写进 `finish` checkpoint 后睡眠。
    registry.install(vec![v1.clone()]);
    let carried = create_task(&session, conversation_id, &v1, json!({ "hold": false })).await?;
    first.resume()?;
    let mut sleeping = false;
    for _ in 0..200 {
        if let Some(record) = storage.task(carried, context).await?
            && matches!(record.state, TaskState::Sleeping { .. })
        {
            sleeping = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    if !sleeping {
        return Err(
            std::io::Error::other("versioned task did not reach its sleeping checkpoint").into(),
        );
    }
    first.join().await;
    session.close(context).await;
    drop(first);
    drop(session);

    // 第二个进程：对话仍选择 "example.versioned"，但这个进程还没有安装它。
    let close_writes = CloseWrites::new();
    let reopened = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            hooks: Some(close_writes.clone() as Arc<dyn SessionHooks>),
            ..Default::default()
        },
    );
    let registry = Registry::new(Vec::new());
    let second = scheduler(
        reopened.clone(),
        storage.clone(),
        registry.clone(),
        close_writes_hook(&close_writes),
    );
    second.open(context).await?;
    let record = storage.task(carried, context).await?;
    let reason = record
        .as_ref()
        .and_then(|record| second.blocked_reason(&blocked_subject(record)));
    println!(
        "after restart, before install: {}",
        reason.map(blocked_name).unwrap_or("ready")
    );
    // 安装新代码后，存储里的名字绑定到新进程安装的定义：v2 的迁移函数把 v1 的 checkpoint
    // 接到 v2 的 `call` 阶段。
    registry.install(vec![versioned_v2()]);
    second.resume()?;
    let settled = second.wait_for_task(carried, context).await?;
    println!("after install: {}", result_text(settled.record()));
    second.join().await;
    reopened.close(context).await;
    Ok(())
}
