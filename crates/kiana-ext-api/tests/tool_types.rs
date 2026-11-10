//! `tool_types` 模块：`ToolDefinition` 形状、`define_tool` 恒等性、`ToolRenderContext`、
//! 工具名/输入/详情类型、`BashOperations` 对象安全。

use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kiana_agent_core::{
    AgentToolError, AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode,
};
use kiana_ai::types::{ConstrainedSampling, JsonValue};
use kiana_ext_api::BoxFuture;
use kiana_ext_api::events::ExtensionEventKind;
use kiana_ext_api::tool_result::{
    BashExecOptions, BashExecResult, BashOperations, BashResult, ExecOptions, ExecResult,
};
use kiana_ext_api::tool_types::{
    BashToolDetails, BashToolInput, BashToolName, EditToolDetails, EditToolInput, EditToolName,
    FindToolDetails, FindToolInput, FindToolName, GrepToolDetails, GrepToolInput, GrepToolName,
    LsToolDetails, LsToolInput, LsToolName, PowerShellToolDetails, PowerShellToolInput,
    PowerShellToolName, PrepareArgumentsFn, PrepareLoadoutFn, ReachesToolsFn, ReachesToolsResult,
    ReadToolDetails, ReadToolInput, ReadToolName, RenderCallFn, RenderResultFn, RenderShell,
    RenderState, ReplaceEdit, ToolAnnotations, ToolDefinition, ToolDefinitionExecute, ToolExposure,
    ToolLoadoutChanges, ToolNamespace, ToolRenderContext, ToolRenderResultOptions,
    ToolRendererResolver, ToolRenderers, TruncatedBy, TruncationResult, WriteToolInput,
    WriteToolName, define_tool,
};
use kiana_ext_api::types::ExtensionToolContextApi;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// 只用于构造 `ToolDefinition` 的执行入口；测试只检查指针身份，不调用。
struct NoopExecute;

impl ToolDefinitionExecute for NoopExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            Ok(AgentToolResult {
                content: Vec::new(),
                details: JsonValue::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn panic_render_call(
    _args: &JsonValue,
    _theme: &kiana_theme::theme::Theme,
    _context: &ToolRenderContext,
) -> Box<dyn kiana_tui::tui::Component> {
    panic!("测试不渲染")
}

fn panic_render_result(
    _result: &AgentToolResult<JsonValue>,
    _options: &ToolRenderResultOptions,
    _theme: &kiana_theme::theme::Theme,
    _context: &ToolRenderContext,
) -> Box<dyn kiana_tui::tui::Component> {
    panic!("测试不渲染")
}

fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "my_tool".into(),
        label: "My Tool".into(),
        description: "does things".into(),
        prompt_snippet: Some("one line".into()),
        prompt_guidelines: Some(vec!["be nice".into()]),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: Some(ConstrainedSampling::Disabled(false)),
        render_shell: Some(RenderShell::SelfRender),
        prepare_arguments: Some(Arc::new(|params: &mut JsonValue| {
            if params.get("legacy").is_some() {
                Some(json!({ "new": true }))
            } else {
                None
            }
        }) as PrepareArgumentsFn),
        output_schema: Some(json!({ "type": "object" })),
        exposure: Some(ToolExposure::ModelOnly),
        namespace: Some(ToolNamespace {
            name: "ns".into(),
            aliases: None,
            description: Some("group".into()),
            instructions: None,
        }),
        reaches_tools: Some(Arc::new(|_input: &JsonValue| {
            ReachesToolsResult::Names(vec!["other".into()])
        }) as ReachesToolsFn),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            ..Default::default()
        }),
        default_active: Some(true),
        prepare_loadout: Some(
            Arc::new(|_loadout: &dyn kiana_ext_api::tool_types::ToolLoadout| {
                Some(ToolLoadoutChanges {
                    descriptions: None,
                    hidden_declarations: Some(vec!["other".into()]),
                })
            }) as PrepareLoadoutFn,
        ),
        execution_mode: Some(ToolExecutionMode::Sequential),
        execute: Arc::new(NoopExecute),
        render_call: Some(Arc::new(panic_render_call) as RenderCallFn),
        render_result: Some(Arc::new(panic_render_result) as RenderResultFn),
    }
}

#[test]
fn define_tool_returns_the_same_tool() {
    let original = definition();
    let execute = Arc::clone(&original.execute);
    let returned = define_tool(original.clone());
    assert!(
        Arc::ptr_eq(&returned.execute, &execute),
        "define_tool 必须原样返回（不克隆回调）"
    );
    assert!(Arc::ptr_eq(&returned.execute, &original.execute));
    assert_eq!(returned.name, original.name);
    assert_eq!(returned.label, original.label);
    assert_eq!(returned.description, original.description);
    assert_eq!(returned.prompt_snippet, original.prompt_snippet);
    assert_eq!(returned.parameters, original.parameters);
    assert_eq!(returned.exposure, original.exposure);
    assert_eq!(returned.execution_mode, original.execution_mode);
}

#[test]
fn tool_definition_is_send_sync_clone_and_debug() {
    fn assert_traits<T: Send + Sync + Clone + std::fmt::Debug + 'static>() {}
    assert_traits::<ToolDefinition>();

    let definition = definition();
    let cloned = definition.clone();
    assert_eq!(cloned.name, "my_tool");
    let debug = format!("{definition:?}");
    assert!(debug.starts_with("ToolDefinition"));
    assert!(debug.contains("my_tool"));
}

#[test]
fn tool_definition_exposes_all_twenty_fields() {
    // 结构体字面量必须写出全部字段；这里再逐一读取，防止字段被悄悄改名/移除。
    let tool = definition();
    assert_eq!(tool.name, "my_tool");
    assert_eq!(tool.label, "My Tool");
    assert_eq!(tool.description, "does things");
    assert_eq!(tool.prompt_snippet.as_deref(), Some("one line"));
    assert_eq!(
        tool.prompt_guidelines.as_deref(),
        Some(["be nice".to_string()].as_slice())
    );
    assert_eq!(tool.parameters["type"], json!("object"));
    assert!(matches!(
        tool.constrained_sampling,
        Some(ConstrainedSampling::Disabled(false))
    ));
    assert_eq!(tool.render_shell, Some(RenderShell::SelfRender));
    assert!(tool.prepare_arguments.is_some());
    assert_eq!(tool.output_schema, Some(json!({ "type": "object" })));
    assert_eq!(tool.exposure, Some(ToolExposure::ModelOnly));
    assert_eq!(tool.namespace.as_ref().map(|n| n.name.as_str()), Some("ns"));
    assert_eq!(
        (tool.reaches_tools.as_ref().unwrap())(&json!({})),
        ReachesToolsResult::Names(vec!["other".into()])
    );
    assert_eq!(
        tool.annotations.as_ref().and_then(|a| a.read_only_hint),
        Some(true)
    );
    assert_eq!(tool.default_active, Some(true));
    assert!(tool.prepare_loadout.is_some());
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
    let _: &dyn ToolDefinitionExecute = tool.execute.as_ref();
    assert!(tool.render_call.is_some());
    assert!(tool.render_result.is_some());
}

#[test]
fn tool_render_context_has_all_fourteen_fields() {
    let state: RenderState = Arc::new(Mutex::new(json!({ "n": 1 })));
    let invalidated = Arc::new(Mutex::new(0usize));
    let counter = Arc::clone(&invalidated);
    let context = ToolRenderContext {
        args: json!({ "command": "ls" }),
        tool_call_id: "call-1".into(),
        invalidate: Arc::new(move || *counter.lock().unwrap() += 1),
        last_component: None,
        state: Arc::clone(&state),
        cwd: PathBuf::from("/w"),
        execution_started: true,
        args_complete: false,
        is_partial: true,
        expanded: false,
        show_images: true,
        is_error: false,
        duration_ms: None,
        output_pad: 2,
    };
    assert_eq!(context.args, json!({ "command": "ls" }));
    assert_eq!(context.tool_call_id, "call-1");
    (context.invalidate)();
    assert_eq!(*invalidated.lock().unwrap(), 1);
    assert!(context.last_component.is_none());
    assert!(Arc::ptr_eq(&context.state, &state));
    assert_eq!(context.cwd, PathBuf::from("/w"));
    assert!(context.execution_started);
    assert!(!context.args_complete);
    assert!(context.is_partial);
    assert!(!context.expanded);
    assert!(context.show_images);
    assert!(!context.is_error);
    assert_eq!(context.duration_ms, None);
    // 冻结的跨单元形状是 `Option<u64>`（与 ToolExecutionEndEvent.duration_ms 同量纲）。
    let duration: Option<u64> = context.duration_ms;
    assert_eq!(duration, None);
    assert_eq!(context.output_pad, 2);
}

#[test]
fn tool_render_result_options_serde() {
    assert_eq!(
        serde_json::to_string(&ToolRenderResultOptions {
            expanded: true,
            is_partial: true,
        })
        .unwrap(),
        r#"{"expanded":true,"isPartial":true}"#
    );
}

#[test]
fn tool_annotations_and_namespace_serde() {
    let annotations = ToolAnnotations {
        read_only_hint: Some(true),
        destructive_hint: Some(false),
        idempotent_hint: None,
        open_world_hint: Some(true),
    };
    assert_eq!(
        serde_json::to_string(&annotations).unwrap(),
        r#"{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":true}"#
    );

    let namespace = ToolNamespace {
        name: "mcp__docs".into(),
        aliases: Some(vec!["docs".into()]),
        description: None,
        instructions: Some("use it".into()),
    };
    assert_eq!(
        serde_json::to_string(&namespace).unwrap(),
        r#"{"name":"mcp__docs","aliases":["docs"],"instructions":"use it"}"#
    );
}

#[test]
fn tool_loadout_changes_serde() {
    let mut descriptions = indexmap::IndexMap::new();
    descriptions.insert("read".to_string(), "Reads a file".to_string());
    let changes = ToolLoadoutChanges {
        descriptions: Some(descriptions),
        hidden_declarations: Some(vec!["write".into()]),
    };
    assert_eq!(
        serde_json::to_string(&changes).unwrap(),
        r#"{"descriptions":{"read":"Reads a file"},"hiddenDeclarations":["write"]}"#
    );
    assert_eq!(
        serde_json::to_string(&ToolLoadoutChanges::default()).unwrap(),
        "{}"
    );
}

#[test]
fn renderers_and_resolver() {
    let renderers = ToolRenderers {
        render_shell: Some(RenderShell::Default),
        render_call: None,
        render_result: None,
    };
    let cloned = renderers.clone();
    assert_eq!(cloned.render_shell, Some(RenderShell::Default));
    assert!(format!("{renderers:?}").contains("render_shell"));

    let resolver: ToolRendererResolver = Arc::new(
        |_tool_name: &str, _next: &kiana_ext_api::tool_types::NextToolRenderersFn| {
            Some(ToolRenderers {
                render_shell: Some(RenderShell::SelfRender),
                render_call: None,
                render_result: None,
            })
        },
    );
    let next = || -> Option<ToolRenderers> { None };
    let resolved = resolver("any", &next).expect("resolver 必须返回渲染器");
    assert_eq!(resolved.render_shell, Some(RenderShell::SelfRender));

    assert_eq!(
        ReachesToolsResult::default(),
        ReachesToolsResult::Names(Vec::new())
    );
    assert_eq!(ReachesToolsResult::All, ReachesToolsResult::All);
}

#[test]
fn render_shell_and_exposure_spellings() {
    assert_eq!(
        serde_json::to_string(&RenderShell::Default).unwrap(),
        r#""default""#
    );
    assert_eq!(
        serde_json::to_string(&RenderShell::SelfRender).unwrap(),
        r#""self""#
    );
    assert_eq!(
        serde_json::to_string(&ToolExposure::ModelOnly).unwrap(),
        r#""model-only""#
    );
}

#[test]
fn builtin_tool_name_literals_serde() {
    let cases: [(String, &str); 8] = [
        (serde_json::to_string(&BashToolName::Bash).unwrap(), "bash"),
        (
            serde_json::to_string(&PowerShellToolName::PowerShell).unwrap(),
            "powershell",
        ),
        (serde_json::to_string(&ReadToolName::Read).unwrap(), "read"),
        (serde_json::to_string(&EditToolName::Edit).unwrap(), "edit"),
        (
            serde_json::to_string(&WriteToolName::Write).unwrap(),
            "write",
        ),
        (serde_json::to_string(&GrepToolName::Grep).unwrap(), "grep"),
        (serde_json::to_string(&FindToolName::Find).unwrap(), "find"),
        (serde_json::to_string(&LsToolName::Ls).unwrap(), "ls"),
    ];
    for (serialized, expected) in cases {
        assert_eq!(serialized, format!("\"{expected}\""));
    }
    // Default 是唯一变体。
    assert_eq!(BashToolName::default(), BashToolName::Bash);
    assert_eq!(LsToolName::default(), LsToolName::Ls);
}

#[test]
fn tool_inputs_and_details_round_trip() {
    let bash = BashToolInput {
        command: "ls".into(),
        timeout: None,
    };
    assert_eq!(serde_json::to_string(&bash).unwrap(), r#"{"command":"ls"}"#);
    let _: PowerShellToolInput = bash.clone();
    assert_eq!(
        serde_json::to_string(&BashToolDetails {
            truncation: None,
            full_output_path: Some("/tmp/out".into()),
        })
        .unwrap(),
        r#"{"fullOutputPath":"/tmp/out"}"#
    );

    let read = ReadToolInput {
        path: "/f".into(),
        offset: None,
        limit: Some(10.0),
    };
    assert_eq!(
        serde_json::to_string(&read).unwrap(),
        r#"{"path":"/f","limit":10}"#
    );
    assert_eq!(
        serde_json::to_string(&ReadToolDetails::default()).unwrap(),
        "{}"
    );

    let edit = EditToolInput {
        path: "/f".into(),
        edits: vec![ReplaceEdit {
            old_text: "a".into(),
            new_text: "b".into(),
        }],
    };
    assert_eq!(
        serde_json::to_string(&edit).unwrap(),
        r#"{"path":"/f","edits":[{"oldText":"a","newText":"b"}]}"#
    );
    let details = EditToolDetails {
        diff: "-a\n+b".into(),
        patch: "--- a\n+++ b".into(),
        first_changed_line: Some(3),
    };
    assert_eq!(
        serde_json::to_string(&details).unwrap(),
        r#"{"diff":"-a\n+b","patch":"--- a\n+++ b","firstChangedLine":3}"#
    );

    assert_eq!(
        serde_json::to_string(&WriteToolInput {
            path: "/f".into(),
            content: String::new(),
        })
        .unwrap(),
        r#"{"path":"/f","content":""}"#
    );

    let grep = GrepToolInput {
        pattern: "p".into(),
        path: Some("src".into()),
        glob: None,
        ignore_case: Some(true),
        literal: None,
        context: Some(2.0),
        limit: None,
    };
    assert_eq!(
        serde_json::to_string(&grep).unwrap(),
        r#"{"pattern":"p","path":"src","ignoreCase":true,"context":2}"#
    );
    let _ = GrepToolDetails::default();
    let _ = FindToolDetails::default();
    let _ = LsToolDetails::default();
    let _: PowerShellToolDetails = BashToolDetails::default();

    let find = FindToolInput {
        pattern: "*.rs".into(),
        path: None,
        limit: Some(1000.0),
    };
    assert_eq!(
        serde_json::to_string(&find).unwrap(),
        r#"{"pattern":"*.rs","limit":1000}"#
    );
    assert_eq!(
        serde_json::to_string(&LsToolInput {
            path: Some("/w".into()),
            limit: None,
        })
        .unwrap(),
        r#"{"path":"/w"}"#
    );
    assert_eq!(
        serde_json::to_string(&LsToolInput::default()).unwrap(),
        "{}"
    );
}

#[test]
fn tool_input_numbers_accept_js_number_semantics() {
    // TS 的字段类型是 `number`：负数与小数都必须能反序列化（`u64` 会在 serde 边界硬报错，
    // 而 TS 由 `read.ts`/`grep.ts`/`find.ts`/`ls.ts` 在使用点用 `Math.max`/`??` 归一化）。
    let read: ReadToolInput =
        serde_json::from_str(r#"{"path":"/f","offset":-1,"limit":1.5}"#).unwrap();
    assert_eq!(read.offset, Some(-1.0));
    assert_eq!(read.limit, Some(1.5));
    // 整数值按 JS `JSON.stringify` 写出（`2` 而不是 `2.0`）。
    let read_int = ReadToolInput {
        path: "/f".into(),
        offset: Some(2.0),
        limit: None,
    };
    assert_eq!(
        serde_json::to_string(&read_int).unwrap(),
        r#"{"path":"/f","offset":2}"#
    );

    let grep: GrepToolInput =
        serde_json::from_str(r#"{"pattern":"p","context":-2.5,"limit":0}"#).unwrap();
    assert_eq!(grep.context, Some(-2.5));
    assert_eq!(grep.limit, Some(0.0));
    assert_eq!(
        serde_json::to_string(&grep).unwrap(),
        r#"{"pattern":"p","context":-2.5,"limit":0}"#
    );

    let find: FindToolInput = serde_json::from_str(r#"{"pattern":"*","limit":-3}"#).unwrap();
    assert_eq!(find.limit, Some(-3.0));
    let ls: LsToolInput = serde_json::from_str(r#"{"limit":-0.5}"#).unwrap();
    assert_eq!(ls.limit, Some(-0.5));

    // `bash` 的 `timeout` 也是 JS `number`（秒）：整数值写成 `2`，不是 `2.0`。
    let bash = BashToolInput {
        command: "ls".into(),
        timeout: Some(2.0),
    };
    assert_eq!(
        serde_json::to_string(&bash).unwrap(),
        r#"{"command":"ls","timeout":2}"#
    );
}

#[test]
fn truncation_result_field_order_and_reason() {
    let result = TruncationResult {
        content: "c".into(),
        truncated: true,
        truncated_by: Some(TruncatedBy::Bytes),
        total_lines: 1,
        total_bytes: 2,
        output_lines: 3,
        output_bytes: 4,
        last_line_partial: true,
        first_line_exceeds_limit: false,
        max_lines: 2000,
        max_bytes: 51200,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"content":"c","truncated":true,"truncatedBy":"bytes","totalLines":1,"totalBytes":2,"outputLines":3,"outputBytes":4,"lastLinePartial":true,"firstLineExceedsLimit":false,"maxLines":2000,"maxBytes":51200}"#
    );
    assert_eq!(
        serde_json::to_string(&TruncatedBy::Lines).unwrap(),
        r#""lines""#
    );
}

#[test]
fn bash_result_and_exec_result_shapes() {
    let result = BashResult {
        output: "o".into(),
        exit_code: Some(2),
        cancelled: false,
        truncated: true,
        full_output_path: None,
        execution_status: None,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"output":"o","exitCode":2,"cancelled":false,"truncated":true}"#
    );
    assert_eq!(
        serde_json::to_string(&BashExecResult {
            exit_code: Some(0),
            execution_status: None,
        })
        .unwrap(),
        r#"{"exitCode":0}"#
    );
    assert_eq!(
        serde_json::to_string(&BashExecResult {
            exit_code: None,
            execution_status: None,
        })
        .unwrap(),
        r#"{"exitCode":null}"#
    );
}

struct EchoBash;

impl BashOperations for EchoBash {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        _cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn Error + Send + Sync>>> {
        Box::pin(async move {
            (options.on_data)(command.as_bytes());
            Ok(BashExecResult {
                exit_code: Some(0),
                execution_status: None,
            })
        })
    }
}

#[tokio::test]
async fn bash_operations_is_object_safe_and_dispatches() {
    let boxed: Box<dyn BashOperations> = Box::new(EchoBash);
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&seen);
    let on_data = move |bytes: &[u8]| sink.lock().unwrap().extend_from_slice(bytes);
    let options = BashExecOptions {
        on_data: &on_data,
        signal: None,
        timeout: Some(1.5),
        env: None,
    };
    let result = boxed
        .exec("echo hi", "/w", options)
        .await
        .expect("mock 不应失败");
    assert_eq!(
        result,
        BashExecResult {
            exit_code: Some(0),
            execution_status: None,
        }
    );
    assert_eq!(&*seen.lock().unwrap(), b"echo hi");

    // Arc<dyn BashOperations> 也实现 BashOperations（下游可共享后装箱）。
    let shared: Arc<dyn BashOperations> = Arc::new(EchoBash);
    let boxed_shared: Box<dyn BashOperations> = Box::new(shared);
    let result = boxed_shared
        .exec(
            "true",
            "/w",
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: Some(indexmap::IndexMap::from([(
                    "A".to_string(),
                    "1".to_string(),
                )])),
            },
        )
        .await
        .expect("mock 不应失败");
    assert_eq!(result.exit_code, Some(0));
}

#[test]
fn exec_options_and_result_are_defined_here_with_js_number_timeout() {
    // §3.5 契约上移：这两个类型由 `kiana-ext-api` 定义；`kiana-core` 的同名低层类型是
    // **另一个**类型（L3 不能依赖 L7），不是本 crate 的别名。
    assert_ne!(
        std::any::TypeId::of::<ExecOptions>(),
        std::any::TypeId::of::<kiana_core::exec::ExecOptions>(),
    );
    assert_ne!(
        std::any::TypeId::of::<ExecResult>(),
        std::any::TypeId::of::<kiana_core::exec::ExecResult>(),
    );

    // `timeout` 是 JS number（毫秒）。`exec.ts` 只在
    // `options?.timeout && options.timeout > 0` 时启动计时器，因此负数、0、NaN
    // 都是“不设超时”，而小数（亚毫秒）和 Infinity 也是合法输入。
    let negative = ExecOptions {
        signal: None,
        timeout: Some(-1.0),
        cwd: None,
    };
    assert_eq!(negative.timeout, Some(-1.0));
    let fractional = ExecOptions {
        signal: None,
        timeout: Some(1.5),
        cwd: Some("/w".to_string()),
    };
    assert_eq!(fractional.timeout, Some(1.5));
    // `cwd` 是字符串，由 Rust 扩展桥读取为子进程 cwd。
    assert_eq!(fractional.cwd.as_deref(), Some("/w"));
    let nan = ExecOptions {
        signal: None,
        timeout: Some(f64::NAN),
        cwd: None,
    };
    assert!(nan.timeout.expect("Some(NaN)").is_nan());
    assert!(ExecOptions::default().timeout.is_none());
    let with_signal = ExecOptions {
        signal: Some(CancellationToken::new()),
        timeout: None,
        cwd: None,
    };
    assert!(with_signal.signal.is_some());

    // `ExecResult` 的 JSON 键顺序 = TS 声明顺序；`code` 是数字。
    let result = ExecResult {
        stdout: "o".to_string(),
        stderr: "e".to_string(),
        code: 3,
        killed: true,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"stdout":"o","stderr":"e","code":3,"killed":true}"#
    );
    assert_eq!(
        serde_json::from_str::<ExecResult>(r#"{"stdout":"o","stderr":"e","code":3,"killed":true}"#)
            .unwrap(),
        result
    );
}

#[test]
fn event_kind_unrelated_smoke() {
    // 保证 tests 间的公共导入未意外遮蔽名字（无需断言逻辑）。
    assert_eq!(ExtensionEventKind::ToolCall.as_str(), "tool_call");
}
