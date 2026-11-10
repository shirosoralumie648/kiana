//! `kiana-interactive` 测试共享夹具。
//!
//! 本文件由 support-subsystems 单元创建，后续单元（im-core / runtime / commands /
//! msg-render）复用同一份：
//!
//! * [`TestTerminal`]：`packages/tui/test/virtual-terminal.ts` 的 Rust 等价物（捕获写入、
//!   start/stop、输入/resize 回调、光标/恢复调用、OSC 7501 上报）。
//! * [`FakeTui`]：只记录计数器的极简 [`Tui`]，用于只关心 requestRender/stop 的组件测试。
//! * [`FauxFixture`]：faux provider + `AgentSession` 的端到端夹具（建模自
//!   `crates/kiana-session/tests/session_support/mod.rs`）。
//! * [`run_local`] / [`wait_until`]：`LocalSet` 上的异步测试驱动与轮询等待。
//!
//! 刻意不 import `msg-render` 的任何类型：msg-render 依赖本单元，反向引用会成环。

#![allow(
    dead_code,
    reason = "共享测试基础设施：各测试 crate 只用到自己需要的部分"
)]
#![allow(
    clippy::arc_with_non_send_sync,
    reason = "DefaultResourceLoader 不是 Send/Sync（扩展运行时是单线程的），Arc 只为共享句柄"
)]

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_agent_core::types::{AgentMessage, AgentToolResult, AgentToolUpdateCallback};

use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::models::provider::{CreateProviderOptions, ProviderApiImpl, create_provider};
use kiana_ai::providers::faux::{
    FauxCore, FauxModelDefinition, FauxResponseStep, RegisterFauxProviderOptions, create_faux_core,
    faux_assistant_message,
};
use kiana_ai::types::{
    Message, Model, ModelThinkingLevel, TextContent, TextKind, UserContentBlock,
};
use kiana_config::auth::storage::{AuthStorage, auth_data_from_value};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::Settings;
use kiana_ext_api::types::ExtensionFactory;
use kiana_resources::loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use kiana_session::agent_session::AgentSession;
use kiana_session::sdk::{CreateAgentSessionOptions, create_agent_session};
use kiana_session_core::session_manager::SessionManager;
use kiana_tui::alt_screen::TuiAltScreenOptions;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal, TerminalColors};
use kiana_tui::tui::{
    Component, FocusTarget, InputListenerId, OverlayBounds, OverlayHandle, OverlayOptions,
    OverlayUnfocusOptions, Tui, TuiInputListener, TuiMode, TuiStopOptions,
};
use serde_json::json;

// ============================================================================
// 捕获型终端
// ============================================================================

type InputHandler = Box<dyn FnMut(&str)>;
type ResizeHandler = Box<dyn FnMut()>;

/// 捕获写入的测试终端（`virtual-terminal.ts` 的 Rust 等价物）。
pub struct TestTerminal {
    columns: usize,
    rows: usize,
    /// 累计写入的字节流。
    pub output: Rc<RefCell<String>>,
    input_handler: Option<InputHandler>,
    resize_handler: Option<ResizeHandler>,
    started: bool,
    stopped: bool,
    cursor_visible: Rc<Cell<bool>>,
    restore_calls: Rc<RefCell<Vec<(String, String)>>>,
    program_status: Rc<RefCell<Vec<ProgramStatus>>>,
}

impl TestTerminal {
    /// 80x24 的默认尺寸（与 TS 的 `new VirtualTerminal()` 一致）。
    pub fn new() -> Self {
        TestTerminal::with_size(80, 24)
    }

    /// 指定尺寸。
    pub fn with_size(columns: usize, rows: usize) -> Self {
        TestTerminal {
            columns,
            rows,
            output: Rc::new(RefCell::new(String::new())),
            input_handler: None,
            resize_handler: None,
            started: false,
            stopped: false,
            cursor_visible: Rc::new(Cell::new(true)),
            restore_calls: Rc::new(RefCell::new(Vec::new())),
            program_status: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// `restore_on_exit` 的调用记录（按调用顺序）。
    pub fn restore_calls(&self) -> Vec<(String, String)> {
        self.restore_calls.borrow().clone()
    }

    /// 共享的 `restore_on_exit` 记录。
    pub fn restore_calls_handle(&self) -> Rc<RefCell<Vec<(String, String)>>> {
        Rc::clone(&self.restore_calls)
    }

    /// 共享的光标可见性句柄。
    pub fn cursor_visible_handle(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.cursor_visible)
    }

    /// 共享输出缓冲。
    pub fn output_handle(&self) -> Rc<RefCell<String>> {
        Rc::clone(&self.output)
    }

    /// 共享的 OSC 7501 上报记录。
    pub fn program_status_handle(&self) -> Rc<RefCell<Vec<ProgramStatus>>> {
        Rc::clone(&self.program_status)
    }

    /// 取出并清空累计输出。
    pub fn take_output(&self) -> String {
        std::mem::take(&mut *self.output.borrow_mut())
    }

    /// 当前累计输出（不清空）。
    pub fn output(&self) -> String {
        self.output.borrow().clone()
    }

    /// `sendInput(data)`：触发输入回调。
    pub fn send_input(&mut self, data: &str) {
        if let Some(handler) = self.input_handler.as_mut() {
            handler(data);
        }
    }

    /// `resize(columns, rows)`：改尺寸并触发 resize 回调。
    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        if let Some(handler) = self.resize_handler.as_mut() {
            handler();
        }
    }

    /// 是否调用过 `start`。
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// 是否调用过 `stop`。
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
}

impl Default for TestTerminal {
    fn default() -> Self {
        TestTerminal::new()
    }
}

impl Terminal for TestTerminal {
    fn restore_on_exit(&mut self, before: &str, after: &str) {
        self.restore_calls
            .borrow_mut()
            .push((before.to_string(), after.to_string()));
    }

    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        self.input_handler = Some(on_input);
        self.resize_handler = Some(on_resize);
        self.started = true;
        self.write("\x1b[?2004h");
    }

    fn stop(&mut self) {
        self.write("\x1b[?2004l");
        self.input_handler = None;
        self.resize_handler = None;
        self.stopped = true;
    }

    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> Pin<Box<dyn Future<Output = ()> + '_>> {
        Box::pin(async {})
    }

    fn write(&mut self, data: &str) {
        self.output.borrow_mut().push_str(data);
    }

    fn columns(&self) -> usize {
        self.columns
    }

    fn rows(&self) -> usize {
        self.rows
    }

    fn kitty_protocol_active(&self) -> bool {
        true
    }

    fn move_by(&mut self, lines: isize) {
        if lines > 0 {
            self.write(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn hide_cursor(&mut self) {
        self.cursor_visible.set(false);
        self.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.cursor_visible.set(true);
        self.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write("\x1b[K");
    }

    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{title}\x07"));
    }

    fn set_progress(&mut self, _active: bool) {}

    fn set_program_status(&mut self, status: &ProgramStatus) {
        self.program_status.borrow_mut().push(status.clone());
    }
}

/// 把写入流里可见的文本行取出来（去掉 ANSI/OSC/APC 与 `\r`，按 `\n` 切分）。
pub fn visible_output_lines(output: &str) -> Vec<String> {
    kiana_tui::text::strip_terminal_sequences(output)
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

// ============================================================================
// 极简 FakeTui
// ============================================================================

/// 只记录计数器的极简 [`Tui`]（无渲染、无终端）。
#[derive(Default)]
pub struct FakeTui {
    /// 顶层子项。
    pub children: Vec<Box<dyn Component>>,
    /// 当前焦点目标。
    pub focus: Option<FocusTarget>,
    /// `stop()` 次数。
    pub stops: Rc<Cell<usize>>,
    /// `start()` 次数。
    pub starts: Rc<Cell<usize>>,
    /// `requestRender(true)` 次数。
    pub forced_renders: Rc<Cell<usize>>,
    /// 渲染请求总次数。
    pub renders: Rc<Cell<usize>>,
}

impl Component for FakeTui {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.renders.set(self.renders.get() + 1);
        let mut lines = Vec::new();
        for child in &mut self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.children)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl Tui for FakeTui {
    fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }

    fn children(&mut self) -> &mut Vec<Box<dyn Component>> {
        &mut self.children
    }

    fn add_child(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    fn remove_child(&mut self, child: &dyn Component) {
        let target = child as *const dyn Component as *const ();
        if let Some(index) = self
            .children
            .iter()
            .position(|candidate| candidate.as_ref() as *const dyn Component as *const () == target)
        {
            self.children.remove(index);
        }
    }

    fn clear_children(&mut self) {
        self.children.clear();
    }

    fn focus(&self) -> Option<FocusTarget> {
        self.focus
    }

    fn set_focus(&mut self, target: Option<FocusTarget>) {
        self.focus = target;
    }

    fn show_overlay(
        &mut self,
        _component: Box<dyn Component>,
        _options: OverlayOptions,
    ) -> Box<dyn OverlayHandle> {
        Box::new(StubOverlayHandle)
    }

    fn hide_overlay(&mut self) {}

    fn has_overlay(&self) -> bool {
        false
    }

    fn start(&mut self) {
        self.starts.set(self.starts.get() + 1);
    }

    fn stop(&mut self, _options: TuiStopOptions) {
        self.stops.set(self.stops.get() + 1);
    }

    fn render_now(&mut self, _force: bool) {
        self.renders.set(self.renders.get() + 1);
    }

    fn request_render(&mut self, force: bool) {
        if force {
            self.forced_renders.set(self.forced_renders.get() + 1);
        }
        self.renders.set(self.renders.get() + 1);
    }

    fn add_input_listener(&mut self, _listener: TuiInputListener) -> InputListenerId {
        InputListenerId(0)
    }

    fn remove_input_listener(&mut self, _id: InputListenerId) {}

    fn query_terminal_colors(
        &mut self,
        _timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = TerminalColors> + '_>> {
        Box::pin(async { TerminalColors::default() })
    }
}

/// 空 overlay handle。
pub struct StubOverlayHandle;

impl OverlayHandle for StubOverlayHandle {
    fn hide(&mut self) {}
    fn set_hidden(&mut self, _hidden: bool) {}
    fn is_hidden(&self) -> bool {
        false
    }
    fn focus(&mut self) {}
    fn unfocus(&mut self, _options: Option<OverlayUnfocusOptions>) {}
    fn is_focused(&self) -> bool {
        false
    }
    fn bounds(&self) -> Option<OverlayBounds> {
        None
    }
}

// ============================================================================
// 异步驱动
// ============================================================================

/// 在 current_thread runtime 上创建 `LocalSet` 并运行测试体。
pub async fn run_local<F: Future>(future: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(future).await
}

/// 让出若干次调度（`spawn_local` 的任务在这些点推进）。
pub async fn yield_tasks(rounds: usize) {
    for _ in 0..rounds {
        tokio::task::yield_now().await;
    }
}

/// 轮询等待条件成立（最多 10000 次让出）。
pub async fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("condition not met after 10000 yields");
}

// ============================================================================
// 消息与工具
// ============================================================================

/// 最小用户消息（`utilities.ts#userMsg`）。
pub fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
        role: kiana_ai::types::UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.to_string()),
        timestamp: kiana_ai::model_catalog::now_epoch_millis() as i64,
    }))
}

/// 最小 assistant 消息（`utilities.ts#assistantMsg`）。
pub fn assistant_msg(text: &str) -> AgentMessage {
    let mut message = faux_assistant_message(text, Default::default());
    message.provider = "anthropic".to_string();
    message.model = "test".to_string();
    AgentMessage::Standard(Message::Assistant(message))
}

/// 工具结果消息。
pub fn tool_result_msg(tool_call_id: &str, tool_name: &str, text: &str) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
        role: kiana_ai::types::ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        details: Some(serde_json::json!({})),
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: kiana_ai::model_catalog::now_epoch_millis() as i64,
        duration_ms: None,
    }))
}

/// 最小工具定义（`kiana-session` 测试夹具的同款）。
pub fn simple_tool_definition(
    name: &str,
    description: &str,
    prompt_snippet: Option<&str>,
) -> kiana_extensions::ToolDefinition {
    struct OkExecute;

    impl kiana_ext_api::tool_types::ToolDefinitionExecute for OkExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: serde_json::Value,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<AgentToolUpdateCallback>,
            _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
        ) -> kiana_agent_core::types::BoxFuture<
            'a,
            Result<AgentToolResult<serde_json::Value>, kiana_agent_core::types::AgentToolError>,
        > {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: "ok".to_string(),
                        text_signature: None,
                    })],
                    details: serde_json::Value::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    kiana_extensions::ToolDefinition {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        prompt_snippet: prompt_snippet.map(str::to_string),
        prompt_guidelines: None,
        parameters: json!({"type": "object", "properties": {}}),
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
        execute: Arc::new(OkExecute),
        render_call: None,
        render_result: None,
    }
}

// ============================================================================
// faux provider / AgentSession 夹具
// ============================================================================

/// 扩展运行时引擎（`kiana-extensions` 的线程局部单例）。
pub struct TestEngine {
    _engine: std::rc::Rc<kiana_extensions::engine::JsEngine>,
}

impl TestEngine {
    /// 引擎的 `Rc`。
    pub fn rc(&self) -> std::rc::Rc<kiana_extensions::engine::JsEngine> {
        std::rc::Rc::clone(&self._engine)
    }
}

/// 安装扩展运行时引擎；返回值必须活到会话结束。
pub fn install_engine() -> TestEngine {
    let engine = std::rc::Rc::new(kiana_extensions::engine::JsEngine::new().expect("js engine"));
    kiana_extensions::engine::install(&engine).expect("engine prelude");
    kiana_extensions::runner::context::install(&engine).expect("context.js");
    kiana_extensions::runner::dispatch::install(std::rc::Rc::clone(&engine)).expect("dispatch.js");
    TestEngine { _engine: engine }
}

/// `Mutex` 加锁（poison 也继续）。
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 记录 `CredentialStore::read` 的包装：观察 `ModelRuntime::get_auth(provider)` 是否真的
/// 查询了某个 provider（Rust 的 `ModelRuntime` 是具体类型，无法像 TS 的 vitest 那样直接
/// spy `getProvider`；`getAuth` 的凭证读取是唯一可观察的副作用）。
pub struct RecordingCredentialStore {
    inner: AuthStorage,
    reads: Arc<Mutex<Vec<String>>>,
}

impl RecordingCredentialStore {
    /// 包装现有 [`AuthStorage`]。
    pub fn new(inner: AuthStorage, reads: Arc<Mutex<Vec<String>>>) -> Self {
        RecordingCredentialStore { inner, reads }
    }
}

#[async_trait::async_trait]
impl kiana_ai::auth::types::CredentialStore for RecordingCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        options: kiana_ai::auth::types::AuthOperationOptions,
    ) -> Result<Option<kiana_ai::auth::types::Credential>, kiana_ai::auth::types::AuthError> {
        lock(&self.reads).push(provider_id.to_string());
        self.inner.read_credential(provider_id, &options).await
    }

    async fn list(
        &self,
        options: kiana_ai::auth::types::AuthOperationOptions,
    ) -> Result<Vec<kiana_ai::auth::types::CredentialInfo>, kiana_ai::auth::types::AuthError> {
        self.inner.list_credentials(&options).await
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: kiana_ai::auth::types::CredentialModifyFn,
        options: kiana_ai::auth::types::AuthOperationOptions,
    ) -> Result<Option<kiana_ai::auth::types::Credential>, kiana_ai::auth::types::AuthError> {
        self.inner.modify_credential(provider_id, f, &options).await
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: kiana_ai::auth::types::AuthOperationOptions,
    ) -> Result<(), kiana_ai::auth::types::AuthError> {
        self.inner.delete_credential(provider_id, &options).await
    }
}

/// [`faux_session`] 的选项。
#[derive(Default)]
pub struct FauxSessionOptions {
    /// 设置（`SettingsManager::in_memory`）。
    pub settings: Settings,
    /// 模型 id（缺省第一个 faux 模型）。
    pub model_id: Option<String>,
    /// 自定义工具。
    pub custom_tools: Vec<kiana_extensions::ToolDefinition>,
    /// 允许的工具名（`None` = 默认全集）。
    pub tools: Option<Vec<String>>,
    /// 会话管理器（缺省内存会话）。
    pub session_manager: Option<SessionManager>,
    /// 是否使用落盘的会话管理器（`exportToHtml` 需要持久化会话）。
    pub persist_session: bool,
    /// 提供后，凭证读取会被记录（用于断言 `getAuth(provider)` 没有被查询）。
    pub credential_reads: Option<Arc<Mutex<Vec<String>>>>,
}

/// faux provider + `AgentSession` 夹具。
pub struct FauxFixture {
    /// 扩展引擎（保持存活）。
    pub engine: TestEngine,
    /// 临时目录。
    pub temp_dir: tempfile::TempDir,
    /// 工作目录。
    pub cwd: String,
    /// agent 目录。
    pub agent_dir: String,
    /// faux provider 核心。
    pub faux: FauxCore,
    /// 模型运行时。
    pub model_runtime: ModelRuntime,
    /// 设置管理器。
    pub settings_manager: SettingsManager,
    /// 认证存储。
    pub auth_storage: AuthStorage,
    /// 当前模型。
    pub model: Model,
    /// 会话。
    pub session: AgentSession,
}

impl FauxFixture {
    /// 设置 faux 响应序列。
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.set_responses(responses);
    }

    /// `session.state().tools` 的名称。
    pub fn tool_names(&self) -> Vec<String> {
        self.session
            .state()
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }
}

/// 创建 faux provider + 已配置 auth 的 `ModelRuntime` 与会话。
pub async fn faux_session(options: FauxSessionOptions) -> FauxFixture {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = temp_dir.path().join("project");
    let agent_dir = temp_dir.path().join("agent");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let cwd = cwd.to_string_lossy().into_owned();
    let agent_dir = agent_dir.to_string_lossy().into_owned();

    let engine = install_engine();
    let faux = create_faux_core(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(true),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "faux-2".to_string(),
                reasoning: Some(false),
                ..Default::default()
            },
        ]),
        ..Default::default()
    });
    let provider_id = faux.provider().to_string();
    let api = faux.api().to_string();
    let models: Vec<Model> = faux.models().to_vec();
    kiana_ai::compat::register_api_provider(
        kiana_ai::compat::ApiProvider::from_streams(&api, Arc::new(faux.clone())),
        None,
    );

    let auth_storage = AuthStorage::in_memory(auth_data_from_value(json!({
        provider_id.clone(): { "type": "api_key", "key": "faux-key" }
    })));
    let credential_store: Arc<dyn kiana_ai::auth::types::CredentialStore> =
        match &options.credential_reads {
            Some(reads) => Arc::new(RecordingCredentialStore::new(
                auth_storage.clone(),
                Arc::clone(reads),
            )),
            None => Arc::new(auth_storage.clone()),
        };
    let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credential_store),
        models_path: ModelsPathOption::Disabled,
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("model runtime");

    let provider = create_provider(CreateProviderOptions {
        id: provider_id.clone(),
        name: Some(provider_id.clone()),
        base_url: Some(models[0].base_url.clone()),
        headers: None,
        auth: kiana_ai::auth::types::ProviderAuth::default(),
        models: models
            .iter()
            .cloned()
            .map(kiana_ai::types::AnyModel::Chat)
            .collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(ProviderApiImpl::Single(Arc::new(faux.clone()))),
        images: None,
        classifiers: None,
    })
    .expect("faux provider");
    let _ = model_runtime.register_native_provider(provider);
    let _ = model_runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await;

    let settings_manager = SettingsManager::in_memory(options.settings);
    let session_manager = match options.session_manager {
        Some(session_manager) => session_manager,
        None if options.persist_session => {
            let session_dir = temp_dir.path().join("sessions");
            SessionManager::create(&cwd, Some(session_dir.to_string_lossy().as_ref()), None)
                .expect("persisted session manager")
        }
        None => SessionManager::in_memory(&cwd, None, None).expect("session manager"),
    };
    let loader: Arc<dyn ResourceLoader> =
        Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: Some(settings_manager.clone()),
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        }));
    loader.reload(None).await.expect("resource loader reload");

    let model = options
        .model_id
        .as_deref()
        .and_then(|id| faux.get_model(Some(id)).cloned())
        .or_else(|| faux.get_model(None).cloned())
        .unwrap_or_else(|| models[0].clone());

    let created = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir.clone()),
        model_runtime: Some(model_runtime.clone()),
        model: Some(model.clone()),
        thinking_level: Some(ModelThinkingLevel::Medium),
        scoped_models: Vec::new(),
        no_tools: None,
        tools: options.tools,
        exclude_tools: None,
        custom_tools: options.custom_tools,
        resource_loader: Some(loader),
        session_manager: Some(session_manager),
        settings_manager: Some(settings_manager.clone()),
        session_start_event: None,
        loop_guard: None,
        budget_config: None,
    })
    .await
    .expect("agent session");

    FauxFixture {
        engine,
        temp_dir,
        cwd,
        agent_dir,
        faux,
        model_runtime,
        settings_manager,
        auth_storage,
        model,
        session: created.session,
    }
}

/// 扩展工厂：注册给定工具（`kiana-session` 测试夹具的同款）。
pub fn tool_factory(tools: Vec<kiana_extensions::ToolDefinition>) -> ExtensionFactory {
    Arc::new(move |kiana| {
        for tool in &tools {
            kiana.register_tool(tool.clone());
        }
        Box::pin(async {})
    })
}

// ============================================================================
// 渲染器构造
// ============================================================================

/// 用捕获终端构造主屏渲染器。
///
/// 与 TS 测试的 `new TuiMainScreen(new VirtualTerminal())` 一致：`showHardwareCursor`
/// 取 API 默认值 `false`（需要硬件光标的测试自行 `set_show_hardware_cursor(true)`）。
pub fn main_screen(terminal: TestTerminal) -> kiana_tui::main_screen::TuiMainScreen {
    kiana_tui::main_screen::TuiMainScreen::with_options(Box::new(terminal), None, None)
}

/// 用捕获终端构造全屏渲染器（`showHardwareCursor` 默认为 `false`）。
pub fn alt_screen(
    terminal: TestTerminal,
    options: TuiAltScreenOptions,
) -> kiana_tui::alt_screen::TuiAltScreen {
    kiana_tui::alt_screen::TuiAltScreen::new(Box::new(terminal), None, None, options)
}

/// 空共享组件（`Rc<RefCell<dyn Component>>`）。
pub fn shared(component: impl Component + 'static) -> kiana_tui::layout_node::SharedComponent {
    Rc::new(RefCell::new(component))
}

// ============================================================================
// AgentSessionRuntime 夹具（建模自 crates/kiana-session/tests/session_support）
// ============================================================================

/// 把 faux 模型转换成扩展 provider 的模型配置。
fn faux_provider_model_config(model: &Model) -> kiana_ext_api::types::ProviderModelConfig {
    kiana_ext_api::types::ProviderModelConfig::Chat(kiana_ext_api::types::ProviderChatModelConfig {
        id: model.id.clone(),
        name: model.name.clone(),
        api: Some(model.api.clone()),
        base_url: Some(model.base_url.clone()),
        input: model.input.clone(),
        input_limits: model.input_limits.clone(),
        cost: model.cost.clone(),
        headers: None,
        model_type: None,
        reasoning: model.reasoning.unwrap_or(false),
        thinking_level_map: model.thinking_level_map.clone(),
        prompt_cache: model.prompt_cache.clone(),
        context_window: model.context_window.unwrap_or(0),
        max_tokens: model.max_tokens.unwrap_or(0),
        sampling_params: None,
        compat: None,
    })
}

/// 注册 faux provider 的扩展工厂（`kiana.registerProvider(...)`）。
pub fn faux_provider_factory(faux: &FauxCore) -> ExtensionFactory {
    let provider_id = faux.provider().to_string();
    let api = faux.api().to_string();
    let base_url = faux
        .models()
        .first()
        .map(|model| model.base_url.clone())
        .unwrap_or_default();
    let models: Vec<kiana_ext_api::types::ProviderModelConfig> = faux
        .models()
        .iter()
        .map(faux_provider_model_config)
        .collect();
    Arc::new(move |kiana| {
        kiana.register_provider(kiana_ext_api::types::ProviderRegistration::Config {
            name: provider_id.clone(),
            config: kiana_ext_api::types::ProviderConfig {
                name: Some(provider_id.clone()),
                base_url: Some(base_url.clone()),
                api_key: Some("faux-key".to_string()),
                api: Some(api.clone()),
                stream_simple: None,
                images: None,
                classifiers: None,
                headers: None,
                auth_header: None,
                models: Some(models.clone()),
                refresh_models: None,
                oauth: None,
            },
        });
        Box::pin(async {})
    })
}

/// [`runtime_host`] 的选项。
pub struct RuntimeHostOptions {
    /// 额外的扩展工厂。
    pub extension_factory: Option<ExtensionFactory>,
    /// 工作目录（缺省临时目录）。
    pub cwd: Option<String>,
    /// 会话管理器（缺省临时目录下新建）。
    pub session_manager: Option<SessionManager>,
    /// 是否以 faux 模型启动（缺省 true）。
    pub bootstrap_model: bool,
}

impl Default for RuntimeHostOptions {
    fn default() -> Self {
        RuntimeHostOptions {
            extension_factory: None,
            cwd: None,
            session_manager: None,
            bootstrap_model: true,
        }
    }
}

impl RuntimeHostOptions {
    /// 默认值（bootstrap_model = true）。
    pub fn new() -> Self {
        RuntimeHostOptions::default()
    }
}

/// 通过 `createAgentSessionRuntime` 装配的测试宿主。
pub struct RuntimeHost {
    /// 运行时。
    pub runtime: kiana_session::agent_session_runtime::AgentSessionRuntime,
    /// faux provider。
    pub faux: FauxCore,
    /// 临时目录。
    pub temp_dir: tempfile::TempDir,
    /// 工作目录。
    pub cwd: String,
    /// agent 目录。
    pub agent_dir: String,
    /// 当前模型。
    pub model: Model,
    /// 扩展引擎（保持存活）。
    pub engine: TestEngine,
}

impl RuntimeHost {
    /// `runtime.session.bindExtensions({})`。
    pub async fn bind_extensions(&self) {
        self.runtime
            .session()
            .bind_extensions(kiana_session::agent_session::ExtensionBindings::default())
            .await;
    }

    /// 设置 faux 响应序列。
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.set_responses(responses);
    }

    /// 追加 faux 响应序列。
    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.faux.append_responses(responses);
    }

    /// 释放运行时。
    pub async fn dispose(&self) {
        self.runtime.dispose().await;
    }
}

/// 创建 runtime 宿主（对应 `test/suite/agent-session-runtime.test.ts#createRuntimeForTest`）。
pub async fn runtime_host(options: RuntimeHostOptions) -> RuntimeHost {
    use kiana_session::agent_session_runtime::{
        AgentSessionRuntimeError, CreateAgentSessionRuntimeFactory,
        CreateAgentSessionRuntimeResult, create_agent_session_runtime,
    };
    use kiana_session::agent_session_services::{
        CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions,
        create_agent_session_from_services, create_agent_session_services,
    };

    let engine = install_engine();
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let cwd = match &options.cwd {
        Some(cwd) => cwd.clone(),
        None => temp_dir.path().to_string_lossy().into_owned(),
    };
    std::fs::create_dir_all(&cwd).expect("cwd");
    let agent_dir = temp_dir.path().join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&agent_dir).expect("agent dir");

    let faux = create_faux_core(RegisterFauxProviderOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "faux-1".to_string(),
                reasoning: Some(true),
                ..Default::default()
            },
            FauxModelDefinition {
                id: "faux-2".to_string(),
                reasoning: Some(false),
                ..Default::default()
            },
        ]),
        ..Default::default()
    });
    let api = faux.api().to_string();
    kiana_ai::compat::register_api_provider(
        kiana_ai::compat::ApiProvider::from_streams(&api, Arc::new(faux.clone())),
        None,
    );
    let model = faux.get_model(None).cloned().expect("faux model");
    let bootstrap_model = options.bootstrap_model.then(|| model.clone());

    let provider_factory = faux_provider_factory(&faux);
    let user_factory = options.extension_factory.clone();
    let factory_agent_dir = agent_dir.clone();
    let factory: CreateAgentSessionRuntimeFactory = Arc::new(move |input| {
        let provider_factory = Arc::clone(&provider_factory);
        let user_factory = user_factory.clone();
        let agent_dir = factory_agent_dir.clone();
        let bootstrap_model = bootstrap_model.clone();
        Box::pin(async move {
            let mut factories: Vec<kiana_ext_api::types::InlineExtension> =
                vec![kiana_ext_api::types::InlineExtension::Factory(
                    provider_factory,
                )];
            if let Some(user_factory) = user_factory {
                factories.push(kiana_ext_api::types::InlineExtension::Factory(user_factory));
            }
            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: input.cwd.clone(),
                agent_dir: Some(agent_dir),
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    extension_factories: factories,
                    no_skills: true,
                    no_prompt_templates: true,
                    no_themes: true,
                    no_context_files: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|error| AgentSessionRuntimeError::Other(error.message))?;
            let created =
                create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                    services: services.clone(),
                    session_manager: input.session_manager,
                    session_start_event: input.session_start_event,
                    model: bootstrap_model.clone(),
                    thinking_level: None,
                    scoped_models: Vec::new(),
                    tools: None,
                    exclude_tools: None,
                    no_tools: None,
                    custom_tools: Vec::new(),

                    loop_guard: None,
                    budget_config: None,
                })
                .await
                .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;
            Ok(CreateAgentSessionRuntimeResult {
                session: created.session,
                extensions_result: created.extensions_result,
                model_fallback_message: created.model_fallback_message,
                diagnostics: services.diagnostics.clone(),
                services,
            })
        })
    });

    let session_manager = options.session_manager.unwrap_or_else(|| {
        let session_dir = temp_dir.path().join("sessions");
        SessionManager::create(&cwd, Some(session_dir.to_string_lossy().as_ref()), None)
            .expect("session manager")
    });
    let runtime = create_agent_session_runtime(
        factory,
        kiana_session::agent_session_runtime::CreateAgentSessionRuntimeOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            session_manager,
            session_start_event: None,
        },
    )
    .await
    .expect("runtime");
    RuntimeHost {
        runtime,
        faux,
        temp_dir,
        cwd,
        agent_dir,
        model,
        engine,
    }
}
