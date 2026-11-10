//! 移植自 `packages/durable/test/examples/11-tasks.ts`：运行一个持久任务。
//!
//! TS 示例经 `Harness.root()` 创建任务；本仓库的 `harness.rs` 尚未实现，因此这里直接用
//! `SessionImpl` 创建同样的任务，再用 `TaskScheduler` 打开并等待它完成。任务的持久语义
//! （每次 `commit` 保存 checkpoint、崩溃后从最后一步继续）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::future::BoxFuture;
use kiana_ai::ModelThinkingLevel;
use kiana_ai::models::{CreateModelsOptions, Models, ModelsImpl};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::scheduler::{
    AgentResolver, InvocationBinding, TaskScheduler, TaskSchedulerOptions, erase_task,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, ConversationHandle, Extension, RegistryReader, RegistrySection,
    RegistrySnapshot, RegistryTool, SchedulerOutcome, TaskRuntime, TaskRuntimeChange,
};
use kiana_durable::session::{SessionImpl, SessionOptions, Transaction};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::timers::host_timers;
use kiana_durable::{
    BoxError, CommitOptions, ConversationId, ConversationOwnership, CreateConversationOptions,
    JsonValue, TaskDefinition, TaskOptions, TaskOutcome, TaskOwnership, TaskPhase, TaskRecord,
    TaskState, Tx,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// 与 TS 示例相同的任务定义
// ---------------------------------------------------------------------------

/// 伪支付服务：重复的 key 返回同一收据。
type Payments = Arc<Mutex<HashMap<String, i64>>>;

struct Prepare;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Prepare {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        // 先保存意图：下一步要做什么，以及（重复执行也安全的）幂等键。
        runtime
            .commit(
                &NextCharge {
                    key: format!("payment-{}", task.id),
                },
                context,
                CommitOptions::default(),
            )
            .await
    }
}

struct NextCharge {
    key: String,
}
#[async_trait]
impl TaskRuntimeChange for NextCharge {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Running {
            checkpoint: json!({ "phase": "charge", "key": self.key }),
        }))
    }
}

struct Charge {
    payments: Payments,
}
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for Charge {
    async fn run(
        &self,
        task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        let key = task
            .state
            .checkpoint()
            .and_then(|checkpoint| checkpoint.get("key"))
            .and_then(JsonValue::as_str)
            .expect("the charge checkpoint names its key")
            .to_owned();
        let amount = task
            .input
            .get("amount")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0);
        let receipt = {
            let mut payments = self.payments.lock().unwrap();
            *payments.entry(key).or_insert(amount * 100)
        };
        // 保存结果；它与本次提交里写的一切一起原子落盘。
        runtime
            .commit(&Complete { receipt }, context, CommitOptions::default())
            .await
    }
}

struct Complete {
    receipt: i64,
}
#[async_trait]
impl TaskRuntimeChange for Complete {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!({ "receipt": self.receipt }),
            },
        }))
    }
}

struct AbortPayment;
#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for AbortPayment {
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

fn payment_task(payments: Payments) -> AnyTask {
    let definition: TaskDefinition<JsonValue, JsonValue, JsonValue, ()> = TaskDefinition {
        name: "example.payment".to_owned(),
        version: 1,
        initial: Arc::new(|_| json!({ "phase": "prepare" })),
        phases: [
            (
                "prepare".to_owned(),
                Arc::new(Prepare) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
            (
                "charge".to_owned(),
                Arc::new(Charge { payments }) as Arc<dyn TaskPhase<_, _, _, _>>,
            ),
        ]
        .into_iter()
        .collect(),
        abort: Arc::new(AbortPayment),
        migrate: None,
        hooks: None,
    };
    erase_task(&definition)
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
            write_at_close: Arc::new(|_writes| Box::pin(async { Ok(()) })),
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

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let context: &dyn Context = &**BACKGROUND_CONTEXT;
    let storage = Arc::new(MemoryStorage::new());
    let session = SessionImpl::new(storage.clone(), SessionOptions::default());
    let payments: Payments = Arc::new(Mutex::new(HashMap::new()));
    let payment = payment_task(Arc::clone(&payments));

    // 每个任务都指名自己的 owner；这个属于会话。
    let create_payment = payment.clone();
    let (conversation, payment_id) = session
        .commit_with(
            move |tx| {
                let payment = create_payment.clone();
                Box::pin(async move {
                    let conversation = tx
                        .create_conversation(&CreateConversationOptions {
                            ownership: ConversationOwnership::Ownerless,
                        })
                        .await?;
                    let mut options = TaskOptions::new(TaskOwnership::Conversation);
                    options.conversation_id = Some(conversation.id);
                    let id = tx
                        .create_task(&payment, json!({ "amount": 5 }), options)
                        .await?;
                    Ok((conversation.id, id))
                })
            },
            context,
            Default::default(),
        )
        .await?;
    let _ = conversation;

    let scheduler = scheduler(
        session.clone(),
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        vec![payment],
    );
    scheduler.open(context).await?;
    scheduler.resume()?;
    // 任务完成后的记录就是持久收据；`waitForTask` 知道它的结果类型。
    let paid = scheduler.wait_for_task(payment_id, context).await?;
    match paid.record().state.outcome() {
        Some(TaskOutcome::Completed { result }) => println!("payment outcome: completed {result}"),
        other => println!("payment outcome: {other:?}"),
    }
    session.close(context).await;
    Ok(())
}
