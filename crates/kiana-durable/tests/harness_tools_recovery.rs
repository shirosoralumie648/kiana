//! `test/harness-tools-recovery.test.ts` 的移植（阶段级子集）。
//!
//! TS 在崩溃/重开后由 scheduler 恢复 `execute` 阶段；这里直接用 [`harness_runtime::TestRuntime`]
//! 驱动 `ExecutePhase`：
//! - 不安全策略中断后，结果来自 `kiana.live` 槽位的持久部分输出（TS `fromSlot(slot)`）；
//! - 只有持久与当前的 replay 策略都是 `safe` 才重跑。
//!
//! 依赖完整 Harness 的用例（真实的进程崩溃、环境重建、代际工具轮）见交付报告。

mod harness_runtime;
mod support;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::models::{CreateModelsOptions, create_models};
use kiana_ai::providers::faux::RegisterFauxProviderOptions;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, ModelThinkingLevel,
    StopReason, TextContent, TextKind, Tool, ToolCall, ToolCallKind, UserContentBlock,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::live::LIVE_DOC;
use kiana_durable::harness::tool::{ExecutePhase, TOOL_TASK, ToolTaskInput};
use kiana_durable::harness::types::{
    Agent, HarnessSettings, ModelRef, ReplayPolicy, Settings, ToolExecuteFn, ToolExecutionResult,
    ToolRegistration,
};
use kiana_durable::types::{
    AssistantEntry, ConversationId, DocTarget, JsonValue, TaskOutcome, TaskPhase, TaskRecord,
    TaskState,
};
use kiana_durable::{NoData, TypedEntryDraft};

use harness_runtime::TestRuntime;
use support::{commit, create_conversation, open_test_session};

fn ctx() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

fn settings() -> Arc<Settings> {
    Arc::new(resolve_settings(Some(&HarnessSettings::default())))
}

fn agent(reference: &ModelRef, tool: ToolRegistration) -> Arc<Agent> {
    Arc::new(Agent {
        model: Some(reference.clone()),
        thinking_level: ModelThinkingLevel::default(),
        extensions: Vec::new(),
        tools: vec![tool.clone()],
        callable: vec![tool],
        deferred: Vec::new(),
        sections: Vec::new(),
        instructions: None,
        cwd: None,
    })
}

async fn append_call(
    session: &kiana_durable::session::SessionImpl,
    conversation_id: ConversationId,
) -> kiana_durable::EntryId {
    commit(session, move |tx| {
        Box::pin(async move {
            let draft = AssistantEntry.typed_draft(TypedEntryDraft {
                model: Some(vec![Message::Assistant(AssistantMessage {
                    role: AssistantRole::Assistant,
                    content: vec![AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: "c1".to_owned(),
                        name: "work".to_owned(),
                        arguments: Default::default(),
                        ..Default::default()
                    })],
                    stop_reason: StopReason::ToolUse,
                    timestamp: 1,
                    ..Default::default()
                })]),
                data: NoData,
                head: None,
                edits: None,
            })?;
            Ok(tx.append_entry(conversation_id, draft).await?.id)
        })
    })
    .await
    .expect("assistant entry appends")
}

/// 把 `kiana.live` 的工具槽写成“中断时已发布部分输出”的样子。
async fn write_slot(
    session: &kiana_durable::session::SessionImpl,
    conversation_id: ConversationId,
    task_id: kiana_durable::TaskId,
) {
    commit(session, move |tx| {
        Box::pin(async move {
            let mut live = tx
                .doc(&DocTarget::conversation(&LIVE_DOC, conversation_id))
                .await?;
            live.value_mut().insert(
                "tools".to_owned(),
                serde_json::json!([{
                    "callId": "c1",
                    "name": "work",
                    "status": "running",
                    "taskId": task_id.get(),
                    "output": "run 1\n",
                    "details": { "run": 1 }
                }]),
            );
            Ok(())
        })
    })
    .await
    .expect("live slot writes");
}

fn execute_record(
    conversation_id: ConversationId,
    assistant: kiana_durable::EntryId,
    stored: &str,
) -> TaskRecord {
    TaskRecord {
        id: kiana_durable::id_from_number(3000),
        conversation_id,
        kind: TOOL_TASK.definition.name.clone(),
        version: 2,
        input: serde_json::to_value(ToolTaskInput::Model {
            assistant,
            call_id: "c1".to_owned(),
        })
        .expect("tool input"),
        trace: None,
        links: None,
        owner: Some(kiana_durable::id_from_number(2999)),
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state: TaskState::Running {
            checkpoint: serde_json::json!({
                "phase": "execute",
                "arguments": {},
                "replay": stored,
            }),
        },
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    }
}

fn work_tool(replay: Option<ReplayPolicy>) -> (ToolRegistration, Arc<AtomicUsize>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&runs);
    let execute: ToolExecuteFn = Arc::new(move |_arguments: JsonValue, _api, _context| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            let run = counter.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("run {run}\n"),
                    text_signature: None,
                })]),
                structured_output: None,
                is_error: Some(false),
                details: None,
                diagnostics: None,
                usage: None,
                control: None,
            })
        })
            as Pin<
                Box<dyn std::future::Future<Output = Result<ToolExecutionResult, BoxError>> + Send>,
            >
    });
    let tool = Tool {
        name: "work".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({ "type": "object" }),
        constrained_sampling: None,
    };
    let mut registration = ToolRegistration::new(tool);
    registration.replay = replay;
    registration.execute = Some(execute);
    (registration, runs)
}

struct Setup {
    runtime: TestRuntime<kiana_durable::harness::tool::ToolHookTable>,
    task: TaskRecord,
    conversation_id: ConversationId,
    runs: Arc<AtomicUsize>,
}

async fn setup(stored: &str, current: Option<ReplayPolicy>, slot: bool) -> Setup {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let assistant = append_call(&session, conversation_id).await;
    let record = execute_record(conversation_id, assistant, stored);
    if slot {
        write_slot(&session, conversation_id, record.id).await;
    }

    let faux =
        kiana_ai::providers::faux::faux_provider_with(RegisterFauxProviderOptions::default());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };
    let (tool, runs) = work_tool(current);
    let runtime = TestRuntime::new(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        record.clone(),
        settings(),
        models as Arc<dyn kiana_ai::models::Models>,
        agent(&reference, tool),
        Arc::new(harness_runtime::TestHookRunner::<
            dyn kiana_durable::harness::types::ToolHooks,
        >::new()),
        1_000,
    );
    Setup {
        runtime,
        task: record,
        conversation_id,
        runs,
    }
}

fn tool_result(setup: &Setup) -> Option<kiana_durable::EntryRecord> {
    futures::executor::block_on(harness_runtime::conversation_entries(
        &setup.runtime.session,
        &*setup.runtime.storage,
        setup.conversation_id,
        ctx(),
    ))
    .into_iter()
    .find(|entry| entry.kind == "kiana.tool-result")
}

fn result_text(entry: &kiana_durable::EntryRecord) -> String {
    let Some(Message::ToolResult(message)) = entry.model.as_ref().and_then(|model| model.first())
    else {
        return String::new();
    };
    message
        .content
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[tokio::test]
async fn an_unsafe_tool_interrupted_after_intent_reports_its_durable_partial_output() {
    let setup = setup("unsafe", None, true).await;
    ExecutePhase
        .run(&setup.task, &setup.runtime, ctx())
        .await
        .expect("an interrupted tool settles");
    assert_eq!(
        setup.runs.load(Ordering::SeqCst),
        0,
        "the tool is not rerun"
    );
    assert!(matches!(
        setup.runtime.task_record().state.outcome(),
        Some(TaskOutcome::Failed { error, .. })
            if error.message == "Tool work was interrupted and may have partially run"
    ));
    let entry = tool_result(&setup).expect("the interrupted result is appended");
    let Some(Message::ToolResult(message)) = entry.model.as_ref().and_then(|model| model.first())
    else {
        panic!("tool result message");
    };
    assert!(message.is_error);
    assert_eq!(message.details, Some(serde_json::json!({ "run": 1 })));
    assert_eq!(
        result_text(&entry),
        "run 1\n|<harness>\n[error] Tool work was interrupted and may have partially run\n</harness>"
    );
}

#[tokio::test]
async fn a_tool_reruns_only_when_the_stored_and_the_current_replay_policy_are_safe() {
    let cases: [(&str, Option<ReplayPolicy>, bool); 3] = [
        ("safe", Some(ReplayPolicy::Safe), true),
        ("safe", Some(ReplayPolicy::Unsafe), false),
        ("unsafe", Some(ReplayPolicy::Safe), false),
    ];
    for (stored, current, reruns) in cases {
        let setup = setup(stored, current, !reruns).await;
        ExecutePhase
            .run(&setup.task, &setup.runtime, ctx())
            .await
            .expect("the execute phase settles");
        assert_eq!(
            setup.runs.load(Ordering::SeqCst),
            if reruns { 1 } else { 0 },
            "stored {stored}, current {current:?}"
        );
        let entry = tool_result(&setup).expect("a result entry");
        let Some(Message::ToolResult(message)) =
            entry.model.as_ref().and_then(|model| model.first())
        else {
            panic!("tool result message");
        };
        assert_eq!(
            message.is_error, !reruns,
            "stored {stored}, current {current:?}"
        );
        if reruns {
            assert_eq!(result_text(&entry), "run 1\n");
            assert!(matches!(
                setup.runtime.task_record().state.outcome(),
                Some(TaskOutcome::Completed { .. })
            ));
        }
    }
}
