//! `definition.rs` 的测试：镜像 `builtin-tool-strict-mode.test.ts` 的
//! 「wrap preserves constrainedSampling / prepareArguments」与
//! `createToolDefinitionFromAgentTool` 丢弃渲染器/prompt 元数据的语义。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_agent_core::{
    AgentTool, AgentToolError, AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode,
};
use kiana_ai::types::{ConstrainedSampling, JsonValue};
use kiana_ext_api::tool_types::{
    PrepareArgumentsFn, RenderCallFn, RenderShell, ToolDefinition, ToolDefinitionExecute,
};
use kiana_ext_api::types::ExtensionToolContextApi;
use kiana_tools::definition::{
    ToolContextFactory, create_tool_definition_from_agent_tool, is_missing_tool_context,
    missing_tool_context, wrap_tool_definition, wrap_tool_definitions,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn ok_result() -> AgentToolResult<JsonValue> {
    AgentToolResult {
        content: Vec::new(),
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

struct ProbeExecute {
    contexts_missing: Arc<Mutex<Vec<bool>>>,
    calls: Arc<AtomicUsize>,
}

impl ToolDefinitionExecute for ProbeExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.contexts_missing
            .lock()
            .expect("lock")
            .push(is_missing_tool_context(&ctx));
        Box::pin(async { Ok(ok_result()) })
    }
}

fn probe_definition(
    contexts_missing: Arc<Mutex<Vec<bool>>>,
    calls: Arc<AtomicUsize>,
) -> ToolDefinition {
    ToolDefinition {
        name: "probe".to_string(),
        label: "probe".to_string(),
        description: "probe tool".to_string(),
        prompt_snippet: Some("snippet".to_string()),
        prompt_guidelines: Some(vec!["guideline".to_string()]),
        parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        constrained_sampling: Some(ConstrainedSampling::Disabled(false)),
        render_shell: Some(RenderShell::SelfRender),
        prepare_arguments: Some(Arc::new(|_args: &mut JsonValue| None) as PrepareArgumentsFn),
        output_schema: Some(json!({"type":"string"})),
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
        execute: Arc::new(ProbeExecute {
            contexts_missing,
            calls,
        }),
        render_call: Some(Arc::new(
            |_args: &JsonValue,
             _theme: &kiana_theme::theme::Theme,
             _ctx: &kiana_ext_api::tool_types::ToolRenderContext| {
                Box::new(kiana_tui::Text::with_padding("", 0, 0))
                    as Box<dyn kiana_tui::tui::Component>
            },
        ) as RenderCallFn),
        render_result: None,
    }
}

#[tokio::test]
async fn wrap_projects_fields_and_preserves_explicit_opt_outs() {
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let definition = probe_definition(Arc::clone(&contexts), Arc::clone(&calls));
    let prepare = definition.prepare_arguments.clone();
    let parameters = definition.parameters.clone();

    let tool = wrap_tool_definition(definition, None);
    assert_eq!(tool.name, "probe");
    assert_eq!(tool.label, "probe");
    assert_eq!(tool.description, "probe tool");
    assert_eq!(tool.parameters, parameters);
    assert_eq!(tool.output_schema, Some(json!({"type":"string"})));
    assert_eq!(
        tool.constrained_sampling,
        Some(ConstrainedSampling::Disabled(false))
    );
    assert!(match (&tool.prepare_arguments, &prepare) {
        (Some(actual), Some(expected)) => Arc::ptr_eq(actual, expected),
        (None, None) => true,
        _ => false,
    });
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));

    // 没有工厂时使用占位上下文（等价 TS 的 ctx === undefined）。
    let result = tool
        .execute
        .execute("call-1", json!({"path":"x"}), None, None)
        .await
        .expect("execute");
    assert_eq!(result.details, JsonValue::Null);
    assert_eq!(*contexts.lock().expect("lock"), vec![true]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wrap_uses_the_context_factory_when_present() {
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let definition = probe_definition(Arc::clone(&contexts), Arc::clone(&calls));
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&factory_calls);
    let factory: ToolContextFactory = Arc::new(
        move |_tool_call_id: &str, _signal: Option<&CancellationToken>| {
            counter.fetch_add(1, Ordering::SeqCst);
            missing_tool_context()
        },
    );

    let tool = wrap_tool_definition(definition, Some(factory));
    tool.execute
        .execute("call-1", JsonValue::Null, None, None)
        .await
        .expect("execute");
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
    // 工厂返回的占位上下文仍是原样传入的。
    assert_eq!(*contexts.lock().expect("lock"), vec![true]);
}

#[test]
fn wraps_multiple_definitions() {
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let definitions = vec![
        probe_definition(Arc::clone(&contexts), Arc::clone(&calls)),
        probe_definition(Arc::clone(&contexts), Arc::clone(&calls)),
    ];
    let tools = wrap_tool_definitions(definitions, None);
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "probe");
    assert_eq!(tools[1].name, "probe");
}

#[tokio::test]
async fn create_tool_definition_from_agent_tool_drops_prompt_and_renderers() {
    let contexts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let definition = probe_definition(Arc::clone(&contexts), Arc::clone(&calls));
    let tool: AgentTool = wrap_tool_definition(definition, None);

    let synthesized = create_tool_definition_from_agent_tool(tool);
    assert_eq!(synthesized.name, "probe");
    assert_eq!(synthesized.label, "probe");
    assert_eq!(synthesized.description, "probe tool");
    assert_eq!(
        synthesized.constrained_sampling,
        Some(ConstrainedSampling::Disabled(false))
    );
    assert_eq!(
        synthesized.execution_mode,
        Some(ToolExecutionMode::Sequential)
    );
    // 语义要求：渲染器与 prompt 元数据被丢弃。
    assert!(synthesized.prompt_snippet.is_none());
    assert!(synthesized.prompt_guidelines.is_none());
    assert!(synthesized.render_call.is_none());
    assert!(synthesized.render_result.is_none());
    assert!(synthesized.render_shell.is_none());

    // 适配器忽略上下文并直接执行 AgentTool。
    let result = synthesized
        .execute
        .execute(
            "call-2",
            JsonValue::Null,
            None,
            None,
            missing_tool_context(),
        )
        .await
        .expect("execute");
    assert_eq!(result.details, JsonValue::Null);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_context_model_registry_does_not_panic() {
    // 回归：占位上下文的 model_registry() 以前会 panic（公开路径不允许）。TS 在
    // `ctx === undefined` 时抛 TypeError，但冻结签名 `-> &ModelRegistry` 没有错误通道，
    // 因此返回一个惰性构造的、不联网/不读 models.json 的注册表。
    let context = missing_tool_context();
    let registry = context.model_registry();
    let _ = registry.get_all();
    // 再次调用仍是同一进程级实例（OnceLock）。
    assert!(std::ptr::eq(context.model_registry(), registry));
}

#[test]
fn missing_context_identity_is_stable() {
    let first = missing_tool_context();
    let second = missing_tool_context();
    assert!(is_missing_tool_context(&first));
    assert!(std::ptr::eq(Arc::as_ptr(&first), Arc::as_ptr(&second)));
}
