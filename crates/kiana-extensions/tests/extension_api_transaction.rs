//! 事务性扩展 API 与 §3.5 逐字错误文案。
//!
//! 移植自 `8423-extension-factory-failure.test.ts`（两个用例）与
//! `spec/ca-extensions.md` §3.5 的错误清单。Rust 定义的扩展走 `kiana-ext-api::ExtensionApi`
//! 的同一套事务实现；JS 定义的扩展经 `js/api.js` 的宿主原语，两边必须给出同一套文案，避免同一错误因扩展语言不同而不同。
#![cfg(feature = "runtime")]
// 同 `event_bus_lifecycle.rs`：`Arc<dyn ExtensionApi>` 是冻结签名，不是并发共享。
#![allow(clippy::arc_with_non_send_sync)]

#[path = "fixtures/extensions/support.rs"]
mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use kiana_core::event_bus::{EventBus, create_event_bus};
use kiana_core::mcp_servers::validate_mcp_server_config;
use kiana_ext_api::types::{
    ExtensionApi, ExtensionFactory, FlagValue, ProviderConfig, ProviderRegistration,
    RegisterFlagOptions,
};
use kiana_extensions::loader::{
    DEFAULT_STALE_MESSAGE, NOT_INITIALIZED_MESSAGE, create_extension_runtime,
    load_extension_from_factory, load_extensions, runtime_flag_values, runtime_invalidate,
    runtime_pending_provider_registrations,
};
use serde_json::json;
use support::{COMMAND_EXTENSION, LoaderHarness, temp_root, write_file};

fn provider_config() -> ProviderConfig {
    ProviderConfig {
        name: None,
        base_url: Some("https://provider.test/v1".to_string()),
        api_key: Some("provider-test-key".to_string()),
        api: None,
        stream_simple: None,
        images: None,
        classifiers: None,
        headers: None,
        auth_header: None,
        models: None,
        refresh_models: None,
        oauth: None,
    }
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn discards_runtime_changes_and_disables_the_failed_api() {
    let runtime = create_extension_runtime();
    let event_bus: Arc<dyn EventBus> = create_event_bus();

    let working: ExtensionFactory = Arc::new(|kiana| {
        Box::pin(async move {
            kiana.register_provider(ProviderRegistration::Config {
                name: "working-provider".to_string(),
                config: provider_config(),
            });
        })
    });
    load_extension_from_factory(
        working,
        &cwd(),
        Arc::clone(&event_bus),
        Arc::clone(&runtime),
        Some("<working>"),
    )
    .await
    .expect("working extension loads");

    let captured_api: Rc<RefCell<Option<Arc<dyn ExtensionApi>>>> = Rc::new(RefCell::new(None));
    let flag_during_load: Rc<RefCell<Option<FlagValue>>> = Rc::new(RefCell::new(None));
    let event_calls = Arc::new(AtomicU32::new(0));

    let captured = Rc::clone(&captured_api);
    let flag = Rc::clone(&flag_during_load);
    let calls = Arc::clone(&event_calls);
    let failing: ExtensionFactory = Arc::new(move |kiana| {
        let captured = Rc::clone(&captured);
        let flag = Rc::clone(&flag);
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            *captured.borrow_mut() = Some(Arc::clone(&kiana));
            let calls_for_handler = Arc::clone(&calls);
            let _ = kiana.events().on(
                "factory-failure",
                Arc::new(move |_| {
                    calls_for_handler.fetch_add(1, Ordering::SeqCst);
                }),
            );
            kiana.register_flag(
                "failed-flag",
                RegisterFlagOptions::Boolean {
                    description: None,
                    default: Some(true),
                },
            );
            *flag.borrow_mut() = kiana.get_flag("failed-flag");
            kiana.unregister_provider("working-provider");
            kiana.register_provider(ProviderRegistration::Config {
                name: "failed-provider".to_string(),
                config: provider_config(),
            });
            panic!("factory failed");
        })
    });

    let error = load_extension_from_factory(
        failing,
        &cwd(),
        Arc::clone(&event_bus),
        Arc::clone(&runtime),
        Some("<failing>"),
    )
    .await
    .expect_err("factory fails");
    assert_eq!(error, "factory failed");

    event_bus.emit("factory-failure", serde_json::Value::Null);
    assert_eq!(*flag_during_load.borrow(), Some(FlagValue::Bool(true)));
    assert!(!runtime_flag_values(&runtime).contains_key("failed-flag"));
    let pending_names: Vec<String> = runtime_pending_provider_registrations(&runtime)
        .into_iter()
        .map(|registration| registration.name)
        .collect();
    assert_eq!(pending_names, vec!["working-provider".to_string()]);
    assert_eq!(event_calls.load(Ordering::SeqCst), 0);

    let api = captured_api
        .borrow()
        .clone()
        .expect("factory captured the api");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        api.register_flag(
            "late-flag",
            RegisterFlagOptions::Boolean {
                description: None,
                default: Some(true),
            },
        );
    }))
    .expect_err("failed api throws");
    assert_eq!(
        panic_message(panic),
        "Extension \"<failing>\" failed to load and its API is no longer active."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_discard_a_concurrently_loaded_factory_s_provider() {
    let runtime = create_extension_runtime();
    let event_bus: Arc<dyn EventBus> = create_event_bus();
    let (release, wait) = tokio::sync::oneshot::channel::<()>();
    let wait = Rc::new(RefCell::new(Some(wait)));
    let cwd = cwd();

    let failing = load_extension_from_factory(
        Arc::new(move |kiana| {
            let wait = Rc::clone(&wait);
            Box::pin(async move {
                kiana.register_provider(ProviderRegistration::Config {
                    name: "failed-provider".to_string(),
                    config: provider_config(),
                });
                let receiver = wait.borrow_mut().take();
                if let Some(receiver) = receiver {
                    let _ = receiver.await;
                }
                panic!("factory failed");
            })
        }),
        &cwd,
        Arc::clone(&event_bus),
        Arc::clone(&runtime),
        Some("<failing>"),
    );

    let (failing_result, ()) = tokio::join!(failing, async {
        load_extension_from_factory(
            Arc::new(|kiana| {
                Box::pin(async move {
                    kiana.register_provider(ProviderRegistration::Config {
                        name: "working-provider".to_string(),
                        config: provider_config(),
                    });
                })
            }),
            &cwd,
            Arc::clone(&event_bus),
            Arc::clone(&runtime),
            Some("<working>"),
        )
        .await
        .expect("working extension loads");
        let _ = release.send(());
    });

    assert_eq!(failing_result.expect_err("factory fails"), "factory failed");
    let pending_names: Vec<String> = runtime_pending_provider_registrations(&runtime)
        .into_iter()
        .map(|registration| registration.name)
        .collect();
    assert_eq!(pending_names, vec!["working-provider".to_string()]);
}

// ===========================================================================
// §3.5 的逐字错误文案
// ===========================================================================

async fn load_error_for(harness: &LoaderHarness, source: &str) -> String {
    let root = temp_root();
    let path = write_file(root.path(), "extensions/failing.ts", source);
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(result.extensions.len(), 0);
    assert_eq!(result.errors.len(), 1);
    result.errors[0].error.clone()
}

#[tokio::test(flavor = "current_thread")]
async fn tool_parameter_schema_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/failing.ts",
        r#"export default function(kiana) { kiana.registerTool({ name: "bad", label: "bad", description: "d", parameters: [] }); }"#,
    );
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        format!(
            "Failed to load extension: Tool \"bad\" registered by extension \"{}\" must define an object parameter schema.",
            path.to_string_lossy()
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn command_name_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/failing.ts",
        r#"export default function(kiana) { kiana.registerCommand("", { handler: async () => {} }); }"#,
    );
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        format!(
            "Failed to load extension: Command registered by extension \"{}\" must have a non-empty string name. Use kiana.registerCommand(\"name\", {{ description, handler }}).",
            path.to_string_lossy()
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn command_handler_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/failing.ts",
        r#"export default function(kiana) { kiana.registerCommand("x", {}); }"#,
    );
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        format!(
            "Failed to load extension: Command \"/x\" registered by extension \"{}\" must define handler().",
            path.to_string_lossy()
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn flag_default_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/failing.ts",
        r#"export default function(kiana) { kiana.registerFlag("f", { type: "boolean", default: "x" }); }"#,
    );
    let result = load_extensions(
        &[root
            .path()
            .join("extensions/failing.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        "Failed to load extension: Invalid default for flag \"f\": expected boolean, got string"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_config_required_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/failing.ts",
        r#"export default function(kiana) { kiana.registerProvider("p"); }"#,
    );
    let result = load_extensions(
        &[root
            .path()
            .join("extensions/failing.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        "Failed to load extension: Provider config is required when registering by name"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn uninitialized_action_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let error = load_error_for(
        &harness,
        r#"export default function(kiana) { kiana.sendMessage({ customType: "x", content: "y" }); }"#,
    )
    .await;
    assert_eq!(
        error,
        format!("Failed to load extension: {NOT_INITIALIZED_MESSAGE}")
    );
    assert_eq!(
        NOT_INITIALIZED_MESSAGE,
        "Extension runtime not initialized. Action methods cannot be called during extension loading."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn set_model_rejection_is_byte_exact() {
    let harness = LoaderHarness::new();
    let error = load_error_for(
        &harness,
        r#"export default async function(kiana) { await kiana.setModel({}); }"#,
    )
    .await;
    assert_eq!(
        error,
        "Failed to load extension: Extension runtime not initialized"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_mcp_server_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/failing.ts",
        r#"export default function(kiana) { kiana.registerMcpServer("srv", {}); }"#,
    );
    let validated =
        validate_mcp_server_config("srv", &json!({})).expect_err("empty config is invalid");
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        format!(
            "Failed to load extension: Invalid MCP server registered by extension \"{}\": {validated}",
            path.to_string_lossy()
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_mcp_server_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let first = write_file(
        root.path(),
        "extensions/first.ts",
        r#"export default function(kiana) { kiana.registerMcpServer("srv", { command: "echo" }); }"#,
    );
    let second = write_file(
        root.path(),
        "extensions/second.ts",
        r#"export default function(kiana) { kiana.registerMcpServer("srv", { command: "echo" }); }"#,
    );
    let result = load_extensions(
        &[
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(
        result.errors[0].error,
        format!(
            "Failed to load extension: MCP server \"srv\" is already registered by extension \"{}\"",
            first.to_string_lossy()
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mcp_namespace_conflict_error_is_byte_exact() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let first = write_file(
        root.path(),
        "extensions/first.ts",
        r#"export default function(kiana) { kiana.registerMcpServer("my-server", { command: "echo" }); }"#,
    );
    let second = write_file(
        root.path(),
        "extensions/second.ts",
        r#"export default function(kiana) { kiana.registerMcpServer("my_server", { command: "echo" }); }"#,
    );
    let result = load_extensions(
        &[
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert_eq!(
        result.errors[0].error,
        "Failed to load extension: MCP server \"my_server\" conflicts with registered server \"my-server\""
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stale_message_is_byte_exact() {
    let runtime = create_extension_runtime();
    let captured: Rc<RefCell<Option<Arc<dyn ExtensionApi>>>> = Rc::new(RefCell::new(None));
    let captured_for_factory = Rc::clone(&captured);
    let extension = load_extension_from_factory(
        Arc::new(move |kiana| {
            let captured = Rc::clone(&captured_for_factory);
            Box::pin(async move {
                *captured.borrow_mut() = Some(kiana);
            })
        }),
        &cwd(),
        create_event_bus() as Arc<dyn EventBus>,
        Arc::clone(&runtime),
        Some("<inline>"),
    )
    .await
    .expect("extension loads");
    assert_eq!(extension.path, "<inline>");

    runtime_invalidate(&runtime, None);
    let api = captured.borrow().clone().expect("captured api");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = api.get_commands();
    }))
    .expect_err("stale api throws");
    assert_eq!(panic_message(panic), DEFAULT_STALE_MESSAGE);
    assert_eq!(
        DEFAULT_STALE_MESSAGE,
        "This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload()."
    );
}

/// 回归（minor）：`invalidate(Some(""))` 按 TS 的 truthiness 处理。
///
/// `loader.ts` 的 `assertActive` 是 `if (state.staleMessage) throw`、`invalidate` 是
/// `if (state.staleMessage) return;`——空串都不算陈旧。修复前 `RuntimeShared` 用
/// `Option::is_some()`：空串会让所有 runtime-backed 调用以空消息 panic，并且后续
/// `invalidate()` 被提前返回。
#[tokio::test(flavor = "current_thread")]
async fn empty_stale_message_is_falsy_for_runtime_invalidate() {
    let runtime = create_extension_runtime();
    let captured: Rc<RefCell<Option<Arc<dyn ExtensionApi>>>> = Rc::new(RefCell::new(None));
    let captured_for_factory = Rc::clone(&captured);
    let extension = load_extension_from_factory(
        Arc::new(move |kiana| {
            let captured = Rc::clone(&captured_for_factory);
            Box::pin(async move {
                *captured.borrow_mut() = Some(kiana);
            })
        }),
        &cwd(),
        create_event_bus() as Arc<dyn EventBus>,
        Arc::clone(&runtime),
        Some("<inline-empty-stale>"),
    )
    .await
    .expect("extension loads");
    assert_eq!(extension.path, "<inline-empty-stale>");

    let api = captured.borrow().clone().expect("captured api");
    runtime_invalidate(&runtime, Some(""));
    // `assertActive()`：空串不抛（`getFlag` 只做活跃检查；未注册的 flag 返回 None）。
    assert_eq!(api.get_flag("missing"), None);
    // `invalidate()`：已存的空串不阻止后续调用继续设置文案。
    runtime_invalidate(&runtime, Some("real"));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = api.get_flag("missing");
    }))
    .expect_err("stale api throws");
    assert_eq!(panic_message(panic), "real");
}

#[tokio::test(flavor = "current_thread")]
async fn loaded_extension_keeps_its_command() {
    // 兜底：确认上面的错误用例没有把正常路径带坏。
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(root.path(), "extensions/ok.ts", COMMAND_EXTENSION);
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.extensions[0].commands.contains_key("test"));
}

// ===========================================================================
// 回归测试：bindCore 之后 provider / virtual model 动作立即生效
// ===========================================================================

use kiana_ext_api::types::ExtensionActions;
use kiana_extensions::loader::{
    ProviderRuntimeActions, bind_runtime_actions, bind_runtime_provider_actions,
};

#[tokio::test(flavor = "current_thread")]
async fn bound_provider_actions_take_effect_immediately() {
    let runtime = create_extension_runtime();
    let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let registered = Arc::clone(&calls);
    let unregistered = Arc::clone(&calls);
    let virtual_registered = Arc::clone(&calls);
    let virtual_unregistered = Arc::clone(&calls);
    bind_runtime_provider_actions(
        &runtime,
        ProviderRuntimeActions {
            register_provider: Some(Arc::new(move |name, _config| {
                registered.lock().unwrap().push(format!("register:{name}"));
            })),
            register_native_provider: Some(Arc::new(|_provider| {})),
            unregister_provider: Some(Arc::new(move |name| {
                unregistered
                    .lock()
                    .unwrap()
                    .push(format!("unregister:{name}"));
            })),
            register_virtual_model: Some(Arc::new(move |definition| {
                virtual_registered
                    .lock()
                    .unwrap()
                    .push(format!("virtual:{}", definition.id));
            })),
            unregister_virtual_model: Some(Arc::new(move |provider, id| {
                virtual_unregistered
                    .lock()
                    .unwrap()
                    .push(format!("unvirtual:{provider}/{id}"));
            })),
        },
    );

    // `bindCore` 之后的调用：不再进 pending 队列（旧实现全部丢失）。
    (runtime.register_provider)(
        "post-bind".to_string(),
        provider_config(),
        Some("<ext>".to_string()),
    );
    (runtime.unregister_provider)("post-bind".to_string(), Some("<ext>".to_string()));
    (runtime.register_virtual_model)(
        kiana_session_core::virtual_models::VirtualModelDefinition {
            provider: "p".to_string(),
            id: "m".to_string(),
            name: "M".to_string(),
            thinking_levels: None,
            context_window: None,
            max_tokens: None,
            input: None,
            route: None,
        },
        Some("<ext>".to_string()),
    );
    (runtime.unregister_virtual_model)("p".to_string(), "m".to_string());

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        [
            "register:post-bind".to_string(),
            "unregister:post-bind".to_string(),
            "virtual:m".to_string(),
            "unvirtual:p/m".to_string(),
        ]
    );
    assert!(runtime_pending_provider_registrations(&runtime).is_empty());
    assert!(
        kiana_extensions::loader::runtime_pending_virtual_model_registrations(&runtime).is_empty()
    );
}

/// 最小 `ExtensionActions`（只填 `getAllTools`，其余用空实现）。
fn actions_with_tools(tools: Vec<kiana_ext_api::types::ToolInfo>) -> ExtensionActions {
    use kiana_config::settings::Settings;
    use kiana_core::slash_commands::SlashCommandInfo;
    ExtensionActions {
        send_message: Arc::new(|_options| {}),
        send_user_message: Arc::new(|_content, _options| {}),
        append_entry: Arc::new(|_custom_type, _data| {}),
        set_session_name: Arc::new(|_name| {}),
        get_session_name: Arc::new(|| None),
        set_label: Arc::new(|_entry_id, _label| {}),
        get_active_tools: Arc::new(Vec::new),
        get_all_tools: Arc::new(move || tools.clone()),
        get_settings: Arc::new(Settings::default),
        set_active_tools: Arc::new(|_names| {}),
        refresh_tools: Arc::new(|| {}),
        get_commands: Arc::new(Vec::<SlashCommandInfo>::new),
        set_model: Arc::new(|_model| Box::pin(async { false })),
        get_thinking_level: Arc::new(|| kiana_agent_core::ThinkingLevel::Off),
        set_thinking_level: Arc::new(|_level| {}),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn get_all_tools_exposes_reaches_tools() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/all-tools.ts",
        r#"
export default function(kiana) {
    const tools = kiana.getAllTools();
    const tool = tools[0];
    globalThis.__allToolsState = {
        count: tools.length,
        hasReaches: "reachesTools" in tool,
        type: typeof tool.reachesTools,
        result: tool.reachesTools({}),
    };
}
"#,
    );

    let runtime = create_extension_runtime();
    bind_runtime_actions(
        &runtime,
        actions_with_tools(vec![kiana_ext_api::types::ToolInfo {
            name: "t".to_string(),
            description: "d".to_string(),
            parameters: json!({"type": "object"}),
            prompt_guidelines: Some(vec!["g".to_string()]),
            reaches_tools: Some(Arc::new(|_input| {
                kiana_ext_api::tool_types::ReachesToolsResult::Names(vec!["reached".to_string()])
            })),
            exposure: kiana_ext_api::tool_types::ToolExposure::Direct,
            namespace: None,
            annotations: None,
            source_info: kiana_core::source_info::SourceInfo {
                path: "t".to_string(),
                source: "local".to_string(),
                scope: kiana_core::source_info::SourceScope::Temporary,
                origin: kiana_core::source_info::SourceOrigin::TopLevel,
                base_dir: None,
            },
        }]),
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        Some(runtime),
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let text = harness
        .engine
        .eval_script::<String>("JSON.stringify(globalThis.__allToolsState)", "<all-tools>")
        .expect("read all tools state");
    let state: serde_json::Value = serde_json::from_str(&text).expect("json");
    // 回归：getAllTools 曾丢掉 reachesTools（JS 侧只能看到普通对象）。
    assert_eq!(state["count"], 1);
    assert_eq!(state["hasReaches"], true);
    assert_eq!(state["type"], "function");
    assert_eq!(state["result"], json!(["reached"]));
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_factory_panic_is_converted_to_a_load_error() {
    let runtime = create_extension_runtime();
    let factory: ExtensionFactory =
        Arc::new(|_kiana| -> kiana_ext_api::LocalBoxFuture<'static, ()> {
            panic!("sync factory panic")
        });

    let error = load_extension_from_factory(
        factory,
        &cwd(),
        create_event_bus(),
        runtime,
        Some("<sync-panic>"),
    )
    .await
    .expect_err("factory panics before returning a future");
    // 回归：同步 panic 曾直接穿出 `load_extension_from_factory`（TS 的 try/catch 会接住）。
    assert_eq!(error, "sync factory panic");
}

// ===========================================================================
// 回归：晚注册（TS 的对象身份）与 API 表面差异
// ===========================================================================

use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute};
use kiana_extensions::loader::{
    live_extension, refresh_extension, refresh_extensions,
    runtime_pending_native_provider_registrations, runtime_pending_virtual_model_registrations,
    runtime_provider_config_errors,
};

struct NoopExecute;

impl ToolDefinitionExecute for NoopExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<
        'a,
        Result<
            kiana_agent_core::AgentToolResult<serde_json::Value>,
            kiana_agent_core::AgentToolError,
        >,
    > {
        Box::pin(async {
            Ok(kiana_agent_core::AgentToolResult {
                content: Vec::new(),
                details: serde_json::Value::Null,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn minimal_tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: String::new(),
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
        execute: Arc::new(NoopExecute),
        render_call: None,
        render_result: None,
    }
}

/// TS：`createExtensionAPI` 闭包里的 `extension` 就是 `initializeExtension` 返回的对象，
/// 工厂结束后通过捕获的 `kiana` 注册的工具/命令/handler/flag 会直接出现在 runner 手里。
/// Rust 的 `Extension` 是值类型，实时状态由 [`live_extension`] 暴露，[`refresh_extension`]
/// 把快照同步到最新（见 loader 模块文档）。
#[tokio::test(flavor = "current_thread")]
async fn late_registrations_reach_the_live_extension_state() {
    let runtime = create_extension_runtime();
    let captured: Rc<RefCell<Option<Arc<dyn ExtensionApi>>>> = Rc::new(RefCell::new(None));
    let captured_for_factory = Rc::clone(&captured);
    let mut extension = load_extension_from_factory(
        Arc::new(move |kiana| {
            let captured = Rc::clone(&captured_for_factory);
            Box::pin(async move {
                *captured.borrow_mut() = Some(kiana);
            })
        }),
        &cwd(),
        create_event_bus(),
        Arc::clone(&runtime),
        Some("<late>"),
    )
    .await
    .expect("extension loads");
    assert!(extension.tools.is_empty());
    assert!(extension.commands.is_empty());
    assert!(extension.flags.is_empty());

    let api = captured.borrow().clone().expect("captured api");
    api.register_tool(minimal_tool("late-tool"));
    api.register_command(
        "late-command",
        kiana_ext_api::types::RegisterCommandOptions {
            description: None,
            get_argument_completions: None,
            handler: Arc::new(|_args, _ctx| Box::pin(async {})),
        },
    );
    api.register_flag(
        "late-flag",
        RegisterFlagOptions::Boolean {
            description: None,
            default: Some(true),
        },
    );
    let handler: kiana_ext_api::events::ExtensionHandlerFn =
        Arc::new(|_event, _ctx| Box::pin(async { Ok(None) }));
    let _unsubscribe = api.on(
        kiana_ext_api::events::ExtensionEventKind::AgentStart,
        kiana_ext_api::events::ExtensionHandler::Standard(handler),
    );

    // 快照仍是旧的（与 TS 的“同一对象”不同，这是 `Extension` 值类型的已知偏离）。
    assert!(extension.tools.is_empty());
    let live = live_extension("<late>").expect("live extension is registered");
    assert!(live.tools.contains_key("late-tool"));
    assert!(live.commands.contains_key("late-command"));
    assert!(live.flags.contains_key("late-flag"));
    assert_eq!(live.handlers.get("agent_start").map(Vec::len), Some(1));
    assert_eq!(
        api.get_flag("late-flag"),
        Some(FlagValue::Bool(true)),
        "late flag default is applied immediately (active state)"
    );

    // 刷新后，runner 手里的快照看到全部晚注册。
    assert!(refresh_extension(&mut extension));
    assert!(extension.tools.contains_key("late-tool"));
    assert!(extension.commands.contains_key("late-command"));
    assert!(extension.flags.contains_key("late-flag"));
    assert_eq!(extension.handlers.get("agent_start").map(Vec::len), Some(1));
    assert_eq!(refresh_extensions(std::slice::from_mut(&mut extension)), 1);
}

async fn js_load_errors(harness: &LoaderHarness, source: &str) -> Vec<String> {
    let root = temp_root();
    write_file(root.path(), "extensions/probe.ts", source);
    let result = load_extensions(
        &[root
            .path()
            .join("extensions/probe.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    result.errors.into_iter().map(|error| error.error).collect()
}

async fn js_load_result(
    harness: &LoaderHarness,
    source: &str,
) -> (
    Vec<String>,
    Vec<kiana_ext_api::types::Extension>,
    Arc<kiana_ext_api::types::ExtensionRuntime>,
) {
    let root = temp_root();
    write_file(root.path(), "extensions/probe.ts", source);
    let result = load_extensions(
        &[root
            .path()
            .join("extensions/probe.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let _ = harness;
    (
        result.errors.into_iter().map(|error| error.error).collect(),
        result.extensions,
        result.runtime,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn null_flag_default_and_type_match_typescript() {
    let harness = LoaderHarness::new();
    let errors = js_load_errors(
        &harness,
        r#"export default function(kiana) { kiana.registerFlag("f", { type: "boolean", default: null }); }"#,
    )
    .await;
    assert_eq!(
        errors,
        vec!["Failed to load extension: Invalid default for flag \"f\": expected boolean, got object".to_string()]
    );

    let errors = js_load_errors(
        &harness,
        r#"export default function(kiana) { kiana.registerFlag("f", { type: null, default: "x" }); }"#,
    )
    .await;
    assert_eq!(
        errors,
        vec![
            "Failed to load extension: Invalid default for flag \"f\": expected null, got string"
                .to_string()
        ]
    );

    // `{ type: null }`（无默认值）在 TS 里能加载。
    let (errors, extensions, _) = js_load_result(
        &harness,
        r#"export default function(kiana) { kiana.registerFlag("f", { type: null }); }"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert!(extensions[0].flags.contains_key("f"));
}

#[tokio::test(flavor = "current_thread")]
async fn missing_flag_options_match_typescript_type_errors() {
    let harness = LoaderHarness::new();
    let errors = js_load_errors(
        &harness,
        r#"export default function(kiana) { kiana.registerFlag("f"); }"#,
    )
    .await;
    assert_eq!(
        errors,
        vec![
            "Failed to load extension: Cannot read properties of undefined (reading 'default')"
                .to_string()
        ]
    );

    let errors = js_load_errors(
        &harness,
        r#"export default function(kiana) { kiana.registerFlag("f", null); }"#,
    )
    .await;
    assert_eq!(
        errors,
        vec![
            "Failed to load extension: Cannot read properties of null (reading 'default')"
                .to_string()
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn numeric_flag_keys_do_not_collide_with_string_keys() {
    // TS 的 Map 键不做字符串化：`getFlag(5)` 命中、`getFlag("5")` 不命中。
    let harness = LoaderHarness::new();
    let (errors, extensions, _) = js_load_result(
        &harness,
        r#"
export default function(kiana) {
    kiana.registerFlag(5, { type: "boolean", default: true });
    kiana.registerFlag("6", { type: "string", default: "s" });
    if (kiana.getFlag(5) !== true) throw new Error("getFlag(5)");
    if (kiana.getFlag("5") !== undefined) throw new Error("getFlag('5')");
    if (kiana.getFlag("6") !== "s") throw new Error("getFlag('6')");
    if (kiana.getFlag(6) !== undefined) throw new Error("getFlag(6)");
}
"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(extensions.len(), 1);
    assert!(extensions[0].flags.contains_key("5"));
}

#[tokio::test(flavor = "current_thread")]
async fn non_function_handlers_and_renderers_do_not_fail_the_load() {
    // TS 的注册方法不做运行期类型检查：值原样保存，调用时才抛 TypeError。
    let harness = LoaderHarness::new();
    let (errors, extensions, _) = js_load_result(
        &harness,
        r#"
export default function(kiana) {
    kiana.on("agent_start", undefined);
    kiana.on("agent_start", null);
    kiana.registerMessageRenderer("m", 5);
    kiana.registerEntryRenderer("e", null);
    kiana.registerMarkdownTransformer(3);
    kiana.registerToolRenderer(5);
    kiana.events.on("channel", undefined);
    kiana.registerShortcut("ctrl+x", {});
}
"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(extensions.len(), 1);
    assert_eq!(
        extensions[0].handlers.get("agent_start").map(Vec::len),
        Some(2)
    );
    assert!(extensions[0].message_renderers.contains_key("m"));
}

#[tokio::test(flavor = "current_thread")]
async fn register_tool_argument_shapes_match_typescript() {
    let harness = LoaderHarness::new();
    let errors = js_load_errors(
        &harness,
        r#"export default function(kiana) { kiana.registerTool(undefined); }"#,
    )
    .await;
    assert_eq!(
        errors,
        vec![
            "Failed to load extension: Cannot read properties of undefined (reading 'parameters')"
                .to_string()
        ]
    );

    let errors = js_load_errors(
        &harness,
        r#"export default function(kiana) { kiana.registerTool(5); }"#,
    )
    .await;
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .starts_with("Failed to load extension: Tool \"undefined\" registered by extension \""),
        "{errors:?}"
    );
    assert!(
        errors[0].ends_with("/extensions/probe.ts\" must define an object parameter schema."),
        "{errors:?}"
    );

    for source in [
        r#"export default function(kiana) { kiana.registerTool({ name: "x", parameters: null }); }"#,
        r#"export default function(kiana) { kiana.registerTool({ name: "x", parameters: function () {} }); }"#,
        r#"export default function(kiana) { kiana.registerTool({ name: "x", parameters: [] }); }"#,
    ] {
        let errors = js_load_errors(&harness, source).await;
        assert_eq!(errors.len(), 1, "{source}: {errors:?}");
        assert!(
            errors[0].contains("Tool \"x\" registered by extension"),
            "{source}: {errors:?}"
        );
    }

    // 没有 name、schema 合法：TS 以 `undefined` 为键注册（扩展加载成功）。
    let (errors, extensions, _) = js_load_result(
        &harness,
        r#"export default function(kiana) { kiana.registerTool({ parameters: { type: "object" } }); }"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert!(extensions[0].tools.contains_key("undefined"));
}

#[tokio::test(flavor = "current_thread")]
async fn native_provider_objects_are_queued_for_bind_core() {
    let harness = LoaderHarness::new();
    let (errors, extensions, runtime) = js_load_result(
        &harness,
        r#"
export default function(kiana) {
    kiana.registerProvider({ id: "native", auth: {}, getModels: () => [], stream: () => {}, streamSimple: () => {} });
    kiana.registerCommand("still-loaded", { handler: async () => {} });
}
"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(extensions.len(), 1);
    assert!(extensions[0].commands.contains_key("still-loaded"));
    let pending = runtime_pending_native_provider_registrations(&runtime);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].provider.id(), "native");
    assert_eq!(pending[0].extension_path, extensions[0].path);
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_provider_models_do_not_fail_the_load() {
    let harness = LoaderHarness::new();
    let (errors, extensions, runtime) = js_load_result(
        &harness,
        r#"
export default function(kiana) {
    kiana.registerProvider("x", { models: [{}] });
    kiana.registerCommand("still-loaded", { handler: async () => {} });
}
"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(extensions.len(), 1);
    assert!(extensions[0].commands.contains_key("still-loaded"));
    // TS 在 `bindCore` 冲刷时抛错：provider 不注册、错误作为扩展错误上报。
    assert!(runtime_pending_provider_registrations(&runtime).is_empty());
    let deferred = runtime_provider_config_errors(&runtime);
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].name, "x");
    assert_eq!(deferred[0].extension_path, extensions[0].path);
    assert!(deferred[0].message.contains("invalid models"));
}

#[tokio::test(flavor = "current_thread")]
async fn null_virtual_model_is_accepted() {
    let harness = LoaderHarness::new();
    let (errors, extensions, runtime) = js_load_result(
        &harness,
        r#"export default function(kiana) { kiana.registerVirtualModel(null); }"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(extensions.len(), 1);
    let pending = runtime_pending_virtual_model_registrations(&runtime);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].definition.provider, "");
    assert_eq!(pending[0].definition.id, "");
}

#[tokio::test(flavor = "current_thread")]
async fn valid_provider_models_are_still_queued() {
    let harness = LoaderHarness::new();
    let (errors, extensions, runtime) = js_load_result(
        &harness,
        r#"
export default function(kiana) {
    kiana.registerProvider("v", {
        baseUrl: "https://provider.test/v1",
        api: "openai-completions",
        models: [{
            id: "m", name: "M", api: "openai-completions", baseUrl: "https://provider.test/v1",
            reasoning: false, input: ["text"],
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
            contextWindow: 1000, maxTokens: 100,
        }],
    });
}
"#,
    )
    .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(extensions.len(), 1);
    let pending = runtime_pending_provider_registrations(&runtime);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].name, "v");
    assert_eq!(pending[0].config.models.as_ref().map(Vec::len), Some(1));
    assert!(runtime_provider_config_errors(&runtime).is_empty());
}
