//! Work envelope Phase 0 端到端测试。
//!
//! 用 faux provider 走真实的 `AgentSession::spawn_subagent_with_work`：
//! 子代理调用一次 shell 工具，`tests_passed` 验收从运行时执行记录判定为 `Passed`，
//! 同时验证 WorkEvent 顺序、MemoryWorkStore 持久化与 WorkResult 终态。

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, BoxFuture,
};
use kiana_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use kiana_ai::types::{JsonObject, JsonValue, StopReason, TextContent, TextKind, UserContentBlock};
use kiana_session::acceptance::AcceptanceStatus;
use kiana_session::agent_definition::builtin_agent_definitions;
use kiana_session::subagent::{
    MemoryWorkStore, RunId, SubagentRuntimeOptions, WorkAcceptancePolicy, WorkClock, WorkEvent,
    WorkEventKind, WorkEventSink, WorkId, WorkOptions, WorkPlanId,
};
use serde_json::json;

struct PassingShellExecute;

impl AgentToolExecute for PassingShellExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "test result: ok. 1 passed; 0 failed; 0 ignored".to_string(),
                    text_signature: None,
                })],
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn shell_tool() -> AgentTool {
    AgentTool {
        name: "bash".to_string(),
        label: "Bash".to_string(),
        description: "run a shell command".to_string(),
        parameters: json!({"type": "object"}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(PassingShellExecute),
    }
}

fn tool_call_message(name: &str, arguments: JsonValue) -> FauxResponseStep {
    let arguments: JsonObject = serde_json::from_value(arguments).expect("object");
    FauxResponseStep::Message(faux_assistant_message(
        faux_tool_call(name, arguments),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
}

fn text_message(text: &str) -> FauxResponseStep {
    FauxResponseStep::Message(faux_assistant_message(text, Default::default()))
}

#[derive(Default)]
struct CollectingSink {
    events: Mutex<Vec<WorkEvent>>,
}

impl WorkEventSink for CollectingSink {
    fn emit(&self, event: &WorkEvent) {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event.clone());
    }
}

struct FixedClock(u64);

impl WorkClock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[tokio::test]
async fn spawn_subagent_with_work_end_to_end() {
    run(async {
        let harness = create_harness(HarnessOptions {
            tools: Some(vec![shell_tool()]),
            ..Default::default()
        })
        .await;
        harness.set_responses(vec![
            tool_call_message("bash", json!({"command": "cargo test"})),
            text_message("done"),
        ]);

        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.name == "general-purpose")
            .expect("builtin general-purpose");
        let sink = Arc::new(CollectingSink::default());
        let store = Arc::new(MemoryWorkStore::new());
        let work_options = WorkOptions {
            work_id: Some(WorkId(7)),
            run_id: Some(RunId::new("run-7")),
            acceptance_policy: WorkAcceptancePolicy::RequirePassed,
            plan_id: Some(WorkPlanId::new("plan-7")),
            event_sink: Some(sink.clone()),
            store: Some(store.clone()),
            clock: Some(Arc::new(FixedClock(1_700_000_000_000))),
            ..Default::default()
        };
        let runtime_options = SubagentRuntimeOptions {
            acceptance_criteria: vec!["tests_passed:cargo test".to_string()],
            ..Default::default()
        };

        let work = harness
            .session
            .spawn_subagent_with_work(
                definition,
                runtime_options,
                "run the tests",
                None,
                work_options,
            )
            .await
            .expect("work run");

        assert_eq!(work.result.work_id, WorkId(7));
        assert_eq!(
            work.result.run_id.as_ref().map(|id| id.0.as_str()),
            Some("run-7")
        );
        assert_eq!(work.result.terminal.status(), "success");
        assert_eq!(work.result.started_at_ms, 1_700_000_000_000);
        assert_eq!(work.result.finished_at_ms, 1_700_000_000_000);
        let acceptance = work.result.acceptance.as_ref().expect("acceptance");
        assert_eq!(acceptance.report.status, AcceptanceStatus::Passed);
        assert_eq!(work.result.evidence.len(), 1);
        assert_eq!(work.result.evidence[0].tool, "bash");
        assert!(work.store_error.is_none());
        assert_eq!(store.records().await.len(), 1);

        let events = sink
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(matches!(events[0].event, WorkEventKind::Created));
        assert!(matches!(events[1].event, WorkEventKind::Started { .. }));
        assert!(matches!(
            events[2].event,
            WorkEventKind::AcceptanceEvaluated { .. }
        ));
        assert!(matches!(events[3].event, WorkEventKind::Terminal { .. }));
        assert!(
            events.iter().all(|event| event
                .plan_id
                .as_ref()
                .is_some_and(|plan| plan.0 == "plan-7")),
            "every event must carry the Work's planId: {events:?}"
        );

        harness.cleanup();
    })
    .await;
}
