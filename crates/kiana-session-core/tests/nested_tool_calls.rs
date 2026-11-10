//! TS: `test/nested-tool-calls.test.ts`（6 例）。
//!
//! host 是测试替身：`runToolCall` 通过名字查表执行异步行为，行为通过同步 sink 上报 partial
//! 结果，host 在返回前逐个转交给 runner 的 `onUpdate`（与 TS host 调用 `tool.execute` 的
//! 可观察行为一致）。

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::*;
use kiana_agent_core::types::{
    AgentToolCall, AgentToolCallOutcome, AgentToolResult, BoxFuture, ToolExecutionMode,
};
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolExecute, AgentToolUpdateCallback};
use kiana_ai::types::{JsonValue, ToolCallKind, Usage, UserContentBlock};
use kiana_session_core::nested_tool_calls::{
    LocalFuture, NESTED_CALL_LIMITS, NestedCallRecorder, NestedToolCallHost, NestedToolCallOptions,
    NestedToolCallRunner, NestedToolExecutionEvent,
};

type UpdateSink = Arc<dyn Fn(AgentToolResult<JsonValue>) + Send + Sync>;
type Behavior = Arc<
    dyn Fn(AgentToolCall, UpdateSink) -> LocalFuture<'static, AgentToolResult<JsonValue>>
        + Send
        + Sync,
>;

/// 实际 `execute` 不被调用（行为查表代替）；满足 `AgentTool.execute` 的形状。
struct NoopExecute;

impl AgentToolExecute for NoopExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async { Ok(empty_result()) })
    }
}

fn empty_result() -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: Vec::new(),
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn text_result(text: &str) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(text_block(text))],
        ..empty_result()
    }
}

fn agent_tool(name: &str, sequential: bool) -> AgentTool {
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: name.to_string(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: sequential.then_some(ToolExecutionMode::Sequential),
        replay: None,
        execute: Arc::new(NoopExecute),
    }
}

struct TestHost {
    tools: Vec<AgentTool>,
    behaviors: HashMap<String, Behavior>,
    sequential: bool,
    events: Arc<Mutex<Vec<NestedToolExecutionEvent>>>,
}

impl TestHost {
    fn emit_now(&self, event: NestedToolExecutionEvent) {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
    }
}

impl NestedToolCallHost for TestHost {
    fn get_tools(&self) -> &[AgentTool] {
        &self.tools
    }

    fn is_sequential(&self) -> bool {
        self.sequential
    }

    fn run_tool_call<'a, F>(
        &'a self,
        tool_call: AgentToolCall,
        parent_tool_call_id: &'a str,
        _signal: Option<&'a kiana_ai::utils::abort::AbortScope>,
        on_update: F,
    ) -> LocalFuture<'a, AgentToolCallOutcome>
    where
        F: Fn(AgentToolResult<JsonValue>) -> LocalFuture<'a, ()> + Send + 'a,
    {
        let behavior = self.behaviors.get(&tool_call.name).cloned();
        Box::pin(async move {
            let Some(behavior) = behavior else {
                return AgentToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result: text_result(&format!("Tool {} not found", tool_call.name)),
                    is_error: true,
                    duration_ms: None,
                };
            };
            let partials: Arc<Mutex<Vec<AgentToolResult<JsonValue>>>> =
                Arc::new(Mutex::new(Vec::new()));
            let sink: UpdateSink = {
                let partials = Arc::clone(&partials);
                Arc::new(move |partial| {
                    partials
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(partial);
                })
            };
            let result = behavior(tool_call.clone(), sink).await;
            let pending: Vec<AgentToolResult<JsonValue>> = {
                let mut partials = partials.lock().unwrap_or_else(|error| error.into_inner());
                std::mem::take(&mut *partials)
            };
            for partial in pending {
                on_update(partial).await;
            }
            let is_error = result.is_error.unwrap_or(false);
            let _ = parent_tool_call_id;
            AgentToolCallOutcome {
                tool_call,
                result,
                is_error,
                duration_ms: None,
            }
        })
    }

    fn emit<'a>(&'a self, event: NestedToolExecutionEvent) -> LocalFuture<'a, ()> {
        Box::pin(async move {
            self.emit_now(event);
        })
    }
}

fn create_runner(
    tools: Vec<AgentTool>,
    behaviors: Vec<(&str, Behavior)>,
    sequential: bool,
) -> (
    Arc<NestedToolCallRunner<TestHost>>,
    Arc<Mutex<Vec<NestedToolExecutionEvent>>>,
) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let host = TestHost {
        tools,
        behaviors: behaviors
            .into_iter()
            .map(|(name, behavior)| (name.to_string(), behavior))
            .collect(),
        sequential,
        events: Arc::clone(&events),
    };
    (Arc::new(NestedToolCallRunner::new(host)), events)
}

fn behavior<F>(function: F) -> Behavior
where
    F: Fn(AgentToolCall, UpdateSink) -> LocalFuture<'static, AgentToolResult<JsonValue>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(function)
}

fn usage(input: u64, cost: f64) -> Usage {
    Usage {
        input,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input,
        cost: kiana_ai::types::UsageCost {
            input: cost,
            total: cost,
            ..Default::default()
        },
    }
}

fn event_summary(event: &NestedToolExecutionEvent) -> (&'static str, String, String) {
    match event {
        NestedToolExecutionEvent::Start {
            tool_call_id,
            parent_tool_call_id,
            ..
        } => (
            "tool_execution_start",
            tool_call_id.clone(),
            parent_tool_call_id.clone(),
        ),
        NestedToolExecutionEvent::Update {
            tool_call_id,
            parent_tool_call_id,
            ..
        } => (
            "tool_execution_update",
            tool_call_id.clone(),
            parent_tool_call_id.clone(),
        ),
        NestedToolExecutionEvent::End {
            tool_call_id,
            parent_tool_call_id,
            ..
        } => (
            "tool_execution_end",
            tool_call_id.clone(),
            parent_tool_call_id.clone(),
        ),
    }
}

fn record_json(calls: &kiana_ai::types::NestedToolCalls) -> &Vec<JsonValue> {
    &calls.calls
}

mod nested_tool_call_runner {
    use super::*;

    /// TS: "assigns ids below the caller, emits events with the parent id, and records the calls"
    #[tokio::test]
    async fn assigns_ids_below_the_caller_emits_events_with_the_parent_id_and_records_the_calls() {
        let echo = behavior(|_tool_call, update| {
            Box::pin(async move {
                update(text_result("partial"));
                text_result("ok")
            })
        });
        let (runner, events) =
            create_runner(vec![agent_tool("echo", false)], vec![("echo", echo)], false);
        let updates: Arc<Mutex<Vec<AgentToolResult<JsonValue>>>> = Arc::new(Mutex::new(Vec::new()));

        let updates_sink = Arc::clone(&updates);
        let first = runner
            .execute(
                "call",
                "echo",
                serde_json::json!({"a": 1}),
                NestedToolCallOptions {
                    signal: None,
                    on_update: Some(Arc::new(move |partial| {
                        updates_sink
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(partial);
                    })),
                },
            )
            .await;
        let missing = runner
            .execute(
                "call",
                "missing",
                serde_json::json!({}),
                NestedToolCallOptions::default(),
            )
            .await;

        assert_eq!(first.tool_call.id, "call/1");
        assert_eq!(missing.tool_call.id, "call/2");
        assert!(missing.is_error);
        assert_eq!(updates.lock().unwrap().len(), 1);

        let summaries: Vec<(&'static str, String, String)> = events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(event_summary)
            .collect();
        assert_eq!(
            summaries,
            [
                (
                    "tool_execution_start",
                    "call/1".to_string(),
                    "call".to_string()
                ),
                (
                    "tool_execution_update",
                    "call/1".to_string(),
                    "call".to_string()
                ),
                (
                    "tool_execution_end",
                    "call/1".to_string(),
                    "call".to_string()
                ),
                (
                    "tool_execution_start",
                    "call/2".to_string(),
                    "call".to_string()
                ),
                (
                    "tool_execution_end",
                    "call/2".to_string(),
                    "call".to_string()
                ),
            ]
        );

        let summary = runner.take_record("call").expect("record");
        let calls = summary.calls.expect("nested calls");
        assert!(calls.complete);
        let records = record_json(&calls);
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].get("id").and_then(JsonValue::as_str),
            Some("call/1")
        );
        assert_eq!(
            records[0].get("name").and_then(JsonValue::as_str),
            Some("echo")
        );
        assert_eq!(
            records[0].get("arguments"),
            Some(&serde_json::json!({"a": 1}))
        );
        assert_eq!(
            records[0].get("status").and_then(JsonValue::as_str),
            Some("ok")
        );
        assert!(
            records[0]
                .get("durationMs")
                .and_then(JsonValue::as_f64)
                .is_some()
        );
        assert_eq!(
            records[1].get("id").and_then(JsonValue::as_str),
            Some("call/2")
        );
        assert_eq!(
            records[1].get("name").and_then(JsonValue::as_str),
            Some("missing")
        );
        assert_eq!(records[1].get("arguments"), Some(&serde_json::json!({})));
        assert_eq!(
            records[1].get("status").and_then(JsonValue::as_str),
            Some("error")
        );
        assert!(
            records[1]
                .get("durationMs")
                .and_then(JsonValue::as_f64)
                .is_some()
        );
        assert_eq!(
            records[1].get("error").and_then(JsonValue::as_str),
            Some("Tool missing not found")
        );

        // 记录只能取一次。
        assert!(runner.take_record("call").is_none());
        assert!(runner.take_record("other").is_none());
    }

    /// TS: "records calls of nested tools on the model-issued call"
    #[tokio::test]
    async fn records_calls_of_nested_tools_on_the_model_issued_call() {
        let slot: Arc<OnceLock<Arc<NestedToolCallRunner<TestHost>>>> = Arc::new(OnceLock::new());
        let slot_for_middle = Arc::clone(&slot);
        let middle = behavior(move |tool_call, _update| {
            let slot = Arc::clone(&slot_for_middle);
            Box::pin(async move {
                let runner = slot.get().expect("runner installed");
                runner
                    .execute(
                        &tool_call.id,
                        "leaf",
                        serde_json::json!({}),
                        NestedToolCallOptions::default(),
                    )
                    .await;
                empty_result()
            })
        });
        let leaf = behavior(|_tool_call, _update| Box::pin(async { empty_result() }));
        let (runner, _events) = create_runner(
            vec![agent_tool("leaf", false)],
            vec![("leaf", leaf), ("middle", middle)],
            false,
        );
        if slot.set(Arc::clone(&runner)).is_err() {
            panic!("slot already initialized");
        }

        runner
            .execute(
                "call",
                "middle",
                serde_json::json!({}),
                NestedToolCallOptions::default(),
            )
            .await;

        let summary = runner.take_record("call").expect("record");
        let calls = summary.calls.expect("nested calls");
        let ids: Vec<&str> = record_json(&calls)
            .iter()
            .map(|call| {
                call.get("id")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(ids, ["call/1", "call/1/1"]);
    }

    /// TS: "sums the usage of nested results at every depth"
    #[tokio::test]
    async fn sums_the_usage_of_nested_results_at_every_depth() {
        let leaf = behavior(|_tool_call, _update| {
            Box::pin(async move {
                let mut result = empty_result();
                result.usage = Some(usage(10, 0.01));
                result
            })
        });
        let plain = behavior(|_tool_call, _update| Box::pin(async { empty_result() }));
        let slot: Arc<OnceLock<Arc<NestedToolCallRunner<TestHost>>>> = Arc::new(OnceLock::new());
        let slot_for_middle = Arc::clone(&slot);
        let middle = behavior(move |tool_call, _update| {
            let slot = Arc::clone(&slot_for_middle);
            Box::pin(async move {
                let runner = slot.get().expect("runner installed");
                runner
                    .execute(
                        &tool_call.id,
                        "leaf",
                        serde_json::json!({}),
                        NestedToolCallOptions::default(),
                    )
                    .await;
                // 只算自己的用量：leaf 的用量由 recorder 计一次。
                let mut result = empty_result();
                result.usage = Some(usage(5, 0.005));
                result
            })
        });
        let (runner, _events) = create_runner(
            vec![agent_tool("leaf", false), agent_tool("plain", false)],
            vec![("leaf", leaf), ("plain", plain), ("middle", middle)],
            false,
        );
        if slot.set(Arc::clone(&runner)).is_err() {
            panic!("slot already initialized");
        }

        runner
            .execute(
                "call",
                "middle",
                serde_json::json!({}),
                NestedToolCallOptions::default(),
            )
            .await;
        runner
            .execute(
                "call",
                "leaf",
                serde_json::json!({}),
                NestedToolCallOptions::default(),
            )
            .await;
        runner
            .execute(
                "call",
                "plain",
                serde_json::json!({}),
                NestedToolCallOptions::default(),
            )
            .await;
        runner
            .execute(
                "free",
                "plain",
                serde_json::json!({}),
                NestedToolCallOptions::default(),
            )
            .await;

        let summary = runner.take_record("call").expect("summary");
        let total = summary.usage.expect("usage");
        assert_eq!(total.input, 25);
        let tolerance = 0.5 * 10f64.powi(-10);
        assert!((total.cost.total - 0.025).abs() < tolerance);
        let free = runner.take_record("free").expect("free record");
        assert!(free.calls.expect("calls").complete);
        assert!(free.usage.is_none());
    }

    /// TS: "serializes concurrent calls to sequential tools"
    #[tokio::test]
    async fn serializes_concurrent_calls_to_sequential_tools() {
        let active = Arc::new(Mutex::new(0usize));
        let max_active = Arc::new(Mutex::new((0usize, 0usize)));
        let make_behavior = |sequential: bool| {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            behavior(move |_tool_call, _update| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                Box::pin(async move {
                    {
                        let mut active = active.lock().unwrap_or_else(|error| error.into_inner());
                        *active += 1;
                        let mut max_active =
                            max_active.lock().unwrap_or_else(|error| error.into_inner());
                        let slot = if sequential { 0 } else { 1 };
                        let current = if slot == 0 {
                            &mut max_active.0
                        } else {
                            &mut max_active.1
                        };
                        *current = (*current).max(*active);
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    *active.lock().unwrap_or_else(|error| error.into_inner()) -= 1;
                    empty_result()
                })
            })
        };
        let (runner, _events) = create_runner(
            vec![
                agent_tool("sequential", true),
                agent_tool("parallel", false),
            ],
            vec![
                ("sequential", make_behavior(true)),
                ("parallel", make_behavior(false)),
            ],
            false,
        );

        futures_join3(&runner, "sequential").await;
        futures_join3(&runner, "parallel").await;

        let max = *max_active.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(max, (1, 3));
    }

    async fn futures_join3(runner: &NestedToolCallRunner<TestHost>, name: &str) {
        let options = || NestedToolCallOptions::default();
        tokio::join!(
            runner.execute("call", name, serde_json::json!({}), options()),
            runner.execute("call", name, serde_json::json!({}), options()),
            runner.execute("call", name, serde_json::json!({}), options()),
        );
    }
}

mod nested_call_recorder {
    use super::*;

    fn call(id: &str, arguments: JsonValue) -> AgentToolCall {
        let arguments = match arguments {
            JsonValue::Object(object) => object.into_iter().collect(),
            _ => kiana_ai::types::JsonObject::new(),
        };
        AgentToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: id.to_string(),
            name: "t".to_string(),
            arguments,
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }
    }

    /// TS: "omits oversized arguments and drops calls beyond the limit"
    #[test]
    fn omits_oversized_arguments_and_drops_calls_beyond_the_limit() {
        let mut recorder = NestedCallRecorder::new();
        assert!(recorder.snapshot().is_none());
        let small = recorder.start(&call("a", serde_json::json!({"x": 1})));
        recorder.finish(small, false, "");
        let snapshot = recorder.snapshot().expect("snapshot");
        assert!(snapshot.complete);
        assert_eq!(snapshot.calls.len(), 1);
        let first = &snapshot.calls[0];
        assert_eq!(first.get("id").and_then(JsonValue::as_str), Some("a"));
        assert_eq!(first.get("name").and_then(JsonValue::as_str), Some("t"));
        assert_eq!(first.get("arguments"), Some(&serde_json::json!({"x": 1})));
        assert_eq!(first.get("status").and_then(JsonValue::as_str), Some("ok"));
        assert!(
            first
                .get("durationMs")
                .and_then(JsonValue::as_f64)
                .is_some()
        );

        let big = recorder.start(&call(
            "b",
            serde_json::json!({"text": "x".repeat(NESTED_CALL_LIMITS.max_argument_bytes_per_call)}),
        ));
        recorder.finish(big, true, &"e".repeat(1000));
        let snapshot = recorder.snapshot().expect("snapshot");
        assert!(!snapshot.complete);
        let second = &snapshot.calls[1];
        assert_eq!(second.get("id").and_then(JsonValue::as_str), Some("b"));
        assert_eq!(
            second.get("status").and_then(JsonValue::as_str),
            Some("error")
        );
        assert!(second.get("arguments").is_none());
        assert!(
            second
                .get("argumentsBytes")
                .and_then(JsonValue::as_u64)
                .is_some_and(|bytes| bytes > NESTED_CALL_LIMITS.max_argument_bytes_per_call as u64)
        );
        assert_eq!(
            second
                .get("error")
                .and_then(JsonValue::as_str)
                .map(str::chars)
                .map(|chars| chars.count()),
            Some(NESTED_CALL_LIMITS.max_error_chars)
        );

        for index in 0..NESTED_CALL_LIMITS.max_calls {
            let record = recorder.start(&call(&format!("c{index}"), serde_json::json!({})));
            recorder.finish(record, false, "");
        }
        assert_eq!(
            recorder.snapshot().expect("snapshot").calls.len(),
            NESTED_CALL_LIMITS.max_calls
        );
    }

    /// TS: "caps the total argument size and marks unfinished calls incomplete"
    #[test]
    fn caps_the_total_argument_size_and_marks_unfinished_calls_incomplete() {
        let mut recorder = NestedCallRecorder::new();
        let chunk = serde_json::json!({"text": "x".repeat(7000)});
        let records: Vec<Option<usize>> = (0..6)
            .map(|index| recorder.start(&call(&format!("c{index}"), chunk.clone())))
            .collect();
        let snapshot = recorder.snapshot().expect("snapshot");
        // 32 KiB 能容纳四个 7000 字节的 arguments 对象。
        assert_eq!(
            snapshot
                .calls
                .iter()
                .filter(|entry| entry.get("arguments").is_some())
                .count(),
            4
        );
        assert!(snapshot.calls.iter().all(|entry| {
            entry.get("status").and_then(JsonValue::as_str) == Some("unfinished")
        }));
        assert!(!snapshot.complete);
        assert_eq!(records.len(), 6);
    }
}
