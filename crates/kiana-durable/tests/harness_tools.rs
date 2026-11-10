//! `test/harness-tools.test.ts` 的移植（阶段级子集）。
//!
//! TS 通过完整 `Harness` 驱动 `ToolTask`；本工作副本里 scheduler 仍是骨架，因此这里用
//! [`harness_runtime::TestRuntime`] 直接驱动 `CallPhase`，覆盖不依赖调度器的行为：
//! - `afterTool` 抛错按 `extension_error` 上报并跳过，工具结果照常结算（TS `hooks.each`）；
//! - `beforeTool` 的抛错把调用阻止为 `blocked` 诊断（TS 的 catch-and-block），且后面的处理器
//!   不再运行。
//!
//! 依赖完整 Harness 的用例（嵌套调用、重放、进度提交、并发轮次）见交付报告。

mod harness_runtime;
mod support;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kiana_ai::models::{CreateModelsOptions, create_models};
use kiana_ai::providers::faux::RegisterFauxProviderOptions;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, ModelThinkingLevel,
    StopReason, TextContent, TextKind, Tool, ToolCall, ToolCallKind, UserContentBlock,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::tool::{CallPhase, TOOL_TASK, ToolTaskInput};
use kiana_durable::harness::types::{
    Agent, HarnessSettings, HookApi, ModelRef, OutputLimits, OutputRetain, ReportFrom, Settings,
    ToolDiagnostic, ToolExecuteFn, ToolExecutionResult, ToolHooks, ToolOutput, ToolRegistration,
};
use kiana_durable::types::{
    AssistantEntry, ConversationId, JsonObject, JsonValue, TaskOutcome, TaskPhase, TaskRecord,
    TaskState,
};
use kiana_durable::{NoData, TypedEntryDraft};

use harness_runtime::{Report, TestHookRunner, TestRuntime};
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
                        id: "call-1".to_owned(),
                        name: "echo".to_owned(),
                        arguments: {
                            let mut arguments = JsonObject::new();
                            arguments.insert("text".to_owned(), JsonValue::String("hi".to_owned()));
                            arguments
                        },
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

fn tool_task_record(
    conversation_id: ConversationId,
    assistant: kiana_durable::EntryId,
) -> TaskRecord {
    TaskRecord {
        id: kiana_durable::id_from_number(3000),
        conversation_id,
        kind: TOOL_TASK.definition.name.clone(),
        version: 2,
        input: serde_json::to_value(ToolTaskInput::Model {
            assistant,
            call_id: "call-1".to_owned(),
        })
        .expect("tool input"),
        trace: None,
        links: None,
        owner: Some(kiana_durable::id_from_number(2999)),
        background: false,
        abort_requested: false,
        abandon_on_restart: None,
        state: TaskState::Running {
            checkpoint: serde_json::json!({ "phase": "call" }),
        },
        started_at: None,
        ended_at: None,
        restarts: None,
        abort_reason: None,
        memos: None,
    }
}

fn echo_tool() -> (ToolRegistration, Arc<AtomicUsize>) {
    let executed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&executed);
    let execute: ToolExecuteFn = Arc::new(move |arguments: JsonValue, _api, _context| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let text = arguments
                .get("text")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_owned();
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text,
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
        name: "echo".to_owned(),
        description: String::new(),
        parameters: kiana_durable::types::JsonValue::Object(Default::default()),
        constrained_sampling: None,
    };
    let mut registration = ToolRegistration::new(tool);
    registration.execute = Some(execute);
    (registration, executed)
}

/// `afterTool`/`beforeTool` 探针。
struct ProbeToolHook {
    after_error: Option<&'static str>,
    before_error: Option<&'static str>,
    before_calls: Arc<AtomicUsize>,
    after_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolHooks for ProbeToolHook {
    async fn before_tool(
        &self,
        call: kiana_durable::harness::types::ToolHookCall,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<kiana_durable::harness::types::BeforeToolOutput>, BoxError> {
        self.before_calls.fetch_add(1, Ordering::SeqCst);
        match self.before_error {
            Some(error) => Err(error.into()),
            None => Ok(Some(kiana_durable::harness::types::BeforeToolOutput {
                arguments: Some(call.call.arguments),
                block: None,
            })),
        }
    }

    async fn after_tool(
        &self,
        _call: kiana_durable::harness::types::ToolHookCall,
        _result: ToolExecutionResult,
        _api: Arc<dyn HookApi>,
        _context: &dyn Context,
    ) -> Result<Option<ToolExecutionResult>, BoxError> {
        self.after_calls.fetch_add(1, Ordering::SeqCst);
        match self.after_error {
            Some(error) => Err(error.into()),
            None => Ok(None),
        }
    }
}

fn probe() -> ProbeToolHook {
    ProbeToolHook {
        after_error: None,
        before_error: None,
        before_calls: Arc::new(AtomicUsize::new(0)),
        after_calls: Arc::new(AtomicUsize::new(0)),
    }
}

struct Setup {
    runtime: TestRuntime<kiana_durable::harness::tool::ToolHookTable>,
    task: TaskRecord,
    conversation_id: ConversationId,
    executed: Arc<AtomicUsize>,
}

async fn setup(hooks: TestHookRunner<dyn ToolHooks>) -> Setup {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let assistant = append_call(&session, conversation_id).await;

    let faux =
        kiana_ai::providers::faux::faux_provider_with(RegisterFauxProviderOptions::default());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };
    let (tool, executed) = echo_tool();
    let record = tool_task_record(conversation_id, assistant);
    let runtime = TestRuntime::new(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        record.clone(),
        settings(),
        models as Arc<dyn kiana_ai::models::Models>,
        agent(&reference, tool),
        Arc::new(hooks),
        1_000,
    );
    Setup {
        runtime,
        task: record,
        conversation_id,
        executed,
    }
}

fn tool_results(setup: &Setup) -> Vec<kiana_durable::EntryRecord> {
    futures::executor::block_on(harness_runtime::conversation_entries(
        &setup.runtime.session,
        &*setup.runtime.storage,
        setup.conversation_id,
        ctx(),
    ))
    .into_iter()
    .filter(|entry| entry.kind == "kiana.tool-result")
    .collect()
}

fn diagnostics(entry: &kiana_durable::EntryRecord) -> Vec<ToolDiagnostic> {
    entry
        .data
        .as_ref()
        .and_then(|data| data.get("diagnostics"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_throwing_after_tool_hook_is_reported_and_the_tool_result_still_settles() {
    let first = Arc::new(ProbeToolHook {
        after_error: Some("after hook broke"),
        ..probe()
    });
    let second = Arc::new(probe());
    let hooks = TestHookRunner::<dyn ToolHooks>::new()
        .add(
            "afterTool",
            Some("ext.a"),
            Arc::clone(&first) as Arc<dyn ToolHooks>,
        )
        .add(
            "afterTool",
            Some("ext.b"),
            Arc::clone(&second) as Arc<dyn ToolHooks>,
        );
    let setup = setup(hooks).await;
    CallPhase
        .run(&setup.task, &setup.runtime, ctx())
        .await
        .expect("a throwing afterTool hook must not fail the tool");
    assert_eq!(setup.executed.load(Ordering::SeqCst), 1);
    assert_eq!(second.after_calls.load(Ordering::SeqCst), 1);
    let results = tool_results(&setup);
    assert_eq!(results.len(), 1);
    assert!(matches!(
        setup.runtime.task_record().state.outcome(),
        Some(TaskOutcome::Completed { .. })
    ));
    assert_eq!(
        setup.runtime.reports(),
        vec![Report {
            message: "after hook broke".to_owned(),
            from: Some(ReportFrom::Extension {
                extension: Some("ext.a".to_owned()),
            }),
        }]
    );
    let _ = (first, second);
}

#[tokio::test]
async fn a_throwing_before_tool_hook_blocks_the_call_and_later_handlers_do_not_run() {
    let first = Arc::new(ProbeToolHook {
        before_error: Some("blocked by hook"),
        ..probe()
    });
    let second = Arc::new(ProbeToolHook {
        before_error: Some("must not run"),
        ..probe()
    });
    let hooks = TestHookRunner::<dyn ToolHooks>::new()
        .add(
            "beforeTool",
            Some("ext.a"),
            Arc::clone(&first) as Arc<dyn ToolHooks>,
        )
        .add(
            "beforeTool",
            Some("ext.b"),
            Arc::clone(&second) as Arc<dyn ToolHooks>,
        );
    let setup = setup(hooks).await;
    CallPhase
        .run(&setup.task, &setup.runtime, ctx())
        .await
        .expect("blocked tool calls settle");
    assert_eq!(
        setup.executed.load(Ordering::SeqCst),
        0,
        "the tool never runs"
    );
    assert_eq!(second.before_calls.load(Ordering::SeqCst), 0);
    let results = tool_results(&setup);
    assert_eq!(results.len(), 1);
    let diagnostics = diagnostics(&results[0]);
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("blocked")
                && diagnostic.message.contains("blocked by hook")
        }),
        "{diagnostics:?}"
    );
    // TS 的 catch-and-block 不产生 extension_error 报告。
    assert!(setup.runtime.reports().is_empty());
    let _ = (first, second);
}

#[tokio::test]
async fn bounds_a_large_output_and_reports_the_dropped_tail() {
    // 与 TS `boundContent`/`truncated` 相同：保留尾部，丢掉的字节以 warn 诊断报告。
    let execute: ToolExecuteFn = Arc::new(move |_arguments: JsonValue, api, _context| {
        Box::pin(async move {
            for index in 0..100 {
                api.output(ToolOutput::Text(format!("line {index}\n")), None);
            }
            Ok(ToolExecutionResult {
                output: None,
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
    let mut tool = ToolRegistration::new(Tool {
        name: "echo".to_owned(),
        description: String::new(),
        parameters: JsonValue::Object(Default::default()),
        constrained_sampling: None,
    });
    tool.execute = Some(execute);
    tool.output_limits = Some(OutputLimits {
        max_bytes: Some(20),
        max_lines: Some(3),
        retain: Some(OutputRetain::Tail),
    });

    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let assistant = append_call(&session, conversation_id).await;
    let faux =
        kiana_ai::providers::faux::faux_provider_with(RegisterFauxProviderOptions::default());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };
    let record = tool_task_record(conversation_id, assistant);
    let runtime = TestRuntime::new(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        record.clone(),
        settings(),
        models as Arc<dyn kiana_ai::models::Models>,
        agent(&reference, tool),
        Arc::new(TestHookRunner::<dyn ToolHooks>::new()),
        1_000,
    );
    CallPhase
        .run(&record, &runtime, ctx())
        .await
        .expect("the bounded result settles");
    let entries = harness_runtime::conversation_entries(
        &runtime.session,
        &*runtime.storage,
        conversation_id,
        ctx(),
    )
    .await;
    let result = entries
        .iter()
        .find(|entry| entry.kind == "kiana.tool-result")
        .expect("tool result");
    let Some(Message::ToolResult(message)) = result.model.as_ref().and_then(|model| model.first())
    else {
        panic!("tool result message");
    };
    let text = message
        .content
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("|");
    assert!(text.contains("line 99"), "{text}");
    assert!(!text.contains("line 0\n"), "the head is dropped: {text}");
    let diagnostics = diagnostics(result);
    let truncated = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code.as_deref() == Some("truncated"))
        .expect("a truncation diagnostic");
    assert_eq!(
        truncated.severity,
        kiana_durable::harness::types::DiagnosticSeverity::Warn
    );
    assert!(
        truncated
            .message
            .starts_with("Output truncated to its end: "),
        "{}",
        truncated.message
    );
}
