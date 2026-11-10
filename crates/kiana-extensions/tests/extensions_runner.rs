//! `ExtensionRunner` 门面（`extensions-runner.test.ts` 中不依赖 JS 加载器的用例）。
//!
//! 覆盖：flag/工具/命令收集、渲染器解析链、快捷键冲突诊断、`invalidate` 的 stale 语义、
//! 错误监听器、`reportUnhandledMcpServers` 的逐字文案与去重、`setUIContext` 的
//! `noOpUIContext` 引用比较、`bindCore` 的默认值与动作绑定、`emitSessionShutdownEvent`。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_agent_core::AgentToolError;
use kiana_ai::types::{JsonValue, UserContent};
use kiana_config::keybindings::{KeyIdList, KeybindingsConfig};
use kiana_config::models::registry::ModelRegistry;
use kiana_core::diagnostics::DiagnosticKind;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::events::{
    ExtensionEvent, ExtensionEventResult, ExtensionHandler, ExtensionHandlerFn,
    SessionShutdownEvent, SessionShutdownReason, UserBashEvent,
};
use kiana_ext_api::tool_types::ToolDefinitionExecute;
use kiana_ext_api::types::{
    CommandHandler, Extension, ExtensionActions, ExtensionCommandContextApi,
    ExtensionContextActions, ExtensionError, ExtensionFlag, ExtensionMode, ExtensionShortcut,
    FlagType, FlagValue, RegisteredCommand,
};
use kiana_ext_api::{LocalBoxFuture, ToolDefinition};
use kiana_extensions::engine::{self, JsEngine};
use kiana_extensions::loader;
use kiana_extensions::runner::context;
use kiana_extensions::runner::dispatch;
use kiana_extensions::runner::noop_ui::{is_no_op_ui_context, no_op_ui_context};
use kiana_extensions::runner::{
    ExtensionRunner, ProviderActions, emit_session_shutdown_event,
    normalize_build_system_prompt_options,
};
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

async fn make_runner(extensions: Vec<Extension>) -> (Rc<JsEngine>, Arc<ExtensionRunner>) {
    let engine = Rc::new(JsEngine::new().expect("engine"));
    engine::install(&engine).expect("prelude");
    context::install(&engine).expect("context.js");
    dispatch::install(Rc::clone(&engine)).expect("dispatch.js");
    use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
        .await
        .expect("model runtime");
    let model_registry = Arc::new(ModelRegistry::new(Arc::new(model_runtime)));
    let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager {
        cwd: "/tmp/runner".to_owned(),
    });
    let runner = Arc::new(ExtensionRunner::new(
        extensions,
        loader::create_extension_runtime(),
        PathBuf::from("/tmp/runner"),
        session,
        model_registry,
    ));
    (engine, runner)
}

fn extension(path: &str) -> Extension {
    Extension {
        path: path.to_owned(),
        resolved_path: path.to_owned(),
        hidden: None,
        replaceable: None,
        source_info: SourceInfo {
            path: path.to_owned(),
            source: "local".to_owned(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        handlers: IndexMap::new(),
        tools: IndexMap::new(),
        message_renderers: IndexMap::new(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: IndexMap::new(),
        flags: IndexMap::new(),
        shortcuts: IndexMap::new(),
    }
}

fn flag(name: &str, value: bool, path: &str) -> ExtensionFlag {
    ExtensionFlag {
        name: name.to_owned(),
        description: None,
        r#type: FlagType::Boolean,
        default: Some(FlagValue::Bool(value)),
        extension_path: path.to_owned(),
    }
}

fn shortcut(key: &str, path: &str) -> ExtensionShortcut {
    ExtensionShortcut {
        shortcut: key.to_owned(),
        description: None,
        handler: Arc::new(|_ctx| Box::pin(async {}) as LocalBoxFuture<'static, ()>),
        extension_path: path.to_owned(),
    }
}

fn command(name: &str) -> RegisteredCommand {
    RegisteredCommand {
        name: name.to_owned(),
        source_info: SourceInfo {
            path: format!("<inline:{name}>"),
            source: "local".to_owned(),
            scope: SourceScope::Project,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        },
        description: None,
        get_argument_completions: None,
        handler: Arc::new(|_args: String, _ctx: Arc<dyn ExtensionCommandContextApi>| {
            Box::pin(async {}) as LocalBoxFuture<'static, ()>
        }) as CommandHandler,
    }
}

struct StubToolExecute;

impl ToolDefinitionExecute for StubToolExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<
        'a,
        Result<kiana_agent_core::AgentToolResult<JsonValue>, AgentToolError>,
    > {
        Box::pin(async {
            Err(AgentToolError {
                message: "unused".to_owned(),
                name: None,
                stack: None,
            })
        })
    }
}

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        label: name.to_owned(),
        description: "stub".to_owned(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(StubToolExecute),
        render_call: None,
        render_result: None,
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

fn stub_context_actions() -> ExtensionContextActions {
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

async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

// ===========================================================================
// 收集类
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn collects_flags_first_wins_and_sets_values() {
    run_local(async {
        let mut first = extension("/ext/a.ts");
        first
            .flags
            .insert("alpha".to_owned(), flag("alpha", true, "/ext/a.ts"));
        first
            .flags
            .insert("beta".to_owned(), flag("beta", false, "/ext/a.ts"));
        let mut second = extension("/ext/b.ts");
        second
            .flags
            .insert("alpha".to_owned(), flag("alpha", false, "/ext/b.ts"));
        let (_engine, runner) = make_runner(vec![first, second]).await;

        let flags = runner.get_flags();
        assert_eq!(flags.len(), 2);
        assert_eq!(flags["alpha"].extension_path, "/ext/a.ts");
        assert_eq!(flags["alpha"].default, Some(FlagValue::Bool(true)));

        runner.set_flag_value("alpha", FlagValue::Bool(false));
        assert_eq!(
            runner.get_flag_values().get("alpha"),
            Some(&FlagValue::Bool(false))
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn collects_tools_from_multiple_extensions_keeping_the_first_registration() {
    run_local(async {
        let mut first = extension("/ext/a.ts");
        first.tools.insert(
            "shared".to_owned(),
            kiana_ext_api::RegisteredTool {
                definition: tool("shared"),
                source_info: first.source_info.clone(),
            },
        );
        let mut second = extension("/ext/b.ts");
        second.tools.insert(
            "shared".to_owned(),
            kiana_ext_api::RegisteredTool {
                definition: tool("shared"),
                source_info: second.source_info.clone(),
            },
        );
        second.tools.insert(
            "other".to_owned(),
            kiana_ext_api::RegisteredTool {
                definition: tool("other"),
                source_info: second.source_info.clone(),
            },
        );
        let (_engine, runner) = make_runner(vec![first, second]).await;
        let tools = runner.get_all_registered_tools();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool.definition.name.as_str())
            .collect();
        assert_eq!(names, vec!["shared", "other"]);
        assert_eq!(
            runner.get_tool_definition("shared").expect("shared").name,
            "shared"
        );
        assert!(runner.get_tool_definition("missing").is_none());
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn suffixes_duplicate_commands_in_insertion_order() {
    run_local(async {
        let mut first = extension("/ext/a.ts");
        first
            .commands
            .insert("deploy".to_owned(), command("deploy"));
        first.commands.insert("build".to_owned(), command("build"));
        let mut second = extension("/ext/b.ts");
        second
            .commands
            .insert("deploy".to_owned(), command("deploy"));
        let (_engine, runner) = make_runner(vec![first, second]).await;
        let commands = runner.get_registered_commands();
        let names: Vec<&str> = commands
            .iter()
            .map(|command| command.invocation_name.as_str())
            .collect();
        assert_eq!(names, vec!["deploy:1", "build", "deploy:2"]);
        assert_eq!(
            runner.get_command("deploy:2").expect("deploy:2").name,
            "deploy"
        );
        assert!(runner.get_command("deploy").is_none());
        assert!(runner.get_command_diagnostics().is_empty());
    })
    .await;
}

// ===========================================================================
// 快捷键诊断
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn warns_and_skips_shortcuts_that_conflict_with_reserved_bindings() {
    run_local(async {
        let mut ext = extension("/ext/shortcuts.ts");
        ext.shortcuts.insert("ctrl+x".to_owned(), shortcut("ctrl+x", "/ext/shortcuts.ts"));
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        keybindings.insert(
            "app.interrupt".to_owned(),
            KeyIdList::One("ctrl+x".to_owned()),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(shortcuts.is_empty());
        let diagnostics = runner.get_shortcut_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, DiagnosticKind::Warning);
        assert_eq!(
            diagnostics[0].message,
            "Extension shortcut 'ctrl+x' from /ext/shortcuts.ts conflicts with built-in shortcut. Skipping."
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn warns_but_allows_non_reserved_builtin_shortcut_conflicts() {
    run_local(async {
        let mut ext = extension("/ext/shortcuts.ts");
        ext.shortcuts
            .insert("ctrl+y".to_owned(), shortcut("ctrl+y", "/ext/shortcuts.ts"));
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        keybindings.insert(
            "app.custom.reload".to_owned(),
            KeyIdList::One("ctrl+y".to_owned()),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(shortcuts.contains_key("ctrl+y"));
        let diagnostics = runner.get_shortcut_diagnostics();
        assert!(
            diagnostics[0]
                .message
                .contains("is built-in shortcut for app.custom.reload")
        );
    })
    .await;
}

// ===========================================================================
// stale / 错误监听器
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn invalidate_sets_the_stale_message_once_and_blocks_later_access() {
    run_local(async {
        let (_engine, runner) = make_runner(vec![extension("/ext/a.ts")]).await;
        assert!(runner.assert_active().is_ok());
        runner.invalidate(Some("stale!".to_owned()));
        let error = runner.assert_active().expect_err("stale");
        assert_eq!(error.message(), "stale!");
        runner.invalidate(Some("second".to_owned()));
        assert_eq!(
            runner.assert_active().expect_err("stale").message(),
            "stale!"
        );
        // 空串是 falsy：TS 的 `if (this.staleMessage)` 不视为陈旧。
        let (_engine, runner) = make_runner(Vec::new()).await;
        runner.invalidate(Some(String::new()));
        assert!(runner.assert_active().is_ok());
        // 默认文案逐字来自 loader 的 DEFAULT_STALE_MESSAGE。
        let (_engine, runner) = make_runner(Vec::new()).await;
        runner.invalidate(None);
        assert_eq!(
            runner.assert_active().expect_err("stale").message(),
            loader::DEFAULT_STALE_MESSAGE
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn error_listeners_receive_errors_and_can_unsubscribe() {
    run_local(async {
        let (_engine, runner) = make_runner(Vec::new()).await;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        let unsubscribe = runner.on_error(Arc::new(move |error| {
            sink.lock().unwrap().push(error.error);
        }));
        runner.emit_error(ExtensionError {
            extension_path: "/ext/a.ts".to_owned(),
            event: "context".to_owned(),
            error: "boom".to_owned(),
            stack: None,
        });
        unsubscribe();
        runner.emit_error(ExtensionError {
            extension_path: "/ext/a.ts".to_owned(),
            event: "context".to_owned(),
            error: "after".to_owned(),
            stack: None,
        });
        assert_eq!(*seen.lock().unwrap(), vec!["boom"]);
    })
    .await;
}

// ===========================================================================
// MCP 未处理告警
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn reports_unhandled_mcp_servers_once_per_name() {
    run_local(async {
        let runtime = loader::create_extension_runtime();
        let config = kiana_core::mcp_servers::validate_mcp_server_config(
            "my-server",
            &json!({ "command": "node", "args": ["server.js"] }),
        )
        .expect("config");
        runtime.mcp_servers.register(kiana_core::mcp_servers::RegisteredMcpServer {
            name: "my-server".to_owned(),
            config,
            extension_path: "/ext/mcp.ts".to_owned(),
        });
        let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager::default());
        use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
            .await
            .expect("model runtime");
        let runner = ExtensionRunner::new(
            Vec::new(),
            runtime,
            PathBuf::from("/tmp"),
            session,
            Arc::new(ModelRegistry::new(Arc::new(model_runtime))),
        );
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _ = runner.on_error(Arc::new(move |error| {
            sink.lock().unwrap().push(error);
        }));

        runner.report_unhandled_mcp_servers();
        runner.report_unhandled_mcp_servers();

        let errors = errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].event, "register_mcp_server");
        assert_eq!(errors[0].extension_path, "/ext/mcp.ts");
        assert_eq!(
            errors[0].error,
            "MCP server \"my-server\" is registered, but no loaded extension connects MCP servers; another extension may have replaced the built-in MCP support"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn skips_the_mcp_warning_when_a_handler_exists() {
    run_local(async {
        let mut ext = extension("/ext/mcp.ts");
        ext.handlers.insert(
            "mcp_servers_change".to_owned(),
            vec![ExtensionHandler::Standard(
                Arc::new(|_event, _ctx| -> LocalBoxFuture<'static, _> {
                    Box::pin(async { Ok(None) })
                }) as ExtensionHandlerFn,
            )],
        );
        let (_, runner) = make_runner(vec![ext]).await;
        assert!(runner.has_handlers("mcp_servers_change"));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _ = runner.on_error(Arc::new(move |error| {
            sink.lock().unwrap().push(error);
        }));
        runner.report_unhandled_mcp_servers();
        assert!(errors.lock().unwrap().is_empty());
    })
    .await;
}

// ===========================================================================
// bind_core / setUIcontext
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn bind_core_wires_context_actions_and_defaults() {
    run_local(async {
        let (_, runner) = make_runner(Vec::new()).await;
        // 未绑定时的默认值。
        let ctx = runner.create_context();
        assert_eq!(ctx.get_system_prompt(), "");
        // `ctx.getSystemPromptOptions()` 的默认值是归一化后的选项（集合字段补全）。
        let options = runner.create_command_context().get_system_prompt_options();
        assert_eq!(
            options.selected_tools,
            Some(vec![
                "read".to_owned(),
                "bash".to_owned(),
                "edit".to_owned(),
                "write".to_owned()
            ])
        );
        assert!(ctx.is_idle());
        assert!(ctx.is_project_trusted());
        assert!(ctx.scoped_models().is_empty());
        assert!(!ctx.has_pending_messages());

        let mut context_actions = stub_context_actions();
        context_actions.get_system_prompt = Arc::new(|| "bound".to_owned());
        runner.bind_core(stub_actions(), context_actions, None);
        let ctx = runner.create_context();
        assert_eq!(ctx.get_system_prompt(), "bound");
        // `thinkingLevel` 来自 bindCore 复制进 runtime 的动作。
        assert_eq!(
            ctx.thinking_level(),
            Some(kiana_agent_core::ThinkingLevel::Off)
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn set_ui_context_uses_the_no_op_singleton_for_has_ui() {
    run_local(async {
        let (_, runner) = make_runner(Vec::new()).await;
        assert!(!runner.has_ui());
        assert!(is_no_op_ui_context(&runner.get_ui_context()));
        runner.set_ui_context(Some(no_op_ui_context()), ExtensionMode::Rpc);
        assert!(runner.has_ui());
        runner.set_ui_context(None, ExtensionMode::Print);
        assert!(!runner.has_ui());
        assert_eq!(runner.create_context().mode(), ExtensionMode::Print);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn bind_core_flushes_pending_provider_registrations_through_provider_actions() {
    run_local(async {
        let runtime = loader::create_extension_runtime();
        // 加载期排队一个 provider 注册（`runtime.registerProvider(name, config)`）。
        (runtime.register_provider)(
            "my-provider".to_owned(),
            kiana_ext_api::ProviderConfig {
                name: Some("My Provider".to_owned()),
                base_url: None,
                api_key: None,
                api: None,
                stream_simple: None,
                images: None,
                classifiers: None,
                headers: None,
                auth_header: None,
                models: None,
                refresh_models: None,
                oauth: None,
            },
            Some("/ext/provider.ts".to_owned()),
        );
        assert_eq!(
            loader::runtime_pending_provider_registrations(&runtime).len(),
            1
        );
        let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager::default());
        use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
            .await
            .expect("model runtime");
        let runner = ExtensionRunner::new(
            Vec::new(),
            Arc::clone(&runtime),
            PathBuf::from("/tmp"),
            session,
            Arc::new(ModelRegistry::new(Arc::new(model_runtime))),
        );
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&calls);
        let provider_actions = ProviderActions {
            register_provider: Some(Arc::new(move |name, _config| {
                sink.lock().unwrap().push(name);
            })),
            ..ProviderActions::default()
        };
        runner.bind_core(
            stub_actions(),
            stub_context_actions(),
            Some(provider_actions),
        );
        assert_eq!(*calls.lock().unwrap(), vec!["my-provider"]);
        assert!(loader::runtime_pending_provider_registrations(&runtime).is_empty());
        // bindCore 之后注册立即生效（不再排队）。
        (runtime.register_provider)(
            "second".to_owned(),
            kiana_ext_api::ProviderConfig {
                name: None,
                base_url: None,
                api_key: None,
                api: None,
                stream_simple: None,
                images: None,
                classifiers: None,
                headers: None,
                auth_header: None,
                models: None,
                refresh_models: None,
                oauth: None,
            },
            None,
        );
        assert_eq!(*calls.lock().unwrap(), vec!["my-provider", "second"]);
    })
    .await;
}

// ===========================================================================
// emitSessionShutdownEvent
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn emit_session_shutdown_event_returns_whether_a_handler_ran() {
    run_local(async {
        let (_, runner) = make_runner(vec![extension("/ext/a.ts")]).await;
        let event = SessionShutdownEvent {
            event_type: kiana_ext_api::events::SessionShutdownEventType::SessionShutdown,
            reason: SessionShutdownReason::Quit,
            target_session_file: None,
        };
        assert!(!emit_session_shutdown_event(&runner, event.clone()).await);
        assert!(runner.get_extension_paths() == vec!["/ext/a.ts".to_owned()]);
    })
    .await;
}

// ===========================================================================
// normalizeBuildSystemPromptOptions
// ===========================================================================

#[test]
fn normalizes_system_prompt_options_with_ts_defaults() {
    let normalized = normalize_build_system_prompt_options(
        &kiana_ext_api::system_prompt::BuildSystemPromptOptions {
            cwd: "/tmp".to_owned(),
            ..Default::default()
        },
    );
    assert_eq!(
        normalized.selected_tools,
        vec!["read", "bash", "edit", "write"]
    );
    assert!(normalized.hidden_tools.is_empty());
    assert_eq!(normalized.append_system_prompt, "");
    assert_eq!(normalized.cwd, "/tmp");
}

// 未使用导入保持与 TS 源的类型面一致（`UserContent`/`UserBashEvent` 由其它用例使用）。
#[allow(dead_code)]
fn _unused(_: UserContent, _: UserBashEvent, _: ExtensionEvent, _: ExtensionEventResult) {}

// ===========================================================================
// shortcut conflicts：TS 的 7 个用例的其余分支
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn blocks_shortcuts_for_reserved_actions_even_when_rebound() {
    run_local(async {
        let mut ext = extension("/ext/rebound.ts");
        ext.shortcuts
            .insert("ctrl+x".to_owned(), shortcut("ctrl+x", "/ext/rebound.ts"));
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        keybindings.insert(
            "app.interrupt".to_owned(),
            KeyIdList::One("ctrl+x".to_owned()),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(!shortcuts.contains_key("ctrl+x"));
        assert_eq!(
            runner.get_shortcut_diagnostics()[0].message,
            "Extension shortcut 'ctrl+x' from /ext/rebound.ts conflicts with built-in shortcut. Skipping."
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocks_shortcuts_when_a_reserved_key_is_also_bound_to_a_non_reserved_action() {
    run_local(async {
        let mut ext = extension("/ext/shared.ts");
        ext.shortcuts
            .insert("ctrl+p".to_owned(), shortcut("ctrl+p", "/ext/shared.ts"));
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        // 遍历顺序把非保留项放在前面：保留项仍必须胜出。
        keybindings.insert(
            "app.custom.reload".to_owned(),
            KeyIdList::One("ctrl+p".to_owned()),
        );
        keybindings.insert(
            "app.model.cycleForward".to_owned(),
            KeyIdList::One("ctrl+p".to_owned()),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(!shortcuts.contains_key("ctrl+p"));
        assert!(
            runner.get_shortcut_diagnostics()[0]
                .message
                .contains("conflicts with built-in")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocks_shortcuts_when_a_reserved_action_has_multiple_keys() {
    run_local(async {
        let mut ext = extension("/ext/multi-reserved.ts");
        ext.shortcuts.insert(
            "ctrl+y".to_owned(),
            shortcut("ctrl+y", "/ext/multi-reserved.ts"),
        );
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        keybindings.insert(
            "app.clear".to_owned(),
            KeyIdList::Many(vec!["ctrl+x".to_owned(), "ctrl+y".to_owned()]),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(!shortcuts.contains_key("ctrl+y"));
        assert!(
            runner.get_shortcut_diagnostics()[0]
                .message
                .contains("conflicts with built-in")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn warns_but_allows_when_a_non_reserved_action_has_multiple_keys() {
    run_local(async {
        let mut ext = extension("/ext/multi-non-reserved.ts");
        ext.shortcuts.insert(
            "ctrl+y".to_owned(),
            shortcut("ctrl+y", "/ext/multi-non-reserved.ts"),
        );
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        keybindings.insert(
            "app.clipboard.pasteImage".to_owned(),
            KeyIdList::Many(vec!["ctrl+x".to_owned(), "ctrl+y".to_owned()]),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(shortcuts.contains_key("ctrl+y"));
        assert!(
            runner.get_shortcut_diagnostics()[0]
                .message
                .contains("is built-in shortcut for app.clipboard.pasteImage")
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn allows_a_shortcut_when_the_reserved_action_no_longer_uses_the_default_key() {
    run_local(async {
        let mut ext = extension("/ext/freed.ts");
        ext.shortcuts
            .insert("ctrl+p".to_owned(), shortcut("ctrl+p", "/ext/freed.ts"));
        let (_engine, runner) = make_runner(vec![ext]).await;
        let mut keybindings = KeybindingsConfig::new();
        keybindings.insert(
            "app.model.cycleForward".to_owned(),
            KeyIdList::One("ctrl+n".to_owned()),
        );
        let shortcuts = runner.get_shortcuts(&keybindings);
        assert!(shortcuts.contains_key("ctrl+p"));
        assert!(runner.get_shortcut_diagnostics().is_empty());
    })
    .await;
}

// ===========================================================================
// 回归：loader 的 command / shortcut 错误上报器
// ===========================================================================

#[path = "fixtures/extensions/support.rs"]
mod support;

/// 回归（major）：`ExtensionRunner::new` 必须安装 loader 的错误上报器；命令 handler 的
/// rejection 到达 `onError` 时带 TS 的 `extensionPath: "command:<name>"` / `event: "command"`
/// （`agent-session.ts:2193-2205`）。
///
/// 修复前 `set_extension_error_reporter` 没有任何生产调用点，错误只落在 `tracing::error!`，
/// 监听器永远收不到；上报路径还错误地使用扩展文件路径。
#[tokio::test(flavor = "current_thread")]
async fn command_handler_errors_reach_error_listeners_with_the_command_path() {
    run_local(async {
        use kiana_extensions::loader::load_extensions;

        let harness = support::LoaderHarness::new();
        context::install(&harness.engine).expect("install context.js");
        dispatch::install(Rc::clone(&harness.engine)).expect("install dispatch.js");
        let root = support::temp_root();
        let path = support::write_file(
            root.path(),
            "extensions/boom-command.ts",
            r#"
export default function(kiana) {
    kiana.registerCommand("boom", {
        handler: async () => { throw new Error("kaboom"); },
    });
}
"#,
        );
        let loaded = load_extensions(
            &[path.to_string_lossy().into_owned()],
            &root.path().to_string_lossy(),
            None,
            None,
        )
        .await;
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);

        use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
            .await
            .expect("model runtime");
        let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager::default());
        let runner = ExtensionRunner::new(
            loaded.extensions,
            loaded.runtime,
            PathBuf::from("/tmp/command-error"),
            session,
            Arc::new(ModelRegistry::new(Arc::new(model_runtime))),
        );

        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let _ = runner.on_error(Arc::new(move |error| {
            sink.lock().unwrap().push(error);
        }));

        let command = runner.get_command("boom").expect("command");
        let ctx = runner.create_command_context();
        (command.handler)(String::new(), ctx).await;

        let errors = errors.lock().unwrap();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].extension_path, "command:boom");
        assert_eq!(errors[0].event, "command");
        assert_eq!(errors[0].error, "kaboom");

        let _ = harness;
    })
    .await;
}

/// 回归（minor）：命令 handler 以命令对象为 `this`（TS `await command.handler(args, ctx)`，
/// `agent-session.ts:2193`）。
///
/// 修复前适配器用 `Function::call_arg`（`this === undefined`），handler 里读 `this.name`
/// 抛 TypeError；命令对象在 TS 里是 `{ name, sourceInfo, ...options }`。
#[tokio::test(flavor = "current_thread")]
async fn command_handler_is_called_with_the_command_object_as_this() {
    run_local(async {
        use kiana_extensions::loader::load_extensions;

        let harness = support::LoaderHarness::new();
        context::install(&harness.engine).expect("install context.js");
        dispatch::install(Rc::clone(&harness.engine)).expect("install dispatch.js");
        let root = support::temp_root();
        let path = support::write_file(
            root.path(),
            "extensions/command-this.ts",
            r#"
export default function(kiana) {
    kiana.registerCommand("this-cmd", {
        description: "command this binding",
        marker: 7,
        handler: async function () {
            globalThis.__commandThis = this.name + ":" + this.marker;
        },
    });
    kiana.registerTool({
        name: "this-tool",
        label: "this-tool",
        description: "tool this binding",
        marker: 11,
        parameters: { type: "object", properties: {} },
        execute: async function () {
            globalThis.__toolThis = this.name + ":" + this.marker;
            return { content: [] };
        },
    });
}
"#,
        );
        let loaded = load_extensions(
            &[path.to_string_lossy().into_owned()],
            &root.path().to_string_lossy(),
            None,
            None,
        )
        .await;
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);

        use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
            .await
            .expect("model runtime");
        let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager::default());
        let runner = ExtensionRunner::new(
            loaded.extensions,
            loaded.runtime,
            PathBuf::from("/tmp/command-this"),
            session,
            Arc::new(ModelRegistry::new(Arc::new(model_runtime))),
        );
        let command = runner.get_command("this-cmd").expect("command");
        let ctx = runner.create_command_context();
        (command.handler)(String::new(), ctx).await;

        let saw: String = harness
            .engine
            .eval_script("globalThis.__commandThis", "<command-this>")
            .expect("read this binding");
        assert_eq!(saw, "this-cmd:7");

        // TS `definition.execute(...)`（`tool-definition-wrapper.ts:22`）同样以工具对象为
        // `this`。
        let definition = runner.get_tool_definition("this-tool").expect("tool");
        let execute = definition.execute.clone();
        let ctx = runner.create_tool_context("call-1", None);
        execute
            .execute("call-1", serde_json::json!({}), None, None, ctx)
            .await
            .expect("tool executes");
        let saw: String = harness
            .engine
            .eval_script("globalThis.__toolThis", "<tool-this>")
            .expect("read tool this binding");
        assert_eq!(saw, "this-tool:11");
    })
    .await;
}
