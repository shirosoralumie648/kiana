//! `core/extensions/wrapper.ts`（设计文档 §4.7、`ca-extensions.md` §2.5）。
//!
//! TS 侧没有独立的 `wrapper.test.ts`；这些断言来自三处既有行为：
//! - `extensions-runner.test.ts` 的 “context creation”（工具上下文与事件处理器共享语义）；
//! - `agent-session-tool-orchestration` 的“扩展工具字段投影”（`wrapToolDefinition` 只把
//!   `AgentTool` 需要的 8 个字段投影过去，其余字段留在 `ToolDefinition` 上）；
//! - 设计文档 §4.7：**每次执行都调用 ctx 工厂**，同一次调用内共享、不同调用之间不共享。
#![cfg(feature = "runtime")]

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use kiana_agent_core::{
    AgentTool, AgentToolCallOutcome, AgentToolError, AgentToolExecute, AgentToolResult,
    ToolExecutionMode,
};
use kiana_ai::types::{JsonValue, TextContent, ToolCall, UserContentBlock};
use kiana_config::models::registry::ModelRegistry;
use kiana_ext_api::ToolDefinition;
use kiana_ext_api::tool_types::ToolDefinitionExecute;
use kiana_ext_api::types::{
    ExtensionActions, ExtensionContextActions, ExtensionToolContextApi, RegisteredTool,
};
use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::runner::ExtensionRunner;
use kiana_extensions::runner::{context, dispatch};
use kiana_extensions::{wrap_registered_tool, wrap_registered_tools};
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};
use serde_json::json;

// ===========================================================================
// harness
// ===========================================================================

#[derive(Default)]
struct FakeSessionManager {
    cwd: String,
}

impl ReadonlySessionManager for FakeSessionManager {
    fn get_cwd(&self) -> &str {
        &self.cwd
    }
    fn get_session_dir(&self) -> &str {
        ""
    }
    fn get_session_id(&self) -> &str {
        "fake"
    }
    fn get_session_file(&self) -> Option<&str> {
        None
    }
    fn get_leaf_id(&self) -> Option<&str> {
        None
    }
    fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        None
    }
    fn get_entry(&self, _id: &str) -> Option<&SessionEntry> {
        None
    }
    fn get_label(&self, _id: &str) -> Option<&str> {
        None
    }
    fn get_branch(&self, _from_id: Option<&str>) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn build_session_projection(&self) -> SessionProjection {
        SessionProjection {
            entries: Vec::new(),
            messages: Vec::new(),
            thinking_level: String::new(),
            model: None,
        }
    }
    fn get_header(&self) -> Option<&SessionHeader> {
        None
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

fn stub_actions() -> ExtensionActions {
    ExtensionActions {
        send_message: Arc::new(|_| {}),
        send_user_message: Arc::new(|_, _| {}),
        append_entry: Arc::new(|_, _| {}),
        set_session_name: Arc::new(|_| {}),
        get_session_name: Arc::new(|| None),
        set_label: Arc::new(|_, _| {}),
        get_active_tools: Arc::new(Vec::new),
        get_all_tools: Arc::new(Vec::new),
        get_settings: Arc::new(Default::default),
        set_active_tools: Arc::new(|_| {}),
        refresh_tools: Arc::new(|| {}),
        get_commands: Arc::new(Vec::new),
        set_model: Arc::new(|_| Box::pin(async { false })),
        get_thinking_level: Arc::new(|| kiana_agent_core::ThinkingLevel::Off),
        set_thinking_level: Arc::new(|_| {}),
    }
}

fn empty_context_actions() -> ExtensionContextActions {
    ExtensionContextActions {
        get_model: Arc::new(|| None),
        get_scoped_models: Arc::new(Vec::new),
        is_idle: Arc::new(|| true),
        is_project_trusted: Arc::new(|| true),
        get_signal: Arc::new(|| None),
        abort: Arc::new(|| {}),
        has_pending_messages: Arc::new(|| false),
        shutdown: Arc::new(|| {}),
        get_context_usage: Arc::new(|| None),
        compact: Arc::new(|_| {}),
        get_system_prompt: Arc::new(String::new),
        get_system_prompt_options: None,
        execute_tool: None,
        get_callable_tools: None,
    }
}

/// 建一个可用 `create_tool_context` 的 runner（真实引擎 + context.js + dispatch.js）。
async fn make_runner(
    context_actions: ExtensionContextActions,
) -> (Rc<JsEngine>, Arc<ExtensionRunner>) {
    let engine = Rc::new(JsEngine::new().expect("create engine"));
    engine::install(&engine).expect("install prelude");
    context::install(&engine).expect("install context.js");
    dispatch::install(Rc::clone(&engine)).expect("install dispatch.js");

    use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
        .await
        .expect("model runtime");
    let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager {
        cwd: "/tmp/wrapper".to_owned(),
    });
    let runner = Arc::new(ExtensionRunner::new(
        Vec::new(),
        kiana_extensions::loader::create_extension_runtime(),
        PathBuf::from("/tmp/wrapper"),
        session,
        Arc::new(ModelRegistry::new(Arc::new(model_runtime))),
    ));
    runner.bind_core(stub_actions(), context_actions, None);
    (engine, runner)
}

fn text_result(text: &str) -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: Default::default(),
            text: text.to_owned(),
            text_signature: None,
        })],
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn registered_tool(definition: ToolDefinition) -> RegisteredTool {
    RegisteredTool {
        definition,
        source_info: kiana_core::source_info::SourceInfo {
            path: "/ext/tools.ts".to_owned(),
            source: "local".to_owned(),
            scope: kiana_core::source_info::SourceScope::Project,
            origin: kiana_core::source_info::SourceOrigin::TopLevel,
            base_dir: None,
        },
    }
}

/// 记录每次执行拿到的上下文快照。
#[derive(Clone, Default)]
struct Executions(Arc<Mutex<Vec<ExecutionRecord>>>);

#[derive(Clone, Debug)]
struct ExecutionRecord {
    tool_call_id: String,
    ctx_ptr: usize,
    callable_tools: Vec<String>,
    nested_result: Option<String>,
}

struct RecordingExecute {
    records: Executions,
    /// 嵌套调用 `ctx.executeTool("nested", …)` 的结果文本。
    run_nested: bool,
}

impl ToolDefinitionExecute for RecordingExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let records = self.records.clone();
        let run_nested = self.run_nested;
        Box::pin(async move {
            let callable_tools = ctx.tools().into_iter().map(|tool| tool.name).collect();
            let nested_result = if run_nested {
                let outcome = ctx
                    .execute_tool("nested", json!({ "answer": 42 }), None)
                    .await;
                outcome
                    .result
                    .content
                    .iter()
                    .map(|block| match block {
                        UserContentBlock::Text(text) => text.text.clone(),
                        UserContentBlock::Image(_) => "<image>".to_owned(),
                    })
                    .next()
            } else {
                None
            };
            records.0.lock().unwrap().push(ExecutionRecord {
                tool_call_id: tool_call_id.to_owned(),
                ctx_ptr: Arc::as_ptr(&ctx) as *const () as usize,
                callable_tools,
                nested_result,
            });
            Ok(text_result("executed"))
        })
    }
}

fn full_definition(execute: Arc<dyn ToolDefinitionExecute>) -> ToolDefinition {
    ToolDefinition {
        name: "wrapped_tool".to_owned(),
        label: "Wrapped Tool".to_owned(),
        description: "A wrapped extension tool".to_owned(),
        prompt_snippet: Some("Use for wrapping".to_owned()),
        prompt_guidelines: Some(vec!["guideline".to_owned()]),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: Some(Arc::new(|args: &mut JsonValue| {
            if let Some(object) = args.as_object_mut() {
                object.insert("prepared".to_owned(), json!(true));
            }
            None
        })),
        output_schema: Some(json!({ "type": "object" })),
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
        execute,
        render_call: None,
        render_result: None,
    }
}

fn callable_tool(name: &str) -> AgentTool {
    AgentTool {
        name: name.to_owned(),
        label: name.to_owned(),
        description: String::new(),
        parameters: json!({ "type": "object" }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(NeverExecute),
    }
}

struct NeverExecute;

impl AgentToolExecute for NeverExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async {
            Err(AgentToolError {
                message: "must not execute".to_owned(),
                name: None,
                stack: None,
            })
        })
    }
}

async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

// ===========================================================================
// 测试
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn wrap_registered_tool_projects_every_agent_tool_field() {
    let (records, runner) = run_local(async {
        let records = Executions::default();
        let (_engine, runner) = make_runner(empty_context_actions()).await;
        (records, runner)
    })
    .await;

    let registered = registered_tool(full_definition(Arc::new(RecordingExecute {
        records,
        run_nested: false,
    })));
    let tool = wrap_registered_tool(&registered, &runner);

    // `wrapToolDefinition` 只投影 `AgentTool` 需要的字段（TS 同名适配器）。
    assert_eq!(tool.name, "wrapped_tool");
    assert_eq!(tool.label, "Wrapped Tool");
    assert_eq!(tool.description, "A wrapped extension tool");
    assert_eq!(
        tool.parameters,
        json!({ "type": "object", "properties": {} })
    );
    assert_eq!(tool.output_schema, Some(json!({ "type": "object" })));
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
    assert!(tool.prepare_arguments.is_some());

    // prepareArguments 原样转发（就地改写 + 返回 None）。
    let mut args = json!({});
    let returned = (tool.prepare_arguments.as_ref().unwrap())(&mut args);
    assert!(returned.is_none());
    assert_eq!(args, json!({ "prepared": true }));
}

#[tokio::test(flavor = "current_thread")]
async fn executes_the_registered_definition_with_a_tool_context() {
    run_local(async {
        let records = Executions::default();
        let execute = Arc::new(RecordingExecute {
            records: records.clone(),
            run_nested: true,
        });
        let (_engine, runner) = make_runner(ExtensionContextActions {
            execute_tool: Some(Arc::new(|_tool_call_id, name, args, _options| {
                let name = name.clone();
                let args = args.clone();
                Box::pin(async move {
                    assert_eq!(name, "nested");
                    assert_eq!(args, json!({ "answer": 42 }));
                    AgentToolCallOutcome {
                        tool_call: ToolCall {
                            id: "nested-1".to_owned(),
                            name,
                            arguments: Default::default(),
                            ..Default::default()
                        },
                        result: text_result("nested-ok"),
                        is_error: false,
                        duration_ms: None,
                    }
                })
            })),
            get_callable_tools: Some(Arc::new(|| {
                vec![callable_tool("read"), callable_tool("bash")]
            })),
            ..empty_context_actions()
        })
        .await;

        let registered = registered_tool(full_definition(execute));
        let tool = wrap_registered_tool(&registered, &runner);

        let result = tool
            .execute
            .execute("call-1", json!({}), None, None)
            .await
            .expect("tool executes");
        assert_eq!(
            result.content,
            text_result("executed").content,
            "the registered definition's execute must run"
        );

        let records = records.0.lock().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool_call_id, "call-1");
        // `ctx.tools()` 来自绑定的 `get_callable_tools`（工具上下文才有）。
        assert_eq!(records[0].callable_tools, vec!["read", "bash"]);
        // `ctx.executeTool()` 来自绑定的 `execute_tool`。
        assert_eq!(records[0].nested_result.as_deref(), Some("nested-ok"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn creates_a_new_context_for_every_execution() {
    run_local(async {
        let records = Executions::default();
        let execute = Arc::new(RecordingExecute {
            records: records.clone(),
            run_nested: false,
        });
        let (_engine, runner) = make_runner(empty_context_actions()).await;
        let registered = registered_tool(full_definition(execute));
        let tool = wrap_registered_tool(&registered, &runner);

        tool.execute
            .execute("call-1", json!({}), None, None)
            .await
            .expect("first execution");
        tool.execute
            .execute("call-2", json!({}), None, None)
            .await
            .expect("second execution");

        let records = records.0.lock().unwrap().clone();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].tool_call_id, "call-1");
        assert_eq!(records[1].tool_call_id, "call-2");
        // 设计文档 §4.7：每次执行都新建上下文，不缓存、不复用。
        assert_ne!(
            records[0].ctx_ptr, records[1].ctx_ptr,
            "each execution must create a fresh tool context"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn wrap_registered_tools_maps_in_order() {
    run_local(async {
        let (_engine, runner) = make_runner(empty_context_actions()).await;
        let tools = vec![
            registered_tool(ToolDefinition {
                name: "alpha".to_owned(),
                label: "Alpha".to_owned(),
                ..full_definition(Arc::new(RecordingExecute {
                    records: Executions::default(),
                    run_nested: false,
                }))
            }),
            registered_tool(ToolDefinition {
                name: "beta".to_owned(),
                label: "Beta".to_owned(),
                ..full_definition(Arc::new(RecordingExecute {
                    records: Executions::default(),
                    run_nested: false,
                }))
            }),
        ];

        let wrapped = wrap_registered_tools(&tools, &runner);
        assert_eq!(
            wrapped
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert_eq!(
            wrapped
                .iter()
                .map(|tool| tool.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Beta"]
        );
    })
    .await;
}
