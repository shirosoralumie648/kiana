//! `test/task-support.ts` 的 Rust 等价物：deferred/aborted/eventually/settled、任务夹具与
//! `openTasks()`。
//!
//! TS 的 `deferred()` 是一个可结算的 promise；这里用 `oneshot` 承载同一个一次结算语义。
//! `settled()` 在一次让出后报告 future 是否已经就绪（TS 用一次宏任务 flush）。

#![allow(dead_code)]
#![allow(clippy::type_complexity)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_chord::context::{AbortSignal, Context};
use kiana_durable::harness::harness::{Harness, HarnessImpl};
use kiana_durable::harness::registry::{RegistryImpl, create_registry_impl};
use kiana_durable::harness::types::{
    AnyTask, HarnessOptions, HarnessSettings, ModelsProvider, Registry, RegistryReader,
    RootOptions, SettingsProvider, Wake,
};
use kiana_durable::types::{
    BoxError, DocTarget, DocumentReader, EntryId, JsonObject, JsonValue, ReportInfo, Storage,
    TaskOutcome, TaskState,
};
use tokio::sync::oneshot;

struct TasksExtension {
    tasks: Vec<AnyTask>,
}

impl TasksExtension {
    fn new(tasks: Vec<AnyTask>) -> Self {
        TasksExtension { tasks }
    }
}

impl kiana_durable::harness::types::Extension for TasksExtension {
    fn name(&self) -> &str {
        "tasks"
    }
    fn tasks(&self) -> Vec<AnyTask> {
        self.tasks.clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一次结算的 deferred。
pub struct Deferred<T = ()> {
    sender: Mutex<Option<oneshot::Sender<Result<T, BoxError>>>>,
    receiver: Mutex<Option<oneshot::Receiver<Result<T, BoxError>>>>,
}

impl<T> Deferred<T> {
    pub fn new() -> Self {
        let (sender, receiver) = oneshot::channel();
        Deferred {
            sender: Mutex::new(Some(sender)),
            receiver: Mutex::new(Some(receiver)),
        }
    }

    /// `promise`：只取一次。
    pub async fn promise(&self) -> Result<T, BoxError> {
        let receiver = lock(&self.receiver)
            .take()
            .expect("the deferred promise is awaited once");
        match receiver.await {
            Ok(value) => value,
            Err(_) => Err(Box::new(std::io::Error::other("deferred dropped"))),
        }
    }

    pub fn resolve(&self, value: T) {
        if let Some(sender) = lock(&self.sender).take() {
            let _ = sender.send(Ok(value));
        }
    }

    pub fn reject(&self, error: BoxError) {
        if let Some(sender) = lock(&self.sender).take() {
            let _ = sender.send(Err(error));
        }
    }
}

impl<T> Default for Deferred<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// `aborted(signal)`：signal 中止时以中止原因失败。
pub async fn aborted(signal: &AbortSignal) -> BoxError {
    if signal.is_aborted() {
        return Box::new(kiana_chord::context::abort_error(signal));
    }
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let sink = Arc::clone(&sender);
    let signal = signal.clone();
    let _subscription = signal.on_abort(move |_reason| {
        if let Some(sender) = lock(&sink).take() {
            let _ = sender.send(());
        }
    });
    let _ = receiver.await;
    Box::new(kiana_chord::context::abort_error(&signal))
}

/// 让出直到 `check` 成立（最多 200 次）。
pub async fn eventually<F, Fut>(mut check: F) -> Result<(), BoxError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return Ok(());
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    Err(Box::new(std::io::Error::other("Condition was not reached")))
}

/// 一次让出后 future 是否已就绪。
pub async fn settled<F: std::future::Future>(future: F) -> bool {
    let mut future = Box::pin(future);
    let mut yielded = Box::pin(async {
        tokio::task::yield_now().await;
    });
    matches!(
        futures::future::select(future.as_mut(), yielded.as_mut()).await,
        futures::future::Either::Left(_)
    )
}

/// `completed(result)`。
pub fn completed(result: JsonValue) -> TaskOutcome {
    TaskOutcome::Completed { result }
}

/// `aborted_with(reason)`。
pub fn aborted_with(reason: &str) -> TaskOutcome {
    TaskOutcome::Aborted {
        reason: Some(reason.to_owned()),
        result: None,
    }
}

/// `openTasks()` 的选项。
#[derive(Default)]
pub struct OpenTasksOptions {
    pub registry: Option<Arc<RegistryImpl>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub settings: Option<Arc<Mutex<HarnessSettings>>>,
    pub on_wake: Option<Arc<dyn Fn(&Wake) + Send + Sync>>,
    pub set_timer: Option<kiana_durable::types::SetTimer>,
    pub read_only: Option<bool>,
    pub on_report:
        Option<Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync>>,
}

/// 一个任务夹具。
pub struct TasksFixture {
    pub harness: Arc<HarnessImpl>,
    pub registry: Arc<RegistryImpl>,
    pub reports: Arc<Mutex<Vec<(String, ReportInfo)>>>,
    pub settings: Arc<Mutex<HarnessSettings>>,
}

impl TasksFixture {
    pub fn report_messages(&self) -> Vec<String> {
        lock(&self.reports)
            .iter()
            .map(|(message, _)| message.clone())
            .collect()
    }

    pub fn report_infos(&self) -> Vec<ReportInfo> {
        lock(&self.reports)
            .iter()
            .map(|(_, info)| info.clone())
            .collect()
    }
}

/// 打开一个 registry 含 `tasks` 的 Harness；报告收进夹具。
pub async fn open_tasks(storage: Arc<dyn Storage>, tasks: Vec<AnyTask>) -> TasksFixture {
    open_tasks_with(storage, tasks, OpenTasksOptions::default()).await
}

pub async fn open_tasks_with(
    storage: Arc<dyn Storage>,
    tasks: Vec<AnyTask>,
    options: OpenTasksOptions,
) -> TasksFixture {
    let registry = options.registry.unwrap_or_else(create_registry_impl);
    if !tasks.is_empty() {
        registry.install(kiana_durable::harness::define::define_extension(
            TasksExtension::new(tasks),
        ));
    }
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let on_report: Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync> =
        match options.on_report {
            Some(handler) => handler,
            None => Arc::new(move |error, info| {
                lock(&sink).push((error.to_string(), info.clone()));
            }),
        };
    let settings = options
        .settings
        .unwrap_or_else(|| Arc::new(Mutex::new(HarnessSettings::default())));
    let settings_reader = Arc::clone(&settings);
    let harness = Harness::open(
        storage,
        HarnessOptions {
            telemetry: None,
            models: ModelsProvider::Direct(kiana_ai::models::create_models(Default::default())),
            identity: None,
            registry: Arc::clone(&registry) as Arc<dyn RegistryReader>,
            settings: Some(SettingsProvider::Dynamic(Arc::new(move || {
                Some(lock(&settings_reader).clone())
            }))),
            env: None,
            conversation_created: None,
            now: options.now,
            set_timer: options.set_timer,
            on_wake: options.on_wake,
            on_report: Some(on_report),
            read_only: options.read_only,
        },
        &**BACKGROUND,
    )
    .await
    .expect("the harness opens");
    TasksFixture {
        harness,
        registry,
        reports,
        settings,
    }
}

static BACKGROUND: std::sync::LazyLock<kiana_chord::context::ContextRef> =
    std::sync::LazyLock::new(|| Arc::clone(&kiana_chord::context::BACKGROUND_CONTEXT));

/// 打开根会话。
pub async fn open_root(
    harness: &Arc<HarnessImpl>,
) -> Arc<dyn kiana_durable::harness::types::Conversation> {
    harness
        .root(&**BACKGROUND, RootOptions::default())
        .await
        .expect("the root")
}

/// 统计读取次数的文档读取端。
pub struct CountingReader {
    inner: Arc<dyn DocumentReader>,
    pub reads: AtomicUsize,
}

impl CountingReader {
    pub fn new(inner: Arc<dyn DocumentReader>) -> Arc<Self> {
        Arc::new(CountingReader {
            inner,
            reads: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl DocumentReader for CountingReader {
    async fn snapshot(
        &self,
        target: &DocTarget,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.snapshot(target, context).await
    }

    async fn snapshot_as_of(
        &self,
        target: &DocTarget,
        at: EntryId,
        context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.snapshot_as_of(target, at, context).await
    }
}

/// 让全部任务终态（测试收尾）。
pub async fn wait_terminal(harness: &Arc<HarnessImpl>, id: kiana_durable::types::TaskId) {
    let _ = harness.wait_for_task(id, &**BACKGROUND).await;
}

/// 构造一个 `pending` 任务的初始 checkpoint。
pub fn phase_checkpoint(phase: &str) -> JsonValue {
    serde_json::json!({ "phase": phase })
}

/// `TaskState::Pending { checkpoint }` 的便捷构造。
pub fn pending(phase: &str) -> TaskState {
    TaskState::Pending {
        checkpoint: phase_checkpoint(phase),
    }
}

/// `TaskState::Terminal { outcome }` 的便捷构造。
pub fn terminal(outcome: TaskOutcome) -> TaskState {
    TaskState::Terminal { outcome }
}

/// `Registry` 的擦除视图（测试断言用）。
pub fn installed_names(registry: &Arc<RegistryImpl>) -> Vec<String> {
    registry
        .snapshot()
        .installed()
        .iter()
        .map(|extension| extension.name().to_owned())
        .collect()
}

/// 测试上下文（`BACKGROUND_CONTEXT`）。
pub fn context() -> kiana_chord::context::ContextRef {
    Arc::clone(&kiana_chord::context::BACKGROUND_CONTEXT)
}

/// 让 watch 投递任务赶上。
pub async fn drained_for_tests() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}
