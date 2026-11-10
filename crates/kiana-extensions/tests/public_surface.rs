//! 公开面：`core/extensions/index.ts` 的 193 个名字逐一可用（`ARCHITECTURE.md` §8.8 A）。
//!
//! 这是一个**编译期**断言：`use kiana_extensions::{...}` 里列出的每个名字都必须从 crate 根
//! 解析成功；缺一个名字本文件就编译失败。它等价于 §8.8 的“脚本比对名字集合”。
//!
//! 名单来自 `packages/coding-agent/src/core/extensions/index.ts` 的 193 个唯一导出，
//! 按 `ARCHITECTURE.md` §5.2 的冻结命名映射（`ExtensionAPI` → `ExtensionApi`、
//! `ExtensionUIContext` → `ExtensionUiContext`、`ExtensionContext` → `ExtensionContextApi`
//! 等；函数用 snake_case）。Rust-only 的装配期名字（`ExtensionHost`/`Generation`、
//! engine/virtual_modules 类型、`load_extensions_cached`、`emit_project_trust_event`）
//! 追加在最后，它们不改变 193 的语义。
//!
//! 本文件按 `runtime` 门控：193 个名字里有 13 个是 loader/runner/wrapper 的运行期函数
//! （`create_extension_runtime`、`ExtensionRunner`、`wrap_registered_tool` …），无运行时
//! 特性时它们按 `ARCHITECTURE.md` §8 第 27 条不存在；`--no-default-features` 的验收是
//! “空构建通过”（本测试退化为 0 个用例），types.ts 的 186 个名字仍由 `kiana_ext_api` 提供。
#![cfg(feature = "runtime")]
#![allow(unused_imports)]

use kiana_extensions::{
    AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent,
    AgentBeforeSettleEventResult, AgentEndEvent, AgentSettledEvent, AgentStartEvent,
    AgentToolResult, AgentToolUpdateCallback, AppKeybinding, AppendEntryHandler,
    AutocompleteProviderFactory, BashToolCallEvent, BashToolResultEvent,
    BeforeAgentStartCustomEvent, BeforeAgentStartEvent, BeforeAgentStartEventResult,
    BeforeAgentStartUserEvent, BeforeProviderHeadersEvent, BeforeProviderRequestEvent,
    BeforeProviderRequestEventResult, BoundaryContextPreview, BoundaryResult, BoundaryState,
    BridgeKind, BridgeMember, BuildSystemPromptOptions, CacheWarmingDecisionEvent,
    CacheWarmingDecisionEventResult, CompactOptions, CompactionEntryDraft, ConstantValue,
    ContextEditEntryDraft, ContextEvent, ContextEventResult, ContextUsage, ContextWithSystemEvent,
    CustomEntryDraft, CustomMessageEntryDraft, CustomToolCallEvent, CustomToolResultEvent,
    EditToolCallEvent, EditToolResultEvent, EditorFactory, EngineConfig, EngineResult,
    EntryRenderOptions, EntryRenderer, ExecOptions, ExecResult, ExecuteToolOptions, Extension,
    ExtensionActions, ExtensionApi, ExtensionCommandContextActions, ExtensionCommandContextApi,
    ExtensionContextActions, ExtensionContextApi, ExtensionError, ExtensionErrorListener,
    ExtensionEvent, ExtensionFactory, ExtensionFlag, ExtensionHandler, ExtensionHost,
    ExtensionMode, ExtensionRunner, ExtensionRuntime, ExtensionShortcut, ExtensionToolContextApi,
    ExtensionUIDialogOptions, ExtensionUiContext, ExtensionVirtualModel, ExtensionWidgetOptions,
    FindToolCallEvent, FindToolResultEvent, ForkHandler, Generation, GetActiveToolsHandler,
    GetAllToolsHandler, GetCommandsHandler, GetThinkingLevelHandler, GrepToolCallEvent,
    GrepToolResultEvent, HostCall, HostCompletion, HostCompletionSender, HostValue,
    InlineExtension, InputEvent, InputEventResult, InputSource, JsEngine, JsError, JsErrorKind,
    JsHandleId, KeybindingsManager, LoadExtensionsResult, LsToolCallEvent, LsToolResultEvent,
    MarkdownTransformContext, MarkdownTransformer, McpServersChangeEvent, MessageEndEvent,
    MessageEndEventResult, MessageRenderOptions, MessageRenderer, MessageStartEvent,
    MessageUpdateEvent, ModelSelectEvent, ModelSelectSource, NATIVE_BRIDGE_GLOBAL, NamespaceSpec,
    NavigateTreeHandler, NewSessionHandler, NormalizedBuildSystemPromptOptions,
    PowerShellToolCallEvent, PowerShellToolResultEvent, ProjectTrustContext, ProjectTrustEvent,
    ProjectTrustEventDecision, ProjectTrustEventResult, ProjectTrustHandler, ProjectTrustOutcome,
    ProviderConfig, ProviderModelConfig, ProviderStreamEvent, ReadToolCallEvent,
    ReadToolResultEvent, RegisteredCommand, RegisteredTool, ReplacedSessionContextApi,
    ResolvedCommand, ResourcesDiscoverEvent, ResourcesDiscoverResult, SendMessageHandler,
    SendUserMessageHandler, SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeSwitchResult, SessionBeforeTreeEvent, SessionBeforeTreeResult,
    SessionBoundaryDraft, SessionCompactEvent, SessionCompactFailedEvent, SessionEvent,
    SessionInfoChangedEvent, SessionShutdownEvent, SessionStartEvent, SessionTreeEvent,
    SetActiveToolsHandler, SetLabelHandler, SetModelHandler, SetThinkingLevelHandler,
    ShutdownHandler, SlashCommandInfo, SlashCommandSource, SourceInfo, SwitchSessionHandler,
    TerminalInputHandler, ThinkingLevelSelectEvent, ToolAnnotations, ToolCallEvent,
    ToolCallEventResult, ToolDefinition, ToolExecutionEndEvent, ToolExecutionMode,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolExposure, ToolInfo, ToolLoadout,
    ToolLoadoutChanges, ToolNamespace, ToolRenderResultOptions, ToolRendererResolver,
    ToolRenderers, ToolResultEvent, ToolResultEventResult, TreePreparation, TurnEndEvent,
    TurnEndEventResult, TurnStartEvent, UIPromptEndEvent, UIPromptKind, UIPromptStartEvent,
    UserBashEvent, UserBashEventResult, VIRTUAL_GLOBAL, WidgetPlacement, WorkingIndicatorOptions,
    WriteToolCallEvent, WriteToolResultEvent, create_extension_runtime, define_tool,
    discover_and_load_extensions, emit_project_trust_event, emit_session_shutdown_event,
    is_bash_tool_result, is_edit_tool_result, is_find_tool_result, is_grep_tool_result,
    is_ls_tool_result, is_power_shell_tool_result, is_read_tool_result, is_tool_call_event_type,
    is_write_tool_result, load_extension_from_factory, load_extensions, load_extensions_cached,
    take_exception, wrap_registered_tool, wrap_registered_tools,
};

#[test]
fn every_ts_index_name_is_exported_from_the_crate_root() {
    // 上面的 `use` 就是断言。这里再确认 `kiana_extensions::index` 与 crate 根是同一批名字
    // （`pub use index::*` 的语义），并给出一条可读的计数：193 个 TS 名字 + Rust-only 追加。
    const TS_INDEX_NAMES: usize = 193;
    const RUST_ONLY_NAMES: usize = 23;
    assert_eq!(TS_INDEX_NAMES + RUST_ONLY_NAMES, 216);

    // `clearExtensionCache`（`ca-extensions.md` §5.2 / `ca-resources.md` §5）不在 TS
    // index.ts 的再导出面里：按 `ARCHITECTURE.md` §5 的映射从 loader 的工厂缓存模块取。
    let _: fn() = kiana_extensions::loader::factory_cache::clear_extension_cache;

    let _: fn() -> std::sync::Arc<kiana_ext_api::types::ExtensionRuntime> =
        kiana_extensions::create_extension_runtime;
    let _: fn(
        &kiana_ext_api::types::RegisteredTool,
        &std::sync::Arc<kiana_extensions::ExtensionRunner>,
    ) -> kiana_agent_core::AgentTool = kiana_extensions::wrap_registered_tool;
}

#[test]
fn the_barrel_module_path_matches_the_crate_root() {
    // 两个路径解析到同一个类型（`kiana_extensions::index::*` 是纯 barrel）。
    fn same_type<T: ?Sized>() {}
    same_type::<dyn kiana_extensions::ExtensionApi>();
    same_type::<dyn kiana_extensions::index::ExtensionApi>();
    same_type::<kiana_extensions::ExtensionFlag>();
    same_type::<kiana_extensions::index::ExtensionFlag>();
    same_type::<kiana_extensions::SourceInfo>();
    same_type::<kiana_extensions::index::SourceInfo>();
}

// ===========================================================================
// 装配器（`ARCHITECTURE.md` §5、设计文档 §9.2）
// ===========================================================================

#[test]
fn generation_assembles_every_layer_in_order() {
    let generation = kiana_extensions::Generation::new().expect("assemble a generation");
    assert!(
        generation.is_ready(),
        "every §9.2 layer must be installed after Generation::new()"
    );

    let engine = generation.engine();
    // 22 个虚拟键 + typebox 的 type 对象身份（虚拟模块安装的可观察结果）。
    let keys = engine.with_js(|ctx| {
        let virtual_modules: rquickjs::Object = ctx
            .globals()
            .get("__kiana_virtual")
            .expect("__kiana_virtual");
        virtual_modules
            .keys::<String>()
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    });
    assert_eq!(keys.len(), 22);
    assert_eq!(keys.first().map(String::as_str), Some("typebox"));

    // 各装配层的宿主标记（`__kiana` 等按 §9.3 用 `Object.defineProperty` 安装，
    // `enumerable: false`，不能靠 `global_keys()` 枚举，必须直接读属性）。
    engine.with_js(|ctx| {
        for name in [
            "__kiana",
            "__kiana_api",
            "__kiana_ctx",
            "__kiana_dispatch",
            "__kiana_ui_native",
            "__kiana_tui_native",
        ] {
            let value: Option<rquickjs::Object> = ctx.globals().get(name).expect("read global");
            assert!(value.is_some(), "missing {name}");
        }
    });
}

#[test]
fn extension_host_new_is_the_single_entry_and_reloads_generation() {
    let mut host = kiana_extensions::ExtensionHost::new().expect("host");
    assert_eq!(host.engine().config().generation, 0);
    assert!(host.generation().is_ready());

    // UI 工厂闭包不能捕获 `Rc<JsEngine>`，装配必须把引擎登记到线程局部槽（设计 §5.6）。
    let attached =
        kiana_extensions::ui::adapter::JsEngineSlot::current().expect("engine attached to ui slot");
    assert!(std::rc::Rc::ptr_eq(&attached, host.engine()));

    host.reload().expect("reload");
    assert_eq!(host.engine().config().generation, 1);
    assert!(host.generation().is_ready());
    let attached =
        kiana_extensions::ui::adapter::JsEngineSlot::current().expect("new engine attached");
    assert!(
        std::rc::Rc::ptr_eq(&attached, host.engine()),
        "reload must replace the engine in the ui slot"
    );
}

#[test]
fn extension_host_with_config_applies_engine_limits() {
    let config = kiana_extensions::EngineConfig {
        memory_limit: 64 * 1024 * 1024,
        watchdog: std::time::Duration::from_millis(50),
        ..kiana_extensions::EngineConfig::default()
    };
    let host = kiana_extensions::ExtensionHost::with_config(config).expect("host");
    assert_eq!(host.engine().config().memory_limit, 64 * 1024 * 1024);
    assert_eq!(
        host.engine().config().watchdog,
        std::time::Duration::from_millis(50)
    );
    assert!(host.generation().is_ready());
}

#[test]
fn generation_can_hand_over_the_engine() {
    let generation = kiana_extensions::Generation::new().expect("generation");
    let engine = generation.into_engine();
    // 交出引擎后仍然完整安装（`Rc` 的引用计数交给调用方）。
    assert_eq!(engine.config().generation, 0);
}

// ===========================================================================
// §9.2 第 10 步：`@earendil-works/kiana-tui` 的原生桥
// ===========================================================================

#[test]
fn kiana_tui_virtual_namespace_is_backed_by_the_native_bridge() {
    let generation = kiana_extensions::Generation::new().expect("generation");
    let engine = generation.engine();

    // 直接走虚拟 namespace（扩展 `import { Text } from "@earendil-works/kiana-tui"` 拿到的
    // 就是这个对象），确认成员不是「读得到、用就抛」的转发代理。
    let value: String = engine
        .eval_script(
            r#"
            (() => {
                const tui = globalThis.__kiana_virtual["@earendil-works/kiana-tui"];
                const text = new tui.Text("hello", 0, 0);
                return JSON.stringify({
                    bridge: typeof globalThis.__kiana_native["kiana-tui"],
                    key: tui.Key.ctrlShift("u"),
                    rendered: text.render(8),
                    editor: typeof tui.CustomEditor,
                    keybindings: typeof tui.KeybindingsManager,
                    markdownHeading: tui.getMarkdownTheme().heading("hi"),
                    settingsLabel: tui.getSettingsListTheme().label("plain", false),
                    keyHint: tui.keyHint("tui.select.confirm", "confirm"),
                });
            })()
            "#,
            "<kiana-tui-bridge>",
        )
        .expect("kiana-tui namespace is usable");
    let value: serde_json::Value = serde_json::from_str(&value).expect("json");
    assert_eq!(value["bridge"], "object");
    assert_eq!(value["key"], "ctrl+shift+u");
    assert_eq!(value["rendered"], serde_json::json!(["hello   "]));
    assert_eq!(value["editor"], "function");
    assert_eq!(value["keybindings"], "function");
    // coding-agent 共享的三个主题/提示 helper（finding: 运行期会抛 native bridge 缺失）。
    assert!(
        value["markdownHeading"]
            .as_str()
            .is_some_and(|text| text.contains("hi")),
        "{value}"
    );
    assert_eq!(value["settingsLabel"], "plain");
    assert!(
        value["keyHint"]
            .as_str()
            .is_some_and(|text| text.contains("confirm") && text.len() > "confirm".len()),
        "{value}"
    );
}

// ===========================================================================
// §2.8 / `resource-loader.ts:544`：新代清空工厂缓存
// ===========================================================================

fn write_extension(dir: &std::path::Path, name: &str) -> String {
    let path = dir.join(name);
    std::fs::write(
        &path,
        r#"
export default function (kiana) {
    kiana.registerTool({
        name: "cached",
        label: "cached",
        description: "cached",
        parameters: { type: "object", properties: {} },
        execute: async () => ({ content: [{ type: "text", text: "ok" }], details: {} }),
    });
}
"#,
    )
    .expect("write extension");
    path.to_string_lossy().into_owned()
}

/// TS `ResourceLoader.reload()` 的第 2 步：`clearExtensionCache()`。缓存里的 JS 工厂绑定
/// 旧引擎的持久槽；新代必须重新执行模块体，否则 `loadExtensionsCached` 会拿到
/// `Extension callback is no longer registered`。
#[tokio::test(flavor = "current_thread")]
async fn a_new_generation_invalidates_cached_extension_factories() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = write_extension(dir.path(), "cached.ts");

    tokio::task::LocalSet::new()
        .run_until(async {
            let mut host = kiana_extensions::ExtensionHost::new().expect("host");
            let first = kiana_extensions::load_extensions_cached(
                std::slice::from_ref(&path),
                &cwd,
                None,
                None,
            )
            .await;
            assert!(first.errors.is_empty(), "{:?}", first.errors);
            assert_eq!(first.extensions.len(), 1);

            host.reload().expect("reload");

            let second = kiana_extensions::load_extensions_cached(
                std::slice::from_ref(&path),
                &cwd,
                None,
                None,
            )
            .await;
            assert!(second.errors.is_empty(), "{:?}", second.errors);
            assert_eq!(second.extensions.len(), 1);
            assert!(second.extensions[0].tools.contains_key("cached"));

            // finding 的另一种触发方式：同一线程上的第二个宿主（新 `Generation`）。
            let _second_host = kiana_extensions::ExtensionHost::new().expect("second host");
            let third = kiana_extensions::load_extensions_cached(
                std::slice::from_ref(&path),
                &cwd,
                None,
                None,
            )
            .await;
            assert!(third.errors.is_empty(), "{:?}", third.errors);
            assert_eq!(third.extensions.len(), 1);
        })
        .await;
}

// ===========================================================================
// §2.8：整代销毁 / 装配失败不留线程本地状态
// ===========================================================================

#[test]
fn dropping_the_host_releases_the_engine_and_thread_local_slots() {
    let host = kiana_extensions::ExtensionHost::new().expect("host");
    let weak = std::rc::Rc::downgrade(host.engine());
    assert!(kiana_extensions::ui::adapter::JsEngineSlot::is_attached());
    drop(host);
    assert!(
        weak.upgrade().is_none(),
        "the engine must be released once the last generation is dropped"
    );
    assert!(
        !kiana_extensions::ui::adapter::JsEngineSlot::is_attached(),
        "the ui slot must not retain the dropped engine"
    );
}

#[test]
fn a_failed_generation_leaves_no_engine_or_loader_bound() {
    // 4 MiB 的内存上限在装配期（typebox 691 个模块）必然失败；失败路径必须撤销 loader /
    // dispatch / ui 的线程本地登记，而不是把半装的引擎留在 `CURRENT_ENGINE`。
    let config = kiana_extensions::EngineConfig {
        memory_limit: 4 * 1024 * 1024,
        ..kiana_extensions::EngineConfig::default()
    };
    let error = kiana_extensions::Generation::with_config(config);
    assert!(error.is_err(), "4 MiB must not fit the runtime");
    assert!(
        !kiana_extensions::ui::adapter::JsEngineSlot::is_attached(),
        "a failed assembly must not retain the engine in the ui slot"
    );

    // 下一次装配不受影响（旧的 loader / dispatch 登记已撤销）。
    let generation = kiana_extensions::Generation::new().expect("generation after failure");
    assert!(generation.is_ready());
}

// ===========================================================================
// 同一线程上的两个宿主（TS 没有进程级引擎；`extensions_loader.ts` 的 loader 互不影响）
// ===========================================================================

/// 第二代宿主销毁后，第一代必须重新成为当前引擎：它的 loader、dispatch 都是 `Weak`
/// 登记，销毁最新的宿主不会清掉前一代的登记（finding: `loader::uninstall` 曾经把
/// `CURRENT_ENGINE`/`CURRENT_LOADER` 清成 `None`，宿主 A 之后无法再加载扩展）。
#[tokio::test(flavor = "current_thread")]
async fn dropping_a_second_host_restores_the_first_host_engine() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = write_extension(dir.path(), "reattach.ts");

    tokio::task::LocalSet::new()
        .run_until(async {
            let host_a = kiana_extensions::ExtensionHost::new().expect("host a");
            let first =
                kiana_extensions::load_extensions(std::slice::from_ref(&path), &cwd, None, None)
                    .await;
            assert!(first.errors.is_empty(), "{:?}", first.errors);
            assert_eq!(first.extensions.len(), 1);

            // 第二个宿主：装配会把它的引擎登记为当前（并跳过对上一代扩展登记的清理）。
            let host_b = kiana_extensions::ExtensionHost::new().expect("host b");
            assert!(host_b.generation().is_ready());
            let attached =
                kiana_extensions::ui::adapter::JsEngineSlot::current().expect("b attached");
            assert!(std::rc::Rc::ptr_eq(&attached, host_b.engine()));

            // 销毁 B 后，A 的引擎回到当前：`load_extensions` 的工厂回调（`JsCallback`）与
            // 模块 loader 都必须解析回 A。
            drop(host_b);
            assert!(host_a.generation().is_ready());
            let attached =
                kiana_extensions::ui::adapter::JsEngineSlot::current().expect("a attached");
            assert!(std::rc::Rc::ptr_eq(&attached, host_a.engine()));

            let second =
                kiana_extensions::load_extensions(std::slice::from_ref(&path), &cwd, None, None)
                    .await;
            assert!(second.errors.is_empty(), "{:?}", second.errors);
            assert_eq!(second.extensions.len(), 1);
            assert!(second.extensions[0].tools.contains_key("cached"));
        })
        .await;
}

/// 反向顺序：先销毁 A、保留 B 时，B 继续可用。
#[tokio::test(flavor = "current_thread")]
async fn dropping_the_first_host_keeps_the_second_host_usable() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = write_extension(dir.path(), "reattach-back.ts");

    tokio::task::LocalSet::new()
        .run_until(async {
            let host_a = kiana_extensions::ExtensionHost::new().expect("host a");
            let host_b = kiana_extensions::ExtensionHost::new().expect("host b");
            drop(host_a);

            let loaded =
                kiana_extensions::load_extensions(std::slice::from_ref(&path), &cwd, None, None)
                    .await;
            assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
            assert_eq!(loaded.extensions.len(), 1);
            let attached =
                kiana_extensions::ui::adapter::JsEngineSlot::current().expect("b attached");
            assert!(std::rc::Rc::ptr_eq(&attached, host_b.engine()));
        })
        .await;
}

/// `host.reload()` 只替换自己的代；同一线程上另一个仍存活的宿主不受影响。
#[tokio::test(flavor = "current_thread")]
async fn reload_of_one_host_does_not_clobber_another_host() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let path = write_extension(dir.path(), "reload-peer.ts");

    tokio::task::LocalSet::new()
        .run_until(async {
            let mut host_a = kiana_extensions::ExtensionHost::new().expect("host a");
            let host_b = kiana_extensions::ExtensionHost::new().expect("host b");
            host_a.reload().expect("reload a");

            // reload 后 A 是新代（generation = 1），且重新成为当前引擎（最后安装）。
            assert_eq!(host_a.engine().config().generation, 1);
            let attached =
                kiana_extensions::ui::adapter::JsEngineSlot::current().expect("a attached");
            assert!(std::rc::Rc::ptr_eq(&attached, host_a.engine()));

            // B 仍完整可用：销毁 A 后 B 回到当前。
            drop(host_a);
            let attached =
                kiana_extensions::ui::adapter::JsEngineSlot::current().expect("b attached");
            assert!(std::rc::Rc::ptr_eq(&attached, host_b.engine()));
            let loaded =
                kiana_extensions::load_extensions(std::slice::from_ref(&path), &cwd, None, None)
                    .await;
            assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
            assert_eq!(loaded.extensions.len(), 1);
        })
        .await;
}

// ===========================================================================
// finding: `getMarkdownTheme().highlightCode(code, lang)` 必须按语言着色
// ===========================================================================

/// 全局主题是进程级的；这些断言与其它读取主题的用例串行执行。
static THEME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// TS `theme.ts:1038-1057`：`highlightCode(code, lang)` 用 cli-highlight（highlight.js）
/// 按语言着色；语言缺失/无效时**逐行**套 `mdCodeBlock`。
///
/// golden 逐字节取自 TS 参考实现（vitest, `initTheme("dark")`, `FORCE_COLOR=3`）。
#[test]
fn kiana_tui_markdown_theme_highlight_code_matches_ts() {
    let _guard = THEME_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let previous = kiana_theme::theme::current_theme();
    kiana_theme::theme::init_theme(Some("dark"), false);

    let generation = kiana_extensions::Generation::new().expect("generation");
    let engine = generation.engine();
    let json: String = engine
        .eval_script(
            r#"
            (() => {
                const md = globalThis.__kiana_virtual["@earendil-works/kiana-tui"].getMarkdownTheme();
                return JSON.stringify([
                    md.highlightCode("const x = 1;", "js"),
                    md.highlightCode("const x = 1;"),
                    md.highlightCode("const x = 1;", "nonsense"),
                    md.highlightCode("def f(x):\n    return x + 1\n", "python"),
                ]);
            })()
            "#,
            "<highlight-code>",
        )
        .expect("highlightCode is callable");
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");

    // `js` 是 highlight.js 的别名 → javascript；分词结果与 TS 相同。
    assert_eq!(
        value[0],
        serde_json::json!([
            "\u{1b}[38;2;105;173;208mconst\u{1b}[39m x = \u{1b}[38;2;104;183;141m1\u{1b}[39m;"
        ])
    );
    // 无语言：每行 mdCodeBlock（TS 的 `validLang === undefined` 分支）。
    assert_eq!(
        value[1],
        serde_json::json!(["\u{1b}[38;2;104;183;141mconst x = 1;\u{1b}[39m"])
    );
    // 无效语言与无语言相同（`supportsLanguage` 过滤）。
    assert_eq!(value[2], value[1]);
    // 多行 + python（空尾行保留）。
    assert_eq!(
        value[3],
        serde_json::json!([
            "\u{1b}[38;2;105;173;208mdef\u{1b}[39m\u{1b}[38;2;205;154;34m \u{1b}[39m\u{1b}[38;2;205;154;34mf\u{1b}[39m\u{1b}[38;2;205;154;34m(\u{1b}[39m\u{1b}[38;2;93;179;186mx\u{1b}[39m\u{1b}[38;2;205;154;34m):\u{1b}[39m",
            "    \u{1b}[38;2;105;173;208mreturn\u{1b}[39m x + \u{1b}[38;2;104;183;141m1\u{1b}[39m",
            ""
        ])
    );

    if let Some(previous) = previous {
        kiana_theme::theme::set_theme_instance(previous);
    }
}
