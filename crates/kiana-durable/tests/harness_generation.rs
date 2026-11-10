//! `test/harness-generation.test.ts` 的移植（阶段级子集）。
//!
//! TS 通过完整 `Harness`（scheduler 单元）驱动 generation；本工作副本里 `harness/scheduler.rs`
//! 仍是骨架，因此这里用 [`harness_runtime::TestRuntime`] 直接驱动 `RequestPhase`，覆盖不依赖
//! 调度器的 generation 行为：hook 抛错的上报与跳过（TS `hooks.each`）、回答条目与 run 的落定。
//! 依赖完整 Harness 的用例（重试、延迟轮询、工具轮编排、事件）见交付报告的 remainingGaps。

mod harness_runtime;
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kiana_ai::models::{CreateModelsOptions, create_models};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
    faux_provider_with,
};
use kiana_ai::types::{
    AssistantMessage, Message, ModelThinkingLevel, UserContent, UserMessage, UserRole,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::UserEntry;
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::generation::{GenerationCheckpoint, RequestCheckpoint, RequestPhase};
use kiana_durable::harness::types::{
    Agent, BeforeRequestInput, ConversationStreamOptions, GenerationHooks, HarnessSettings,
    HookApi, ModelRef, OnYieldOutput, ReportFrom, Settings,
};
use kiana_durable::types::{
    ConversationId, EntryId, JsonValue, RunningTask, TaskOutcome, TaskPhase, TaskRecord, TaskState,
};

use harness_runtime::{Report, TestHookRunner, TestRuntime};
use kiana_durable::Storage;
use support::{commit, create_conversation, open_test_session};

fn ctx() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn settings() -> Arc<Settings> {
    Arc::new(resolve_settings(Some(&HarnessSettings {
        stream: Some(ConversationStreamOptions::default()),
        ..Default::default()
    })))
}

fn agent(reference: &ModelRef) -> Arc<Agent> {
    Arc::new(Agent {
        model: Some(reference.clone()),
        thinking_level: ModelThinkingLevel::default(),
        extensions: Vec::new(),
        tools: Vec::new(),
        callable: Vec::new(),
        deferred: Vec::new(),
        sections: Vec::new(),
        instructions: None,
        cwd: None,
    })
}

fn generation_record(
    conversation_id: ConversationId,
    state: TaskState<JsonValue, JsonValue>,
) -> TaskRecord {
    TaskRecord {
        id: kiana_durable::id_from_number(1000),
        conversation_id,
        kind: "kiana.generation".to_owned(),
        version: 1,
        input: JsonValue::Object(Default::default()),
        trace: None,
        links: None,
        owner: None,
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state,
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    }
}

async fn append_user(
    session: &kiana_durable::session::SessionImpl,
    conversation_id: ConversationId,
    text: &str,
) -> EntryId {
    let text = text.to_owned();
    commit(session, move |tx| {
        Box::pin(async move {
            let draft = UserEntry.typed_draft(kiana_durable::TypedEntryDraft {
                model: Some(vec![Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text(text),
                    timestamp: 1,
                })]),
                data: kiana_durable::NoData,
                head: None,
                edits: None,
            })?;
            Ok(tx.append_entry(conversation_id, draft).await?.id)
        })
    })
    .await
    .expect("user entry appends")
}

fn request_state(reference: &ModelRef, cutoff: EntryId) -> TaskState<JsonValue, JsonValue> {
    let checkpoint = GenerationCheckpoint::Request(RequestCheckpoint {
        attempt: 1,
        compacted: None,
        model: reference.clone(),
        thinking_level: ModelThinkingLevel::default(),
        stream_options: ConversationStreamOptions::default(),
        cutoff,
    });
    TaskState::Running {
        checkpoint: serde_json::to_value(checkpoint).expect("checkpoint"),
    }
}

/// 可配置的 hook 探针：记录调用次数，按配置抛错或给出 continuation。
#[derive(Default)]
struct ProbeHook {
    before_error: Option<&'static str>,
    yield_error: Option<&'static str>,
    continuation: Option<&'static str>,
    before_calls: Arc<AtomicUsize>,
    yield_calls: Arc<AtomicUsize>,
}

impl ProbeHook {
    fn new() -> Self {
        Self::default()
    }

    fn before(mut self, error: &'static str) -> Self {
        self.before_error = Some(error);
        self
    }

    fn yielding(mut self, error: Option<&'static str>, continuation: Option<&'static str>) -> Self {
        self.yield_error = error;
        self.continuation = continuation;
        self
    }

    fn before_calls(&self) -> usize {
        self.before_calls.load(Ordering::SeqCst)
    }

    fn yield_calls(&self) -> usize {
        self.yield_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl GenerationHooks for ProbeHook {
    async fn before_request(
        &self,
        request: BeforeRequestInput,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<BeforeRequestInput>, BoxError> {
        self.before_calls.fetch_add(1, Ordering::SeqCst);
        match self.before_error {
            Some(error) => Err(error.into()),
            None => Ok(Some(request)),
        }
    }

    async fn after_response(
        &self,
        _message: AssistantMessage,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    async fn on_yield(
        &self,
        _answer: AssistantMessage,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<OnYieldOutput>, BoxError> {
        self.yield_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.yield_error {
            return Err(error.into());
        }
        Ok(self.continuation.map(|text| OnYieldOutput {
            continue_run: UserContent::Text(text.to_owned()),
        }))
    }

    async fn after_tools(
        &self,
        _assistant: EntryId,
        _results: Vec<EntryId>,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

/// 每次测试都需要的装配。
struct Setup {
    runtime: TestRuntime<kiana_durable::harness::generation::GenerationHookTable>,
    conversation_id: ConversationId,
    task: RunningTask,
}

async fn setup(
    hooks: TestHookRunner<dyn GenerationHooks>,
    replies: Vec<AssistantMessage>,
) -> Setup {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let cutoff = append_user(&session, conversation_id, "go").await;
    let faux = faux_provider_with(RegisterFauxProviderOptions::default());
    faux.set_responses(replies.into_iter().map(Into::into).collect());
    let models_builder = create_models(CreateModelsOptions::default());
    models_builder.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };
    let record = generation_record(conversation_id, request_state(&reference, cutoff));
    let runtime = TestRuntime::new(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        record.clone(),
        settings(),
        models_builder as Arc<dyn kiana_ai::models::Models>,
        agent(&reference),
        Arc::new(hooks),
        1_000,
    );
    Setup {
        runtime,
        conversation_id,
        task: record,
    }
}

async fn run_request(setup: &Setup) -> Result<(), BoxError> {
    RequestPhase.run(&setup.task, &setup.runtime, ctx()).await
}

async fn assistant_entries(setup: &Setup) -> Vec<kiana_durable::EntryRecord> {
    harness_runtime::conversation_entries(
        &setup.runtime.session,
        &*setup.runtime.storage,
        setup.conversation_id,
        ctx(),
    )
    .await
    .into_iter()
    .filter(|entry| entry.kind == "kiana.assistant")
    .collect()
}

#[tokio::test]
async fn a_throwing_on_yield_handler_is_reported_and_the_answer_still_commits() {
    let first = Arc::new(ProbeHook::new().yielding(Some("observer failed"), None));
    let second = Arc::new(ProbeHook::new().yielding(None, None));
    let hooks = TestHookRunner::<dyn GenerationHooks>::new()
        .add(
            "onYield",
            Some("ext.a"),
            Arc::clone(&first) as Arc<dyn GenerationHooks>,
        )
        .add(
            "onYield",
            Some("ext.b"),
            Arc::clone(&second) as Arc<dyn GenerationHooks>,
        );
    let setup = setup(
        hooks,
        vec![faux_assistant_message(
            "hello",
            FauxAssistantMessageOptions::default(),
        )],
    )
    .await;
    run_request(&setup)
        .await
        .expect("the throwing handler must not fail the generation");
    // 后一个处理器仍然运行。
    assert_eq!(second.yield_calls(), 1);
    // 模型回答仍然被追加为 `kiana.assistant`。
    let entries = assistant_entries(&setup).await;
    assert_eq!(entries.len(), 1, "the answer entry is appended");
    // 错误按 `extension_error` 上报，并带上扩展名。
    let reports = setup.runtime.reports();
    assert_eq!(
        reports,
        vec![Report {
            message: "observer failed".to_owned(),
            from: Some(ReportFrom::Extension {
                extension: Some("ext.a".to_owned()),
            }),
        }]
    );
    assert!(matches!(
        setup.runtime.task_record().state.outcome(),
        Some(TaskOutcome::Completed { .. })
    ));
}

#[tokio::test]
async fn a_throwing_before_request_handler_is_reported_and_later_handlers_still_run() {
    let first = Arc::new(ProbeHook::new().before("hook broke"));
    let second = Arc::new(ProbeHook::new());
    let hooks = TestHookRunner::<dyn GenerationHooks>::new()
        .add(
            "beforeRequest",
            Some("ext.a"),
            Arc::clone(&first) as Arc<dyn GenerationHooks>,
        )
        .add(
            "beforeRequest",
            Some("ext.b"),
            Arc::clone(&second) as Arc<dyn GenerationHooks>,
        );
    let setup = setup(
        hooks,
        vec![faux_assistant_message(
            "hi",
            FauxAssistantMessageOptions::default(),
        )],
    )
    .await;
    run_request(&setup).await.expect("hooks.each continues");
    assert_eq!(first.before_calls(), 1);
    assert_eq!(second.before_calls(), 1);
    assert_eq!(assistant_entries(&setup).await.len(), 1);
    let reports = setup.runtime.reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].message, "hook broke");
    assert_eq!(
        reports[0].from,
        Some(ReportFrom::Extension {
            extension: Some("ext.a".to_owned()),
        })
    );
}

/// `startRun` 的提交把输入提交的 trace 带进新任务（TS `Transaction.createTask` 的
/// `submissionIds`）：首个提交定 trace，其余各追加一条 link。
#[tokio::test]
async fn a_started_run_derives_its_task_trace_and_links_from_the_input_submissions() {
    use indexmap::IndexMap;
    use kiana_durable::harness::generation::create_generation;
    use kiana_durable::types::{SubmissionCreate, SubmissionKind, SubmissionStatus};

    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let mut first_trace = IndexMap::new();
    first_trace.insert("traceparent".to_owned(), "00-aaa-bbb-01".to_owned());
    let mut second_trace = IndexMap::new();
    second_trace.insert("traceparent".to_owned(), "00-ccc-ddd-02".to_owned());

    let first_link = first_trace.clone();
    let second_link = second_trace.clone();
    let task_id = commit(&session, move |tx| {
        Box::pin(async move {
            let first = tx
                .create_submission(SubmissionCreate {
                    trace: Some(first_trace),
                    conversation_id,
                    request_id: Some("first".to_owned()),
                    kind: SubmissionKind::Input,
                    status: SubmissionStatus::Queued,
                    entry: None,
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            let second = tx
                .create_submission(SubmissionCreate {
                    trace: Some(second_trace),
                    conversation_id,
                    request_id: Some("second".to_owned()),
                    kind: SubmissionKind::Input,
                    status: SubmissionStatus::Queued,
                    entry: None,
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            create_generation(tx, conversation_id, Some(&[first.id, second.id])).await
        })
    })
    .await
    .expect("the run is created");

    let record = storage
        .task(task_id, ctx())
        .await
        .expect("task reads")
        .expect("task exists");
    assert_eq!(record.trace, Some(first_link));
    assert_eq!(record.links, Some(vec![second_link]));
}
