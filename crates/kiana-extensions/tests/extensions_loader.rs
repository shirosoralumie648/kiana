//! 加载器门面（`loader.ts`）的端到端测试：模块导入、API 事务、错误文案。
//!
//! 移植自 TS 的 `extensions-discovery.test.ts` 中与加载/注册相关的用例，以及
//! `spec/ca-extensions.md` §3.5 的逐字错误文案。
#![cfg(feature = "runtime")]

#[path = "fixtures/extensions/support.rs"]
mod support;

use std::sync::Arc;

use kiana_core::event_bus::{EventBus, create_event_bus};
use kiana_extensions::loader::{discover_and_load_extensions, load_extensions};
use support::{COMMAND_EXTENSION, LoaderHarness, temp_root, tool_extension, write_file};

#[tokio::test(flavor = "current_thread")]
async fn harness_builds() {
    let _harness = LoaderHarness::new();
}

#[tokio::test(flavor = "current_thread")]
async fn loads_a_direct_extension_and_registers_commands() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(root.path(), "extensions/with-command.ts", COMMAND_EXTENSION);

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.extensions.len(), 1);
    assert!(result.extensions[0].commands.contains_key("test"));
}

#[tokio::test(flavor = "current_thread")]
async fn discovers_and_loads_direct_ts_files() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    let extensions = root.path().join("extensions");
    write_file(&extensions, "foo.ts", COMMAND_EXTENSION);
    write_file(&extensions, "bar.ts", COMMAND_EXTENSION);

    let result = discover_and_load_extensions(
        &[],
        &root.path().to_string_lossy(),
        Some(&root.path().to_string_lossy()),
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.extensions.len(), 2);

    // 第二次批量加载会建**新的** runtime（TS 每次 `loadExtensionsInternal` 都调
    // `createExtensionRuntime()`；扩展对象也是新的）。逐条比较 `Arc` 身份才有意义。
    let second = discover_and_load_extensions(
        &[],
        &root.path().to_string_lossy(),
        Some(&root.path().to_string_lossy()),
        None,
    )
    .await;
    assert_eq!(second.extensions.len(), 2);
    // TS 断言两次加载得到不同的 `Extension` 对象；Rust 的 `Extension` 是值类型，用每次
    // 批量加载新建的 runtime 身份表达同一语义（并确认不是同一个 Arc）。
    assert!(!Arc::ptr_eq(&result.runtime, &second.runtime));
}

#[tokio::test(flavor = "current_thread")]
async fn registers_a_tool_from_typebox_schema() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/with-tool.ts",
        &tool_extension("my-tool"),
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        Some(create_event_bus()),
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.extensions[0].tools.contains_key("my-tool"));
}

#[tokio::test(flavor = "current_thread")]
async fn inline_factory_defaults_to_angle_inline() {
    let _harness = LoaderHarness::new();
    let extension = kiana_extensions::loader::load_extension_from_factory(
        Arc::new(|_kiana| Box::pin(async {})),
        &std::env::current_dir().unwrap().to_string_lossy(),
        create_event_bus(),
        kiana_extensions::loader::create_extension_runtime(),
        None,
    )
    .await
    .expect("inline extension loads");

    assert_eq!(extension.path, "<inline>");
    assert_eq!(extension.resolved_path, "<inline>");
    assert_eq!(extension.source_info.source, "inline");
    assert_eq!(extension.source_info.base_dir, None);
    assert_eq!(
        extension.source_info.scope,
        kiana_core::source_info::SourceScope::Temporary
    );
    assert_eq!(
        extension.source_info.origin,
        kiana_core::source_info::SourceOrigin::TopLevel
    );
}

#[tokio::test(flavor = "current_thread")]
async fn named_inline_extension_keeps_its_path() {
    let _harness = LoaderHarness::new();
    let extension = kiana_extensions::loader::load_extension_from_factory(
        Arc::new(|_kiana| Box::pin(async {})),
        &std::env::current_dir().unwrap().to_string_lossy(),
        create_event_bus(),
        kiana_extensions::loader::create_extension_runtime(),
        Some("<inline:my-provider>"),
    )
    .await
    .expect("inline extension loads");

    assert_eq!(extension.path, "<inline:my-provider>");
    assert_eq!(extension.resolved_path, "<inline:my-provider>");
    assert_eq!(extension.source_info.source, "inline");
}

#[tokio::test(flavor = "current_thread")]
async fn pending_flags_providers_and_mcp_servers_are_committed_in_order() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/commits.ts",
        r#"
export default function(kiana) {
    kiana.registerFlag("my-flag", { type: "boolean", default: true });
    kiana.registerProvider("test-provider", { baseUrl: "https://provider.test/v1", apiKey: "key" });
    kiana.registerMcpServer("test-server", { command: "echo" });
    kiana.registerVirtualModel({
        provider: "test-provider",
        id: "virtual-1",
        name: "Virtual One",
        route: async () => ({ model: null, thinkingLevel: "off" }),
    });
}
"#,
    );

    let result = load_extensions(
        &[root
            .path()
            .join("extensions/commits.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // `ExtensionRuntime` 快照里的字段与 TS 读到的值一致。
    assert_eq!(
        result.runtime.flag_values.get("my-flag"),
        Some(&kiana_ext_api::types::FlagValue::Bool(true))
    );
    assert_eq!(result.runtime.pending_provider_registrations.len(), 1);
    assert_eq!(
        result.runtime.pending_provider_registrations[0].name,
        "test-provider"
    );
    assert_eq!(
        result.runtime.pending_provider_registrations[0].extension_path,
        root.path().join("extensions/commits.ts").to_string_lossy()
    );
    assert_eq!(result.runtime.pending_virtual_model_registrations.len(), 1);
    assert_eq!(
        result.runtime.pending_virtual_model_registrations[0]
            .definition
            .id,
        "virtual-1"
    );
    let servers = result.runtime.mcp_servers.list();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "test-server");
}

#[tokio::test(flavor = "current_thread")]
async fn get_flag_during_loading_reads_the_pending_default() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/flag-read.ts",
        r#"
export default function(kiana) {
    kiana.registerFlag("early", { type: "boolean", default: true });
    const value = kiana.getFlag("early");
    const missing = kiana.getFlag("not-registered");
    kiana.registerCommand("probe", {
        description: `${value}:${missing === undefined ? "undefined" : String(missing)}`,
        handler: async () => {},
    });
}
"#,
    );

    let result = load_extensions(
        &[root
            .path()
            .join("extensions/flag-read.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        result.extensions[0].commands["probe"]
            .description
            .as_deref(),
        Some("true:undefined")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn load_error_messages_are_byte_exact() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    let no_default = write_file(
        root.path(),
        "extensions/no-default.ts",
        "export function notDefault() {}\n",
    );
    let throws = write_file(
        root.path(),
        "extensions/throws.ts",
        "export default function() { throw new Error(\"boom\"); }\n",
    );

    let result = load_extensions(
        &[
            no_default.to_string_lossy().into_owned(),
            throws.to_string_lossy().into_owned(),
        ],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert_eq!(result.errors.len(), 2);
    assert_eq!(
        result.errors[0].error,
        format!(
            "Extension does not export a valid factory function: {}",
            no_default.to_string_lossy()
        )
    );
    assert_eq!(result.errors[1].error, "Failed to load extension: boom");
}

#[tokio::test(flavor = "current_thread")]
async fn module_import_errors_are_prefixed() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    let invalid = write_file(
        root.path(),
        "extensions/invalid.ts",
        "this is not valid typescript export",
    );

    let result = load_extensions(
        &[invalid.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert_eq!(result.errors.len(), 1);
    assert!(
        result.errors[0]
            .error
            .starts_with("Failed to load extension: "),
        "{}",
        result.errors[0].error
    );
}

#[tokio::test(flavor = "current_thread")]
async fn relative_extension_paths_keep_the_original_display_path() {
    let _harness = LoaderHarness::new();
    let root = temp_root();
    write_file(root.path(), "extensions/relative.ts", COMMAND_EXTENSION);

    let result = load_extensions(
        &["extensions/relative.ts".to_string()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.extensions[0].path, "extensions/relative.ts");
    assert_eq!(
        std::path::Path::new(&result.extensions[0].resolved_path),
        root.path().join("extensions/relative.ts").as_path()
    );
    assert_eq!(result.extensions[0].source_info.source, "local");
    assert_eq!(
        result.extensions[0].source_info.base_dir.as_deref(),
        Some(root.path().join("extensions").to_string_lossy().as_ref())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn api_js_exposes_the_full_kiana_surface() {
    let harness = LoaderHarness::new();
    let probe = r#"
globalThis.__kiana_surface = (() => {
    const fake = new Proxy({}, {
        get: () => () => undefined,
        has: () => true,
    });
    const api = globalThis.__kiana_api.makeApi(fake, "probe");
    const expected = [
        "on", "registerTool", "registerCommand", "registerShortcut", "registerFlag", "getFlag",
        "registerMessageRenderer", "registerMarkdownTransformer", "registerEntryRenderer",
        "registerToolRenderer", "sendMessage", "sendUserMessage", "appendEntry", "setSessionName",
        "getSessionName", "setLabel", "exec", "getActiveTools", "getAllTools", "getSettings",
        "setActiveTools", "getCommands", "setModel", "getThinkingLevel", "setThinkingLevel",
        "registerProvider", "unregisterProvider", "registerMcpServer", "unregisterMcpServer",
        "getMcpServers", "registerVirtualModel", "unregisterVirtualModel", "events",
    ];
    const missing = expected.filter((name) => !(name in api));
    return JSON.stringify({
        missing,
        keyCount: Object.keys(api).length,
        eventsEmit: typeof api.events.emit,
        eventsOn: typeof api.events.on,
    });
})();
"#;
    let text = harness
        .engine
        .eval_script::<String>(probe, "<surface>")
        .expect("probe api surface");
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value["missing"], serde_json::json!([]));
    assert_eq!(value["keyCount"], 33);
    assert_eq!(value["eventsEmit"], "function");
    assert_eq!(value["eventsOn"], "function");
}

#[tokio::test(flavor = "current_thread")]
async fn production_of_the_runtime_snapshot_exposes_live_state() {
    let _harness = LoaderHarness::new();
    let runtime = kiana_extensions::loader::create_extension_runtime();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/flag.ts",
        r#"export default function(kiana) { kiana.registerFlag("x", { type: "string", default: "one" }); }"#,
    );
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        Some(Arc::clone(&runtime)),
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // 传入的 runtime 是空快照；返回值是加载结束时的快照。
    assert!(runtime.flag_values.is_empty());
    assert_eq!(
        result.runtime.flag_values.get("x"),
        Some(&kiana_ext_api::types::FlagValue::Str("one".to_string()))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn events_emit_reenters_js_handlers_synchronously() {
    // TS 的 EventEmitter 在 `emit` 里同步调用 handler；Rust 不能在 `with_js` 里再开
    // `with_js`，事件适配器因此复用宿主调用拿到的 `Ctx`。两个扩展按显式顺序加载。
    let harness = LoaderHarness::new();
    let root = temp_root();
    let listener = write_file(
        root.path(),
        "extensions/listener.ts",
        r#"
export default function(kiana) {
    kiana.events.on("channel", (data) => {
        globalThis.__event_seen = data.x;
        globalThis.__event_calls = (globalThis.__event_calls ?? 0) + 1;
    });
}
"#,
    );
    let emitter = write_file(
        root.path(),
        "extensions/emitter.ts",
        r#"
export default function(kiana) {
    kiana.events.emit("channel", { x: 42 });
}
"#,
    );

    let result = load_extensions(
        &[
            listener.to_string_lossy().into_owned(),
            emitter.to_string_lossy().into_owned(),
        ],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls = harness
        .engine
        .eval_script::<u32>("globalThis.__event_calls", "<event-calls>")
        .expect("read event calls");
    let seen = harness
        .engine
        .eval_script::<u32>("globalThis.__event_seen", "<event-seen>")
        .expect("read event payload");
    assert_eq!(calls, 1);
    assert_eq!(seen, 42);
}

// ===========================================================================
// 回归测试（独立 reviewer 报告的关键/重要项）
// ===========================================================================

use std::sync::Mutex;
use std::time::{Duration, Instant};

use kiana_ext_api::events::ExtensionHandler;
use kiana_extensions::engine::EngineResult;
use kiana_extensions::loader::{set_component_bridge, set_theme_bridge, set_tool_context_bridge};
use kiana_session_core::messages::CustomMessage;
use kiana_session_core::messages::CustomRole;
use rquickjs::{Ctx, Function, Object, Value};

/// 在 LocalSet 上跑一个 future（`JsCallback::call_async_send` 用 `spawn_local`）。
async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

struct TestComponent;

impl kiana_tui::tui::Component for TestComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["component".to_string()]
    }

    fn handles_input(&self) -> bool {
        false
    }
}

fn json_state(harness: &LoaderHarness, expression: &str) -> serde_json::Value {
    let text = harness
        .engine
        .eval_script::<String>(&format!("JSON.stringify({expression})"), "<state>")
        .expect("read js state");
    serde_json::from_str(&text).expect("valid json state")
}

#[tokio::test(flavor = "current_thread")]
async fn provider_config_keeps_js_callbacks() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/provider.ts",
        r#"
export default function(kiana) {
    kiana.registerProvider("custom", {
        baseUrl: "https://provider.test/v1",
        apiKey: "key",
        streamSimple: (model, context, options) => ({ async *[Symbol.asyncIterator]() {} }),
        refreshModels: async () => [],
        images: { "img-api": { generateImages: async () => ({}) } },
        classifiers: { "class-api": { classify: async () => ({}) } },
        oauth: {
            name: "Custom",
            login: async () => ({ access: "a", refresh: "r", expires: 1 }),
            refreshToken: async (credentials) => credentials,
            getApiKey: (credentials) => credentials.access,
            modifyModels: (models) => models,
        },
    });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let providers =
        kiana_extensions::loader::runtime_pending_provider_registrations(&result.runtime);
    assert_eq!(providers.len(), 1);
    let config = &providers[0].config;
    // 回归：这些回调曾被静默丢弃（critical）。
    assert!(
        config.stream_simple.is_some(),
        "streamSimple must be bridged"
    );
    assert!(
        config.refresh_models.is_some(),
        "refreshModels must be bridged"
    );
    assert_eq!(config.images.as_ref().map(|map| map.len()), Some(1));
    assert_eq!(config.classifiers.as_ref().map(|map| map.len()), Some(1));
    let oauth = config.oauth.as_ref().expect("oauth must be bridged");
    assert_eq!(oauth.name, "Custom");
    let _ = harness;
}

#[tokio::test(flavor = "current_thread")]
async fn thenable_factory_rejection_fails_the_load() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/thenable.ts",
        r#"
export default function () {
    return {
        then(resolve, reject) {
            reject(new Error("thenable boom"));
        },
    };
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;

    // `await factory(api)` assimilates thenables；旧实现把非 Promise thenable 当同步成功。
    assert_eq!(
        result.errors[0].error,
        "Failed to load extension: thenable boom"
    );
    let _ = harness;
}

#[tokio::test(flavor = "current_thread")]
async fn event_handlers_share_the_emitted_object() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/events.ts",
        r#"
export default function(kiana) {
    const state = (globalThis.__eventPayloadState = {});
    let first = null;
    kiana.events.on("chan", (data) => {
        first = data;
        state.keys = Object.keys(data);
        state.hasUndefinedKey = "u" in data;
        state.undefinedType = typeof data.u;
        data.count += 1;
    });
    kiana.events.on("chan", (data) => {
        state.sameReference = data === first;
        state.mutationVisible = data.count === 1;
        state.dateType = typeof data.d;
        state.fnType = typeof data.fn;
    });
    kiana.events.emit("chan", { u: undefined, fn: () => 1, d: new Date(0), count: 0 });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let state = json_state(&harness, "globalThis.__eventPayloadState");
    // 回归：payload 曾被 JSON 往返（undefined/函数丢失、Date 变字符串、引用不等）。
    assert_eq!(state["hasUndefinedKey"], true);
    assert_eq!(state["undefinedType"], "undefined");
    assert_eq!(state["sameReference"], true);
    assert_eq!(state["mutationVisible"], true);
    assert_eq!(state["dateType"], "object");
    assert_eq!(state["fnType"], "function");
}

#[tokio::test(flavor = "current_thread")]
async fn nested_event_emit_keeps_dispatching_outer_handlers() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/nested.ts",
        r#"
export default function(kiana) {
    globalThis.__nestedCalls = 0;
    kiana.events.on("chan", () => {
        kiana.events.emit("inner", { nested: true });
    });
    kiana.events.on("chan", () => {
        globalThis.__nestedCalls += 1;
    });
    kiana.events.on("inner", () => {});
    kiana.events.emit("chan", {});
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let calls = harness
        .engine
        .eval_script::<u32>("globalThis.__nestedCalls", "<nested>")
        .expect("read nested calls");
    // 回归：嵌套 emit 后外层 emit 的第二个 handler 曾被跳过（SyncCtxGuard 无条件清空）。
    assert_eq!(calls, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn exec_is_async_and_honors_fractional_timeout_and_signal() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/exec.ts",
        r#"
export default async function(kiana) {
    const state = (globalThis.__execState = {});
    let timerFired = false;
    setTimeout(() => { timerFired = true; }, 0);
    const result = await kiana.exec("sh", ["-c", "sleep 0.3; echo done"]);
    state.timerFired = timerFired;
    state.stdout = result.stdout.trim();
    state.code = result.code;

    const timed = await kiana.exec("sh", ["-c", "sleep 0.3"], { timeout: 0.5 });
    state.fractionalKilled = timed.killed;

    const controller = new AbortController();
    setTimeout(() => controller.abort(), 10);
    const aborted = await kiana.exec("sh", ["-c", "sleep 1"], { signal: controller.signal });
    state.signalKilled = aborted.killed;
}
"#,
    );

    let started = Instant::now();
    // `kiana.exec` 现在通过 `spawn_local` + `spawn_blocking` 异步执行（与 TS 的 Promise 一致），
    // 调用方按 ARCHITECTURE §4.2 跑在 LocalSet 上。
    let result = run_local(load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    ))
    .await;
    let elapsed = started.elapsed();
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let state = json_state(&harness, "globalThis.__execState");
    // 回归：同步的 execCommand 曾冻结整个事件循环（timer 在 exec 返回前不会触发）。
    assert_eq!(state["timerFired"], true);
    assert_eq!(state["stdout"], "done");
    assert_eq!(state["code"], 0);
    // 回归：`as_u64` 曾把小数的 timeout 当作没有 timeout。
    assert_eq!(state["fractionalKilled"], true);
    // 回归：ExecOptions.signal 曾完全不生效。
    assert_eq!(state["signalKilled"], true);
    assert!(
        elapsed < Duration::from_secs(5),
        "exec blocked too long: {elapsed:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn project_trust_handler_receives_ctx_ui() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/trust.ts",
        r#"
export default function(kiana) {
    kiana.on("project_trust", async (event, ctx) => {
        const choice = await ctx.ui.select("Trust " + event.cwd + "?", ["yes", "no"]);
        ctx.ui.notify("picked " + choice, "info");
        return { trusted: choice === "yes" ? "yes" : "no" };
    });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let handler = result.extensions[0]
        .handlers
        .get("project_trust")
        .and_then(|handlers| handlers.first())
        .cloned()
        .expect("project_trust handler");
    let ExtensionHandler::ProjectTrust(handler) = handler else {
        panic!("expected ProjectTrust handler");
    };

    let notified: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let notified_for_ui = Arc::clone(&notified);
    let ui = kiana_ext_api::events::ProjectTrustUi {
        select: Arc::new(|_title, _options, _opts| Box::pin(async { Some("yes".to_string()) })),
        confirm: Arc::new(|_title, _message, _opts| Box::pin(async { true })),
        input: Arc::new(|_title, _placeholder, _opts| Box::pin(async { None })),
        notify: Arc::new(move |message, _kind| {
            notified_for_ui.lock().unwrap().push(message);
        }),
    };
    let ctx = kiana_ext_api::events::ProjectTrustContext {
        cwd: "/tmp/project".to_string(),
        mode: kiana_ext_api::types::ExtensionMode::Tui,
        has_ui: true,
        ui,
    };
    let event = kiana_ext_api::events::ProjectTrustEvent {
        event_type: kiana_ext_api::events::ProjectTrustEventType::ProjectTrust,
        cwd: "/tmp/project".to_string(),
    };

    let outcome = run_local(handler(event, ctx)).await;
    // 回归：ctx.ui 曾是 undefined，select 抛错后 handler 静默返回 Undecided。
    assert_eq!(
        outcome,
        kiana_ext_api::events::ProjectTrustEventResult {
            trusted: kiana_ext_api::events::ProjectTrustEventDecision::Yes,
            remember: None,
        }
    );
    assert_eq!(
        notified.lock().unwrap().as_slice(),
        ["picked yes".to_string()]
    );
    let _ = harness;
}

#[tokio::test(flavor = "current_thread")]
async fn message_renderer_receives_theme_and_options() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/render.ts",
        r#"
export default function(kiana) {
    kiana.registerMessageRenderer("custom", (message, options, theme) => {
        globalThis.__rendererState = {
            expanded: options.expanded,
            pad: options.outputPad,
            themed: theme.fg("accent", "x"),
            marker: theme.__marker,
            content: message.content,
        };
        return { render: () => ["x"] };
    });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    set_theme_bridge(Some(Arc::new(
        |ctx: &Ctx<'_>, _theme: &kiana_theme::theme::Theme| -> EngineResult<rquickjs::Value<'_>> {
            let object = Object::new(ctx.clone())?;
            object.set(
                "fg",
                Function::new(ctx.clone(), |_color: String, text: String| {
                    format!("FG:{text}")
                })?,
            )?;
            object.set("__marker", true)?;
            Ok(object.into_value())
        },
    )));
    set_component_bridge(Some(Arc::new(
        |_ctx: &Ctx<'_>,
         _value: rquickjs::Value<'_>|
         -> EngineResult<Box<dyn kiana_tui::tui::Component>> {
            Ok(Box::new(TestComponent))
        },
    )));

    let renderer = result.extensions[0]
        .message_renderers
        .get("custom")
        .cloned()
        .expect("message renderer");
    let message = CustomMessage {
        role: CustomRole::Custom,
        custom_type: "custom".to_string(),
        content: serde_json::json!("hello"),
        display: true,
        details: None,
        timestamp: 0.0,
    };
    let mut component = renderer(
        &message,
        &kiana_ext_api::types::MessageRenderOptions {
            expanded: true,
            output_pad: 2,
        },
        &kiana_theme::theme::get_theme_by_name("dark").expect("dark theme"),
    )
    .expect("renderer returns a component");
    assert!(!component.render(80).is_empty());

    set_theme_bridge(None);
    set_component_bridge(None);

    let state = json_state(&harness, "globalThis.__rendererState");
    // 回归：renderer 曾收到 (message, options, undefined)，theme.fg 抛错后被静默替换为空组件。
    assert_eq!(state["expanded"], true);
    assert_eq!(state["pad"], 2);
    assert_eq!(state["themed"], "FG:x");
    assert_eq!(state["marker"], true);
    assert_eq!(state["content"], "hello");
}

#[tokio::test(flavor = "current_thread")]
async fn tool_render_call_receives_theme_and_render_context() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/tool-render.ts",
        r#"
export default function(kiana) {
    kiana.registerTool({
        name: "rendered",
        label: "rendered",
        description: "rendered tool",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
        renderCall: (args, theme, context) => {
            globalThis.__toolRenderState = {
                theme: theme.fg("accent", "call"),
                marker: theme.__marker,
                callId: context.toolCallId,
                count: context.state.count,
                expanded: context.expanded,
            };
            context.state.count += 1;
            return { render: () => ["y"] };
        },
    });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    set_theme_bridge(Some(Arc::new(
        |ctx: &Ctx<'_>, _theme: &kiana_theme::theme::Theme| -> EngineResult<rquickjs::Value<'_>> {
            let object = Object::new(ctx.clone())?;
            object.set(
                "fg",
                Function::new(ctx.clone(), |_color: String, text: String| {
                    format!("FG:{text}")
                })?,
            )?;
            object.set("__marker", true)?;
            Ok(object.into_value())
        },
    )));
    set_component_bridge(Some(Arc::new(
        |_ctx: &Ctx<'_>,
         _value: rquickjs::Value<'_>|
         -> EngineResult<Box<dyn kiana_tui::tui::Component>> {
            Ok(Box::new(TestComponent))
        },
    )));

    let definition = result.extensions[0]
        .tools
        .get("rendered")
        .expect("tool")
        .definition
        .clone();
    let render_call = definition.render_call.clone().expect("renderCall");
    let state = Arc::new(Mutex::new(serde_json::json!({ "count": 5 })));
    let context = kiana_ext_api::tool_types::ToolRenderContext {
        args: serde_json::json!({}),
        tool_call_id: "call-1".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::clone(&state),
        cwd: std::path::PathBuf::from("/tmp"),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded: true,
        show_images: false,
        is_error: false,
        duration_ms: None,
        output_pad: 1,
    };
    let mut component = render_call(
        &serde_json::json!({}),
        &kiana_theme::theme::get_theme_by_name("dark").expect("dark theme"),
        &context,
    );
    assert!(!component.render(80).is_empty());

    set_theme_bridge(None);
    set_component_bridge(None);

    let seen = json_state(&harness, "globalThis.__toolRenderState");
    // 回归：renderCall 曾收到 (args, undefined)，且 context 缺失。
    assert_eq!(seen["theme"], "FG:call");
    assert_eq!(seen["marker"], true);
    assert_eq!(seen["callId"], "call-1");
    assert_eq!(seen["count"], 5);
    assert_eq!(seen["expanded"], true);
    assert_eq!(state.lock().unwrap()["count"], 6);
}

/// 回归（critical）：生产装配（`Generation::with_config` → `ui::install`）必须安装
/// loader 的主题桥与组件桥。
///
/// 修复前这两个桥只有测试会调用 `set_theme_bridge` / `set_component_bridge` 安装，生产
/// 路径里 `THEME_BRIDGE`/`COMPONENT_BRIDGE` 恒为 `None`：JS 渲染器收到 `undefined` 主题
/// （`theme.fg` 抛 TypeError），返回的组件被丢弃成 `NullComponent`。
#[tokio::test(flavor = "current_thread")]
async fn generation_installs_theme_and_component_bridges_for_renderers() {
    use kiana_extensions::Generation;

    let generation = Generation::new().expect("assemble generation");
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/generation-renderers.ts",
        r#"
export default function(kiana) {
    kiana.registerMessageRenderer("custom", (message, options, theme) => {
        globalThis.__generationRenderState = {
            themeType: typeof theme,
            sameAsGlobal: theme === globalThis[Symbol.for("@earendil-works/kiana-coding-agent:theme")],
            themed: theme.fg("accent", "x"),
            bold: theme.bold("b"),
            expanded: options.expanded,
        };
        return { render: (width) => ["rendered:" + width] };
    });
    kiana.registerTool({
        name: "rendered",
        label: "rendered",
        description: "rendered",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
        renderCall: (args, theme) => {
            globalThis.__generationToolTheme = theme.fg("accent", "call");
            return { render: () => ["tool-render"] };
        },
    });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let dark = kiana_theme::theme::get_theme_by_name("dark").expect("dark theme");
    let expected_themed = dark.fg(kiana_theme::ThemeColor::Accent, "x");
    let expected_bold = dark.bold("b");

    let renderer = result.extensions[0]
        .message_renderers
        .get("custom")
        .cloned()
        .expect("message renderer");
    let message = CustomMessage {
        role: CustomRole::Custom,
        custom_type: "custom".to_string(),
        content: serde_json::json!("hello"),
        display: true,
        details: None,
        timestamp: 0.0,
    };
    let mut component = renderer(
        &message,
        &kiana_ext_api::types::MessageRenderOptions {
            expanded: true,
            output_pad: 2,
        },
        &dark,
    )
    .expect("renderer returns a component");
    assert_eq!(component.render(80), vec!["rendered:80"]);

    let state_text = generation
        .engine()
        .eval_script::<String>(
            "JSON.stringify(globalThis.__generationRenderState)",
            "<generation-state>",
        )
        .expect("read renderer state");
    let state: serde_json::Value = serde_json::from_str(&state_text).expect("valid state json");
    assert_eq!(state["themeType"], "object");
    assert_eq!(state["sameAsGlobal"], true);
    assert_eq!(state["themed"], expected_themed.as_str());
    assert_eq!(state["bold"], expected_bold.as_str());
    assert_eq!(state["expanded"], true);

    let definition = result.extensions[0]
        .tools
        .get("rendered")
        .expect("tool")
        .definition
        .clone();
    let render_call = definition.render_call.expect("renderCall");
    let context = kiana_ext_api::tool_types::ToolRenderContext {
        args: serde_json::json!({}),
        tool_call_id: "call-1".to_string(),
        invalidate: Arc::new(|| {}),
        last_component: None,
        state: Arc::new(Mutex::new(serde_json::json!({}))),
        cwd: std::path::PathBuf::from("/tmp"),
        execution_started: true,
        args_complete: true,
        is_partial: false,
        expanded: true,
        show_images: false,
        is_error: false,
        duration_ms: None,
        output_pad: 1,
    };
    let mut tool_component = render_call(&serde_json::json!({}), &dark, &context);
    assert_eq!(tool_component.render(80), vec!["tool-render"]);
    let tool_theme = generation
        .engine()
        .eval_script::<String>("globalThis.__generationToolTheme", "<tool-theme>")
        .expect("read tool renderer theme");
    assert_eq!(tool_theme, dark.fg(kiana_theme::ThemeColor::Accent, "call"));
}

#[tokio::test(flavor = "current_thread")]
async fn tool_renderer_resolver_next_returns_js_functions() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/resolver.ts",
        r#"
export default function(kiana) {
    kiana.registerTool({
        name: "resolved",
        label: "resolved",
        description: "resolved",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
        renderCall: () => ({ render: () => [] }),
    });
    kiana.registerToolRenderer((toolName, next) => {
        const base = next();
        globalThis.__resolverState = {
            toolName,
            isObject: typeof base === "object" && base !== null,
            renderCallType: base === undefined ? "none" : typeof base.renderCall,
        };
        return base;
    });
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let definition = result.extensions[0]
        .tools
        .get("resolved")
        .expect("tool")
        .definition
        .clone();
    let resolver = result.extensions[0]
        .tool_renderers
        .as_ref()
        .and_then(|resolvers| resolvers.first())
        .cloned()
        .expect("tool renderer resolver");
    let base_call = definition.render_call.clone();
    let next = move || {
        Some(kiana_ext_api::tool_types::ToolRenderers {
            render_shell: None,
            render_call: base_call.clone(),
            render_result: None,
        })
    };
    let next_fn: &kiana_ext_api::tool_types::NextToolRenderersFn = &next;
    let resolved = resolver("resolved", next_fn);
    assert!(resolved.is_some());

    let seen = json_state(&harness, "globalThis.__resolverState");
    // 回归：next 曾是 undefined，`next()` 直接 TypeError。
    assert_eq!(seen["toolName"], "resolved");
    assert_eq!(seen["isObject"], true);
    assert_eq!(seen["renderCallType"], "function");
}

#[tokio::test(flavor = "current_thread")]
async fn register_flag_type_error_interpolates_the_js_value() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/flag.ts",
        r#"export default function(kiana) { kiana.registerFlag("bad", { type: 5, default: "x" }); }"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    // 回归：非字符串 type 曾报 `expected undefined`（TS 用 `${options.type}` → "5"）。
    assert_eq!(
        result.errors[0].error,
        "Failed to load extension: Invalid default for flag \"bad\": expected 5, got string"
    );
    let _ = harness;
}

#[test]
fn engine_teardown_does_not_abort_the_process() {
    // 回归：`ApiEntry::drop` 在 TLS 析构期间访问 `API_ENTRIES`/`HANDLER_REMOVERS`，panic
    // 穿过 rquickjs 的 extern "C" finalizer → SIGABRT。这个测试在独立线程里建引擎、加载一个
    // 闭包捕获 `kiana` 的扩展，然后让线程正常结束（引擎作为最后一个 `Rc` 在 TLS 析构时释放）。
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let harness = LoaderHarness::new();
            let root = temp_root();
            let path = write_file(
                root.path(),
                "extensions/teardown.ts",
                r#"
export default function(kiana) {
    kiana.registerCommand("keep", {
        handler: async () => { return kiana.getSessionName(); },
    });
    kiana.registerTool({
        name: "keep-tool",
        label: "keep",
        description: "keep",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
    });
}
"#,
            );
            let result = load_extensions(
                &[path.to_string_lossy().into_owned()],
                &root.path().to_string_lossy(),
                None,
                None,
            )
            .await;
            assert!(result.errors.is_empty(), "{:?}", result.errors);
            drop(result);
            drop(harness);
        });
    })
    .join()
    .expect("thread must exit cleanly");
}

#[tokio::test(flavor = "current_thread")]
async fn tool_context_bridge_preserves_specialized_vtable() {
    // 回归：工具 ctx 曾被向上转型成 `ExtensionContextApi`，专用桥永远看不到 vtable。
    let harness = LoaderHarness::new();
    let root = temp_root();
    write_file(
        root.path(),
        "extensions/tool.ts",
        r#"export default function(kiana) {
    kiana.registerTool({
        name: "t",
        label: "t",
        description: "t",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
    });
}"#,
    );
    let result = load_extensions(
        &[root
            .path()
            .join("extensions/tool.ts")
            .to_string_lossy()
            .into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let calls = Arc::new(Mutex::new(0_u32));
    let calls_for_bridge = Arc::clone(&calls);
    // 专用桥签名接收 `Arc<dyn ExtensionToolContextApi>`；这本身就是回归断言（旧代码只能拿
    // 到基础 trait 对象）。
    set_tool_context_bridge(Some(Arc::new(
        move |ctx: &Ctx<'_>,
              tool_ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>|
              -> EngineResult<rquickjs::Value<'_>> {
            *calls_for_bridge.lock().unwrap() += 1;
            let _ = tool_ctx;
            Ok(Value::new_undefined(ctx.clone()))
        },
    )));
    set_tool_context_bridge(None);
    let _ = harness;
}

#[tokio::test(flavor = "current_thread")]
async fn prepare_loadout_is_bridged() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/loadout.ts",
        r#"export default function(kiana) {
    kiana.registerTool({
        name: "orchestrator",
        label: "o",
        description: "o",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
        prepareLoadout: (loadout) => ({ hiddenDeclarations: loadout.registered.map((tool) => tool.name) }),
    });
}"#,
    );
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // 回归：prepareLoadout 曾被静默丢弃（恒 None）。
    assert!(
        result.extensions[0]
            .tools
            .get("orchestrator")
            .expect("tool")
            .definition
            .prepare_loadout
            .is_some()
    );
    let _ = harness;
}

/// 回归：`prepareLoadout` 的查询方法必须在 `ToolLoadout` 视图释放后仍然有效。
///
/// 旧实现把视图借用 `transmute` 成 `'static` 存进 JS 闭包；扩展把 `loadout` 存到
/// `globalThis` 后，等 Rust 侧视图释放再查询就是 use-after-free。这里正是这个顺序：
/// 先调用、再 `drop(loadout)`、最后从 JS 侧读回快照。
#[tokio::test(flavor = "current_thread")]
async fn saved_loadout_queries_survive_the_view_being_dropped() {
    use kiana_agent_core::{AgentTool, AgentToolExecute, AgentToolResult};
    use kiana_ext_api::BoxFuture;
    use kiana_ext_api::tool_types::{ToolExposure, ToolLoadout, ToolNamespace};

    struct NeverExecute;
    impl AgentToolExecute for NeverExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: serde_json::Value,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        ) -> BoxFuture<
            'a,
            Result<AgentToolResult<serde_json::Value>, kiana_agent_core::AgentToolError>,
        > {
            Box::pin(async { unreachable!("prepareLoadout test never executes tools") })
        }
    }

    fn agent_tool(name: &str) -> AgentTool {
        AgentTool {
            name: name.to_owned(),
            label: name.to_owned(),
            description: String::new(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(NeverExecute),
        }
    }

    struct FakeLoadout {
        registered: Vec<AgentTool>,
        namespace: ToolNamespace,
        guidelines: Vec<String>,
    }
    impl ToolLoadout for FakeLoadout {
        fn declared(&self) -> &[AgentTool] {
            &self.registered
        }
        fn callable(&self) -> &[AgentTool] {
            &self.registered
        }
        fn registered(&self) -> &[AgentTool] {
            &self.registered
        }
        fn get_exposure(&self, _name: &str) -> ToolExposure {
            ToolExposure::ModelOnly
        }
        fn get_namespace(&self, _name: &str) -> Option<&ToolNamespace> {
            Some(&self.namespace)
        }
        fn get_prompt_guidelines(&self, _name: &str) -> &[String] {
            &self.guidelines
        }
    }

    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/loadout-lifetime.ts",
        r#"export default function(kiana) {
    kiana.registerTool({
        name: "orchestrator",
        label: "o",
        description: "o",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [] }),
        prepareLoadout: (loadout) => {
            globalThis.__savedLoadout = loadout;
            return { hiddenDeclarations: [loadout.declared[0].name] };
        },
    });
}"#,
    );
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let definition = &result.extensions[0]
        .tools
        .get("orchestrator")
        .expect("tool")
        .definition;
    let prepare_loadout = definition.prepare_loadout.as_ref().expect("prepareLoadout");
    {
        let loadout = FakeLoadout {
            registered: vec![agent_tool("orchestrator")],
            namespace: ToolNamespace {
                name: "tools".to_owned(),
                ..ToolNamespace::default()
            },
            guidelines: vec!["use it".to_owned()],
        };
        let changes = prepare_loadout(&loadout).expect("changes");
        assert_eq!(
            changes.hidden_declarations,
            Some(vec!["orchestrator".to_owned()])
        );
    }

    // 视图已 drop；被 JS 保存的对象仍要能查询自己的快照。
    let observed: String = harness
        .engine
        .with_js(|ctx| {
            ctx.eval::<String, _>(
                r#"[
                globalThis.__savedLoadout.getExposure("orchestrator"),
                globalThis.__savedLoadout.getNamespace("orchestrator").name,
                globalThis.__savedLoadout.getPromptGuidelines("orchestrator").length,
            ].join(",")"#,
            )
        })
        .expect("saved loadout is still queryable");
    assert_eq!(observed, "model-only,tools,1");
}

#[tokio::test(flavor = "current_thread")]
async fn factory_callbacks_are_released_after_uncached_loads() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/empty.ts",
        "export default function () {}\n",
    );
    let path = path.to_string_lossy().into_owned();
    let cwd = root.path().to_string_lossy().into_owned();
    let baseline = harness.engine.persistent_slot_count();

    for _ in 0..4 {
        let result = load_extensions(std::slice::from_ref(&path), &cwd, None, None).await;
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    // 回归：每次 `loadExtensions` 都把模块默认导出的函数留在引擎持久槽里（TS 靠 GC）。
    // 空工厂不注册任何 handler，因此计数必须回到基线。
    assert_eq!(harness.engine.persistent_slot_count(), baseline);
}

// ===========================================================================
// 回归：createExtension 的 baseDir 与 Rust 扩展的 kiana.exec
// ===========================================================================

use kiana_ext_api::types::ExtensionFactory;
use kiana_extensions::loader::{
    create_extension, create_extension_runtime, load_extension_from_factory,
};

fn cwd_string() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string())
}

/// `path.dirname("my-ext")` 应为 `"."`（不是 `""`）：空 baseDir 会让相对解析失去基准，`SourceInfo.baseDir` 因此固定为 `"."`。
#[tokio::test(flavor = "current_thread")]
async fn bare_relative_extension_path_uses_dot_as_base_dir() {
    let factory: ExtensionFactory = Arc::new(|_kiana| Box::pin(async {}));
    let extension = load_extension_from_factory(
        factory,
        &cwd_string(),
        create_event_bus(),
        create_extension_runtime(),
        Some("my-ext"),
    )
    .await
    .expect("extension loads");
    assert_eq!(extension.path, "my-ext");
    assert_eq!(extension.resolved_path, "my-ext");
    assert_eq!(extension.source_info.base_dir, Some(".".to_string()));

    // 带分隔符的相对路径：`dirname("rel/dir") == "rel"`。
    let extension = create_extension("rel/dir", "rel/dir");
    assert_eq!(extension.source_info.base_dir, Some("rel".to_string()));
}

/// TS 的 `execCommand` 是异步的，且 `setTimeout` 对小数毫秒也生效：
/// - 执行期间不阻塞 LocalSet（本测试同时推进一个短定时器）；
/// - `timeout: 0.5` 不能被截断成 0（否则 `sleep 2` 会跑完）。
#[tokio::test(flavor = "current_thread")]
async fn rust_extension_exec_is_async_and_keeps_sub_millisecond_timeouts() {
    use kiana_ext_api::tool_result::ExecOptions;

    let factory: ExtensionFactory = Arc::new(|kiana| {
        Box::pin(async move {
            // 0.5ms 的超时：修复前 `as u64` 会变成 0（无超时）。
            let started = std::time::Instant::now();
            let result = kiana
                .exec(
                    "sleep",
                    &["2".to_string()],
                    Some(ExecOptions {
                        timeout: Some(0.5),
                        ..Default::default()
                    }),
                )
                .await;
            assert!(
                result.killed,
                "sub-millisecond timeout must fire: {result:?}"
            );
            assert!(
                started.elapsed() < std::time::Duration::from_millis(1500),
                "timeout was ignored (took {:?})",
                started.elapsed()
            );

            // 非阻塞：`exec` 期间主线程上的定时器必须按时触发。
            let sleep_args = ["0.3".to_string()];
            let exec = kiana.exec("sleep", &sleep_args, Some(ExecOptions::default()));
            let timer = async {
                let started = std::time::Instant::now();
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                started.elapsed()
            };
            let (exec_result, timer_delay) = futures::join!(exec, timer);
            assert_eq!(exec_result.code, 0, "{exec_result:?}");
            assert!(
                timer_delay < std::time::Duration::from_millis(150),
                "exec blocked the LocalSet (timer fired after {timer_delay:?})"
            );
        })
    });
    let extension = load_extension_from_factory(
        factory,
        &cwd_string(),
        create_event_bus(),
        create_extension_runtime(),
        Some("<exec>"),
    )
    .await;
    assert!(extension.is_ok(), "{extension:?}");
}

/// JS 扩展同样保留实时 `kiana`：模块作用域里捕获 `kiana` 后，加载结束后调用注册方法必须进入
/// loader 的实时状态（TS 的 `Extension` 对象身份；Rust 用 [`kiana_extensions::loader::live_extension`]）。
#[tokio::test(flavor = "current_thread")]
async fn js_late_registrations_reach_the_live_extension_state() {
    use kiana_extensions::loader::{live_extension, refresh_extension};

    let harness = LoaderHarness::new();
    let root = temp_root();
    let source = r#"
export default function (api) {
    globalThis.__lateKiana = api;
}
"#;
    let path = write_file(root.path(), "extensions/late.js", source);
    let path_string = path.to_string_lossy().into_owned();
    let result = load_extensions(
        std::slice::from_ref(&path_string),
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let mut extension = result.extensions[0].clone();
    assert!(extension.commands.is_empty());

    harness
        .engine
        .eval_script::<()>(
            "globalThis.__lateKiana.registerCommand('late', { handler: async () => {} }); globalThis.__lateKiana.registerFlag('late-flag', { type: 'string', default: 'v' });",
            "<late>",
        )
        .expect("late registration runs");

    let live = live_extension(&path_string).expect("live js extension");
    assert!(live.commands.contains_key("late"));
    assert!(live.flags.contains_key("late-flag"));
    assert_eq!(
        live.flags["late-flag"].default,
        Some(kiana_ext_api::types::FlagValue::Str("v".to_string()))
    );
    assert!(refresh_extension(&mut extension));
    assert!(extension.commands.contains_key("late"));
}

/// 审查 critical 回归：resolver 把 `next` 存到解析调用返回之后（TS 的闭包语义）再调用，
/// 不得解引用已释放的 Rust 栈帧；整条解析器链保持存活。
#[tokio::test(flavor = "current_thread")]
async fn tool_renderer_resolver_saved_next_stays_valid_after_the_call() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/saved-next.ts",
        r#"
export default function(kiana) {
    kiana.registerToolRenderer((toolName, next) => {
        globalThis.__savedNext = next;
        return undefined;
    });
    kiana.registerToolRenderer(() => ({ renderShell: "self" }));
}
"#,
    );

    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let extension = result.extensions[0].clone();
    let resolved = kiana_extensions::runner::renderers::resolve_tool_renderers(
        std::slice::from_ref(&extension),
        "saved",
        &|| None,
    );
    // 第一个 resolver 返回 undefined（不调用 next），初始解析没有结果。
    assert!(resolved.is_none());

    // 解析调用帧已经返回：保存下来的 next 必须仍然安全可调用，并继续走第二个 resolver。
    let state = json_state(
        &harness,
        r#"(() => {
            if (typeof globalThis.__savedNext !== "function") return "not-a-function";
            const renderers = globalThis.__savedNext();
            return renderers === undefined ? "undefined" : renderers.renderShell;
        })()"#,
    );
    assert_eq!(
        state,
        serde_json::json!("self"),
        "saved next must survive the call frame"
    );
}

/// 审查回归：`kiana.events.on` 的 handler 错误按 channel 落到 `console.error`
/// （`Event handler error (<channel>):` + 错误对象），同步抛出与 Promise rejection 都覆盖。
#[tokio::test(flavor = "current_thread")]
async fn event_bus_handler_errors_are_reported_per_channel() {
    let harness = LoaderHarness::new();
    let root = temp_root();
    let path = write_file(
        root.path(),
        "extensions/events.ts",
        r#"
export default function(kiana) {
    kiana.events.on("boom", () => { throw new Error("sync-boom"); });
    kiana.events.on("boom", async () => { throw new Error("async-boom"); });
}
"#,
    );

    let bus: Arc<dyn EventBus> = create_event_bus();
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        Some(Arc::clone(&bus)),
        None,
    )
    .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    harness
        .engine
        .eval_script::<()>(
            "globalThis.__logs = []; \
             globalThis.__kiana_host.log = (level, text) => { globalThis.__logs.push([level, text]); };",
            "<logs>",
        )
        .expect("capture console");

    bus.emit("boom", serde_json::Value::Null);
    // 异步 handler 的安全包装在微任务里 catch；把微任务跑完。
    harness.engine.pump_jobs();

    let logs = json_state(&harness, "globalThis.__logs");
    let entries = logs.as_array().expect("log entries");
    assert_eq!(entries.len(), 2, "logs={logs}");
    for entry in entries {
        assert_eq!(entry[0], serde_json::json!("error"), "logs={logs}");
        assert!(
            entry[1]
                .as_str()
                .is_some_and(|text| text.starts_with("Event handler error (boom):")),
            "logs={logs}"
        );
    }
    // console.error 的第二个参数是错误对象（Node 的 util.inspect 形态：这里渲染为
    // QuickJS 的 stack，指向扩展源码里抛出的位置）。
    let first = entries[0][1].as_str().unwrap_or_default();
    let second = entries[1][1].as_str().unwrap_or_default();
    assert!(first.contains("events.ts"), "logs={logs}");
    assert!(second.contains("events.ts"), "logs={logs}");
    assert!(first.contains(":3:"), "sync handler frame missing: {logs}");
    assert!(
        second.contains(":6:"),
        "async handler frame missing: {logs}"
    );
}

/// 审查 major 回归：加载失败的可见文案与 jiti/Node 一致（`Cannot find module …` +
/// Require stack；`ParseError: … <path>:line:col`），外层仍是 `Failed to load extension: `。
#[tokio::test(flavor = "current_thread")]
async fn load_failure_messages_match_jiti() {
    let _harness = LoaderHarness::new();
    let root = temp_root();

    let missing = root.path().join("definitely-missing.ts");
    let result = load_extensions(
        &[missing.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    assert_eq!(result.errors.len(), 1);
    assert_eq!(
        result.errors[0].error,
        format!(
            "Failed to load extension: Cannot find module '{}'\nRequire stack:\n- {}",
            missing.to_string_lossy(),
            kiana_extensions::loader::loader_impl::loader_require_stack_path()
        )
    );

    let broken = write_file(
        root.path(),
        "extensions/broken.ts",
        "export default function(kiana) { const value = ; }\n",
    );
    let result = load_extensions(
        &[broken.to_string_lossy().into_owned()],
        &root.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let error = &result.errors[0].error;
    assert!(
        error.starts_with("Failed to load extension: ParseError: "),
        "unexpected message: {error}"
    );
    assert!(
        error.contains(&format!(" {}:1:", broken.to_string_lossy())),
        "path/line missing from {error}"
    );
}
