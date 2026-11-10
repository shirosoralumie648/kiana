//! 兼容语料（`examples/extensions/**`，设计文档 §10.1、`ca-extensions.md` §3.11/§7.7）。
//!
//! 语料以**逐字副本**放在 `tests/fixtures/corpus/extensions/`（80 个条目：70 个单文件 +
//! 9 个目录 + README，共 86 个 `.ts`）；示例需要的 npm 包在
//! `tests/fixtures/corpus/node_modules/`，因此 CI 不需要网络、不需要真实 `~/.kiana`
//! （`ARCHITECTURE.md` §4.8 第 8 条）。
//!
//! 覆盖设计文档 §10.1 的四项：
//!
//! 1. [`corpus_ts_files_strip_without_diagnostics`]：86 个 `.ts` 全部去类型、0 诊断；
//!    [`every_ts_file_declares_and_evaluates`]：每个文件都能被加载器声明（模块图求值），
//!    只有“没有默认导出”这类工厂层错误例外；
//! 2. [`all_entries_load_and_match_the_registration_matrix`]：79 个入口执行工厂，注册项
//!    key 集合与 `tests/fixtures/corpus/registrations.json`（§3.11 矩阵）一致；
//! 3. [`representative_examples_behave`]：§7.7 的代表示例的关键行为；
//! 4. [`real_npm_package_regressions`]：设计文档 §10.4 的四个真实 npm 包回归。
//!
//! # 已知的 C/D 级
//!
//! - `doom-overlay/` 是 **C** 级（设计 §7.2）：`/doom-overlay` 需要 `WebAssembly` +
//!   Emscripten FS；默认构建不提供 `WebAssembly`（`webassembly`（wasmtime）特性会绑定
//!   `Module`/`Instance`/`instantiate`，但 Emscripten 的 FS/内存假设仍需 shim）。它的**模块加载**本身不需要
//!   wasm，因此不在“不可加载”之列；这里按任务要求跳过并在 [`KNOWN_SKIPS`] 记录原因。
//! - `gondolin/` 是 **D** 级：`@earendil-works/gondolin` 依赖 `ssh2`（原生 addon）与
//!   `cbor2`/`undici`，无法作为纯 JS 语料 vendored。
//!
//! 其余 77 个入口必须全部加载（`Generation::with_config` 第 10 步已把 ui-adapter 的
//! `__kiana_tui` 发布成 `@earendil-works/kiana-tui` 的原生桥，因此 `Key`/`CustomEditor` 可用）。
#![cfg(feature = "runtime")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_ext_api::types::{
    ExtensionActions, ExtensionCommandContextActions, ExtensionContextActions, ExtensionMode,
    ToolInfo,
};
use kiana_ext_api::ui::{
    AutocompleteProviderFactory, CustomFactory, CustomOptions, EditorFactory,
    ExtensionUIDialogOptions, ExtensionUiContext, FooterFactory, HeaderFactory, NotifyKind,
    TerminalInputHandler, ThemeInfo, ThemeOrName, ThemeResult, WidgetContent,
    WorkingIndicatorOptions,
};
use kiana_extensions::loader::discovery::resolve_extension_entries;
use kiana_extensions::loader::{
    load_extensions, runtime_pending_native_provider_registrations,
    runtime_pending_provider_registrations, runtime_pending_virtual_model_registrations,
    runtime_provider_config_errors,
};
use kiana_extensions::runner::ExtensionRunner;
use kiana_extensions::{EngineConfig, Generation};
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};
use kiana_theme::theme::Theme;

// ===========================================================================
// 路径与清单
// ===========================================================================

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus")
}

fn extensions_root() -> PathBuf {
    corpus_root().join("extensions")
}

fn collect_ts_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read corpus dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_ts_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "ts") {
            out.push(path);
        }
    }
}

fn ts_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_ts_files(&extensions_root(), &mut files);
    files.sort();
    files
}

/// 79 个入口：70 个顶层 `.ts` + 9 个目录（目录按 `package.json` 的 `kiana.extensions` /
/// `index.ts` 解析，与 `loader.ts#resolveExtensionEntries` 相同）。名字按 §3.11 的写法：
/// 单文件写文件名，目录带结尾 `/`。
fn entry_paths() -> Vec<(String, PathBuf)> {
    let root = extensions_root();
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&root)
        .expect("read extensions dir")
        .flatten()
    {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "ts") {
            entries.push((name, path));
        } else if path.is_dir() {
            let resolved = resolve_extension_entries(&path)
                .unwrap_or_else(|| panic!("directory entry {name} has no resolvable index"));
            entries.push((format!("{name}/"), PathBuf::from(resolved[0].clone())));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

/// 设计 §7.2 的 C/D 级：不加载，记录原因。
const KNOWN_SKIPS: &[(&str, &str)] = &[
    (
        "doom-overlay/",
        "C: /doom-overlay needs WebAssembly + Emscripten FS; the default build omits the optional \
         `webassembly` feature (wasmtime), and Emscripten memory/FS still need shims",
    ),
    (
        "gondolin/",
        "D: @earendil-works/gondolin depends on the native `ssh2` addon (cpu-features) plus \
         cbor2/undici; it cannot be vendored as pure JS",
    ),
];

/// 依赖其它单元尚未落地能力的入口（历史上是 `kiana-tui.Key`/`CustomEditor` 原生桥）。
///
/// 原生桥已由装配器发布（`Generation::with_config` 第 10 步），名单必须保持为空：
/// 任何一个入口的失败都必须是真实失败，而不是被白名单吞掉。
const BLOCKED_BY_OTHER_UNITS: &[(&str, &str)] = &[];

/// §7.7 的代表示例（设计 §10.1 的 “25 个代表”超集：§7.7 列表 + `plan-mode`/`subagent`/
/// `custom-provider-anthropic`）。
const REPRESENTATIVES: &[&str] = &[
    "hello.ts",
    "permission-gate.ts",
    "input-transform.ts",
    "input-transform-streaming.ts",
    "prompt-customizer.ts",
    "provider-payload.ts",
    "dynamic-tools.ts",
    "todo.ts",
    "tools.ts",
    "preset.ts",
    "custom-compaction.ts",
    "trigger-compact.ts",
    "reload-runtime.ts",
    "event-bus.ts",
    "session-name.ts",
    "widget-placement.ts",
    "working-indicator.ts",
    "hidden-thinking-label.ts",
    "timed-confirm.ts",
    "overlay-test.ts",
    "rpc-demo.ts",
    "jev-router.ts",
    "structured-output.ts",
    "truncated-tool.ts",
    "custom-provider-anthropic/",
    "plan-mode/",
    "subagent/",
];

// ===========================================================================
// harness
// ===========================================================================

async fn run_local<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

/// 语料装配：看门狗放宽到 5 s。
///
/// 生产默认是 100 ms（设计 §2.7）；`sandbox/` 的 `@anthropic-ai/sandbox-runtime` 有 664 个
/// 模块，debug 构建下 oxc 去类型 + 声明明显慢于设计文档在 QuickJS 里的 80 ms 实测，因此
/// 语料测试用宿主可配的上限（内存/看门狗都是 EngineConfig 的一部分，不改变语义）。
fn corpus_generation() -> Generation {
    let config = EngineConfig {
        watchdog: Duration::from_secs(5),
        ..EngineConfig::default()
    };
    Generation::with_config(config).expect("assemble corpus generation")
}

async fn load_entry(entry: &Path) -> kiana_ext_api::types::LoadExtensionsResult {
    load_extensions(
        &[entry.to_string_lossy().into_owned()],
        &corpus_root().to_string_lossy(),
        None,
        None,
    )
    .await
}

// ===========================================================================
// 代表示例的行为 harness（设计 §10.1 第 3 条）
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

/// 记录 `kiana.*` 动作的可观察副作用。
#[derive(Debug, Default)]
struct ActionLog {
    session_name: Option<String>,
    appended: Vec<(String, Option<serde_json::Value>)>,
    labels: Vec<(String, Option<String>)>,
    active_tools: Vec<Vec<String>>,
    current_tools: Vec<String>,
    thinking_levels: Vec<String>,
    compact_calls: usize,
    compact_instructions: Vec<Option<String>>,
    shutdowns: usize,
    aborts: usize,
    reloads: usize,
    sent_user_messages: usize,
}

/// 记录 `ctx.ui.*` 的副作用；未特别实现的成员转发给 no-op 单例。
#[derive(Default)]
struct UiLog {
    notifications: Vec<(String, Option<NotifyKind>)>,
    statuses: Vec<(String, Option<String>)>,
    working_messages: Vec<Option<String>>,
    working_visible: Vec<bool>,
    working_indicators: Vec<Option<WorkingIndicatorOptions>>,
    hidden_labels: Vec<Option<String>>,
    widgets: Vec<(
        String,
        Option<Vec<String>>,
        Option<kiana_ext_api::ui::WidgetPlacement>,
    )>,
    titles: Vec<String>,
    editor_text: String,
    editor_texts_set: Vec<String>,
    footer_sets: usize,
    header_sets: usize,
    autocomplete_providers: usize,
    tools_expanded: bool,
    select_calls: Vec<String>,
    confirm_calls: Vec<String>,
    input_calls: Vec<String>,
    editor_calls: Vec<String>,
    custom_calls: usize,
    select_result: Option<String>,
    input_result: Option<String>,
    editor_result: Option<String>,
}

struct RecordingUi {
    inner: Arc<dyn ExtensionUiContext>,
    log: Mutex<UiLog>,
}

impl RecordingUi {
    fn new() -> Self {
        Self {
            inner: kiana_extensions::runner::noop_ui::no_op_ui_context(),
            log: Mutex::new(UiLog::default()),
        }
    }

    fn log(&self) -> std::sync::MutexGuard<'_, UiLog> {
        self.log.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

impl ExtensionUiContext for RecordingUi {
    fn select(
        &self,
        title: &str,
        _options: &[String],
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'_, Option<String>> {
        let result = {
            let mut log = self.log();
            log.select_calls.push(title.to_owned());
            log.select_result.clone()
        };
        Box::pin(async move { result })
    }

    fn confirm(
        &self,
        title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'_, bool> {
        self.log().confirm_calls.push(title.to_owned());
        Box::pin(async { false })
    }

    fn input(
        &self,
        title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'_, Option<String>> {
        let result = {
            let mut log = self.log();
            log.input_calls.push(title.to_owned());
            log.input_result.clone()
        };
        Box::pin(async move { result })
    }

    fn notify(&self, message: &str, kind: Option<NotifyKind>) {
        self.log().notifications.push((message.to_owned(), kind));
    }

    fn on_terminal_input(
        &self,
        _handler: TerminalInputHandler,
    ) -> kiana_core::event_bus::Unsubscribe {
        Box::new(|| {})
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.log()
            .statuses
            .push((key.to_owned(), text.map(str::to_owned)));
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.log().working_messages.push(message.map(str::to_owned));
    }

    fn set_working_visible(&self, visible: bool) {
        self.log().working_visible.push(visible);
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        self.log().working_indicators.push(options);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.log().hidden_labels.push(label.map(str::to_owned));
    }

    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<kiana_ext_api::ui::ExtensionWidgetOptions>,
    ) {
        let lines = content.map(|content| match content {
            WidgetContent::Lines(lines) => lines,
            WidgetContent::Component(_) => vec!["<component>".to_owned()],
        });
        self.log().widgets.push((
            key.to_owned(),
            lines,
            options.and_then(|options| options.placement),
        ));
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {
        self.log().footer_sets += 1;
    }

    fn set_header(&self, _factory: Option<HeaderFactory>) {
        self.log().header_sets += 1;
    }

    fn set_title(&self, title: &str) {
        self.log().titles.push(title.to_owned());
    }

    fn custom(
        &self,
        _factory: CustomFactory,
        _options: Option<CustomOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'_, Box<dyn std::any::Any + Send>> {
        self.log().custom_calls += 1;
        Box::pin(async { Box::new(()) as Box<dyn std::any::Any + Send> })
    }

    fn paste_to_editor(&self, text: &str) {
        self.log().editor_text.push_str(text);
    }

    fn set_editor_text(&self, text: &str) {
        let mut log = self.log();
        log.editor_text = text.to_owned();
        log.editor_texts_set.push(text.to_owned());
    }

    fn get_editor_text(&self) -> String {
        self.log().editor_text.clone()
    }

    fn editor(
        &self,
        title: &str,
        _prefill: Option<&str>,
    ) -> kiana_ext_api::LocalBoxFuture<'_, Option<String>> {
        let result = {
            let mut log = self.log();
            log.editor_calls.push(title.to_owned());
            log.editor_result.clone()
        };
        Box::pin(async move { result })
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {
        self.log().autocomplete_providers += 1;
    }

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> &Theme {
        self.inner.theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        self.inner.get_all_themes()
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        self.inner.get_theme(name)
    }

    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult {
        self.inner.set_theme(theme)
    }

    fn get_tools_expanded(&self) -> bool {
        self.log().tools_expanded
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.log().tools_expanded = expanded;
    }
}

fn tool_info(name: &str) -> ToolInfo {
    ToolInfo {
        name: name.to_owned(),
        description: name.to_owned(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
        prompt_guidelines: None,
        reaches_tools: None,
        exposure: kiana_ext_api::tool_types::ToolExposure::Direct,
        namespace: None,
        annotations: None,
        source_info: kiana_core::source_info::SourceInfo {
            path: format!("<builtin:{name}>"),
            source: "builtin".to_owned(),
            scope: kiana_core::source_info::SourceScope::User,
            origin: kiana_core::source_info::SourceOrigin::TopLevel,
            base_dir: None,
        },
    }
}

fn action_log() -> (
    Arc<Mutex<ActionLog>>,
    ExtensionActions,
    ExtensionContextActions,
    ExtensionCommandContextActions,
) {
    let log = Arc::new(Mutex::new(ActionLog {
        current_tools: vec![
            "read".to_owned(),
            "bash".to_owned(),
            "edit".to_owned(),
            "write".to_owned(),
        ],
        ..ActionLog::default()
    }));

    let actions = {
        let log = Arc::clone(&log);
        let set_name = Arc::clone(&log);
        let get_name = Arc::clone(&log);
        let set_label = Arc::clone(&log);
        let active = Arc::clone(&log);
        let get_active = Arc::clone(&log);
        let thinking = Arc::clone(&log);
        let tools: Vec<ToolInfo> = ["read", "bash", "edit", "write", "grep", "find", "ls"]
            .into_iter()
            .map(tool_info)
            .collect();
        ExtensionActions {
            send_message: Arc::new(|_options| {}),
            send_user_message: {
                let log = Arc::clone(&log);
                Arc::new(move |_content, _options| log.lock().unwrap().sent_user_messages += 1)
            },
            append_entry: {
                let log = Arc::clone(&log);
                Arc::new(move |custom_type, data| {
                    log.lock().unwrap().appended.push((custom_type, data));
                })
            },
            set_session_name: Arc::new(move |name| {
                set_name.lock().unwrap().session_name = Some(name);
            }),
            get_session_name: Arc::new(move || get_name.lock().unwrap().session_name.clone()),
            set_label: Arc::new(move |entry, label| {
                set_label.lock().unwrap().labels.push((entry, label));
            }),
            get_active_tools: Arc::new(move || get_active.lock().unwrap().current_tools.clone()),
            get_all_tools: Arc::new(move || tools.clone()),
            get_settings: Arc::new(kiana_config::settings::Settings::default),
            set_active_tools: {
                let active = Arc::clone(&active);
                Arc::new(move |names| {
                    let mut log = active.lock().unwrap();
                    log.active_tools.push(names.clone());
                    log.current_tools = names;
                })
            },
            refresh_tools: Arc::new(|| {}),
            get_commands: Arc::new(Vec::<kiana_core::slash_commands::SlashCommandInfo>::new),
            set_model: Arc::new(|_model| Box::pin(async { false })),
            get_thinking_level: Arc::new(|| kiana_agent_core::ThinkingLevel::Off),
            set_thinking_level: {
                let thinking = Arc::clone(&thinking);
                Arc::new(move |level| {
                    thinking
                        .lock()
                        .unwrap()
                        .thinking_levels
                        .push(format!("{level:?}"));
                })
            },
        }
    };

    let context_actions = {
        let compact = Arc::clone(&log);
        let shutdown = Arc::clone(&log);
        let abort = Arc::clone(&log);
        ExtensionContextActions {
            get_model: Arc::new(|| None),
            get_scoped_models: Arc::new(Vec::new),
            is_idle: Arc::new(|| true),
            is_project_trusted: Arc::new(|| true),
            get_signal: Arc::new(|| None),
            abort: Arc::new(move || abort.lock().unwrap().aborts += 1),
            has_pending_messages: Arc::new(|| false),
            shutdown: Arc::new(move || shutdown.lock().unwrap().shutdowns += 1),
            get_context_usage: Arc::new(|| None),
            compact: Arc::new(move |options| {
                let mut log = compact.lock().unwrap();
                log.compact_calls += 1;
                log.compact_instructions
                    .push(options.and_then(|options| options.custom_instructions));
            }),
            get_system_prompt: Arc::new(String::new),
            get_system_prompt_options: None,
            execute_tool: None,
            get_callable_tools: None,
        }
    };

    let command_actions = {
        let reload = Arc::clone(&log);
        ExtensionCommandContextActions {
            wait_for_idle: Arc::new(|| Box::pin(async {})),
            new_session: Arc::new(|_| Box::pin(async { Default::default() })),
            fork: Arc::new(|_, _| Box::pin(async { Default::default() })),
            navigate_tree: Arc::new(|_, _| Box::pin(async { Default::default() })),
            switch_session: Arc::new(|_, _| Box::pin(async { Default::default() })),
            reload: Arc::new(move || {
                let log = Arc::clone(&reload);
                Box::pin(async move { log.lock().unwrap().reloads += 1 })
            }),
        }
    };

    (log, actions, context_actions, command_actions)
}

struct CorpusHarness {
    generation: Generation,
    runner: Arc<ExtensionRunner>,
    ui: Arc<RecordingUi>,
    log: Arc<Mutex<ActionLog>>,
    cwd: tempfile::TempDir,
}

impl CorpusHarness {
    /// 加载一个代表示例并建立绑定好的 runner；`ui` 决定 `ctx.hasUI`。
    async fn load(generation: &Generation, name: &str, ui: bool) -> Self {
        let path = extensions_root().join(name.trim_end_matches('/'));
        let entry = if path.is_dir() {
            resolve_extension_entries(&path)
                .unwrap_or_else(|| panic!("{name} has no resolvable index"))
                .first()
                .map(PathBuf::from)
                .expect("one entry")
        } else {
            path
        };
        let result = load_entry(&entry).await;
        assert!(result.errors.is_empty(), "{name}: {:?}", result.errors);

        let cwd = tempfile::TempDir::new().expect("temp cwd");
        std::fs::create_dir_all(cwd.path().join(kiana_core::config::CONFIG_DIR_NAME))
            .expect("create .kiana");
        let model_runtime = kiana_config::models::runtime::ModelRuntime::create(
            kiana_config::models::runtime::CreateModelRuntimeOptions::default(),
        )
        .await
        .expect("model runtime");
        let model_registry = Arc::new(kiana_config::models::registry::ModelRegistry::new(
            Arc::new(model_runtime),
        ));
        let session: Arc<dyn ReadonlySessionManager> = Arc::new(FakeSessionManager {
            cwd: cwd.path().to_string_lossy().into_owned(),
        });
        let runner = Arc::new(ExtensionRunner::new(
            result.extensions,
            result.runtime,
            cwd.path().to_path_buf(),
            session,
            model_registry,
        ));
        let recording = Arc::new(RecordingUi::new());
        if ui {
            runner.set_ui_context(Some(recording.clone()), ExtensionMode::Tui);
        } else {
            runner.set_ui_context(None, ExtensionMode::Print);
        }
        let (log, actions, context_actions, command_actions) = action_log();
        runner.bind_core(actions, context_actions, None);
        runner.bind_command_context(Some(command_actions));

        Self {
            generation: generation.clone(),
            runner,
            ui: recording,
            log,
            cwd,
        }
    }

    fn log(&self) -> std::sync::MutexGuard<'_, ActionLog> {
        self.log.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn ui_log(&self) -> std::sync::MutexGuard<'_, UiLog> {
        self.ui.log()
    }

    async fn command(&self, name: &str, args: &str) {
        let command = self
            .runner
            .get_command(name)
            .unwrap_or_else(|| panic!("command {name} is registered"));
        let context = self.runner.create_command_context();
        (command.handler)(args.to_owned(), context).await;
    }

    async fn shortcut(&self, key: &str) {
        let handler = self.runner.with_extensions(|extensions| {
            extensions
                .iter()
                .find_map(|extension| extension.shortcuts.get(key).cloned())
                .unwrap_or_else(|| panic!("shortcut {key} is registered"))
                .handler
        });
        (handler)(self.runner.create_context()).await;
    }

    /// 泵空 JS job 与宿主完成队列（`ctx.compact` 等带回调的 host op 是异步受理的）。
    async fn drain(&self) {
        let engine = self.generation.engine();
        for _ in 0..64 {
            let mut progressed = engine.pump_jobs();
            progressed += engine.try_drain_completions();
            if progressed == 0 {
                tokio::task::yield_now().await;
                progressed = engine.pump_jobs() + engine.try_drain_completions();
                if progressed == 0 {
                    break;
                }
            }
        }
    }

    fn cwd(&self) -> &Path {
        self.cwd.path()
    }
}

fn text_of(outcome: &kiana_agent_core::AgentToolResult<serde_json::Value>) -> String {
    outcome
        .content
        .iter()
        .find_map(|block| match block {
            kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

impl CorpusHarness {
    async fn execute_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> kiana_agent_core::AgentToolResult<serde_json::Value> {
        let tool = self
            .runner
            .get_all_registered_tools()
            .into_iter()
            .find(|tool| tool.definition.name == name)
            .unwrap_or_else(|| panic!("tool {name} is registered"));
        tool.definition
            .execute
            .execute(
                "corpus-call",
                args,
                None,
                None,
                kiana_tools::definition::missing_tool_context(),
            )
            .await
            .unwrap_or_else(|error| panic!("tool {name} executes: {error:?}"))
    }
}

fn session_start_event() -> kiana_ext_api::events::ExtensionEvent {
    kiana_ext_api::events::ExtensionEvent::Session(kiana_ext_api::events::SessionEvent::Start(
        serde_json::from_value(serde_json::json!({
            "type": "session_start",
            "reason": "startup",
        }))
        .expect("session_start event"),
    ))
}

fn bash_tool_call(command: &str) -> kiana_ext_api::events::ToolCallEvent {
    serde_json::from_value(serde_json::json!({
        "type": "tool_call",
        "toolCallId": "corpus-call",
        "toolName": "bash",
        "input": { "command": command },
    }))
    .expect("bash tool_call event")
}

/// 一个入口的可比较注册摘要（§3.11 矩阵的 key 集合）。
#[derive(Debug, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(default)]
struct Registration {
    tools: Vec<String>,
    commands: Vec<String>,
    flags: Vec<String>,
    shortcuts: Vec<String>,
    handlers: BTreeMap<String, usize>,
    message_renderers: Vec<String>,
    entry_renderers: Option<Vec<String>>,
    tool_renderers: bool,
    markdown_transformer: bool,
    providers: Vec<String>,
    virtual_models: usize,
}

impl Registration {
    fn sorted(mut self) -> Self {
        self.tools.sort();
        self.commands.sort();
        self.flags.sort();
        self.shortcuts.sort();
        self.message_renderers.sort();
        if let Some(renderers) = self.entry_renderers.as_mut() {
            renderers.sort();
        }
        self.providers.sort();
        self
    }
}

fn registration_of(result: &kiana_ext_api::types::LoadExtensionsResult) -> Registration {
    let extension = result.extensions.first().expect("one loaded extension");
    let mut handlers = BTreeMap::new();
    for (event, handler_list) in &extension.handlers {
        handlers.insert(event.clone(), handler_list.len());
    }
    Registration {
        tools: extension.tools.keys().cloned().collect(),
        commands: extension.commands.keys().cloned().collect(),
        flags: extension.flags.keys().cloned().collect(),
        shortcuts: extension.shortcuts.keys().cloned().collect(),
        handlers,
        message_renderers: extension.message_renderers.keys().cloned().collect(),
        entry_renderers: extension
            .entry_renderers
            .as_ref()
            .map(|renderers| renderers.keys().cloned().collect()),
        tool_renderers: extension.tool_renderers.is_some(),
        markdown_transformer: extension.markdown_transformer.is_some(),
        providers: runtime_pending_provider_registrations(&result.runtime)
            .iter()
            .map(|provider| provider.name.clone())
            .collect(),
        virtual_models: runtime_pending_virtual_model_registrations(&result.runtime).len(),
    }
    .sorted()
}

fn is_known_skip(name: &str) -> bool {
    KNOWN_SKIPS.iter().any(|(entry, _)| *entry == name)
}

// ===========================================================================
// 1. 语料清单与去类型（设计 §10.1 第 1 条）
// ===========================================================================

#[test]
fn corpus_inventory_matches_the_spec() {
    let files = ts_files();
    assert_eq!(files.len(), 86, "86 .ts files (design §10.1 / §6.1)");

    let mut dirs: Vec<String> = std::fs::read_dir(extensions_root())
        .expect("extensions dir")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs,
        vec![
            "custom-provider-anthropic",
            "custom-provider-gitlab-duo",
            "doom-overlay",
            "dynamic-resources",
            "gondolin",
            "plan-mode",
            "sandbox",
            "subagent",
            "with-deps",
        ],
        "9 directory entries (design §7.2)"
    );

    let entries = entry_paths();
    assert_eq!(entries.len(), 79, "70 single files + 9 directories");
    assert_eq!(
        entries
            .iter()
            .filter(|(name, _)| name.ends_with('/'))
            .count(),
        9
    );
    assert!(extensions_root().join("README.md").is_file());
    assert!(
        corpus_root().join("node_modules").is_dir(),
        "vendored npm dependencies (CI needs no network)"
    );
    for package in [
        "node_modules/ms",
        "node_modules/shell-quote",
        "node_modules/zod",
        "node_modules/@anthropic-ai/sdk",
        "node_modules/@anthropic-ai/sandbox-runtime",
    ] {
        assert!(
            corpus_root().join(package).is_dir(),
            "missing vendored package {package}"
        );
    }
}

#[test]
fn corpus_ts_files_strip_without_diagnostics() {
    let mut failures = Vec::new();
    for file in ts_files() {
        let source = std::fs::read_to_string(&file).expect("corpus file is readable");
        if let Err(error) =
            kiana_extensions::loader::transpile::strip_types(&file.to_string_lossy(), &source)
        {
            failures.push(format!(
                "{}: {}",
                file.display(),
                error.diagnostics().join("\n")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "86-file corpus must strip with 0 diagnostics:\n{}",
        failures.join("\n")
    );
}

/// 每个 `.ts` 都能被真正的模块加载器声明并求值。
///
/// 非入口文件（`doom-engine.ts`、`plan-mode/utils.ts`、`subagent/agents.ts` …）没有默认导出，
/// 加载器会在**工厂阶段**报 `Extension does not export a valid factory function`——这正好证明
/// 模块体已经求值成功。已知的 C/D 与其它单元缺口按各自的文案记录。
#[tokio::test(flavor = "current_thread")]
async fn every_ts_file_declares_and_evaluates() {
    run_local(async {
        let _generation = corpus_generation();
        let root = extensions_root();
        let mut failures = Vec::new();
        for file in ts_files() {
            let relative = file
                .strip_prefix(&root)
                .expect("under corpus root")
                .to_string_lossy()
                .into_owned();
            let result = load_entry(&file).await;
            if result.errors.is_empty() {
                continue;
            }
            let errors = result
                .errors
                .iter()
                .map(|error| error.error.clone())
                .collect::<Vec<_>>()
                .join(" | ");
            let factory_only =
                errors.starts_with("Extension does not export a valid factory function:");
            // C/D 级目录的文件（`doom-overlay/*.ts`、`gondolin/index.ts`）按目录级原因跳过。
            let known_skip = KNOWN_SKIPS
                .iter()
                .any(|(name, _)| relative.starts_with(name.trim_end_matches('/')));
            if !(factory_only || known_skip) {
                failures.push(format!("{relative}: {errors}"));
            }
        }
        assert!(
            failures.is_empty(),
            "every corpus .ts must declare and evaluate:\n{}",
            failures.join("\n")
        );
    })
    .await;
}

// ===========================================================================
// 2. 79 个入口 + 注册矩阵（设计 §10.1 第 2 条）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn all_entries_load_and_match_the_registration_matrix() {
    run_local(async {
        let _generation = corpus_generation();
        let expected: BTreeMap<String, Registration> = serde_json::from_str(
            &std::fs::read_to_string(corpus_root().join("registrations.json"))
                .expect("registrations fixture"),
        )
        .expect("registrations fixture parses");

        // 夹具本身钉住两个 `registerShortcut` 的 §3.11 矩阵值（防止“重新生成夹具”
        // 把一条静默失效的注册路径固化掉）。
        assert_eq!(expected["preset.ts"].shortcuts, vec!["ctrl+shift+u"]);
        assert_eq!(expected["preset.ts"].flags, vec!["preset"]);
        assert_eq!(expected["plan-mode/"].shortcuts, vec!["ctrl+alt+p"]);
        assert_eq!(expected["plan-mode/"].flags, vec!["plan"]);

        let entries = entry_paths();
        let mut loaded = 0;
        let mut failures = Vec::new();
        for (name, path) in &entries {
            if is_known_skip(name) {
                continue;
            }
            let result = load_entry(path).await;
            if !result.errors.is_empty() {
                let errors = result
                    .errors
                    .iter()
                    .map(|error| error.error.clone())
                    .collect::<Vec<_>>()
                    .join(" | ");
                failures.push(format!("{name}: {errors}"));
                continue;
            }
            assert_eq!(
                result.extensions.len(),
                1,
                "{name} must produce exactly one extension"
            );
            // 契约错误（provider 配置等）对每个入口都必须为空。
            if !runtime_provider_config_errors(&result.runtime).is_empty() {
                failures.push(format!("{name}: provider config errors"));
            }
            if !runtime_pending_native_provider_registrations(&result.runtime).is_empty() {
                failures.push(format!(
                    "{name}: native providers need a real provider object"
                ));
            }
            let actual = registration_of(&result);
            match expected.get(name) {
                Some(wanted) if actual == *wanted => {}
                Some(wanted) => failures.push(format!(
                    "{name}: registration mismatch\n  expected: {wanted:?}\n  actual:   {actual:?}"
                )),
                None => failures.push(format!("{name}: missing from registrations.json")),
            }
            loaded += 1;
        }

        assert!(
            failures.is_empty(),
            "corpus entry failures:\n{}",
            failures.join("\n")
        );
        // 79 个入口 - 2 个 C/D（doom-overlay 需要 wasmtime、gondolin 需要原生 ssh2）。
        assert_eq!(loaded, 77, "every A-level corpus entry must load");
        assert!(
            BLOCKED_BY_OTHER_UNITS.is_empty(),
            "no corpus entry may be blocked by another unit"
        );
        assert_eq!(
            expected.keys().filter(|name| !is_known_skip(name)).count(),
            77,
            "registrations.json must cover every A-level entry"
        );
    })
    .await;
}

/// C/D 级示例必须带着记录的原因跳过（不是静默忽略）。
#[test]
fn known_skips_are_documented() {
    assert_eq!(KNOWN_SKIPS.len(), 2);
    let names: Vec<&str> = KNOWN_SKIPS.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, vec!["doom-overlay/", "gondolin/"]);
    for (name, reason) in KNOWN_SKIPS {
        assert!(!reason.is_empty(), "{name} needs a recorded reason");
        assert!(
            extensions_root().join(name).is_dir(),
            "{name} must exist in the corpus"
        );
    }
}

// ===========================================================================
// 3. §7.7 代表示例的行为（设计 §10.1 第 3 条）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn representative_examples_behave() {
    run_local(async {
        let generation = corpus_generation();
        let entries: BTreeMap<String, PathBuf> = entry_paths().into_iter().collect();
        assert_eq!(REPRESENTATIVES.len(), 27, "§7.7 + the three extra entries");

        for name in REPRESENTATIVES {
            let path = entries
                .get(*name)
                .unwrap_or_else(|| panic!("representative {name} is not an entry"));
            let result = load_entry(path).await;
            if !result.errors.is_empty() {
                let errors = result
                    .errors
                    .iter()
                    .map(|error| error.error.clone())
                    .collect::<Vec<_>>()
                    .join(" | ");
                panic!("{name} must load before its behavior can be asserted: {errors}");
            }
            let extension = result.extensions.first().expect("extension");
            match *name {
                "hello.ts" => {
                    // 工具可以真正执行：`kiana.registerTool` 的 JS 闭包经桥回到 Rust。
                    let registered = extension.tools.get("hello").expect("hello tool");
                    let outcome = registered
                        .definition
                        .execute
                        .execute(
                            "call-1",
                            serde_json::json!({ "name": "World" }),
                            None,
                            None,
                            kiana_tools::definition::missing_tool_context(),
                        )
                        .await
                        .expect("hello tool executes");
                    assert_eq!(text_of(&outcome), "Hello, World!");
                    assert_eq!(outcome.details, serde_json::json!({ "greeted": "World" }));
                }
                "permission-gate.ts" => {
                    // 无 UI：危险命令按 fail-closed 阻断，文案逐字。
                    let harness = CorpusHarness::load(&generation, name, false).await;
                    let mut event = bash_tool_call("sudo rm -rf /tmp/x");
                    let blocked = harness
                        .runner
                        .emit_tool_call(&mut event)
                        .await
                        .expect("tool_call dispatch")
                        .expect("permission-gate returns a result");
                    assert_eq!(blocked.block, Some(true));
                    assert_eq!(
                        blocked.reason.as_deref(),
                        Some("Dangerous command blocked (no UI for confirmation)")
                    );

                    // 无 UI 时非危险命令不阻断。
                    let mut safe = bash_tool_call("echo hello");
                    assert!(
                        harness
                            .runner
                            .emit_tool_call(&mut safe)
                            .await
                            .expect("tool_call dispatch")
                            .is_none()
                    );
                }
                "input-transform.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    let transformed = harness
                        .runner
                        .emit_input(
                            "?quick What is TypeScript?",
                            None,
                            kiana_ext_api::events::InputSource::Interactive,
                            None,
                        )
                        .await
                        .expect("input dispatch");
                    match transformed {
                        kiana_ext_api::events::InputEventResult::Transform { text, .. } => {
                            assert_eq!(
                                text,
                                "Respond briefly in 1-2 sentences: What is TypeScript?"
                            )
                        }
                        other => panic!("expected transform, got {other:?}"),
                    }
                    assert_eq!(
                        harness
                            .runner
                            .emit_input(
                                "ping",
                                None,
                                kiana_ext_api::events::InputSource::Interactive,
                                None
                            )
                            .await
                            .expect("input dispatch"),
                        kiana_ext_api::events::InputEventResult::Handled
                    );
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "pong")
                    );
                }
                "input-transform-streaming.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    // 非 trigger 文本直接放行；steering 期间也放行（不调用 exec）。
                    for (text, streaming) in [
                        ("hello there", None),
                        (
                            "show me the changes",
                            Some(kiana_ext_api::events::StreamingBehavior::Steer),
                        ),
                    ] {
                        assert_eq!(
                            harness
                                .runner
                                .emit_input(
                                    text,
                                    None,
                                    kiana_ext_api::events::InputSource::Interactive,
                                    streaming
                                )
                                .await
                                .expect("input dispatch"),
                            kiana_ext_api::events::InputEventResult::Continue
                        );
                    }
                }
                "prompt-customizer.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    let options = kiana_ext_api::BuildSystemPromptOptions {
                        cwd: harness.cwd().to_string_lossy().into_owned(),
                        selected_tools: Some(vec!["read".to_owned(), "bash".to_owned()]),
                        ..Default::default()
                    };
                    let combined = harness
                        .runner
                        .emit_before_agent_start(
                            kiana_extensions::runner::BeforeAgentStartTrigger::User {
                                prompt: "hello".to_owned(),
                                images: None,
                            },
                            options,
                        )
                        .await
                        .expect("before_agent_start dispatch");
                    let guidance = combined
                        .system_prompt_options
                        .sections
                        .get("tool_guidance")
                        .expect("prompt-customizer writes tool_guidance");
                    assert!(guidance.contains("Use `read` for file contents"));
                    assert!(guidance.contains("Use `bash` for file operations"));
                }
                "provider-payload.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness
                        .runner
                        .emit_before_provider_request(serde_json::json!({
                            "model": "corpus-model",
                            "temperature": 1,
                        }))
                        .await
                        .expect("before_provider_request dispatch");
                    let event = kiana_ext_api::events::ExtensionEvent::AfterProviderResponse(
                        serde_json::from_value(serde_json::json!({
                            "type": "after_provider_response",
                            "status": 201,
                            "headers": { "x-test": "1" },
                        }))
                        .expect("after_provider_response event"),
                    );
                    harness.runner.emit(event).await.expect("emit response");

                    let log_path = harness
                        .cwd()
                        .join(kiana_core::config::CONFIG_DIR_NAME)
                        .join("provider-payload.log");
                    let log = std::fs::read_to_string(&log_path).expect("payload log");
                    assert!(log.contains("\"model\": \"corpus-model\""));
                    assert!(log.contains("[201]"));
                }
                "dynamic-tools.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    // session_start 注册第一个动态工具。
                    harness
                        .runner
                        .emit(session_start_event())
                        .await
                        .expect("session_start dispatch");
                    let outcome = harness
                        .execute_tool("echo_session", serde_json::json!({ "message": "hi" }))
                        .await;
                    assert_eq!(text_of(&outcome), "[session] hi");

                    // 命令在运行期再注册一个工具，注册立即生效。
                    harness.command("add-echo-tool", "foo").await;
                    let outcome = harness
                        .execute_tool("foo", serde_json::json!({ "message": "bar" }))
                        .await;
                    assert_eq!(text_of(&outcome), "[foo] bar");
                    assert_eq!(
                        outcome.details,
                        serde_json::json!({ "tool": "foo", "prefix": "[foo] " })
                    );
                }
                "todo.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    let added = harness
                        .execute_tool(
                            "todo",
                            serde_json::json!({ "action": "add", "text": "write tests" }),
                        )
                        .await;
                    assert_eq!(text_of(&added), "Added todo #1: write tests");
                    assert_eq!(added.details["nextId"], serde_json::json!(2));

                    let toggled = harness
                        .execute_tool("todo", serde_json::json!({ "action": "toggle", "id": 1 }))
                        .await;
                    assert_eq!(text_of(&toggled), "Todo #1 completed");
                    assert_eq!(toggled.details["todos"][0]["done"], serde_json::json!(true));

                    let listed = harness
                        .execute_tool("todo", serde_json::json!({ "action": "list" }))
                        .await;
                    assert_eq!(text_of(&listed), "[x] #1: write tests");
                }
                "tools.ts" => {
                    // 非 TUI 模式：命令给出逐字错误提示，不打开选择器。
                    let print = CorpusHarness::load(&generation, name, true).await;
                    // 记录型 UI + print 模式：`ctx.hasUI` 为真，但 `ctx.mode !== "tui"`。
                    print
                        .runner
                        .set_ui_context(Some(print.ui.clone()), ExtensionMode::Print);
                    print.command("tools", "").await;
                    assert!(
                        print.ui_log().notifications.iter().any(|(message, kind)| {
                            message == "/tools requires TUI mode"
                                && *kind == Some(NotifyKind::Error)
                        }),
                        "notifications: {:?}",
                        print.ui_log().notifications
                    );

                    // TUI 模式：命令确实打开自定义组件（记录型 UI 直接完成它）。
                    let tui = CorpusHarness::load(&generation, name, true).await;
                    tui.command("tools", "").await;
                    assert_eq!(tui.ui_log().custom_calls, 1);
                }
                "custom-compaction.ts" => {
                    // 处理器在模型目录里找 Gemini Flash 并发起真实补全；CI 不能触网，
                    // 这里只断言注册与“自定义压缩”分支存在（行为由 §7.2 的 runtime 用例覆盖）。
                    assert!(extension.handlers.contains_key("session_before_compact"));
                }
                "trigger-compact.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness
                        .command("trigger-compact", "  focus on auth  ")
                        .await;
                    harness.drain().await;
                    let log = harness.log();
                    assert_eq!(log.compact_calls, 1);
                    assert_eq!(
                        log.compact_instructions,
                        vec![Some("focus on auth".to_owned())]
                    );
                    drop(log);
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "Compaction started")
                    );
                }
                "reload-runtime.ts" => {
                    let harness = CorpusHarness::load(&generation, name, false).await;
                    harness.command("reload-runtime", "").await;
                    assert_eq!(harness.log().reloads, 1);

                    let outcome = harness
                        .execute_tool("reload_runtime", serde_json::json!({}))
                        .await;
                    assert_eq!(
                        text_of(&outcome),
                        "Queued /reload-runtime as a follow-up command."
                    );
                    assert_eq!(harness.log().sent_user_messages, 1);
                }
                "event-bus.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness
                        .runner
                        .emit(session_start_event())
                        .await
                        .expect("session_start dispatch");
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, kind)| {
                                message == "Event from event-bus-example: Session started"
                                    && *kind == Some(NotifyKind::Info)
                            }),
                        "notifications: {:?}",
                        harness.ui_log().notifications
                    );

                    harness.command("emit", "hello").await;
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| { message == "Event from /emit command: hello" }),
                        "notifications: {:?}",
                        harness.ui_log().notifications
                    );
                }
                "session-name.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.command("session-name", "regression run").await;
                    assert_eq!(
                        harness.log().session_name.as_deref(),
                        Some("regression run")
                    );
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "Session named: regression run")
                    );
                    harness.command("session-name", "").await;
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| { message == "Session: regression run" }),
                        "notifications: {:?}",
                        harness.ui_log().notifications
                    );
                }
                "widget-placement.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness
                        .runner
                        .emit(session_start_event())
                        .await
                        .expect("session_start dispatch");
                    let widgets = harness.ui_log().widgets.clone();
                    assert_eq!(widgets.len(), 2);
                    assert_eq!(widgets[0].0, "widget-above");
                    assert_eq!(
                        widgets[0].1.as_deref(),
                        Some(["Above editor widget".to_owned()].as_slice())
                    );
                    assert_eq!(
                        widgets[0].2,
                        Some(kiana_ext_api::ui::WidgetPlacement::AboveEditor),
                        "missing placement normalizes to the aboveEditor default"
                    );
                    assert_eq!(widgets[1].0, "widget-below");
                    assert_eq!(
                        widgets[1].2,
                        Some(kiana_ext_api::ui::WidgetPlacement::BelowEditor)
                    );
                }
                "working-indicator.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness
                        .runner
                        .emit(session_start_event())
                        .await
                        .expect("session_start dispatch");
                    {
                        let log = harness.ui_log();
                        let spinner = log.working_indicators[0].as_ref().expect("spinner");
                        assert_eq!(spinner.frames.as_ref().map(Vec::len), Some(10));
                        assert_eq!(spinner.interval_ms, Some(80.0));
                        assert!(
                            log.statuses
                                .iter()
                                .any(|(key, text)| key == "working-indicator"
                                    && text.as_deref().is_some_and(|text| {
                                        text.contains("Indicator: custom spinner")
                                    }))
                        );
                    }
                    harness.command("working-indicator", "dot").await;
                    {
                        let log = harness.ui_log();
                        let dot = log
                            .working_indicators
                            .last()
                            .cloned()
                            .flatten()
                            .expect("dot");
                        assert_eq!(dot.frames.as_ref().map(Vec::len), Some(1));
                    }
                    assert!(
                        harness.ui_log().notifications.iter().any(|(message, _)| {
                            message == "Working indicator set to: static dot"
                        })
                    );
                    harness.command("working-indicator", "reset").await;
                    assert!(
                        harness
                            .ui_log()
                            .working_indicators
                            .last()
                            .cloned()
                            .flatten()
                            .is_none(),
                        "reset restores the built-in spinner (undefined)"
                    );
                }
                "hidden-thinking-label.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.command("thinking-label", "Reasoning").await;
                    assert_eq!(
                        harness
                            .ui_log()
                            .hidden_labels
                            .last()
                            .cloned()
                            .flatten()
                            .as_deref(),
                        Some("Reasoning")
                    );
                    harness.command("thinking-label", "").await;
                    assert_eq!(
                        harness.ui_log().hidden_labels.last().cloned().flatten(),
                        None,
                        "empty args reset the label"
                    );
                }
                "timed-confirm.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.command("timed", "").await;
                    assert_eq!(harness.ui_log().confirm_calls, vec!["Timed Confirmation"]);
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "Cancelled or timed out")
                    );

                    harness.command("timed-select", "").await;
                    assert_eq!(harness.ui_log().select_calls, vec!["Pick an option"]);
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "Selection cancelled or timed out")
                    );

                    harness.command("timed-signal", "").await;
                    assert_eq!(harness.ui_log().confirm_calls.len(), 2);
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "Cancelled by user"),
                        "notifications: {:?}",
                        harness.ui_log().notifications
                    );
                }
                "overlay-test.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.command("overlay-test", "").await;
                    assert_eq!(
                        harness.ui_log().custom_calls,
                        1,
                        "the command opens the overlay custom component"
                    );
                }
                "rpc-demo.ts" => {
                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.ui_log().input_result = Some("hello rpc".to_owned());
                    harness.command("rpc-input", "").await;
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "You entered: hello rpc")
                    );

                    harness.ui_log().editor_result = Some("line1\nline2".to_owned());
                    harness.command("rpc-editor", "").await;
                    assert_eq!(harness.ui_log().editor_calls, vec!["Edit some text"]);
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message == "Editor submitted (2 lines)")
                    );

                    harness.command("rpc-prefill", "").await;
                    assert_eq!(
                        harness.ui_log().editor_texts_set.last().map(String::as_str),
                        Some("This text was set by the rpc-demo extension.")
                    );

                    harness
                        .runner
                        .emit(session_start_event())
                        .await
                        .expect("session_start dispatch");
                    let log = harness.ui_log();
                    assert_eq!(log.titles, vec!["kiana RPC Demo"]);
                    assert!(log.widgets.iter().any(|(key, _, _)| key == "rpc-demo"));
                    assert!(log.statuses.iter().any(|(key, _)| key == "rpc-demo"));
                }
                "jev-router.ts" => {
                    let registrations =
                        runtime_pending_virtual_model_registrations(&result.runtime);
                    assert_eq!(registrations.len(), 1, "registerVirtualModel");
                    let definition = &registrations[0].definition;
                    assert_eq!(definition.provider, "jev");
                    assert_eq!(definition.id, "auto");
                }
                "structured-output.ts" => {
                    let tool = extension
                        .tools
                        .get("structured_output")
                        .expect("structured_output tool");
                    // 示例本身用 `promptSnippet`/`promptGuidelines` + `terminate`（没有 outputSchema）；
                    // 这三个元数据必须原样穿过注册路径。
                    assert_eq!(
                        tool.definition.prompt_snippet.as_deref(),
                        Some("Emit a final structured answer as a terminating tool result")
                    );
                    assert_eq!(
                        tool.definition.prompt_guidelines.as_ref().map(Vec::len),
                        Some(2)
                    );
                    assert_eq!(
                        tool.definition.parameters["type"],
                        serde_json::json!("object")
                    );

                    let harness = CorpusHarness::load(&generation, name, true).await;
                    let outcome = harness
                        .execute_tool(
                            "structured_output",
                            serde_json::json!({
                                "headline": "Done",
                                "summary": "All checks passed.",
                                "actionItems": ["ship it"],
                            }),
                        )
                        .await;
                    assert_eq!(text_of(&outcome), "Saved structured output: Done");
                    assert_eq!(
                        outcome.details["actionItems"],
                        serde_json::json!(["ship it"])
                    );
                    assert_eq!(outcome.terminate, Some(true));
                }
                "truncated-tool.ts" => {
                    let rg = extension.tools.get("rg").expect("rg tool");
                    // 描述里嵌了 `DEFAULT_MAX_LINES`/`DEFAULT_MAX_BYTES`（`formatSize`）等
                    // coding-agent 直接值，证明虚拟 namespace 的常量导入可用。
                    assert!(
                        rg.definition.description.contains("Output is truncated to"),
                        "description: {}",
                        rg.definition.description
                    );
                    assert!(
                        rg.definition.description.contains("2000 lines"),
                        "description: {}",
                        rg.definition.description
                    );
                }
                "custom-provider-anthropic/" => {
                    let providers = runtime_pending_provider_registrations(&result.runtime);
                    assert_eq!(providers.len(), 1);
                    assert_eq!(providers[0].name, "custom-anthropic");
                    assert!(
                        providers[0]
                            .config
                            .models
                            .as_ref()
                            .is_some_and(|models| !models.is_empty()),
                        "provider registers models"
                    );
                }
                "subagent/" => {
                    assert!(extension.tools.contains_key("subagent"));
                }
                "preset.ts" => {
                    assert!(extension.flags.contains_key("preset"));
                    assert!(extension.shortcuts.contains_key("ctrl+shift+u"));

                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.command("preset", "unknown").await;
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, kind)| {
                                message == "Unknown preset \"unknown\". Available: (none defined)"
                                    && *kind == Some(NotifyKind::Error)
                            }),
                        "notifications: {:?}",
                        harness.ui_log().notifications
                    );
                }
                "plan-mode/" => {
                    assert!(extension.flags.contains_key("plan"));
                    assert!(extension.shortcuts.contains_key("ctrl+alt+p"));

                    let harness = CorpusHarness::load(&generation, name, true).await;
                    harness.command("plan", "").await;
                    {
                        let log = harness.log();
                        let tools = log.active_tools.last().expect("setActiveTools");
                        assert!(tools.contains(&"read".to_owned()));
                        assert!(tools.contains(&"grep".to_owned()));
                        assert!(tools.contains(&"questionnaire".to_owned()));
                        assert!(!tools.contains(&"edit".to_owned()));
                        assert!(!tools.contains(&"write".to_owned()));
                    }
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message
                                == "Plan mode enabled. Built-in write tools disabled.")
                    );

                    // 计划模式下危险 bash 被拦截（文案包含逐字前缀）。
                    let mut event = bash_tool_call("rm -rf /tmp/x");
                    let blocked = harness
                        .runner
                        .emit_tool_call(&mut event)
                        .await
                        .expect("tool_call dispatch")
                        .expect("plan-mode blocks destructive bash");
                    assert_eq!(blocked.block, Some(true));
                    assert!(
                        blocked.reason.as_deref().is_some_and(|reason| reason
                            .starts_with("Plan mode: command blocked (not allowlisted).")),
                        "reason: {blocked:?}"
                    );

                    // 快捷键再次切换，恢复进入前的工具集合。
                    harness.shortcut("ctrl+alt+p").await;
                    assert!(
                        harness
                            .ui_log()
                            .notifications
                            .iter()
                            .any(|(message, _)| message
                                == "Plan mode disabled. Full access restored.")
                    );
                    assert_eq!(
                        harness.log().active_tools.last().cloned(),
                        Some(vec![
                            "read".to_owned(),
                            "bash".to_owned(),
                            "edit".to_owned(),
                            "write".to_owned()
                        ])
                    );
                }
                other => panic!("unhandled representative {other}"),
            }
        }
    })
    .await;
}

/// `with-deps/` 的烟雾测试：扩展自己的 npm 依赖（`ms@2.1.3`）在工具执行时可用。
///
/// 语料把依赖 vendored 到 `tests/fixtures/corpus/node_modules/`（真实安装时它在
/// `with-deps/node_modules/`；解析器从入口文件逐级向上找，二者等价）。
#[tokio::test(flavor = "current_thread")]
async fn with_deps_example_executes_with_its_npm_dependency() {
    run_local(async {
        let _generation = corpus_generation();
        let entry = extensions_root().join("with-deps").join("index.ts");
        let result = load_entry(&entry).await;
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let extension = result.extensions.first().expect("with-deps loads");
        let tool = extension
            .tools
            .get("parse_duration")
            .expect("parse_duration tool");
        let outcome = tool
            .definition
            .execute
            .execute(
                "call-1",
                serde_json::json!({ "duration": "1h" }),
                None,
                None,
                kiana_tools::definition::missing_tool_context(),
            )
            .await
            .expect("parse_duration executes");
        let text = outcome
            .content
            .iter()
            .find_map(|block| match block {
                kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .expect("text content");
        assert_eq!(text, "1h = 3600000 milliseconds");
    })
    .await;
}

// ===========================================================================
// 4. 设计文档 §10.4 的真实 npm 包回归
// ===========================================================================

/// 写一个临时扩展（放在语料根下，让裸包名解析到 `tests/fixtures/corpus/node_modules`）。
fn write_temp_extension(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, source).expect("write temp extension");
    path
}

async fn run_regression_extension(source: &str) -> (serde_json::Value, Vec<String>) {
    let temp = tempfile::Builder::new()
        .prefix(".corpus-regression-")
        .tempdir_in(corpus_root())
        .expect("temp dir in corpus");
    let path = write_temp_extension(temp.path(), "regression.ts", source);
    let result = load_extensions(
        &[path.to_string_lossy().into_owned()],
        &temp.path().to_string_lossy(),
        None,
        None,
    )
    .await;
    let errors = result
        .errors
        .iter()
        .map(|error| error.error.clone())
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return (serde_json::Value::Null, errors);
    }
    let extension = result.extensions.first().expect("loaded");
    let registered = extension.tools.get("probe").expect("probe tool");
    let outcome = registered
        .definition
        .execute
        .execute(
            "regression-call",
            serde_json::json!({}),
            None,
            None,
            kiana_tools::definition::missing_tool_context(),
        )
        .await
        .expect("regression tool executes");
    (outcome.details, Vec::new())
}

#[tokio::test(flavor = "current_thread")]
async fn real_npm_package_regressions() {
    run_local(async {
        let _generation = corpus_generation();

        // 1. 多文件 CJS：`shell-quote`（入口 require 了 quote.js / parse.js）。
        let (details, errors) = run_regression_extension(
            r#"
import { quote, parse } from "shell-quote";

export default function (kiana) {
    kiana.registerTool({
        name: "probe",
        label: "probe",
        description: "probe",
        parameters: { type: "object", properties: {} },
        execute: async () => ({
            content: [{ type: "text", text: "ok" }],
            details: {
                quoted: quote(["a b", "c"]),
                roundTrip: parse(quote(["a b", "c"])),
                modules: Object.keys(globalThis.__kiana_cjs ?? {}).filter((key) =>
                    /(quote|parse)\.js$/.test(key),
                ).length,
            },
        }),
    });
}
"#,
        )
        .await;
        assert!(errors.is_empty(), "shell-quote load: {errors:?}");
        // 1.12.0（sandbox-runtime 的 `^1.8.3` 同代）用单引号；设计 §10.4 的 `a\\ b` 是旧版形态。
        assert_eq!(details["quoted"], serde_json::json!("'a b' c"));
        assert_eq!(details["roundTrip"], serde_json::json!(["a b", "c"]));
        assert_eq!(details["modules"], serde_json::json!(2));

        // 2. `zod`：`safeParse({allowedDomains:'nope'})` 的 issue code。
        let (details, errors) = run_regression_extension(
            r#"
import { z } from "zod";

export default function (kiana) {
    kiana.registerTool({
        name: "probe",
        label: "probe",
        description: "probe",
        parameters: { type: "object", properties: {} },
        execute: async () => {
            const schema = z.object({ allowedDomains: z.array(z.string()) });
            const network = schema.safeParse({ allowedDomains: "nope" });
            return {
                content: [{ type: "text", text: "ok" }],
                details: {
                    success: network.success,
                    code: network.error.issues[0].code,
                    ok: schema.safeParse({ allowedDomains: ["example.com"] }).success,
                },
            };
        },
    });
}
"#,
        )
        .await;
        assert!(errors.is_empty(), "zod load: {errors:?}");
        assert_eq!(details["success"], serde_json::json!(false));
        assert_eq!(details["code"], serde_json::json!("invalid_type"));
        assert_eq!(details["ok"], serde_json::json!(true));

        // 3. `@anthropic-ai/sandbox-runtime` 的 `getDefaultWritePaths()` 10 项（ESM 包）。
        let (details, errors) = run_regression_extension(
            r#"
import { getDefaultWritePaths } from "@anthropic-ai/sandbox-runtime";

export default function (kiana) {
    kiana.registerTool({
        name: "probe",
        label: "probe",
        description: "probe",
        parameters: { type: "object", properties: {} },
        execute: async () => ({
            content: [{ type: "text", text: "ok" }],
            details: { count: getDefaultWritePaths().length },
        }),
    });
}
"#,
        )
        .await;
        assert!(errors.is_empty(), "sandbox-runtime load: {errors:?}");
        assert_eq!(details["count"], serde_json::json!(10));

        // 4. `@anthropic-ai/sdk` + 假 fetch 的 6 个 SSE chunk：
        //    `text` 收到 ["hello"]、`finalMessage().content[0].text === "hello"`、
        //    `AbortController.prototype.abort` 调用次数为 0（抓“定时器立即触发”的 bug）。
        let (details, errors) = run_regression_extension(
            r#"
import Anthropic from "@anthropic-ai/sdk";

const events = [
    'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":0}}}\n\n',
    'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
    'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}\n\n',
    'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
    'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}\n\n',
    'event: message_stop\ndata: {"type":"message_stop"}\n\n',
];

export default function (kiana) {
    kiana.registerTool({
        name: "probe",
        label: "probe",
        description: "probe",
        parameters: { type: "object", properties: {} },
        execute: async () => {
            const state = { texts: [], abortCount: 0, fetchCalls: 0 };
            const originalAbort = AbortController.prototype.abort;
            AbortController.prototype.abort = function () {
                state.abortCount++;
                return originalAbort.call(this);
            };
            globalThis.fetch = function (url, init) {
                state.fetchCalls++;
                state.requestUrl = String(url);
                state.requestBody = init && init.body;
                const encoder = new TextEncoder();
                const stream = new ReadableStream({
                    start(controller) {
                        for (const event of events) controller.enqueue(encoder.encode(event));
                        controller.close();
                    },
                });
                return Promise.resolve(new Response(stream, {
                    status: 200,
                    headers: { "content-type": "text/event-stream", "request-id": "req_test" },
                }));
            };
            try {
                const client = new Anthropic({ apiKey: "test-key", maxRetries: 0 });
                const stream = client.messages.stream({
                    model: "claude-3-5-sonnet-20241022",
                    max_tokens: 16,
                    messages: [{ role: "user", content: "hi" }],
                });
                stream.on("text", (delta) => state.texts.push(delta));
                const message = await stream.finalMessage();
                state.finalText = message.content[0].text;
                state.stopReason = message.stop_reason;
            } catch (error) {
                state.error = error && error.stack ? error.stack : String(error);
            }
            AbortController.prototype.abort = originalAbort;
            return {
                content: [{ type: "text", text: "ok" }],
                details: JSON.parse(JSON.stringify(state)),
            };
        },
    });
}
"#,
        )
        .await;
        assert!(errors.is_empty(), "Anthropic SDK load: {errors:?}");
        assert_eq!(details["error"], serde_json::Value::Null, "{details}");
        assert_eq!(details["texts"], serde_json::json!(["hello"]));
        assert_eq!(details["finalText"], serde_json::json!("hello"));
        assert_eq!(details["stopReason"], serde_json::json!("end_turn"));
        assert_eq!(details["abortCount"], serde_json::json!(0));
        assert_eq!(details["fetchCalls"], serde_json::json!(1));
    })
    .await;
}

/// 设计 §10.4 第 4 条的反例断言：`setTimeout(cb, 10)` 不得在微任务阶段执行；
/// `ReadableStream` 的 `pull` 同步 `enqueue` 后 `read()` 必须拿到值（不得挂起）。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timer_and_readable_stream_invariants() {
    run_local(async {
        let generation = corpus_generation();
        let engine = generation.engine();
        engine
            .eval_script::<()>(
                r#"
                globalThis.__invariants = { timer: "pending", read: "pending" };
                setTimeout(function () { __invariants.timer = "fired"; }, 10);
                var stream = new ReadableStream({
                    pull: function (controller) {
                        controller.enqueue(new TextEncoder().encode("value"));
                        controller.close();
                    },
                });
                stream.getReader().read().then(function (result) {
                    __invariants.read = result.done
                        ? "done"
                        : new TextDecoder().decode(result.value);
                });
                "#,
                "<invariants>",
            )
            .expect("eval invariants");

        // 只泵微任务（不推进 tokio 时间）：定时器不得触发。
        engine.pump_jobs();
        let value = engine
            .eval_script::<String>("JSON.stringify(globalThis.__invariants)", "<invariants>")
            .expect("read invariants");
        let value: serde_json::Value = serde_json::from_str(&value).expect("json");
        assert_eq!(
            value["timer"], "pending",
            "setTimeout(cb, 10) must not run during the microtask phase"
        );
        assert_eq!(
            value["read"], "value",
            "ReadableStream pull+enqueue must be readable without waiting for timers"
        );

        // 推进虚拟时间 10 ms：定时器队列在 tokio 到点后触发。
        let due = engine
            .next_timer_deadline()
            .expect("setTimeout(cb, 10) must schedule a timer");
        tokio::time::sleep_until(due).await;
        engine.fire_due_timers();
        engine.pump_jobs();
        let value = engine
            .eval_script::<String>("JSON.stringify(globalThis.__invariants)", "<invariants>")
            .expect("read invariants");
        let value: serde_json::Value = serde_json::from_str(&value).expect("json");
        assert_eq!(
            value["timer"], "fired",
            "the due timer must run once the deadline passes"
        );
    })
    .await;
}
