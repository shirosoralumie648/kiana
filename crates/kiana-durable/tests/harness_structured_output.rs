//! `test/harness-structured-output.test.ts` 的移植（阶段级子集）。
//!
//! TS 通过完整 `Harness` 驱动工具调用；这里用 [`harness_runtime::TestRuntime`] 直接驱动
//! `CallPhase`，覆盖 schema 契约：
//! - 模型发起的调用违反 `structuredOutputSchema` 时，结果照常给模型，宿主收到
//!   `extension_error` 报告（TS: "the model never sees structured output, so its call stands"）；
//! - 嵌套调用违反 schema 时，结果变成 `isError` 并带 `invalid_structured_output` 诊断。
//!
//! 参数校验/修复（`prepareToolArguments` 的 TypeBox 等价物）的用例见 harness_tools.rs 与
//! 交付报告。

mod harness_runtime;
mod support;

use std::pin::Pin;
use std::sync::Arc;

use kiana_ai::models::{CreateModelsOptions, create_models};
use kiana_ai::providers::faux::RegisterFauxProviderOptions;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, ModelThinkingLevel,
    StopReason, TextContent, TextKind, Tool, ToolCall, ToolCallKind, UserContentBlock,
};
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::BoxError;
use kiana_durable::harness::agent::resolve_settings;
use kiana_durable::harness::tool::{CallPhase, NESTED_CALL_DOC, TOOL_TASK, ToolTaskInput};
use kiana_durable::harness::types::{
    Agent, HarnessSettings, ModelRef, ReportFrom, Settings, ToolExecuteFn, ToolExecutionResult,
    ToolRegistration,
};
use kiana_durable::types::{
    AssistantEntry, ConversationId, DocTarget, DocumentReader, JsonValue, TaskOutcome, TaskPhase,
    TaskRecord, TaskState,
};
use kiana_durable::{NoData, TypedEntryDraft};

use harness_runtime::{TestHookRunner, TestRuntime};
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

fn schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": { "a": { "type": "string" } },
        "required": ["a"],
        "additionalProperties": false
    })
}

fn structured_tool(structured: JsonValue) -> ToolRegistration {
    let execute: ToolExecuteFn = Arc::new(move |_arguments: JsonValue, _api, _context| {
        let structured = structured.clone();
        Box::pin(async move {
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "done".to_owned(),
                    text_signature: None,
                })]),
                structured_output: Some(structured),
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
    let mut registration = ToolRegistration::new(Tool {
        name: "echo".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({ "type": "object" }),
        constrained_sampling: None,
    });
    registration.structured_output_schema = Some(schema());
    registration.execute = Some(execute);
    registration
}

async fn append_assistant_call(
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

fn record_with(conversation_id: ConversationId, input: ToolTaskInput) -> TaskRecord {
    TaskRecord {
        id: kiana_durable::id_from_number(4000),
        conversation_id,
        kind: TOOL_TASK.definition.name.clone(),
        version: 2,
        input: serde_json::to_value(input).expect("tool input"),
        trace: None,
        links: None,
        owner: Some(kiana_durable::id_from_number(3999)),
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

fn build_runtime(
    session: kiana_durable::session::SessionImpl,
    storage: Arc<dyn kiana_durable::Storage>,
    record: &TaskRecord,
    tool: ToolRegistration,
) -> TestRuntime<kiana_durable::harness::tool::ToolHookTable> {
    let faux =
        kiana_ai::providers::faux::faux_provider_with(RegisterFauxProviderOptions::default());
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(faux.provider.clone());
    let model = faux.get_model(None).expect("faux model").clone();
    let reference = ModelRef {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
    };
    TestRuntime::new(
        session,
        storage,
        record.clone(),
        settings(),
        models as Arc<dyn kiana_ai::models::Models>,
        agent(&reference, tool),
        Arc::new(TestHookRunner::<dyn kiana_durable::harness::types::ToolHooks>::new()),
        1_000,
    )
}

#[tokio::test]
async fn a_model_call_with_a_schema_mismatch_reports_and_keeps_the_call() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let assistant = append_assistant_call(&session, conversation_id).await;
    let record = record_with(
        conversation_id,
        ToolTaskInput::Model {
            assistant,
            call_id: "call-1".to_owned(),
        },
    );
    let runtime = build_runtime(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        &record,
        structured_tool(serde_json::json!({ "a": 1 })),
    );
    CallPhase
        .run(&record, &runtime, ctx())
        .await
        .expect("the call settles");

    // 模型看到的调用照常成立：结果条目没有 structuredOutput，也不是错误。
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
    assert!(!message.is_error);
    // `structuredOutput` 不进条目：条目 JSON 里没有该键。
    let stored = serde_json::to_value(result).expect("entry serializes");
    assert!(stored.get("structuredOutput").is_none());

    // 宿主收到 `extension_error` 报告（没有扩展声明该工具时 extension 为 None）。
    let reports = runtime.reports();
    assert_eq!(reports.len(), 1);
    assert!(
        reports[0]
            .message
            .starts_with("Tool echo returned structuredOutput that does not match its schema: a: "),
        "{}",
        reports[0].message
    );
    assert_eq!(
        reports[0].from,
        Some(ReportFrom::Extension { extension: None })
    );
    assert!(matches!(
        runtime.task_record().state.outcome(),
        Some(TaskOutcome::Completed { .. })
    ));
}

#[tokio::test]
async fn a_nested_call_with_a_schema_mismatch_ends_as_an_error_with_the_diagnostic() {
    let (storage, session, _publications) = open_test_session();
    let conversation_id = create_conversation(&session).await;
    let parent = support::create_task(
        &session,
        &kiana_durable::harness::generation::GENERATION_TASK,
        conversation_id,
        None,
    )
    .await
    .expect("the caller task exists");
    let call = ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: "call-1".to_owned(),
        name: "echo".to_owned(),
        arguments: Default::default(),
        ..Default::default()
    };
    let input = ToolTaskInput::Nested {
        parent,
        parent_call_id: "parent-call".to_owned(),
        key: "k".to_owned(),
        call,
        progress: None,
    };

    let record = record_with(conversation_id, input);
    let target = DocTarget::task_family(&NESTED_CALL_DOC, parent, "k", JsonValue::Null);
    let target_for_commit = target.clone();
    let task_id = record.id;
    commit(&session, move |tx| {
        Box::pin(async move {
            let mut state = tx.doc(&target_for_commit).await?;
            state
                .value_mut()
                .insert("taskId".to_owned(), JsonValue::from(task_id.get()));
            Ok(())
        })
    })
    .await
    .expect("nested call state");

    let runtime = build_runtime(
        session,
        storage.clone() as Arc<dyn kiana_durable::Storage>,
        &record,
        structured_tool(serde_json::json!({ "a": 1 })),
    );
    CallPhase
        .run(&record, &runtime, ctx())
        .await
        .expect("the nested call settles");
    let state = runtime
        .snapshot(&target, ctx())
        .await
        .expect("nested state reads")
        .expect("nested state exists");
    let result = state.get("result").expect("result recorded");
    assert_eq!(result.get("isError"), Some(&JsonValue::Bool(true)));
    assert_eq!(result.get("structuredOutput"), None);
    let diagnostics = result
        .get("diagnostics")
        .and_then(JsonValue::as_array)
        .expect("diagnostics");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.get("code").and_then(JsonValue::as_str) == Some("invalid_structured_output")
                && diagnostic
                    .get("message")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|message| {
                        message.starts_with(
                        "Tool echo returned structuredOutput that does not match its schema: a: "
                    )
                    })
        }),
        "{diagnostics:?}"
    );
    assert!(
        runtime.reports().is_empty(),
        "a nested call reports into its result"
    );
    assert!(matches!(
        runtime.task_record().state.outcome(),
        Some(TaskOutcome::Completed { .. })
    ));
}
