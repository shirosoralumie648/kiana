//! 移植自 `packages/durable/test/examples/23-child-tasks.ts`：一个拥有子任务的 checkout 同时
//! 发起四笔支付并等待它们。
//!
//! TS 示例的 `Harness.watchTaskGraph` 尚未移植（`harness.rs` 是占位实现），这里改为直接从同一份
//! 存储读取活任务、按 `owner` 边打印同一棵树；`waitForTask` 会像 Harness 一样先 `resume()`。

use std::collections::HashSet;
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
    JoinPolicy, JsonValue, Page, Seq, Storage, StorageWrite, StoredDocument, StoredEntry,
    SubmissionId, SubmissionQuery, SubmissionRecord, TaskDefinition, TaskId, TaskOptions,
    TaskOutcome, TaskOutcomeError, TaskOwnership, TaskPhase, TaskQuery, TaskRecord, TaskState,
    TaskStatus, Tx,
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
    storage: Arc<dyn Storage>,
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
// 与 TS 示例相同的任务定义
// ---------------------------------------------------------------------------

type Charged = Arc<Mutex<HashSet<String>>>;

/// 银行卡已确认的扣款（TS 里是模块级的 `charged` 集合）。
struct Charge {
    charged: Charged,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Charge {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let card = task
            .input
            .get("card")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_owned();
        if card.starts_with("expired") {
            runtime
                .commit(&Decline { card }, context, CommitOptions::default())
                .await?;
            return Ok(());
        }
        self.charged.lock().unwrap().insert(card);
        // 银行过一会儿确认：等待时不持有 invocation。
        runtime
            .sleep_until(runtime.now() + 100, json!({ "phase": "confirm" }), context)
            .await
    }
}

struct Decline {
    card: String,
}

#[async_trait]
impl TaskRuntimeChange for Decline {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Failed {
                error: TaskOutcomeError {
                    message: format!("{} declined", self.card),
                    detail: None,
                },
                result: None,
            },
        }))
    }
}

struct Confirm;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Confirm {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let card = task.input.get("card").cloned().unwrap_or(JsonValue::Null);
        runtime
            .commit(&CompletePayment { card }, context, CommitOptions::default())
            .await
    }
}

struct CompletePayment {
    card: JsonValue,
}

#[async_trait]
impl TaskRuntimeChange for CompletePayment {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!({ "card": self.card }),
            },
        }))
    }
}

/// 每笔支付在被中止时撤销自己的效果。
struct AbortPayment {
    charged: Charged,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortPayment {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let card = task
            .input
            .get("card")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_owned();
        let refunded = self.charged.lock().unwrap().remove(&card);
        println!(
            "  payment {card} aborted{}",
            if refunded { ", refunded" } else { "" }
        );
        runtime
            .commit(&AbortNow, context, CommitOptions::default())
            .await
    }
}

struct AbortNow;

#[async_trait]
impl TaskRuntimeChange for AbortNow {
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

/// checkout 把支付创建成子任务，并等待全部完成。`failFast` 让第一笔失败的支付中止其余支付；
/// checkout 自身不会被中止，它在所有支付都结束后自行决定结果。
struct CheckoutPay {
    payment: AnyTask,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for CheckoutPay {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(
                &CreatePayments {
                    payment: self.payment.clone(),
                },
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct CreatePayments {
    payment: AnyTask,
}

#[async_trait]
impl TaskRuntimeChange for CreatePayments {
    async fn run(&self, tx: &dyn Tx, current: &TaskRecord) -> Result<Option<TaskState>, BoxError> {
        let cards: Vec<String> = current
            .input
            .get("cards")
            .and_then(JsonValue::as_array)
            .map(|cards| {
                cards
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut payments: Vec<TaskId> = Vec::new();
        for card in cards {
            let mut options = TaskOptions::new(TaskOwnership::Task {
                task_id: current.id,
            });
            options.conversation_id = Some(current.conversation_id);
            payments.push(
                tx.create_task(&self.payment, json!({ "card": card }), options)
                    .await?,
            );
        }
        Ok(Some(TaskState::Waiting {
            checkpoint: json!({ "phase": "decide", "payments": payments }),
            on: payments,
            policy: JoinPolicy::FailFast,
        }))
    }
}

struct CheckoutDecide;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for CheckoutDecide {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let checkpoint = task.state.checkpoint().cloned().unwrap_or(JsonValue::Null);
        let payments: Vec<TaskId> = serde_json::from_value(
            checkpoint
                .get("payments")
                .cloned()
                .unwrap_or(JsonValue::Array(Vec::new())),
        )?;
        let outcomes = runtime.outcomes(&payments, context).await?;
        println!(
            "  payments: {}",
            outcomes
                .iter()
                .map(outcome_name)
                .collect::<Vec<_>>()
                .join(", ")
        );
        let paid = outcomes
            .iter()
            .all(|outcome| matches!(outcome, TaskOutcome::Completed { .. }));
        runtime
            .commit(&Decide { paid }, context, CommitOptions::default())
            .await
    }
}

struct Decide {
    paid: bool,
}

#[async_trait]
impl TaskRuntimeChange for Decide {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        let outcome = if self.paid {
            TaskOutcome::Completed {
                result: json!("order placed"),
            }
        } else {
            TaskOutcome::Failed {
                error: TaskOutcomeError {
                    message: "payment failed".to_owned(),
                    detail: None,
                },
                result: None,
            }
        };
        Ok(Some(TaskState::Terminal { outcome }))
    }
}

/// 只在每笔支付都结束后运行，所以退款已经发生。
struct AbortCheckout;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortCheckout {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        println!("  checkout aborted");
        runtime
            .commit(&AbortNow, context, CommitOptions::default())
            .await
    }
}

fn payment_task(charged: Charged) -> AnyTask {
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: "example.payment".to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "charge" })),
        phases: [
            (
                "charge".to_owned(),
                Arc::new(Charge {
                    charged: Arc::clone(&charged),
                }) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
            ("confirm".to_owned(), Arc::new(Confirm)),
        ]
        .into_iter()
        .collect(),
        abort: Arc::new(AbortPayment { charged }),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

fn checkout_task(payment: AnyTask) -> AnyTask {
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: "example.checkout".to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "pay" })),
        phases: [
            (
                "pay".to_owned(),
                Arc::new(CheckoutPay { payment }) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
            (
                "decide".to_owned(),
                Arc::new(CheckoutDecide) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
        ]
        .into_iter()
        .collect(),
        abort: Arc::new(AbortCheckout),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

fn outcome_name(outcome: &TaskOutcome) -> &'static str {
    match outcome {
        TaskOutcome::Completed { .. } => "completed",
        TaskOutcome::Failed { .. } => "failed",
        TaskOutcome::Aborted { .. } => "aborted",
        TaskOutcome::Orphaned { .. } => "orphaned",
        TaskOutcome::Faulted { .. } => "faulted",
    }
}

fn status_name(record: &TaskRecord) -> String {
    match &record.state {
        TaskState::Pending { .. } => "pending".to_owned(),
        TaskState::Running { .. } => "running".to_owned(),
        TaskState::Waiting { on, .. } => format!(
            "waiting on {}",
            on.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TaskState::Sleeping { .. } => "sleeping".to_owned(),
        TaskState::Completing { .. } => "completing".to_owned(),
        TaskState::Terminal { .. } => "terminal".to_owned(),
    }
}

/// 打印活任务按 owner 边组成的树（TS: `printGraph(await harness.watchTaskGraph())`）。
async fn print_graph(storage: &dyn Storage, context: &dyn Context) -> Result<(), BoxError> {
    let page = storage
        .scan_tasks(&TaskQuery::default(), 1000, None, context)
        .await?;
    let nodes: Vec<TaskRecord> = page
        .items
        .into_iter()
        .filter(|record| record.state.status() != TaskStatus::Terminal)
        .collect();
    fn print_node(nodes: &[TaskRecord], node: &TaskRecord, depth: usize) {
        println!(
            "  {}{} {}: {}",
            "  ".repeat(depth),
            node.kind,
            node.id,
            status_name(node)
        );
        for child in nodes
            .iter()
            .filter(|candidate| candidate.owner == Some(node.id))
        {
            print_node(nodes, child, depth + 1);
        }
    }
    for node in nodes.iter().filter(|node| node.owner.is_none()) {
        print_node(&nodes, node, 0);
    }
    Ok(())
}

async fn create_checkout(
    session: &SessionImpl,
    conversation_id: ConversationId,
    checkout: &AnyTask,
    cards: &[&str],
) -> Result<TaskId, BoxError> {
    let checkout = checkout.clone();
    let cards: Vec<String> = cards.iter().map(|card| (*card).to_owned()).collect();
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    session
        .commit_with(
            move |tx| {
                let checkout = checkout.clone();
                let cards = cards.clone();
                Box::pin(async move {
                    let mut options = TaskOptions::new(TaskOwnership::Conversation);
                    options.conversation_id = Some(conversation_id);
                    tx.create_task(&checkout, json!({ "cards": cards }), options)
                        .await
                })
            },
            context,
            Default::default(),
        )
        .await
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let storage: Arc<dyn Storage> = Arc::new(KeepOpenStorage::new());
    let charged: Charged = Arc::new(Mutex::new(HashSet::new()));
    let payment = payment_task(Arc::clone(&charged));
    let checkout = checkout_task(payment.clone());

    let close_writes = CloseWrites::new();
    let session = SessionImpl::new(
        storage.clone(),
        SessionOptions {
            hooks: Some(close_writes.clone() as Arc<dyn SessionHooks>),
            ..Default::default()
        },
    );
    let conversation_id = session
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
            context,
            Default::default(),
        )
        .await?;
    let first = scheduler(
        session.clone(),
        storage.clone(),
        vec![payment.clone(), checkout.clone()],
        close_writes_hook(&close_writes),
    );
    close_writes.attach(&first);
    first.open(context).await?;

    println!("One card is declined:");
    let id = create_checkout(
        &session,
        conversation_id,
        &checkout,
        &["visa-1", "expired-2", "visa-3", "visa-4"],
    )
    .await?;
    // `Harness.waitForTask` 会先 resume。
    first.resume()?;
    let settled = first.wait_for_task(id, context).await?;
    println!(
        "  checkout: {}",
        settled
            .record()
            .state
            .outcome()
            .map(outcome_name)
            .unwrap_or("active")
    );

    println!("The customer cancels:");
    let id = create_checkout(
        &session,
        conversation_id,
        &checkout,
        &["visa-5", "visa-6", "visa-7", "visa-8"],
    )
    .await?;
    first.resume()?;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    print_graph(&*storage, context).await?;
    first.abort(id, context, false).await?;
    let settled = first.wait_for_task(id, context).await?;
    println!(
        "  checkout: {}",
        settled
            .record()
            .state
            .outcome()
            .map(outcome_name)
            .unwrap_or("active")
    );

    println!("The process stops while the payments run, and a new one continues:");
    let id = create_checkout(
        &session,
        conversation_id,
        &checkout,
        &["visa-9", "visa-10", "visa-11", "visa-12"],
    )
    .await?;
    first.resume()?;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    session.close(context).await;
    drop(first);
    drop(session);

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
        vec![payment, checkout],
        close_writes_hook(&close_writes),
    );
    close_writes.attach(&second);
    second.open(context).await?;
    second.resume()?;
    let settled = second.wait_for_task(id, context).await?;
    println!(
        "  checkout: {}",
        settled
            .record()
            .state
            .outcome()
            .map(outcome_name)
            .unwrap_or("active")
    );
    reopened.close(context).await;
    Ok(())
}
