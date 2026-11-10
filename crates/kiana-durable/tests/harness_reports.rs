//! `test/harness-reports.test.ts` 的 Rust 端口（回归：报告映射、safeNow、监听器容纳）。
//!
//! TS 的四个用例里，两个依赖完整的 generation/extension 运行时；这里覆盖本单元负责的
//! 报告面：任务自身错误与 fault 的 `ReportInfo`、`safeNow` 只报告一次、`safeReport` 吞掉
//! 抛错的 `onReport` 并按类型计数、以及一个抛错的视图监听器只结束自己的 watch。

#![allow(clippy::await_holding_lock)]
#![allow(clippy::type_complexity)]

#[path = "support/chat.rs"]
mod chat_support;
#[path = "support/harness.rs"]
mod harness_support;
mod support;
#[path = "support/task.rs"]
mod task_support;

fn context() -> kiana_chord::context::ContextRef {
    task_support::context()
}

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_durable::harness::harness::safe_now;
use kiana_durable::harness::types::{ConversationView, ReportTask, TaskRuntime};
use kiana_durable::types::{
    BoxError, CommitOptions, EntryDraft, JsonValue, ReportInfo, SessionChange, Storage,
    TaskDefinition, TaskOptions, TaskOwnership, TaskPhase, TaskRecord,
};
use kiana_telemetry::TelemetryContext;
use kiana_telemetry::memory::InMemoryTelemetryContext;

use kiana_durable::Session;
use task_support::{TasksFixture, open_root, open_tasks};

fn memory() -> Arc<dyn Storage> {
    Arc::new(kiana_durable::storage::memory::MemoryStorage::new()) as Arc<dyn Storage>
}

fn js_error(message: &str) -> BoxError {
    Box::new(std::io::Error::other(message.to_owned()))
}

/// 一个先报告再抛错的阶段。
struct FaultyPhase;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for FaultyPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime.report(&*js_error("noted"), None);
        Err(js_error("broken"))
    }
}

fn faulty_task() -> kiana_durable::harness::types::AnyTask {
    let definition: TaskDefinition = TaskDefinition {
        name: "test.faulty".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({"phase": "run"})),
        phases: indexmap::IndexMap::from([(
            "run".to_owned(),
            Arc::new(FaultyPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(FaultyPhase) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

struct CreateTask {
    task: kiana_durable::harness::types::AnyTask,
}

#[async_trait]
impl SessionChange for CreateTask {
    async fn run(
        &self,
        tx: &dyn kiana_durable::types::Tx,
    ) -> Result<Box<dyn Any + Send>, BoxError> {
        let id = tx
            .create_task(
                &self.task,
                JsonValue::Object(Default::default()),
                TaskOptions::new(TaskOwnership::Conversation),
            )
            .await?;
        Ok(Box::new(id))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn says_which_task_reported_a_failure_or_faulted() {
    let fixture: TasksFixture = open_tasks(memory(), vec![faulty_task()]).await;
    let root = open_root(&fixture.harness).await;
    let context = task_support::context();
    let value = root
        .commit(
            &CreateTask {
                task: faulty_task(),
            },
            &*context,
            CommitOptions::default(),
        )
        .await
        .expect("create the task");
    let id = *value
        .downcast::<kiana_durable::types::TaskId>()
        .expect("task id");
    fixture.harness.resume().expect("resume");
    let settled = fixture
        .harness
        .wait_for_task(id, &*context)
        .await
        .expect("the task settles");
    assert!(matches!(
        settled.outcome(),
        kiana_durable::types::TaskOutcome::Faulted { .. }
    ));
    task_support::eventually(|| {
        let fixture = &fixture;
        async move { fixture.report_infos().len() >= 2 }
    })
    .await
    .expect("two reports");
    let infos = fixture.report_infos();
    let messages = fixture.report_messages();
    // 任务自己的错误与 fault 都带安装它的扩展名。
    let task = ReportTask {
        id,
        kind: "test.faulty".to_owned(),
        conversation_id: root.id(),
    };
    assert_eq!(messages, vec!["noted".to_owned(), "broken".to_owned()]);
    assert_eq!(
        infos[0],
        ReportInfo::TaskError {
            task: task.clone(),
            extension: Some("tasks".to_owned()),
        }
    );
    assert_eq!(
        infos[1],
        ReportInfo::TaskFaulted {
            task,
            extension: Some("tasks".to_owned()),
            reason: kiana_durable::types::TaskFaultedReason::Threw,
        }
    );
    fixture.harness.close(&*context).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn safe_now_reports_a_throwing_clock_once() {
    let reports: Arc<Mutex<Vec<(String, ReportInfo)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&reports);
    let report: Arc<dyn Fn(&(dyn std::error::Error + Send + Sync), &ReportInfo) + Send + Sync> =
        Arc::new(move |error, info| {
            sink.lock().unwrap().push((error.to_string(), info.clone()));
        });
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        panic!("clock broke");
    });
    let now = safe_now(Some(clock), report);
    let first = now();
    let second = now();
    // 回落到 Date.now（纪元毫秒）。
    assert!(first > 1_600_000_000_000, "{first}");
    assert!(second > 0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let reports = reports.lock().unwrap();
    assert_eq!(reports.len(), 1, "only the first throw is reported");
    assert_eq!(reports[0].0, "clock broke");
    assert_eq!(
        reports[0].1,
        ReportInfo::HostError {
            callback: kiana_durable::types::HostCallback::Now,
            task: None,
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn safe_report_contains_a_throwing_on_report_and_counts_each_type() {
    let recorder = InMemoryTelemetryContext::new();
    let telemetry: Arc<dyn TelemetryContext> = Arc::new(recorder.clone());
    let report = kiana_durable::harness::harness::safe_report(
        Some(Arc::new(|_error, _info| panic!("onReport broke"))),
        Some(Arc::clone(&telemetry)),
    );
    let infos = [
        ReportInfo::SessionFailed,
        ReportInfo::ListenerError,
        ReportInfo::HostError {
            callback: kiana_durable::types::HostCallback::Settings,
            task: None,
        },
        ReportInfo::TaskFaulted {
            task: ReportTask {
                id: kiana_durable::id_from_number(7),
                kind: "test.kind".to_owned(),
                conversation_id: kiana_durable::types::ROOT_CONVERSATION_ID,
            },
            extension: Some("ext".to_owned()),
            reason: kiana_durable::types::TaskFaultedReason::Threw,
        },
    ];
    for info in &infos {
        report(&*js_error("boom"), info, None);
    }
    let metrics = recorder.get_metrics();
    assert_eq!(metrics.len(), infos.len(), "one reports metric per report");
    let types: Vec<String> = metrics
        .iter()
        .map(|metric| match metric.attributes.get("type") {
            Some(kiana_telemetry::AttributeValue::String(value)) => value.clone(),
            other => panic!("unexpected type attribute: {other:?}"),
        })
        .collect();
    assert_eq!(
        types,
        vec![
            "session_failed",
            "listener_error",
            "host_error",
            "task_faulted"
        ]
    );
}

/// 一个一投递就抛错的视图监听器。
struct ThrowingListener;

#[async_trait]
impl kiana_durable::types::WatchListener<ConversationView> for ThrowingListener {
    async fn call(
        &self,
        _value: ConversationView,
        _ops: Vec<kiana_chord::delta::Op>,
        _context: std::sync::Arc<dyn Context>,
    ) {
        panic!("listener failed");
    }
}

/// 记录帧数的监听器。
struct CountingListener {
    frames: Arc<AtomicUsize>,
}

#[async_trait]
impl kiana_durable::types::WatchListener<ConversationView> for CountingListener {
    async fn call(
        &self,
        _value: ConversationView,
        _ops: Vec<kiana_chord::delta::Op>,
        _context: std::sync::Arc<dyn Context>,
    ) {
        self.frames.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_throwing_view_listener_ends_only_its_own_watch() {
    let fixture = open_tasks(memory(), Vec::new()).await;
    let root = open_root(&fixture.harness).await;
    let context = task_support::context();
    let failing = root.watch(&*context).await.expect("watch");
    failing.start(Arc::new(ThrowingListener));
    let frames = Arc::new(AtomicUsize::new(0));
    let healthy = root.watch(&*context).await.expect("watch");
    healthy.start(Arc::new(CountingListener {
        frames: Arc::clone(&frames),
    }));
    struct Note {
        conversation_id: kiana_durable::types::ConversationId,
    }
    #[async_trait]
    impl SessionChange for Note {
        async fn run(
            &self,
            tx: &dyn kiana_durable::types::Tx,
        ) -> Result<Box<dyn Any + Send>, BoxError> {
            tx.append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "note".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
            Ok(Box::new(()))
        }
    }
    root.commit(
        &Note {
            conversation_id: root.id(),
        },
        &*context,
        CommitOptions::default(),
    )
    .await
    .expect("commit");
    let end = failing.closed().await;
    match end {
        kiana_durable::types::WatchEnd::ListenerError(message) => {
            assert!(message.contains("listener failed"), "{message}");
        }
        other => panic!("unexpected end: {other:?}"),
    }
    task_support::eventually(|| {
        let frames = Arc::clone(&frames);
        async move { frames.load(Ordering::SeqCst) >= 1 }
    })
    .await
    .expect("the healthy observer still receives frames");
    let _ = healthy.stop().await;
    fixture.harness.close(&*context).await;
}

// ---------------------------------------------------------------------------
// 扩展归属与失败分类（TS: harness-reports 的后三个用例）
// ---------------------------------------------------------------------------

use kiana_durable::env::ExecutionEnv;
use kiana_durable::harness::define::{define_extension, hook, section, wrap_tool};
use kiana_durable::harness::generation::generation_task;
use kiana_durable::harness::types::{
    AnyTask, BeforeRequestInput, EnvBuilder, EnvTarget, GenerationHooks, HookApi, OnYieldOutput,
    Registry, TaskRuntimeChange, ToolRegistration,
};
use kiana_durable::types::{ConversationId, HostCallback, TaskOutcome, TaskState, Tx};

use crate::harness_support::TestExtension;
use crate::task_support::{Deferred, OpenTasksOptions, open_tasks_with};

/// 一个抛错的分段渲染。
fn failing_section(message: &'static str) -> Arc<dyn kiana_durable::harness::types::PromptSection> {
    section(
        "broken",
        Arc::new(move |_input, _context| {
            Box::pin(async move { Err(js_error(message)) as Result<Option<String>, BoxError> })
        }),
        None,
    )
}

/// 一个只让 `afterResponse` 抛错的 generation hook。
struct BrokenAfterResponse;

#[async_trait]
impl GenerationHooks for BrokenAfterResponse {
    async fn before_request(
        &self,
        request: BeforeRequestInput,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<BeforeRequestInput>, BoxError> {
        Ok(Some(request))
    }

    async fn after_response(
        &self,
        _message: kiana_ai::types::AssistantMessage,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Err(js_error("hook"))
    }

    async fn on_yield(
        &self,
        _answer: kiana_ai::types::AssistantMessage,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<OnYieldOutput>, BoxError> {
        Ok(None)
    }

    async fn after_tools(
        &self,
        _assistant: kiana_durable::types::EntryId,
        _results: Vec<kiana_durable::types::EntryId>,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

/// TS: "name the extension of a failing section or hook, and tell them from a failing environment builder".
#[tokio::test(flavor = "multi_thread")]
async fn names_the_extension_of_a_failing_section_or_hook_and_tells_them_from_an_environment_builder()
 {
    use crate::chat_support::{OpenChatOptions, open_chat_with};
    let setup = crate::chat_support::ChatSetup::new();
    setup
        .faux
        .set_responses(vec![crate::chat_support::answer("answer")]);
    setup.registry.install(define_extension(
        TestExtension::new("sections").with_sections(vec![failing_section("section")]),
    ));
    let env: EnvBuilder = Arc::new(|_target: EnvTarget, _context| {
        Box::pin(
            async move { Err(js_error("env")) as Result<Option<Arc<dyn ExecutionEnv>>, BoxError> },
        )
    });
    let (harness, root) =
        open_chat_with(memory(), &setup, OpenChatOptions { env: Some(env) }).await;
    let submission = root
        .submit(harness_support::text_input("hi").into_draft(), &*context())
        .await
        .expect("submit");
    let _ = submission.wait(&*context()).await.expect("settled");
    let messages = setup.report_messages();
    let infos = setup.report_infos();
    let info_of = |message: &str| -> Option<ReportInfo> {
        messages
            .iter()
            .position(|candidate| candidate == message)
            .map(|index| infos[index].clone())
    };
    let generation = |info: &ReportInfo| -> (String, ConversationId) {
        match info {
            ReportInfo::HostError {
                task: Some(task), ..
            }
            | ReportInfo::ExtensionError {
                task: Some(task), ..
            } => (task.kind.clone(), task.conversation_id),
            other => panic!("unexpected report: {other:?}"),
        }
    };
    let env_info = info_of("env").expect("the env builder failure is reported");
    assert!(matches!(
        env_info,
        ReportInfo::HostError {
            callback: HostCallback::Env,
            ..
        }
    ));
    assert_eq!(
        generation(&env_info),
        ("kiana.generation".to_owned(), root.id())
    );
    let section_info = info_of("section").expect("the section failure is reported");
    assert!(matches!(
        &section_info,
        ReportInfo::ExtensionError {
            extension: Some(extension),
            ..
        } if extension == "sections"
    ));
    assert_eq!(
        generation(&section_info),
        ("kiana.generation".to_owned(), root.id())
    );
    harness.close(&*context()).await;
}

/// TS 的同一个用例还断言 hook 失败带扩展名。
#[tokio::test(flavor = "multi_thread")]
async fn names_the_extension_of_a_failing_hook() {
    use crate::chat_support::{OpenChatOptions, open_chat_with};
    let setup = crate::chat_support::ChatSetup::new();
    setup
        .faux
        .set_responses(vec![crate::chat_support::answer("answer")]);
    setup
        .registry
        .install(define_extension(TestExtension::new("hooks").with_hooks(
            vec![hook(
                &generation_task(),
                Arc::new(BrokenAfterResponse) as Arc<dyn GenerationHooks>,
            )],
        )));
    let (harness, root) = open_chat_with(memory(), &setup, OpenChatOptions::default()).await;
    let submission = root
        .submit(harness_support::text_input("hi").into_draft(), &*context())
        .await
        .expect("submit");
    let _ = submission.wait(&*context()).await.expect("settled");
    let messages = setup.report_messages();
    let infos = setup.report_infos();
    let index = messages
        .iter()
        .position(|message| message == "hook")
        .unwrap_or_else(|| panic!("the hook failure is reported: {messages:?}"));
    assert_eq!(
        infos[index],
        ReportInfo::ExtensionError {
            extension: Some("hooks".to_owned()),
            task: Some(ReportTask {
                id: match &infos[index] {
                    ReportInfo::ExtensionError {
                        task: Some(task), ..
                    } => task.id,
                    _ => unreachable!(),
                },
                kind: "kiana.generation".to_owned(),
                conversation_id: root.id(),
            }),
            // TS 的 hook 失败只带 `task`（task 内有 conversationId），没有顶层 conversationId。
            conversation_id: None,
        }
    );
    harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// 一份被替换的任务定义仍然归属于运行它的扩展
// ---------------------------------------------------------------------------

struct ReplacedPhase {
    started: Arc<Deferred<()>>,
    gate: Arc<Deferred<()>>,
    label: &'static str,
}

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for ReplacedPhase {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        self.started.resolve(());
        let _ = self.gate.promise().await;
        Err(js_error(&format!("broken in {}", self.label)))
    }
}

struct NoopAbort;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for NoopAbort {
    async fn run(
        &self,
        _task: &TaskRecord,
        _runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

fn replaced_task(
    started: &Arc<Deferred<()>>,
    gate: &Arc<Deferred<()>>,
    label: &'static str,
) -> AnyTask {
    let definition = TaskDefinition {
        name: "test.replaced".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({"phase": "run"})),
        phases: indexmap::IndexMap::from([(
            "run".to_owned(),
            Arc::new(ReplacedPhase {
                started: Arc::clone(started),
                gate: Arc::clone(gate),
                label,
            }) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        )]),
        abort: Arc::new(NoopAbort) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

/// TS: "name the extension whose definition ran, also after another one replaced it".
#[tokio::test(flavor = "multi_thread")]
async fn names_the_extension_whose_definition_ran_after_another_one_replaced_it() {
    let started = Arc::new(Deferred::<()>::new());
    let gate = Arc::new(Deferred::<()>::new());
    let old = replaced_task(&started, &gate, "old");
    let fixture = open_tasks(memory(), Vec::new()).await;
    fixture.registry.install(define_extension(
        TestExtension::new("old-extension").with_tasks(vec![old.clone()]),
    ));
    let root = open_root(&fixture.harness).await;
    let value = root
        .commit(
            &CreateTask { task: old },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("create the task");
    let id = *value
        .downcast::<kiana_durable::types::TaskId>()
        .expect("task id");
    fixture.harness.resume().expect("resume");
    let _ = started.promise().await;
    fixture.registry.uninstall("old-extension");
    let new = replaced_task(&started, &gate, "new");
    fixture.registry.install(define_extension(
        TestExtension::new("new-extension").with_tasks(vec![new]),
    ));
    gate.resolve(());
    let settled = fixture
        .harness
        .wait_for_task(id, &*context())
        .await
        .expect("the task settles");
    assert!(matches!(settled.outcome(), TaskOutcome::Faulted { .. }));
    task_support::eventually(|| {
        let fixture = &fixture;
        async move {
            fixture
                .report_infos()
                .iter()
                .any(|info| matches!(info, ReportInfo::TaskFaulted { .. }))
        }
    })
    .await
    .expect("the fault is reported");
    let messages = fixture.report_messages();
    let infos = fixture.report_infos();
    let index = infos
        .iter()
        .position(|info| matches!(info, ReportInfo::TaskFaulted { .. }))
        .expect("faulted report");
    assert_eq!(messages[index], "broken in old");
    assert!(matches!(
        &infos[index],
        ReportInfo::TaskFaulted {
            extension: Some(extension),
            ..
        } if extension == "old-extension"
    ));
    fixture.harness.close(&*context()).await;
}

// ---------------------------------------------------------------------------
// wake/计时器/监听器/扩展/Session 失败的区分
// ---------------------------------------------------------------------------

struct SleeperRun;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for SleeperRun {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        // 宿主 setTimer 抛错：报告后改用 setTimeout 装载。
        let _ = runtime.set_timer(Arc::new(|| {}), 10.0);
        runtime
            .sleep_until(
                runtime.now() + 60_000,
                serde_json::json!({"phase": "done"}),
                context,
            )
            .await
    }
}

struct SleeperDone;

#[async_trait]
impl TaskPhase<JsonValue, JsonValue, JsonValue, ()> for SleeperDone {
    async fn run(
        &self,
        _task: &TaskRecord,
        runtime: &dyn TaskRuntime<JsonValue, JsonValue, JsonValue, ()>,
        context: &dyn Context,
    ) -> Result<(), BoxError> {
        runtime
            .commit(&CompletedChange, context, CommitOptions::default())
            .await
    }
}

struct CompletedChange;

#[async_trait]
impl TaskRuntimeChange for CompletedChange {
    async fn run(
        &self,
        _tx: &dyn Tx,
        _current: &TaskRecord,
    ) -> Result<Option<TaskState>, BoxError> {
        Ok(Some(TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: JsonValue::Null,
            },
        }))
    }
}

fn sleeper_task() -> AnyTask {
    let definition = TaskDefinition {
        name: "test.sleeper".to_owned(),
        version: 1,
        initial: Arc::new(|_| serde_json::json!({"phase": "run"})),
        phases: indexmap::IndexMap::from([
            (
                "run".to_owned(),
                Arc::new(SleeperRun) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            ),
            (
                "done".to_owned(),
                Arc::new(SleeperDone) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
            ),
        ]),
        abort: Arc::new(NoopAbort) as Arc<dyn TaskPhase<JsonValue, JsonValue, JsonValue, ()>>,
        migrate: None,
        hooks: None,
    };
    kiana_durable::harness::scheduler::erase_task(&definition)
}

/// 追加一条 note 条目（让一次提交失败用）。
struct NoteChange {
    conversation_id: ConversationId,
}

#[async_trait]
impl SessionChange for NoteChange {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn Any + Send>, BoxError> {
        let entry = tx
            .append_entry(
                self.conversation_id,
                EntryDraft {
                    key_order: None,
                    kind: "note".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            )
            .await?;
        Ok(Box::new(entry.id))
    }
}

/// TS: "tell extension, listener, wake, and timer failures from a failed Session".
#[tokio::test(flavor = "multi_thread")]
async fn tells_extension_listener_wake_and_timer_failures_from_a_failed_session() {
    use kiana_durable::types::Session;

    let storage = Arc::new(support::ControlledStorage::new());
    let fixture = open_tasks_with(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![sleeper_task()],
        OpenTasksOptions {
            on_wake: Some(Arc::new(|_wake| panic!("alarm"))),
            set_timer: Some(Arc::new(|_callback, _delay, _options| panic!("no timers"))),
            ..Default::default()
        },
    )
    .await;
    let echo = harness_support::tool("echo");
    fixture.registry.install(define_extension(
        TestExtension::new("tools").with_tools(vec![echo.clone()]),
    ));
    fixture
        .registry
        .install(define_extension(TestExtension::new("wraps").with_wraps(
            vec![wrap_tool(
                &echo,
                Arc::new(|mut tool: ToolRegistration| {
                    tool.tool.name = "renamed".to_owned();
                    tool
                }),
            )],
        )));
    let _ = fixture
        .harness
        .subscribe_commits(Arc::new(|_publication, _context| panic!("listener")));
    let root = open_root(&fixture.harness).await;
    let _ = root.agent(&*context()).await.expect("agent");
    let value = root
        .commit(
            &CreateTask {
                task: sleeper_task(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await
        .expect("create the sleeper");
    let _id = *value
        .downcast::<kiana_durable::types::TaskId>()
        .expect("task id");
    fixture.harness.resume().expect("resume");
    task_support::eventually(|| {
        let fixture = &fixture;
        async move {
            let messages = fixture.report_messages();
            ["alarm", "no timers"]
                .iter()
                .all(|wanted| messages.iter().any(|message| message == wanted))
        }
    })
    .await
    .expect("wake and timer failures are reported");
    let messages = fixture.report_messages();
    let infos = fixture.report_infos();
    let info_of = |message: &str| -> Option<ReportInfo> {
        messages
            .iter()
            .position(|candidate| candidate == message)
            .map(|index| infos[index].clone())
    };
    let wrap = info_of("Wrapper renamed echo to renamed").expect("the wrapper is reported");
    assert_eq!(
        wrap,
        ReportInfo::ExtensionError {
            extension: Some("wraps".to_owned()),
            task: None,
            conversation_id: Some(root.id()),
        }
    );
    assert_eq!(info_of("listener"), Some(ReportInfo::ListenerError));
    assert_eq!(
        info_of("alarm"),
        Some(ReportInfo::HostError {
            callback: HostCallback::OnWake,
            task: None,
        })
    );
    assert_eq!(
        info_of("no timers"),
        Some(ReportInfo::HostError {
            callback: HostCallback::SetTimer,
            task: None,
        })
    );

    // 一次失败的 Storage 调用让 Session 失败（与上面那些非致命失败区分）。
    storage.fail_next_commit(js_error("disk gone"));
    let _ = root
        .commit(
            &NoteChange {
                conversation_id: root.id(),
            },
            &*context(),
            CommitOptions::default(),
        )
        .await;
    let _ = fixture.harness.closed().await;
    let messages = fixture.report_messages();
    let infos = fixture.report_infos();
    let index = messages
        .iter()
        .position(|message| message == "disk gone")
        .expect("the storage failure is reported");
    assert_eq!(infos[index], ReportInfo::SessionFailed);
    fixture.harness.close(&*context()).await;
}
