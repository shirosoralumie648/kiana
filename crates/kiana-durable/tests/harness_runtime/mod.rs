//! 测试用的最小任务运行端。
//!
//! `test/harness-*.test.ts` 通过完整的 `Harness`（scheduler 单元）驱动内建任务；本工作副本里
//! `harness/scheduler.rs` 仍是骨架，所以这里提供一个只够驱动**单个内建任务阶段**的
//! [`TestRuntime`]：它把 `TaskRuntime::commit` 直接落到真实 [`SessionImpl`] 的事务上，任务状态
//! 留在内存里。它不是 scheduler 的实现，也不参与生产代码——只让 generation/tool/compaction 的
//! 阶段级行为（hook 派发、时钟、槽位、摘要请求）可以被真实地断言。
#![allow(dead_code)]

use std::any::Any;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::models::Models;
use kiana_chord::context::{AbortSignal, Context};
use kiana_durable::Storage;
use kiana_durable::env::ExecutionEnv;
use kiana_durable::harness::context::read_context;
use kiana_durable::harness::types::{
    Agent, AnyTask, ContextView, ContextViewOptions, ConversationHandle, HookRunner,
    RegistrySection, RegistrySnapshot, RegistryTool, ReportFrom, Settings, SettledTask,
    TaskRuntime, TaskRuntimeChange,
};
use kiana_durable::session::{ContextSource, SessionImpl, Transaction, TransactionChange};
use kiana_durable::types::{
    BoxError, CommitOptions, ConversationId, DocTarget, DocumentObserver, DocumentReader,
    DocumentWatch, EntryId, EntryRecord, JsonObject, JsonValue, TaskId, TaskOutcome, TaskQuery,
    TaskRecord, TaskState,
};

/// 一次 `runtime.report()`。
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// 错误文本。
    pub message: String,
    /// 归因。
    pub from: Option<ReportFrom>,
}

/// 记录 hook 处理器的运行端（TS: `HookRunner` 的测试替身）。
pub struct TestHookRunner<H: ?Sized + Send + Sync + 'static> {
    handlers: Vec<(String, Option<String>, Arc<H>)>,
}

impl<H: ?Sized + Send + Sync + 'static> TestHookRunner<H> {
    /// 空表。
    pub fn new() -> Self {
        TestHookRunner {
            handlers: Vec::new(),
        }
    }

    /// 注册一个处理器；`name` 是 hook 名（`"beforeRequest"` 等）。
    pub fn add(mut self, name: &str, extension: Option<&str>, handler: Arc<H>) -> Self {
        self.handlers
            .push((name.to_owned(), extension.map(str::to_owned), handler));
        self
    }
}

impl<H: ?Sized + Send + Sync + 'static> Default for TestHookRunner<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: ?Sized + Send + Sync + 'static> HookRunner for TestHookRunner<H> {
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (name, _, _) in &self.handlers {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    fn handlers(&self, name: &str) -> Vec<Arc<dyn Any + Send + Sync>> {
        self.handlers_with_extension(name)
            .into_iter()
            .map(|(_, handler)| handler)
            .collect()
    }

    fn handlers_with_extension(
        &self,
        name: &str,
    ) -> Vec<(Option<String>, Arc<dyn Any + Send + Sync>)> {
        self.handlers
            .iter()
            .filter(|(handler_name, _, _)| handler_name == name)
            .map(|(_, extension, handler)| {
                (
                    extension.clone(),
                    Arc::new(Arc::clone(handler)) as Arc<dyn Any + Send + Sync>,
                )
            })
            .collect()
    }
}

/// 空 registry 快照：阶段级测试只用 agent 上的工具与分段。
#[derive(Default)]
pub struct TestRegistry;

impl RegistrySnapshot for TestRegistry {
    fn installed(&self) -> Vec<Arc<dyn kiana_durable::harness::types::Extension>> {
        Vec::new()
    }

    fn extension(&self, _name: &str) -> Option<Arc<dyn kiana_durable::harness::types::Extension>> {
        None
    }

    fn tools(&self) -> Vec<RegistryTool> {
        Vec::new()
    }

    fn sections(&self) -> Vec<RegistrySection> {
        Vec::new()
    }

    fn tasks(&self) -> Vec<AnyTask> {
        Vec::new()
    }

    fn task(&self, _name: &str) -> Option<AnyTask> {
        None
    }
}

/// 单个阶段的最小运行端。
pub struct TestRuntime<H: ?Sized + Send + Sync + 'static> {
    /// 真实的 session（提交落到它的事务上）。
    pub session: SessionImpl,
    /// 同一个 storage。
    pub storage: Arc<dyn Storage>,
    /// 当前任务记录（提交返回的状态替换它）。
    pub task: Mutex<TaskRecord>,
    /// 解析后的设置。
    pub settings: Arc<Settings>,
    /// 模型访问。
    pub models: Arc<dyn Models>,
    /// 本阶段的 agent。
    pub agent: Arc<Agent>,
    /// hook 表。
    pub hooks: Arc<dyn HookRunner>,
    /// 中止信号。
    pub signal: AbortSignal,
    /// Harness 时钟。
    pub clock: AtomicI64,
    /// `runtime.report()` 收集。
    pub reports: Mutex<Vec<Report>>,
    /// 备忘（first-wins）。
    pub memos: Mutex<IndexMap<String, JsonValue>>,
    /// `sleepUntil` 记录的时间点。
    pub sleeps: Mutex<Vec<i64>>,
    /// 任务定义名（genertion/compaction 的 hooks 用）。
    pub kind: String,
    _hook: std::marker::PhantomData<fn() -> H>,
}

impl<H: ?Sized + Send + Sync + 'static> TestRuntime<H> {
    /// 新建。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: SessionImpl,
        storage: Arc<dyn Storage>,
        task: TaskRecord,
        settings: Arc<Settings>,
        models: Arc<dyn Models>,
        agent: Arc<Agent>,
        hooks: Arc<dyn HookRunner>,
        clock: i64,
    ) -> Self {
        TestRuntime {
            session,
            storage,
            kind: task.kind.clone(),
            task: Mutex::new(task),
            settings,
            models,
            agent,
            hooks,
            signal: AbortSignal::new(),
            clock: AtomicI64::new(clock),
            reports: Mutex::new(Vec::new()),
            memos: Mutex::new(IndexMap::new()),
            sleeps: Mutex::new(Vec::new()),
            _hook: std::marker::PhantomData,
        }
    }

    /// 当前任务记录。
    pub fn task_record(&self) -> TaskRecord {
        lock(&self.task).clone()
    }

    /// 收集到的报告。
    pub fn reports(&self) -> Vec<Report> {
        lock(&self.reports).clone()
    }

    /// 把时钟拨快。
    pub fn advance(&self, by: i64) {
        self.clock.fetch_add(by, Ordering::SeqCst);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 把 [`TaskRuntimeChange`] 适配成 session 的 [`TransactionChange`]。
struct CommitAdapter<'a> {
    change: &'a dyn TaskRuntimeChange,
    current: TaskRecord,
    next: Mutex<Option<Option<TaskState>>>,
}

#[async_trait]
impl TransactionChange for CommitAdapter<'_> {
    async fn run(&self, tx: &Transaction) -> Result<Box<dyn Any + Send>, BoxError> {
        let next = self.change.run(tx, &self.current).await?;
        *lock(&self.next) = Some(next);
        Ok(Box::new(()))
    }
}

#[async_trait]
impl<H: Send + Sync + 'static> TaskRuntime<JsonValue, JsonValue, JsonValue, H> for TestRuntime<H> {
    fn task_id(&self) -> TaskId {
        self.task_record().id
    }

    fn conversation_id(&self) -> ConversationId {
        self.task_record().conversation_id
    }

    fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(TestRegistry)
    }

    async fn agent(&self, _context: &dyn Context) -> Result<Arc<Agent>, BoxError> {
        Ok(Arc::clone(&self.agent))
    }

    fn settings(&self) -> Result<Arc<Settings>, BoxError> {
        Ok(Arc::clone(&self.settings))
    }

    fn models(&self) -> Arc<dyn Models> {
        Arc::clone(&self.models)
    }

    async fn env(&self, _context: &dyn Context) -> Result<Option<Arc<dyn ExecutionEnv>>, BoxError> {
        Ok(None)
    }

    fn hooks(&self) -> Arc<dyn HookRunner> {
        Arc::clone(&self.hooks)
    }

    async fn commit(
        &self,
        change: &dyn TaskRuntimeChange,
        context: &dyn Context,
        options: CommitOptions,
    ) -> Result<(), BoxError> {
        let adapter = CommitAdapter {
            change,
            current: self.task_record(),
            next: Mutex::new(None),
        };
        self.session
            .commit_transaction(&adapter, context.context_ref(), None, options)
            .await?;
        if let Some(next) = lock(&adapter.next).take().flatten() {
            lock(&self.task).state = next;
        }
        Ok(())
    }

    async fn memo(
        &self,
        name: &str,
        _context: &dyn Context,
    ) -> Result<Option<JsonValue>, BoxError> {
        Ok(lock(&self.memos).get(name).cloned())
    }

    async fn memo_put(
        &self,
        name: &str,
        candidate: JsonValue,
        _context: &dyn Context,
    ) -> Result<JsonValue, BoxError> {
        let mut memos = lock(&self.memos);
        Ok(memos.entry(name.to_owned()).or_insert(candidate).clone())
    }

    async fn get_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        if id.get() == self.task_id().get() {
            return Ok(Some(self.task_record()));
        }
        self.storage.task(id, context).await
    }

    async fn wait_for_task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        for _ in 0..10_000 {
            if let Some(record) = self.get_task(id, context).await?
                && matches!(record.state, TaskState::Terminal { .. })
            {
                return Ok(SettledTask::new(record));
            }
            tokio::task::yield_now().await;
        }
        Err("Timed out waiting for task".into())
    }

    async fn abort_owned(&self, _id: TaskId, _context: &dyn Context) -> Result<(), BoxError> {
        Ok(())
    }

    async fn owned_tasks(&self, context: &dyn Context) -> Result<Vec<TaskRecord>, BoxError> {
        let query = TaskQuery {
            conversation_id: Some(self.conversation_id()),
            ..Default::default()
        };
        let page = self
            .storage
            .scan_tasks(&query, 10_000, None, context)
            .await?;
        let me = self.task_id();
        Ok(page
            .items
            .into_iter()
            .filter(|record| record.owner == Some(me))
            .collect())
    }

    async fn outcomes(
        &self,
        ids: &[TaskId],
        context: &dyn Context,
    ) -> Result<Vec<TaskOutcome>, BoxError> {
        let mut outcomes = Vec::new();
        for id in ids {
            let record = self.get_task(*id, context).await?;
            outcomes.push(
                match record.and_then(|record| record.state.outcome().cloned()) {
                    Some(outcome) => outcome,
                    None => TaskOutcome::Aborted {
                        reason: None,
                        result: None,
                    },
                },
            );
        }
        Ok(outcomes)
    }

    async fn conversation(
        &self,
        _id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, BoxError> {
        Ok(None)
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        Ok(self
            .storage
            .entry(id, context)
            .await?
            .map(|stored| stored.entry))
    }

    async fn context(
        &self,
        conversation_id: ConversationId,
        context: &dyn Context,
        options: ContextViewOptions,
    ) -> Result<ContextView, BoxError> {
        read_context(
            &self.session,
            &*self.storage,
            conversation_id,
            context,
            options.at,
        )
        .await
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn set_timer(
        &self,
        _callback: kiana_durable::timers::TimerCallback,
        _delay_ms: f64,
    ) -> kiana_durable::timers::Disarm {
        // 阶段级测试不跑调度器：计时器永不触发（partial 提交因此不发生）。
        Box::new(|| {})
    }

    fn report(&self, error: &(dyn std::error::Error + Send + Sync), from: Option<ReportFrom>) {
        lock(&self.reports).push(Report {
            message: error.to_string(),
            from,
        });
    }

    async fn sleep_until(
        &self,
        until: i64,
        _checkpoint: JsonValue,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        lock(&self.sleeps).push(until);
        Ok(())
    }
}

#[async_trait]
impl<H: ?Sized + Send + Sync + 'static> DocumentReader for TestRuntime<H> {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        DocumentReader::snapshot(&self.session, target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        DocumentReader::snapshot_as_of(&self.session, target, at, context).await
    }
}

#[async_trait]
impl<H: ?Sized + Send + Sync + 'static> DocumentObserver for TestRuntime<H> {
    async fn watch_doc(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        DocumentObserver::watch_doc(&self.session, target, context).await
    }
}

// ---------------------------------------------------------------------------
// 会话记录读取辅助
// ---------------------------------------------------------------------------

/// 会话的条目（按提交顺序）。
pub async fn conversation_entries(
    session: &SessionImpl,
    storage: &dyn Storage,
    conversation_id: ConversationId,
    context: &dyn Context,
) -> Vec<EntryRecord> {
    read_context(session, storage, conversation_id, context, None)
        .await
        .expect("context reads")
        .entries
}
