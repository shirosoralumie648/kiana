//! `ExtensionContext` / `ExtensionToolContext` / `ExtensionCommandContext` /
//! `ReplacedSessionContext`（`ca-extensions.md` §2.3.10、`design-extension-runtime.md` §4.4）。
//!
//! 移植的 TS 用例：
//! - `extensions-runner.test.ts` 的 `context creation`（5 例）与 `command context`
//!   （`passes fork options through to the bound handler`）；
//! - `2860-replaced-session-context.test.ts` 的 stale / `withSession` 语义（用 fake
//!   `ContextHost` 而不是完整 session runtime）；
//! - `agent-session-model-extension.test.ts` 的 `getSystemPromptOptions` 与
//!   `6162-extension-active-tools-next-turn.test.ts` 的 `ctx.getSystemPrompt()` 读时新鲜度。

mod support;

use std::sync::{Arc, Mutex};

use kiana_agent_core::{
    AgentTool, AgentToolError, AgentToolResult, AgentToolUpdateCallback,
    ThinkingLevel as AgentThinkingLevel,
};
use kiana_ai::types::{JsonValue, Model, UserContent};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_ext_api::BoxFuture;
use kiana_ext_api::system_prompt::BuildSystemPromptOptions;
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, DeliverAs, ExecuteToolOptions, ExtensionCommandContextApi,
    ExtensionContextApi, ExtensionMode, ExtensionToolContextApi, ForkOptions, NavigateTreeOptions,
    NewSessionOptions, NewSessionResult, ReplacedSessionContextApi, SendMessageOptions,
    SendUserMessageDeliverAs, SendUserMessageOptions, SwitchSessionOptions,
};
use kiana_ext_api::ui::ExtensionUiContext;
use kiana_extensions::engine::JsEngine;
use kiana_extensions::runner::context::{
    self, ContextError, ContextHost, ExtensionContext, ReplacedSessionActions, nested_call_fallback,
};
use kiana_extensions::runner::noop_ui::no_op_ui_context;
use kiana_session_core::session_manager::{
    ReadonlySessionManager, SessionEntry, SessionHeader, SessionProjection, SessionTreeNode,
};
use support::{Harness, run_local};
use tokio_util::sync::CancellationToken;

// ===========================================================================
// Fake host
// ===========================================================================

/// `ExtensionRunner` 的最小替身：所有状态都在 `Mutex` 里，测试可以随时改。
struct FakeHost {
    state: Arc<Mutex<FakeState>>,
}

struct FakeState {
    stale: Option<String>,
    mode: ExtensionMode,
    cwd: String,
    ui: Arc<dyn ExtensionUiContext>,
    has_ui: bool,
    model: Option<Model>,
    scoped_models: Vec<ScopedModel>,
    thinking_level: Option<AgentThinkingLevel>,
    is_idle: bool,
    trusted: bool,
    signal: Option<CancellationToken>,
    pending: bool,
    usage: Option<ContextUsage>,
    system_prompt: String,
    options: BuildSystemPromptOptions,
    session_manager: Arc<dyn ReadonlySessionManager>,
    model_registry: Arc<ModelRegistry>,
    execute_tool: bool,
    execute_signal_cancelled: Option<bool>,
    compact_error: bool,
    replacement: Option<(
        std::sync::Weak<FakeHost>,
        Arc<dyn kiana_ext_api::types::ReplacedSessionContextApi>,
    )>,
    calls: Vec<String>,
    aborts: usize,
    shutdowns: usize,
    compacts: usize,
}

impl FakeHost {
    fn new(model_registry: Arc<ModelRegistry>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(Mutex::new(FakeState {
                stale: None,
                mode: ExtensionMode::Print,
                cwd: "/tmp/kiana-test".to_owned(),
                ui: no_op_ui_context(),
                has_ui: false,
                model: None,
                scoped_models: Vec::new(),
                thinking_level: None,
                is_idle: true,
                trusted: true,
                signal: None,
                pending: false,
                usage: None,
                system_prompt: String::new(),
                options: BuildSystemPromptOptions {
                    cwd: "/tmp/kiana-test".to_owned(),
                    ..Default::default()
                },
                session_manager: Arc::new(FakeSessionManager),
                model_registry,
                execute_tool: false,
                execute_signal_cancelled: None,
                compact_error: false,
                replacement: None,
                calls: Vec::new(),
                aborts: 0,
                shutdowns: 0,
                compacts: 0,
            })),
        })
    }

    fn update(&self, update: impl FnOnce(&mut FakeState)) {
        update(&mut self.state.lock().expect("fake host"));
    }

    fn record(&self, call: impl Into<String>) {
        self.update(|state| state.calls.push(call.into()));
    }

    fn calls(&self) -> Vec<String> {
        self.state.lock().expect("fake host").calls.clone()
    }

    fn invalidate(&self, message: &str) {
        self.update(|state| state.stale = Some(message.to_owned()));
    }

    fn set_ui(&self, ui: Arc<dyn ExtensionUiContext>, mode: ExtensionMode) {
        self.update(|state| {
            state.ui = ui;
            state.has_ui = true;
            state.mode = mode;
        });
    }
}

impl ContextHost for FakeHost {
    fn assert_active(&self) -> Result<(), ContextError> {
        match &self.state.lock().expect("fake host").stale {
            Some(message) => Err(ContextError::new(message.clone())),
            None => Ok(()),
        }
    }

    fn ui(&self) -> Arc<dyn ExtensionUiContext> {
        Arc::clone(&self.state.lock().expect("fake host").ui)
    }

    fn mode(&self) -> ExtensionMode {
        self.state.lock().expect("fake host").mode
    }

    fn has_ui(&self) -> bool {
        self.state.lock().expect("fake host").has_ui
    }

    fn cwd(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.state.lock().expect("fake host").cwd)
    }

    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        Arc::clone(&self.state.lock().expect("fake host").session_manager)
    }

    fn model_registry(&self) -> Arc<ModelRegistry> {
        Arc::clone(&self.state.lock().expect("fake host").model_registry)
    }

    fn model(&self) -> Option<Model> {
        self.state.lock().expect("fake host").model.clone()
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        self.state.lock().expect("fake host").scoped_models.clone()
    }

    fn thinking_level(&self) -> Option<AgentThinkingLevel> {
        self.state.lock().expect("fake host").thinking_level
    }

    fn is_idle(&self) -> bool {
        self.state.lock().expect("fake host").is_idle
    }

    fn is_project_trusted(&self) -> bool {
        self.state.lock().expect("fake host").trusted
    }

    fn signal(&self) -> Option<CancellationToken> {
        self.state.lock().expect("fake host").signal.clone()
    }

    fn abort(&self) {
        self.update(|state| state.aborts += 1);
    }

    fn has_pending_messages(&self) -> bool {
        self.state.lock().expect("fake host").pending
    }

    fn shutdown(&self) {
        self.update(|state| state.shutdowns += 1);
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        self.state.lock().expect("fake host").usage.clone()
    }

    fn compact(&self, options: Option<CompactOptions>) {
        let instructions = options
            .as_ref()
            .and_then(|options| options.custom_instructions.clone());
        let error = self.state.lock().expect("fake host").compact_error;
        self.update(|state| {
            state.compacts += 1;
            state.calls.push(format!(
                "compact:{}",
                instructions.unwrap_or_else(|| "none".to_owned())
            ));
        });
        let Some(options) = options else { return };
        if error {
            if let Some(on_error) = options.on_error {
                on_error(Box::new(std::io::Error::other("compaction failed")));
            }
        } else if let Some(on_complete) = options.on_complete {
            on_complete(kiana_compaction::CompactionResult {
                summary: "summary text".to_owned(),
                first_kept_entry_id: "entry-1".to_owned(),
                tokens_before: 42,
                estimated_tokens_after: None,
                usage: None,
                details: None,
            });
        }
    }

    fn get_system_prompt(&self) -> String {
        self.state.lock().expect("fake host").system_prompt.clone()
    }

    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        self.state.lock().expect("fake host").options.clone()
    }

    fn wait_for_idle(&self) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
        self.record("waitForIdle");
        Box::pin(async {})
    }

    fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, NewSessionResult> {
        let options = options.unwrap_or_default();
        let parent = options.parent_session;
        let with_session = options.with_session;
        let setup = options.setup;
        let state = Arc::clone(&self.state);
        let replacement = state.lock().expect("fake host").replacement.clone();
        Box::pin(async move {
            state
                .lock()
                .expect("fake host")
                .calls
                .push(format!("newSession:{}", parent.unwrap_or_default()));
            // TS 顺序：先 apply 新 runtime，再 `await options.setup(sessionManager)`，最后
            // `await withSession(...)`（agent-session-runtime.ts:254-258、187-193）。
            if let Some(setup) = setup {
                let manager = kiana_session_core::session_manager::SessionManager::in_memory(
                    "/tmp/kiana-test",
                    None,
                    None,
                )
                .expect("in-memory session manager");
                setup(Arc::new(Mutex::new(manager))).await;
                state
                    .lock()
                    .expect("fake host")
                    .calls
                    .push("setup".to_owned());
            }
            if let Some((old, replaced)) = replacement {
                if let Some(old) = old.upgrade() {
                    old.invalidate(STALE);
                }
                if let Some(with_session) = with_session {
                    with_session(replaced).await;
                }
            }
            NewSessionResult { cancelled: false }
        })
    }

    fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, NewSessionResult> {
        let state = Arc::clone(&self.state);
        let with_session = options
            .as_ref()
            .and_then(|options| options.with_session.clone());
        let replacement = state.lock().expect("fake host").replacement.clone();
        Box::pin(async move {
            let position = options.as_ref().and_then(|options| options.position);
            state.lock().expect("fake host").calls.push(format!(
                "fork:{entry_id}:{}",
                match position {
                    Some(kiana_ext_api::events::ForkPosition::Before) => "before",
                    Some(kiana_ext_api::events::ForkPosition::At) => "at",
                    None => "none",
                }
            ));
            if let Some((old, replaced)) = replacement {
                if let Some(old) = old.upgrade() {
                    old.invalidate(STALE);
                }
                if let Some(with_session) = with_session {
                    with_session(replaced).await;
                }
            }
            NewSessionResult { cancelled: false }
        })
    }

    fn navigate_tree(
        &self,
        target_id: String,
        _options: Option<NavigateTreeOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, NewSessionResult> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            state
                .lock()
                .expect("fake host")
                .calls
                .push(format!("navigateTree:{target_id}"));
            NewSessionResult { cancelled: false }
        })
    }

    fn switch_session(
        &self,
        session_path: String,
        options: Option<SwitchSessionOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, NewSessionResult> {
        let state = Arc::clone(&self.state);
        let with_session = options.and_then(|options| options.with_session);
        let replacement = state.lock().expect("fake host").replacement.clone();
        Box::pin(async move {
            state
                .lock()
                .expect("fake host")
                .calls
                .push(format!("switchSession:{session_path}"));
            if let Some((old, replaced)) = replacement {
                if let Some(old) = old.upgrade() {
                    old.invalidate(STALE);
                }
                if let Some(with_session) = with_session {
                    with_session(replaced).await;
                }
            }
            NewSessionResult { cancelled: false }
        })
    }

    fn reload(&self) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
        self.record("reload");
        Box::pin(async {})
    }

    fn get_callable_tools(&self) -> Vec<AgentTool> {
        vec![tool_stub("read")]
    }

    fn execute_tool(
        &self,
        tool_call_id: &str,
        name: &str,
        args: JsonValue,
        options: ExecuteToolOptions,
    ) -> Option<BoxFuture<'static, kiana_agent_core::AgentToolCallOutcome>> {
        if !self.state.lock().expect("fake host").execute_tool {
            return None;
        }
        let signal = options.signal.is_some();
        let cancelled = options.signal.as_ref().map(|token| token.is_cancelled());
        self.update(|state| state.execute_signal_cancelled = cancelled);
        let call = format!(
            "executeTool:{tool_call_id}:{name}:{}:{signal}",
            serde_json::to_string(&args).unwrap_or_default()
        );
        self.record(call);
        let name = name.to_owned();
        let tool_call_id = tool_call_id.to_owned();
        let on_update = options.on_update;
        let future: BoxFuture<'static, kiana_agent_core::AgentToolCallOutcome> =
            Box::pin(async move {
                if let Some(on_update) = on_update {
                    on_update(kiana_agent_core::AgentToolResult {
                        content: vec![kiana_ai::types::UserContentBlock::Text(
                            kiana_ai::types::TextContent {
                                kind_text: kiana_ai::types::TextKind::Text,
                                text: "partial".to_owned(),
                                text_signature: None,
                            },
                        )],
                        details: serde_json::json!({ "partial": true }),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    });
                }
                nested_call_fallback(&tool_call_id, &name, JsonValue::Null)
            });
        Some(future)
    }
}

/// `ReplacedSessionContext` 的 `sendMessage`/`sendUserMessage` 记录器。
#[derive(Default)]
struct RecordingActions {
    calls: Mutex<Vec<String>>,
}

impl RecordingActions {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("recording actions").clone()
    }
}

impl ReplacedSessionActions for RecordingActions {
    fn send_message(
        &self,
        message: SendMessageOptions,
    ) -> kiana_ext_api::LocalBoxFuture<'static, Result<(), String>> {
        let text = match &message.message.content {
            kiana_ai::types::UserContent::Text(text) => text.clone(),
            kiana_ai::types::UserContent::Blocks(_) => "<blocks>".to_owned(),
        };
        let deliver = match message.deliver_as {
            Some(DeliverAs::Steer) => "steer",
            Some(DeliverAs::FollowUp) => "followUp",
            Some(DeliverAs::NextTurn) => "nextTurn",
            None => "none",
        };
        let details = match &message.message.details {
            Some(value) => format!(":details={value}"),
            None => String::new(),
        };
        self.calls.lock().expect("recording actions").push(format!(
            "sendMessage:{}:{text}:display={}:turn={}:deliverAs={deliver}{details}",
            message.message.custom_type,
            message.message.display,
            message.trigger_turn.unwrap_or(false),
        ));
        Box::pin(async { Ok(()) })
    }

    fn send_user_message(
        &self,
        content: UserContent,
        options: Option<SendUserMessageOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, Result<(), String>> {
        let text = match content {
            UserContent::Text(text) => text,
            UserContent::Blocks(_) => "<blocks>".to_owned(),
        };
        let deliver = match options.and_then(|options| options.deliver_as) {
            Some(SendUserMessageDeliverAs::Steer) => "steer",
            Some(SendUserMessageDeliverAs::FollowUp) => "followUp",
            None => "none",
        };
        self.calls
            .lock()
            .expect("recording actions")
            .push(format!("sendUserMessage:{text}:deliverAs={deliver}"));
        Box::pin(async { Ok(()) })
    }
}

struct FakeSessionManager;

impl ReadonlySessionManager for FakeSessionManager {
    fn get_cwd(&self) -> &str {
        "/tmp/kiana-test"
    }
    fn get_session_dir(&self) -> &str {
        "/tmp/kiana-test/sessions"
    }
    fn get_session_id(&self) -> &str {
        "session-1"
    }
    fn get_session_file(&self) -> Option<&str> {
        Some("/tmp/kiana-test/sessions/session-1.jsonl")
    }
    fn get_leaf_id(&self) -> Option<&str> {
        Some("leaf-1")
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
            thinking_level: "off".to_owned(),
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
        Some("test session".to_owned())
    }
}

struct StubToolExecute;

impl kiana_agent_core::AgentToolExecute for StubToolExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async {
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

fn tool_stub(name: &str) -> AgentTool {
    AgentTool {
        name: name.to_owned(),
        label: name.to_owned(),
        description: format!("{name} tool"),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(StubToolExecute),
    }
}

// ===========================================================================
// Harness helpers
// ===========================================================================

fn new_engine() -> Harness {
    let harness = Harness::new();
    context::install(&harness.engine).expect("install context.js");
    harness
}

/// 建一个 `ModelRegistry`（禁网、无凭证）。
async fn new_model_registry() -> Arc<ModelRegistry> {
    use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions::default())
        .await
        .expect("model runtime");
    Arc::new(ModelRegistry::new(Arc::new(runtime)))
}

/// 在注册表里装一个自带 `streamSimple` 的 provider（`8964-extension-provider-streaming`
/// 的 Rust 等价物：扩展 `kiana.registerProvider` 最终落到同一个配置）。
fn install_faux_provider(registry: &ModelRegistry) {
    use kiana_ai::event_stream::AssistantMessageEventStream;
    use kiana_ai::types::{
        AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, DoneReason,
        TextContent as AiTextContent, TextKind as AiTextKind,
    };
    use kiana_config::models::composer::{
        ExtensionStreamSimpleFn, ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig,
        ProviderModelConfigBase,
    };

    let stream_simple: Arc<ExtensionStreamSimpleFn> = Arc::new(|_model, _context, _options| {
        let stream = AssistantMessageEventStream::new();
        let mut message = AssistantMessage {
            role: AssistantRole::Assistant,
            ..Default::default()
        };
        message
            .content
            .push(AssistantContentBlock::Text(AiTextContent {
                kind_text: AiTextKind::Text,
                text: "custom provider response".to_owned(),
                text_signature: None,
            }));
        message.stop_reason = kiana_ai::types::StopReason::Stop;
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(message.clone()),
        });
        let _ = stream.push(AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: Box::new(message.clone()),
        });
        let _ = stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "custom provider response".to_owned(),
            partial: Box::new(message.clone()),
        });
        let _ = stream.push(AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "custom provider response".to_owned(),
            partial: Box::new(message.clone()),
        });
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(message),
        });
        stream
    });
    registry
        .register_provider(
            "extension-provider",
            Some(ProviderConfigInput {
                api: Some("issue-8964-extension-api".to_owned()),
                base_url: Some("http://127.0.0.1:1".to_owned()),
                api_key: Some("extension-key".to_owned()),
                stream_simple: Some(stream_simple),
                models: Some(vec![ProviderModelConfig::Chat(ProviderChatModelConfig {
                    base: ProviderModelConfigBase {
                        id: "faux-model".to_owned(),
                        name: "Faux Model".to_owned(),
                        api: Some("issue-8964-extension-api".to_owned()),
                        base_url: Some("http://127.0.0.1:1".to_owned()),
                        ..Default::default()
                    },
                    context_window: 100_000.0,
                    max_tokens: 4_096.0,
                    ..Default::default()
                })]),
                ..Default::default()
            }),
        )
        .expect("register faux provider");
}

/// 在引擎里创建 JS 上下文对象并挂到 `globalThis.__ctx`。
fn set_js_context(harness: &Harness, context: Arc<ExtensionContext>) {
    harness
        .engine
        .with_js(|ctx| -> rquickjs::Result<()> {
            let value = context::create_context_value(&ctx, Arc::clone(context.host()))?;
            ctx.globals().set("__ctx", value)
        })
        .expect("create context");
}

fn set_js_tool_context(
    harness: &Harness,
    host: Arc<dyn ContextHost>,
    tool_call_id: &str,
    signal: Option<CancellationToken>,
) {
    harness
        .engine
        .with_js(|ctx| -> rquickjs::Result<()> {
            let value = context::create_tool_context_value(&ctx, host, tool_call_id, signal)?;
            ctx.globals().set("__ctx", value)
        })
        .expect("create tool context");
}

fn set_js_command_context(harness: &Harness, host: Arc<dyn ContextHost>) {
    harness
        .engine
        .with_js(|ctx| -> rquickjs::Result<()> {
            let value = context::create_command_context_value(&ctx, host)?;
            ctx.globals().set("__ctx", value)
        })
        .expect("create command context");
}

fn set_js_replaced_context(
    harness: &Harness,
    host: Arc<dyn ContextHost>,
    actions: Arc<dyn ReplacedSessionActions>,
) {
    harness
        .engine
        .with_js(|ctx| -> rquickjs::Result<()> {
            let value = context::create_replaced_context_value(&ctx, host, actions)?;
            ctx.globals().set("__ctx", value)
        })
        .expect("create replaced context");
}

/// 求值一个返回 Promise 的 JS 表达式并等待其完成（`eval_promise` 的返回值是 `undefined`，
/// 因此把结果写进 `globalThis.__await` 再读回）。
fn eval_await<T: for<'js> rquickjs::FromJs<'js>>(harness: &Harness, source: &str) -> T {
    harness
        .engine
        .with_js(|ctx| -> Result<T, String> {
            let promise = ctx
                .eval_promise(format!("globalThis.__await = await ({source});"))
                .map_err(|error| format!("eval_promise failed: {error:?}"))?;
            loop {
                if let Some(result) = promise.result::<()>() {
                    result.map_err(|error| format!("promise failed: {error:?}"))?;
                    break;
                }
                if !ctx.execute_pending_job() {
                    return Err("promise did not settle".to_owned());
                }
            }
            let value = ctx
                .globals()
                .get::<_, T>("__await")
                .map_err(|error| format!("read __await failed: {error:?}"))?;
            Ok(value)
        })
        .unwrap_or_else(|error| panic!("{error}"))
}

/// 一个与进程级 no-op 单例**不同**的 UI `Arc`（用来测试“有 UI 但没有 JS 桥”的路径）。
fn distinct_ui() -> Arc<dyn ExtensionUiContext> {
    struct NoEvents;
    impl kiana_extensions::runner::ui_prompt::UiPromptHost for NoEvents {
        fn emit_ui_prompt(&self, _event: kiana_ext_api::events::ExtensionEvent) {}
    }
    kiana_extensions::runner::ui_prompt::UiPromptContext::wrap(
        no_op_ui_context(),
        Arc::new(NoEvents),
    )
}

const STALE: &str = "This extension ctx is stale after session replacement or reload. Do not use a captured kiana or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().";

// ===========================================================================
// context creation（extensions-runner.test.ts）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn exposes_the_current_abort_signal_on_extension_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    let token = CancellationToken::new();
    host.update(|state| state.signal = Some(token.clone()));

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(harness.eval::<bool>("__ctx.signal !== undefined && !__ctx.signal.aborted"));
        // 同一个宿主 token 复用同一个 JS 对象（TS 的 `ctx.signal` 是会话持有的
        // AbortSignal 本身；Rust 的 ContextHost 以 CancellationToken 表达，见 deviations）。
        assert!(harness.eval::<bool>("__ctx.signal === __ctx.signal"));
        harness.eval::<()>("globalThis.__ctxA = __ctx;");
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(harness.eval::<bool>("__ctx.signal === __ctxA.signal"));
        // 不同 token → 不同对象。
        let other = FakeHost::new(host.state.lock().expect("fake host").model_registry.clone());
        other.update(|state| state.signal = Some(CancellationToken::new()));
        set_js_context(&harness, ExtensionContext::base(other));
        assert!(harness.eval::<bool>("__ctx.signal !== __ctxA.signal"));
        set_js_context(&harness, ExtensionContext::base(host.clone()));

        // Node 的 signal 实例没有自有可枚举属性（`aborted` 在原型上）。
        assert_eq!(harness.eval::<usize>("Object.keys(__ctx.signal).length"), 0);
        assert_eq!(harness.eval::<String>("JSON.stringify(__ctx.signal)"), "{}");
        token.cancel();
        assert!(harness.eval::<bool>("__ctx.signal.aborted"));
        assert_eq!(
            harness.eval::<String>("__ctx.signal.reason.name"),
            "AbortError"
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_print_mode_and_has_ui_false_by_default() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert_eq!(harness.eval::<String>("__ctx.mode"), "print");
        assert!(!harness.eval::<bool>("__ctx.hasUI"));
        // 无 UI 时 `ctx.ui` 是 noOpUIContext 的等价物。
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.ui.getAllThemes())"),
            "[]"
        );
        assert_eq!(harness.eval::<String>("__ctx.ui.getEditorText()"), "");
        assert!(!eval_await::<bool>(&harness, "__ctx.ui.confirm('t', 'm')"));
        assert_eq!(
            eval_await::<Option<String>>(&harness, "__ctx.ui.input('t')"),
            None
        );
        assert_eq!(
            eval_await::<Option<String>>(&harness, "__ctx.ui.select('t', ['a'])"),
            None
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.ui.setTheme('dark'))"),
            r#"{"success":false,"error":"UI not available"}"#
        );
    })
    .await;
}

/// 无 UI 时 `ctx.ui.theme` 是 TS 的**稳定 Proxy**：对象本身一直存在，属性读取在主题模块
/// （theme.ts 的 `globalThis[Symbol.for(...)]`）注册后转发到当前主题；未注册时抛逐字文案。
#[tokio::test(flavor = "current_thread")]
async fn no_ui_theme_is_the_stable_ts_proxy() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(harness.eval::<bool>("__ctx.ui.theme !== undefined"));
        assert_eq!(harness.eval::<String>("typeof __ctx.ui.theme"), "object");
        assert!(harness.eval::<bool>("__ctx.ui.theme === __ctx.ui.theme"));
        // 未注册主题实例：任何属性访问抛 TS 的逐字文案。
        let error = harness.expect_error("__ctx.ui.theme.fg");
        assert_eq!(error.name, "Error");
        assert_eq!(
            error.message,
            "Theme not initialized. Call initTheme() first."
        );
        // theme.ts 注册全局实例后按属性转发（含方法调用）。
        harness.eval::<()>(
            "globalThis[Symbol.for('@earendil-works/kiana-coding-agent:theme')] = { \
               name: 'dark', \
               fg: (color, text) => color + ':' + text, \
             };",
        );
        assert_eq!(harness.eval::<String>("__ctx.ui.theme.name"), "dark");
        assert_eq!(
            harness.eval::<String>("__ctx.ui.theme.fg('accent', 'text')"),
            "accent:text"
        );
        // 枚举顺序与 TS 的对象字面量一致（theme 在 getEditorComponent 与 getAllThemes 之间）。
        assert!(harness.eval::<bool>(
            "Object.keys(__ctx.ui).indexOf('theme') === \
             Object.keys(__ctx.ui).indexOf('getEditorComponent') + 1"
        ));
        assert!(harness.eval::<bool>(
            "Object.keys(__ctx.ui).indexOf('getAllThemes') === \
             Object.keys(__ctx.ui).indexOf('theme') + 1"
        ));
        // 旧的全局 key 也认（theme.ts 同时写两个 symbol）。
        harness.eval::<()>(
            "delete globalThis[Symbol.for('@earendil-works/kiana-coding-agent:theme')]; \
             globalThis[Symbol.for('@mariozechner/kiana-coding-agent:theme')] = { name: 'legacy' };",
        );
        assert_eq!(harness.eval::<String>("__ctx.ui.theme.name"), "legacy");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_project_trust_state_on_extension_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.update(|state| state.trusted = false);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(!harness.eval::<bool>("__ctx.isProjectTrusted()"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_rpc_mode_with_has_ui_true_when_an_rpc_ui_context_is_provided() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.set_ui(distinct_ui(), ExtensionMode::Rpc);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert_eq!(harness.eval::<String>("__ctx.mode"), "rpc");
        assert!(harness.eval::<bool>("__ctx.hasUI"));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_tui_mode_with_has_ui_true_when_a_tui_ui_context_is_provided() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.set_ui(distinct_ui(), ExtensionMode::Tui);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert_eq!(harness.eval::<String>("__ctx.mode"), "tui");
        assert!(harness.eval::<bool>("__ctx.hasUI"));
        // 没有安装 ui-adapter 的 JS 桥时，真实的 UI 上下文不能静默降级为 no-op。
        let error = harness.expect_error("__ctx.ui");
        assert!(error.message.contains("UI bridge is not installed"));
    })
    .await;
}

// ===========================================================================
// 读时求值（6162 / agent-session-model-extension.test.ts）
// ===========================================================================

/// 每个成员都在访问时读宿主状态（`runner.getModel()` 等），而不是构造时快照。
#[tokio::test(flavor = "current_thread")]
async fn reads_members_at_access_time_instead_of_snapshotting() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));

        assert_eq!(harness.eval::<String>("String(__ctx.model)"), "undefined");
        assert_eq!(
            harness.eval::<String>("String(__ctx.thinkingLevel)"),
            "undefined"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.scopedModels)"),
            "[]"
        );
        assert_eq!(harness.eval::<String>("String(__ctx.signal)"), "undefined");

        host.update(|state| {
            state.model = Some(Model {
                id: "model-1".to_owned(),
                name: "Model One".to_owned(),
                ..Default::default()
            });
            state.scoped_models = vec![ScopedModel {
                model: Model {
                    id: "model-2".to_owned(),
                    name: "Model Two".to_owned(),
                    ..Default::default()
                },
                thinking_level: Some(kiana_ai::types::ModelThinkingLevel::High),
            }];
            state.thinking_level = Some(AgentThinkingLevel::High);
            state.system_prompt = "fresh prompt".to_owned();
            state.is_idle = false;
            state.trusted = false;
            state.cwd = "/tmp/kiana-test-2".to_owned();
            state.mode = ExtensionMode::Rpc;
        });

        assert_eq!(harness.eval::<String>("__ctx.model.id"), "model-1");
        assert_eq!(harness.eval::<String>("__ctx.model.name"), "Model One");
        assert_eq!(harness.eval::<String>("__ctx.thinkingLevel"), "high");
        assert_eq!(
            harness.eval::<String>("__ctx.scopedModels[0].model.id"),
            "model-2"
        );
        // 同一快照的重复读取保持引用身份（TS 的 `toBe(scoped)`）。
        assert!(harness.eval::<bool>("__ctx.scopedModels === __ctx.scopedModels"));
        assert_eq!(
            harness.eval::<String>("__ctx.scopedModels[0].thinkingLevel"),
            "high"
        );
        assert_eq!(
            harness.eval::<String>("__ctx.getSystemPrompt()"),
            "fresh prompt"
        );
        assert!(!harness.eval::<bool>("__ctx.isIdle()"));
        assert!(!harness.eval::<bool>("__ctx.isProjectTrusted()"));
        assert_eq!(harness.eval::<String>("__ctx.cwd"), "/tmp/kiana-test-2");
        assert_eq!(harness.eval::<String>("__ctx.mode"), "rpc");

        // TS: `runner.createContext().scopedModels` 返回宿主持有的同一个数组
        // （extensions-runner.test.ts 的 `expect(...).toBe(scoped)`）。Rust 宿主返回的是
        // 快照，JS 外观按 JSON interning 复用它：跨两个 ctx 读取是同一个对象，扩展对它的
        // 修改在原对象上可见（下一条读取仍是同一份快照）。
        harness.eval::<()>("globalThis.__ctxA = __ctx;");
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(harness.eval::<bool>("__ctx.scopedModels === __ctxA.scopedModels"));
        harness.eval::<()>(
            "__ctx.scopedModels.push({ model: { id: 'mutated' }, thinkingLevel: 'low' });",
        );
        assert!(
            harness
                .eval::<bool>("__ctx.scopedModels.some((entry) => entry.model.id === 'mutated')")
        );
        assert!(harness.eval::<bool>("__ctx.scopedModels === __ctxA.scopedModels"));

        host.update(|state| {
            state.model = None;
            state.thinking_level = None;
            state.scoped_models.clear();
        });
        assert_eq!(harness.eval::<String>("String(__ctx.model)"), "undefined");
        assert_eq!(
            harness.eval::<String>("String(__ctx.thinkingLevel)"),
            "undefined"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.scopedModels)"),
            "[]"
        );
    })
    .await;
}

/// `6162-extension-active-tools-next-turn.test.ts`：`ctx.getSystemPrompt()` 每次调用都
/// 重新取当前系统提示（active tools 变化后立刻可见）。
#[tokio::test(flavor = "current_thread")]
async fn refreshes_the_system_prompt_between_reads() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.update(|state| state.system_prompt = "before: read".to_owned());

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert_eq!(
            harness.eval::<String>("__ctx.getSystemPrompt()"),
            "before: read"
        );

        host.update(|state| state.system_prompt = "after: read, edit".to_owned());
        assert_eq!(
            harness.eval::<String>("__ctx.getSystemPrompt()"),
            "after: read, edit"
        );
    })
    .await;
}

// ===========================================================================
// stale（2860-replaced-session-context.test.ts）
// ===========================================================================

/// 所有同步成员（getter 与同步方法）在 stale 后抛出同一段逐字文案。
const STALE_SYNC_MEMBERS: &[&str] = &[
    "__ctx.ui",
    "__ctx.mode",
    "__ctx.hasUI",
    "__ctx.cwd",
    "__ctx.sessionManager",
    "__ctx.modelRegistry",
    "__ctx.model",
    "__ctx.scopedModels",
    "__ctx.thinkingLevel",
    "__ctx.signal",
    "__ctx.isIdle()",
    "__ctx.isProjectTrusted()",
    "__ctx.abort()",
    "__ctx.hasPendingMessages()",
    "__ctx.shutdown()",
    "__ctx.getContextUsage()",
    "__ctx.compact()",
    "__ctx.getSystemPrompt()",
];

#[tokio::test(flavor = "current_thread")]
async fn throws_the_verbatim_stale_message_for_every_sync_member() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.invalidate(STALE);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        for member in STALE_SYNC_MEMBERS {
            let error = harness.expect_error(member);
            assert_eq!(error.name, "Error", "`{member}`");
            assert_eq!(error.message, STALE, "`{member}`");
        }
    })
    .await;
}

/// `2860-replaced-session-context.test.ts:170-177` 的顺序：**先读** `ctx.sessionManager`
/// （填充任何缓存），会话替换后再读必须抛同一段逐字 stale 文案。TS 给每个成员都套了
/// `runner.assertActive()` 的 getter，所以“缓存过”不能成为绕过 stale 检查的理由。
#[tokio::test(flavor = "current_thread")]
async fn throws_the_stale_message_after_a_member_was_read_before_invalidation() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        // 读取并缓存两个宿主实例成员（以及其它读时成员）。
        assert_eq!(
            harness.eval::<String>("__ctx.sessionManager.getSessionFile()"),
            "/tmp/kiana-test/sessions/session-1.jsonl"
        );
        assert!(
            harness.eval::<bool>("Array.isArray(__ctx.modelRegistry.getRegisteredProviderIds())")
        );
        assert_eq!(
            harness.eval::<String>("__ctx.scopedModels.length.toString()"),
            "0"
        );
        assert_eq!(harness.eval::<String>("String(__ctx.model)"), "undefined");
        assert_eq!(harness.eval::<String>("__ctx.cwd"), "/tmp/kiana-test");

        host.invalidate(STALE);

        // 已缓存/已读过的成员再次读取仍然抛错（TS 的读时 assertActive）。
        for member in STALE_SYNC_MEMBERS {
            let error = harness.expect_error(member);
            assert_eq!(error.name, "Error", "`{member}`");
            assert_eq!(error.message, STALE, "`{member}`");
        }
    })
    .await;
}

/// 命令方法的 stale 检查同样发生在读取成员之后（缓存 getter 不得绕过）。
#[tokio::test(flavor = "current_thread")]
async fn cached_members_still_check_active_on_a_command_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_command_context(&harness, host.clone());
        assert_eq!(
            harness.eval::<String>("__ctx.sessionManager.getSessionId()"),
            "session-1"
        );
        host.invalidate(STALE);
        let error = harness.expect_error("__ctx.sessionManager");
        assert_eq!(error.message, STALE);
        let error = harness.expect_error("__ctx.modelRegistry");
        assert_eq!(error.message, STALE);
    })
    .await;
}

/// TS 的命令方法是**非 async** 箭头（`() => { this.assertActive(); return fn(); }`），
/// stale 时在调用点同步抛出（不是返回 rejected Promise）。
#[tokio::test(flavor = "current_thread")]
async fn throws_synchronously_on_a_stale_command_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.invalidate(STALE);

    run_local(async {
        set_js_command_context(&harness, host.clone());
        for call in [
            "__ctx.fork('entry-1')",
            "__ctx.newSession()",
            "__ctx.navigateTree('leaf-1')",
            "__ctx.switchSession('/tmp/other.jsonl')",
            "__ctx.reload()",
            "__ctx.waitForIdle()",
            "__ctx.getSystemPromptOptions()",
        ] {
            let error = harness.expect_error(call);
            assert_eq!(error.name, "Error", "`{call}`");
            assert_eq!(error.message, STALE, "`{call}`");
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_replaced_members_on_a_stale_replaced_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    let actions = Arc::new(RecordingActions::default());
    host.invalidate(STALE);

    run_local(async {
        set_js_replaced_context(&harness, host.clone(), actions.clone());
        for call in [
            "__ctx.sendMessage({ customType: 'note', content: 'hi', display: false })",
            "__ctx.sendUserMessage('hi')",
        ] {
            assert_eq!(
                settle_call(&harness, call).await,
                Err(STALE.to_owned()),
                "`{call}`"
            );
        }
        assert!(actions.calls().is_empty());
    })
    .await;
}

// ===========================================================================
// 命令上下文（extensions-runner.test.ts）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn passes_fork_options_through_to_the_bound_handler() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_command_context(&harness, host.clone());

        assert_eq!(
            settle_call(&harness, "__ctx.fork('entry-1')").await,
            Ok(r#"{"cancelled":false}"#.to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.fork('entry-2', { position: 'at' })").await,
            Ok(r#"{"cancelled":false}"#.to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.navigateTree('leaf-1')").await,
            Ok(r#"{"cancelled":false}"#.to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.switchSession('/tmp/other.jsonl')").await,
            Ok(r#"{"cancelled":false}"#.to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.reload()").await,
            Ok("null".to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.waitForIdle()").await,
            Ok("null".to_owned())
        );

        assert_eq!(
            host.calls(),
            vec![
                "fork:entry-1:none".to_owned(),
                "fork:entry-2:at".to_owned(),
                "navigateTree:leaf-1".to_owned(),
                "switchSession:/tmp/other.jsonl".to_owned(),
                "reload".to_owned(),
                "waitForIdle".to_owned(),
            ]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn exposes_get_system_prompt_options_on_a_command_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_command_context(&harness, host.clone());
        let options: JsonValue = serde_json::from_str(
            &harness.eval::<String>("JSON.stringify(__ctx.getSystemPromptOptions())"),
        )
        .expect("parse options");
        assert_eq!(
            options["cwd"],
            JsonValue::String("/tmp/kiana-test".to_owned())
        );

        host.update(|state| state.options.cwd = "/tmp/kiana-test-3".to_owned());
        let refreshed: JsonValue = serde_json::from_str(
            &harness.eval::<String>("JSON.stringify(__ctx.getSystemPromptOptions())"),
        )
        .expect("parse options");
        assert_eq!(
            refreshed["cwd"],
            JsonValue::String("/tmp/kiana-test-3".to_owned())
        );
    })
    .await;
}

// ===========================================================================
// 工具上下文 + executeTool 回退
// ===========================================================================

/// `executeTool` 没有绑定动作时的逐字回退结果（`ca-extensions.md` §2.3.10）。
const NESTED_CALL_FALLBACK_JSON: &str = concat!(
    r#"{"toolCall":{"type":"toolCall","id":"call-1/0","name":"bash","arguments":{}},"#,
    r#""result":{"content":[{"type":"text","text":"Nested tool calls are not available in this context"}],"details":{}},"#,
    r#""isError":true}"#
);

#[tokio::test(flavor = "current_thread")]
async fn tool_context_lists_tools_and_falls_back_to_the_exact_nested_call_result() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_tool_context(&harness, host.clone(), "call-1", None);

        assert_eq!(harness.eval::<String>("__ctx.tools[0].name"), "read");
        assert!(harness.eval::<bool>("typeof __ctx.tools[0].execute === 'function'"));
        // 工具 ctx 同时是普通上下文。
        assert_eq!(harness.eval::<String>("__ctx.mode"), "print");

        assert_eq!(
            settle_call(&harness, "__ctx.executeTool('bash', { command: 'ls' })").await,
            Ok(NESTED_CALL_FALLBACK_JSON.to_owned())
        );
        // 没有绑定 executeToolFn：宿主完全没有被调用。
        assert!(host.calls().is_empty(), "{:?}", host.calls());
    })
    .await;

    // Rust 视图的同一回退。
    let context = ExtensionContext::tool(host.clone(), "call-1", None);
    run_local(async {
        let outcome = context
            .execute_tool("bash", serde_json::json!({ "command": "ls" }), None)
            .await;
        assert_eq!(outcome.tool_call.id, "call-1/0");
        assert_eq!(outcome.tool_call.name, "bash");
        assert!(outcome.tool_call.arguments.is_empty());
        assert!(outcome.is_error);
        assert_eq!(context.tools().len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tool_context_forwards_execute_tool_to_the_bound_handler_with_the_parent_signal() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.update(|state| state.execute_tool = true);
    let token = CancellationToken::new();

    run_local(async {
        set_js_tool_context(&harness, host.clone(), "call-9", Some(token.clone()));
        // `ctx.signal` 读的是 runner 当前的 signal（TS 的 `getSignalFn()`），不是工具的
        // signal；工具的 signal 只作为 `executeTool` options.signal 的缺省值。
        assert_eq!(harness.eval::<String>("String(__ctx.signal)"), "undefined");

        assert_eq!(
            settle_call(&harness, "__ctx.executeTool('bash', { command: 'ls' })").await,
            Ok(NESTED_CALL_FALLBACK_JSON.replace("call-1/0", "call-9/0"))
        );
        assert_eq!(
            host.calls(),
            vec![r#"executeTool:call-9:bash:{"command":"ls"}:true"#.to_owned()]
        );
    })
    .await;
}

// ===========================================================================
// ReplacedSessionContext
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn forwards_send_message_and_send_user_message_through_the_replaced_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    let actions = Arc::new(RecordingActions::default());

    run_local(async {
        set_js_replaced_context(&harness, host.clone(), actions.clone());
        // replaced 同时是命令上下文（7 个会话控制方法可用）。
        assert_eq!(harness.eval::<String>("typeof __ctx.fork"), "function");

        assert_eq!(
            settle_call(
                &harness,
                "__ctx.sendMessage({ customType: 'note', content: 'hello', display: true }, { triggerTurn: true, deliverAs: 'nextTurn' })",
            )
            .await,
            Ok("null".to_owned())
        );
        assert_eq!(
            settle_call(
                &harness,
                "__ctx.sendUserMessage('hi there', { deliverAs: 'followUp', expandPromptTemplates: false })",
            )
            .await,
            Ok("null".to_owned())
        );

        assert_eq!(
            actions.calls(),
            vec![
                "sendMessage:note:hello:display=true:turn=true:deliverAs=nextTurn".to_owned(),
                "sendUserMessage:hi there:deliverAs=followUp".to_owned(),
            ]
        );
    })
    .await;
}

/// 固定失败的 replaced actions（验证 TS 的 `Promise<void>` rejection）。
struct FailingActions;

impl ReplacedSessionActions for FailingActions {
    fn send_message(
        &self,
        _message: SendMessageOptions,
    ) -> kiana_ext_api::LocalBoxFuture<'static, Result<(), String>> {
        Box::pin(async { Err("compaction in progress".to_owned()) })
    }
    fn send_user_message(
        &self,
        _content: UserContent,
        _options: Option<SendUserMessageOptions>,
    ) -> kiana_ext_api::LocalBoxFuture<'static, Result<(), String>> {
        Box::pin(async { Err("Agent is already processing.".to_owned()) })
    }
}

/// 回归（fixer, minor）：`ctx.sendMessage`/`ctx.sendUserMessage` 的失败必须 reject
/// 扩展里等待的 Promise（TS `createReplacedSessionContext` 直接挂会话方法）。
#[tokio::test(flavor = "current_thread")]
async fn rejects_replaced_context_send_promises_on_failure() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_replaced_context(&harness, host.clone(), Arc::new(FailingActions));
        assert_eq!(
            settle_call(
                &harness,
                "__ctx.sendMessage({ customType: 'note', content: 'hi', display: false })",
            )
            .await,
            Err("compaction in progress".to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.sendUserMessage('hi there')").await,
            Err("Agent is already processing.".to_owned())
        );
    })
    .await;
}

// ===========================================================================
// 2860：withSession 拿到新的 ReplacedSessionContext，旧 ctx 立即 stale
// ===========================================================================

/// 捕获 stale panic 的逐字文案。
fn stale_panic_message<R>(run: impl FnOnce() -> R) -> String {
    let payload = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(_) => panic!("stale ctx must panic"),
        Err(payload) => payload,
    };
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "current_thread")]
async fn stales_the_captured_context_and_hands_the_replacement_to_new_session_with_session() {
    let registry = new_model_registry().await;
    let host = FakeHost::new(Arc::clone(&registry));
    let replacement_host = FakeHost::new(Arc::clone(&registry));
    let actions = Arc::new(RecordingActions::default());
    let replaced: Arc<dyn ReplacedSessionContextApi> =
        ExtensionContext::replaced(replacement_host, actions.clone());
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    let old = ExtensionContext::command(host.clone());
    let old_in_callback = Arc::clone(&old);
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_in_callback = Arc::clone(&seen);

    let with_session: kiana_ext_api::types::WithSessionFn =
        Arc::new(move |ctx: Arc<dyn ReplacedSessionContextApi>| {
            let old = Arc::clone(&old_in_callback);
            let seen = Arc::clone(&seen_in_callback);
            Box::pin(async move {
                let message = stale_panic_message(|| old.get_system_prompt());
                seen.lock().expect("seen").push(message);
                ctx.send_user_message(
                    UserContent::Text("Hello from the new session!".to_owned()),
                    None,
                )
                .await;
            })
        });

    run_local(async {
        ExtensionCommandContextApi::new_session(
            old.as_ref(),
            Some(NewSessionOptions {
                parent_session: Some("parent.jsonl".to_owned()),
                setup: None,
                with_session: Some(with_session),
            }),
        )
        .await;
    })
    .await;

    assert_eq!(host.calls(), vec!["newSession:parent.jsonl".to_owned()]);
    assert_eq!(seen.lock().expect("seen").clone(), vec![STALE.to_owned()]);
    assert_eq!(
        actions.calls(),
        vec!["sendUserMessage:Hello from the new session!:deliverAs=none".to_owned()]
    );
    // 新 ctx 仍然可用。
    assert_eq!(
        host.state.lock().expect("fake host").stale.as_deref(),
        Some(STALE)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stales_the_captured_context_and_hands_the_replacement_to_fork_with_session() {
    let registry = new_model_registry().await;
    let host = FakeHost::new(Arc::clone(&registry));
    let replacement_host = FakeHost::new(Arc::clone(&registry));
    let actions = Arc::new(RecordingActions::default());
    let replaced: Arc<dyn ReplacedSessionContextApi> =
        ExtensionContext::replaced(replacement_host, actions.clone());
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    let old = ExtensionContext::command(host.clone());
    let old_in_callback = Arc::clone(&old);
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_in_callback = Arc::clone(&seen);

    let with_session: kiana_ext_api::types::WithSessionFn =
        Arc::new(move |ctx: Arc<dyn ReplacedSessionContextApi>| {
            let old = Arc::clone(&old_in_callback);
            let seen = Arc::clone(&seen_in_callback);
            Box::pin(async move {
                let message = stale_panic_message(|| old.get_system_prompt());
                seen.lock().expect("seen").push(message);
                ctx.send_message(SendMessageOptions {
                    message: kiana_ext_api::types::CustomMessagePayload {
                        custom_type: "note".to_owned(),
                        content: UserContent::Text("forked".to_owned()),
                        display: true,
                        details: None,
                    },
                    trigger_turn: None,
                    deliver_as: None,
                })
                .await;
            })
        });

    run_local(async {
        ExtensionCommandContextApi::fork(
            old.as_ref(),
            "entry-1",
            Some(ForkOptions {
                position: Some(kiana_ext_api::events::ForkPosition::At),
                with_session: Some(with_session),
            }),
        )
        .await;
    })
    .await;

    assert_eq!(host.calls(), vec!["fork:entry-1:at".to_owned()]);
    assert_eq!(seen.lock().expect("seen").clone(), vec![STALE.to_owned()]);
    assert_eq!(
        actions.calls(),
        vec!["sendMessage:note:forked:display=true:turn=false:deliverAs=none".to_owned()]
    );
}

// ===========================================================================
// 种类限制
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn restricts_kind_specific_members() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        // base ctx：TS 的对象字面量没有工具/命令成员 → 读取是 undefined，调用是
        // "… is not a function"（不是自定义的 only-available 错误）。
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        for member in [
            "tools",
            "executeTool",
            "sendMessage",
            "sendUserMessage",
            "fork",
            "newSession",
            "navigateTree",
            "switchSession",
            "reload",
            "waitForIdle",
            "getSystemPromptOptions",
        ] {
            assert_eq!(
                harness.eval::<String>(&format!("typeof __ctx.{member}")),
                "undefined",
                "`{member}`"
            );
        }
        for expression in [
            "__ctx.executeTool('bash', {})",
            "__ctx.sendMessage({ customType: 't', content: 'c', display: false })",
            "__ctx.sendUserMessage('c')",
            "__ctx.fork('entry-1')",
            "__ctx.newSession()",
            "__ctx.navigateTree('leaf-1')",
            "__ctx.switchSession('/tmp/s.jsonl')",
            "__ctx.reload()",
            "__ctx.waitForIdle()",
            "__ctx.getSystemPromptOptions()",
        ] {
            let error = harness.expect_error(expression);
            assert_eq!(error.name, "TypeError", "`{expression}`");
            assert!(
                error.message.contains("not a function"),
                "`{expression}`: {}",
                error.message
            );
        }

        // tool ctx：有 tools/executeTool，但没有命令方法。
        set_js_tool_context(&harness, host.clone(), "call-1", None);
        assert_eq!(
            harness.eval::<String>("typeof __ctx.executeTool"),
            "function"
        );
        assert_eq!(harness.eval::<String>("typeof __ctx.fork"), "undefined");
        let error = harness.expect_error("__ctx.fork('entry-1')");
        assert_eq!(error.name, "TypeError");
        assert!(error.message.contains("not a function"));

        // command ctx：有命令方法，但没有 executeTool/tools。
        set_js_command_context(&harness, host.clone());
        assert_eq!(harness.eval::<String>("typeof __ctx.fork"), "function");
        assert_eq!(harness.eval::<String>("typeof __ctx.tools"), "undefined");
        let error = harness.expect_error("__ctx.executeTool('bash', {})");
        assert_eq!(error.name, "TypeError");
        assert!(error.message.contains("not a function"));

        // replaced ctx：命令 + sendMessage/sendUserMessage，但没有工具成员。
        let actions = Arc::new(RecordingActions::default());
        set_js_replaced_context(&harness, host.clone(), actions);
        assert_eq!(
            harness.eval::<String>("typeof __ctx.sendMessage"),
            "function"
        );
        assert_eq!(
            harness.eval::<String>("typeof __ctx.newSession"),
            "function"
        );
        assert_eq!(
            harness.eval::<String>("typeof __ctx.executeTool"),
            "undefined"
        );
        let error = harness.expect_error("__ctx.executeTool('bash', {})");
        assert_eq!(error.name, "TypeError");
        assert!(error.message.contains("not a function"));
    })
    .await;
}

// ===========================================================================
// JS 外观：自有可枚举成员（TS 的对象字面量形状）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn exposes_own_enumerable_members_like_the_ts_object_literal() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert_eq!(harness.eval::<usize>("Object.keys(__ctx).length"), 18);
        assert!(harness.eval::<bool>("Object.keys(__ctx).includes('getSystemPrompt')"));
        assert!(harness.eval::<bool>("Object.keys(__ctx).includes('ui')"));
        // `{...ctx}` 复制全部成员（而不是空对象）。
        assert_eq!(
            harness.eval::<usize>("Object.keys({ ...__ctx }).length"),
            18
        );
        assert_eq!(
            harness.eval::<String>("({ ...__ctx }).cwd"),
            "/tmp/kiana-test"
        );

        // TS `createToolContext` 用 `Object.defineProperties` 叠加在 `createContext()` 的
        // 对象上：`tools` 是**不可枚举、不可配置**的 getter，`executeTool` 是不可写、
        // 不可枚举、不可配置的值。因此 `Object.keys`/展开仍是 18 个成员。
        set_js_tool_context(&harness, host.clone(), "call-1", None);
        assert_eq!(harness.eval::<usize>("Object.keys(__ctx).length"), 18);
        assert_eq!(
            harness.eval::<usize>("Object.keys({ ...__ctx }).length"),
            18
        );
        assert!(!harness.eval::<bool>("Object.keys(__ctx).includes('tools')"));
        assert!(!harness.eval::<bool>("Object.keys(__ctx).includes('executeTool')"));
        assert!(harness.eval::<bool>("'tools' in __ctx && 'executeTool' in __ctx"));
        // 属性描述符逐项对照 Object.defineProperties 的默认值。
        assert!(
            !harness.eval::<bool>("Object.getOwnPropertyDescriptor(__ctx, 'tools').enumerable")
        );
        assert!(
            !harness.eval::<bool>("Object.getOwnPropertyDescriptor(__ctx, 'tools').configurable")
        );
        assert!(harness.eval::<bool>(
            "typeof Object.getOwnPropertyDescriptor(__ctx, 'tools').get === 'function'"
        ));
        assert!(
            !harness
                .eval::<bool>("Object.getOwnPropertyDescriptor(__ctx, 'executeTool').enumerable")
        );
        assert!(
            !harness
                .eval::<bool>("Object.getOwnPropertyDescriptor(__ctx, 'executeTool').configurable")
        );
        assert!(
            !harness.eval::<bool>("Object.getOwnPropertyDescriptor(__ctx, 'executeTool').writable")
        );
        // `ctx.executeTool = x` 不会替换成员（严格模式下抛 TypeError，非严格模式静默失败）。
        harness.eval::<()>(
            "try { __ctx.executeTool = 1; } catch (_error) { globalThis.__sloppy = 1; }",
        );
        assert_eq!(
            harness.eval::<String>("typeof __ctx.executeTool"),
            "function"
        );

        set_js_command_context(&harness, host.clone());
        assert_eq!(harness.eval::<usize>("Object.keys(__ctx).length"), 25);

        let actions = Arc::new(RecordingActions::default());
        set_js_replaced_context(&harness, host.clone(), actions);
        assert_eq!(harness.eval::<usize>("Object.keys(__ctx).length"), 27);
        assert!(harness.eval::<bool>("Object.keys(__ctx).includes('sendMessage')"));
    })
    .await;
}

// ===========================================================================
// signal：真正的 AbortSignal
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn exposes_the_signal_as_a_real_abort_signal() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    let token = CancellationToken::new();
    host.update(|state| state.signal = Some(token.clone()));

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(harness.eval::<bool>("__ctx.signal instanceof AbortSignal"));
        // `AbortSignal.any` 校验每个元素 `instanceof AbortSignal`。
        assert!(harness.eval::<bool>(
            "(() => { const combined = AbortSignal.any([__ctx.signal, AbortSignal.timeout(1000)]); \
              return combined instanceof AbortSignal; })()"
        ));
        assert!(harness.eval::<bool>("__ctx.signal === __ctx.signal"));
        // Node 的 signal 实例没有自有可枚举属性（`aborted` 在原型上）。
        assert_eq!(harness.eval::<usize>("Object.keys(__ctx.signal).length"), 0);
        assert_eq!(harness.eval::<String>("JSON.stringify(__ctx.signal)"), "{}");

        token.cancel();
        // 读取时轮询宿主 token；reason 稳定（每次读取同一个对象）。
        assert!(harness.eval::<bool>("__ctx.signal.aborted"));
        assert!(harness.eval::<bool>("__ctx.signal.reason === __ctx.signal.reason"));
        assert_eq!(
            harness.eval::<String>("__ctx.signal.reason.name"),
            "AbortError"
        );

        // abort 之后添加的监听器在微任务里调用（Node 语义）。
        harness.eval::<()>(
            "globalThis.__late = 0; \
             __ctx.signal.addEventListener('abort', () => { globalThis.__late += 1; });",
        );
        assert_eq!(harness.eval::<i32>("globalThis.__late"), 0);
        harness.pump();
        assert_eq!(harness.eval::<i32>("globalThis.__late"), 1);
    })
    .await;
}

// ===========================================================================
// sessionManager：getLeafId/getHeader 是 null
// ===========================================================================

struct NullSessionManager;

impl ReadonlySessionManager for NullSessionManager {
    fn get_cwd(&self) -> &str {
        "/tmp/kiana-test"
    }
    fn get_session_dir(&self) -> &str {
        "/tmp/kiana-test/sessions"
    }
    fn get_session_id(&self) -> &str {
        "session-null"
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
            thinking_level: "off".to_owned(),
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

#[tokio::test(flavor = "current_thread")]
async fn returns_null_for_empty_leaf_and_header_like_ts() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.update(|state| state.session_manager = Arc::new(NullSessionManager));

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert!(harness.eval::<bool>("__ctx.sessionManager.getLeafId() === null"));
        assert!(harness.eval::<bool>("__ctx.sessionManager.getHeader() === null"));
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.sessionManager.getLeafId())"),
            "null"
        );
        // TS: `getBranch(fromId?: string)` 的 `fromId ?? this.leafId` —— 显式 undefined/null
        // 与省略等价（不能是类型转换错误）。
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.sessionManager.getBranch())"),
            "[]"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.sessionManager.getBranch(undefined))"),
            "[]"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.sessionManager.getBranch(null))"),
            "[]"
        );
    })
    .await;
}

// ===========================================================================
// getSystemPromptOptions：跨 ctx 的引用身份与扩展修改可见
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn get_system_prompt_options_keeps_reference_identity_and_mutations() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.update(|state| state.options.selected_tools = Some(vec!["read".to_owned()]));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "globalThis.__o1 = __ctx.getSystemPromptOptions(); \
             __o1.selectedTools.push('mutated_tool');",
        );
        // 新的 ctx 实例读到的仍是同一个宿主对象（TS: `seenOptions[0] === seenOptions[1]`）。
        set_js_command_context(&harness, host.clone());
        assert!(harness.eval::<bool>("__ctx.getSystemPromptOptions() === globalThis.__o1"));
        assert!(
            harness.eval::<bool>(
                "__ctx.getSystemPromptOptions().selectedTools.includes('mutated_tool')"
            )
        );
        // 宿主状态变化时刷新（读时求值）。
        host.update(|state| state.options.cwd = "/tmp/kiana-test-9".to_owned());
        assert!(harness.eval::<bool>("__ctx.getSystemPromptOptions() !== globalThis.__o1"));
        assert_eq!(
            harness.eval::<String>("__ctx.getSystemPromptOptions().cwd"),
            "/tmp/kiana-test-9"
        );
    })
    .await;
}

// ===========================================================================
// compact：onComplete / onError
// ===========================================================================

/// 没有 JS 回调时 `ctx.compact(options)` 与 TS 一样在调用栈上同步触发 `compactFn`。
#[tokio::test(flavor = "current_thread")]
async fn compact_without_callbacks_runs_synchronously() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>("__ctx.compact({ customInstructions: 'now' });");
        // 没有泵任何任务：宿主动作已经发生。
        assert_eq!(host.state.lock().expect("fake host").compacts, 1);
        assert_eq!(host.calls(), vec!["compact:now".to_owned()]);
        // 不带参数同样同步（`undefined` 交给 compactFn）。
        harness.eval::<()>("__ctx.compact();");
        assert_eq!(host.state.lock().expect("fake host").compacts, 2);
        assert_eq!(
            host.calls(),
            vec!["compact:now".to_owned(), "compact:none".to_owned()]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn compact_invokes_on_complete_and_on_error() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "globalThis.__events = []; \
             __ctx.compact({ customInstructions: 'summarize', \
               onComplete: (result) => __events.push('complete:' + result.summary), \
               onError: (error) => __events.push('error:' + error.message) });",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["complete:summary text"]"#
        );

        host.update(|state| state.compact_error = true);
        harness.eval::<()>(
            "__ctx.compact({ onComplete: (result) => __events.push('complete:' + result.summary), \
               onError: (error) => __events.push('error:' + error.message) });",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["complete:summary text","error:compaction failed"]"#
        );
        assert_eq!(host.state.lock().expect("fake host").compacts, 2);
    })
    .await;
}

// ===========================================================================
// executeTool options：显式 signal + onUpdate
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_forwards_explicit_signal_and_on_update() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    host.update(|state| state.execute_tool = true);
    let parent_token = CancellationToken::new();

    run_local(async {
        set_js_tool_context(&harness, host.clone(), "call-3", Some(parent_token.clone()));
        harness.eval::<()>(
            "globalThis.__partials = []; \
             globalThis.__controller = new AbortController(); \
             __controller.abort();",
        );
        let result = settle_call(
            &harness,
            "__ctx.executeTool('bash', { command: 'ls' }, { \
               signal: __controller.signal, \
               onUpdate: (partial) => __partials.push(partial.content[0].text) })",
        )
        .await;
        assert_eq!(
            result,
            Ok(NESTED_CALL_FALLBACK_JSON.replace("call-1/0", "call-3/0"))
        );
        // 显式 signal 覆盖父工具的 signal：FakeHost 看到的是已取消的 token。
        assert_eq!(
            host.state
                .lock()
                .expect("fake host")
                .execute_signal_cancelled,
            Some(true)
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__partials)"),
            r#"["partial"]"#
        );
    })
    .await;
}

// ===========================================================================
// ctx.tools[i].execute（完整 AgentTool）
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn tools_entries_expose_a_callable_execute() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_tool_context(&harness, host.clone(), "call-1", None);
        assert_eq!(
            harness.eval::<String>("typeof __ctx.tools[0].execute"),
            "function"
        );
        assert_eq!(harness.eval::<String>("__ctx.tools[0].label"), "read");
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__ctx.tools[0].parameters)"),
            r#"{"type":"object","properties":{}}"#
        );
        let result = settle_call(&harness, "__ctx.tools[0].execute('nested-1', {})").await;
        // StubToolExecute 返回空 content 的结果对象。
        let json: JsonValue = serde_json::from_str(&result.expect("tool execute resolves"))
            .expect("parse tool result");
        assert_eq!(json["details"], JsonValue::Null);
    })
    .await;
}

// ===========================================================================
// withSession（2860）：三个会话替换流程都走 JS 回调
// ===========================================================================

fn replacement_for(
    registry: &Arc<ModelRegistry>,
) -> (
    Arc<FakeHost>,
    Arc<dyn ReplacedSessionContextApi>,
    Arc<RecordingActions>,
) {
    let replacement_host = FakeHost::new(Arc::clone(registry));
    let actions = Arc::new(RecordingActions::default());
    let replaced: Arc<dyn ReplacedSessionContextApi> =
        ExtensionContext::replaced(replacement_host, actions.clone());
    (FakeHost::new(Arc::clone(registry)), replaced, actions)
}

#[tokio::test(flavor = "current_thread")]
async fn js_new_session_runs_with_session_with_the_replacement_context() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let (host, replaced, actions) = replacement_for(&registry);
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "globalThis.__events = []; \
             globalThis.__call = __ctx.newSession({ parentSession: 'parent.jsonl', \
               withSession: async (ctx) => { \
                 globalThis.__events.push('with'); \
                 globalThis.__replacedCtx = ctx; \
                 await ctx.sendUserMessage('Hello from the new session!'); \
               } }).then((result) => { globalThis.__events.push('resolved:' + result.cancelled); });",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["with","resolved:false"]"#
        );
        // withSession 拿到的是 ReplacedSessionContext（命令 + send*）。
        assert!(harness.eval::<bool>("globalThis.__replacedCtx.sendMessage instanceof Function"));
        assert_eq!(
            harness.eval::<String>("globalThis.__replacedCtx.sessionManager.getSessionId()"),
            "session-1"
        );
        assert_eq!(
            actions.calls(),
            vec!["sendUserMessage:Hello from the new session!:deliverAs=none".to_owned()]
        );
        assert_eq!(host.calls(), vec!["newSession:parent.jsonl".to_owned()]);
    })
    .await;
}

/// `finishSessionReplacement` 在 `withSession` settle 之前不会让 `newSession` 的 Promise
/// resolve（agent-session-runtime.ts:187-193）。
#[tokio::test(flavor = "current_thread")]
async fn new_session_resolves_only_after_the_with_session_callback_settles() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let (host, replaced, _actions) = replacement_for(&registry);
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "globalThis.__events = []; \
             __ctx.newSession({ withSession: async () => { \
               await new Promise((resolve) => setTimeout(resolve, 10)); \
               globalThis.__events.push('callback-done'); \
             } }).then(() => globalThis.__events.push('resolved'));",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["callback-done","resolved"]"#
        );
    })
    .await;
}

/// 回调抛出 → 对应的 `newSession` Promise 被拒绝（TS 的 `await withSession(...)`）。
#[tokio::test(flavor = "current_thread")]
async fn a_throwing_with_session_callback_rejects_the_command_promise() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let (host, replaced, _actions) = replacement_for(&registry);
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        let result = settle_call(
            &harness,
            "__ctx.newSession({ withSession: async () => { throw new Error('boom'); } })",
        )
        .await;
        assert_eq!(result, Err("boom".to_owned()));
        // TS 里 rejection 只传给命令调用方，不会再记一条扩展错误（applyCallback 对
        // withSession/setup 不调用 `kiana.recordError`）。
        let errors = harness.take_errors();
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    })
    .await;
}

/// `newSession({ setup })`：新会话管理器交给回调、被 await；抛错中止命令。
#[tokio::test(flavor = "current_thread")]
async fn new_session_awaits_setup_with_the_new_session_manager() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let (host, replaced, _actions) = replacement_for(&registry);
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "globalThis.__events = []; \
             __ctx.newSession({ \
               setup: async (sessionManager) => { \
                 globalThis.__setupSessionFile = sessionManager.getSessionFile(); \
                 globalThis.__setupCwd = sessionManager.getCwd(); \
                 await new Promise((resolve) => setTimeout(resolve, 5)); \
                 globalThis.__events.push('setup-done'); \
               }, \
               withSession: async () => { globalThis.__events.push('with'); }, \
             }).then(() => globalThis.__events.push('resolved'));",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["setup-done","with","resolved"]"#
        );
        // in-memory SessionManager：没有 session file，cwd 是构造参数。
        assert_eq!(
            harness.eval::<String>("String(__setupSessionFile)"),
            "undefined"
        );
        assert_eq!(harness.eval::<String>("__setupCwd"), "/tmp/kiana-test");
        assert_eq!(
            host.calls(),
            vec!["newSession:".to_owned(), "setup".to_owned()]
        );

        // setup 抛错 → newSession 拒绝，withSession 不执行（新的命令上下文：上一个已被
        // 第一次 newSession 变 stale）。
        let (error_host, error_replaced, _actions) = replacement_for(&registry);
        let weak = Arc::downgrade(&error_host);
        error_host.update(|state| state.replacement = Some((weak, error_replaced)));
        set_js_command_context(&harness, error_host.clone());
        harness.eval::<()>("globalThis.__errorEvents = [];");
        let result = settle_call(
            &harness,
            "__ctx.newSession({ \
               setup: async () => { throw new Error('setup failed'); }, \
               withSession: async () => { globalThis.__errorEvents.push('with'); }, \
             })",
        )
        .await;
        assert_eq!(result, Err("setup failed".to_owned()));
        // setup 被调用过（回调本身 reject）；TS 的 `await options.setup(...)` 先抛出，
        // withSession 不会执行。
        assert_eq!(
            error_host.calls(),
            vec!["newSession:".to_owned(), "setup".to_owned()]
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__errorEvents)"),
            "[]"
        );
    })
    .await;
}

/// withSession 拿到的是**新** runner 的上下文：会话管理器/模式都来自替换后的 host；
/// 之后再次替换时，这个 captured replaced ctx 也必须变 stale（TS 的 assertActive）。
#[tokio::test(flavor = "current_thread")]
async fn with_session_uses_the_replacement_host_and_goes_stale_after_another_replacement() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(Arc::clone(&registry));
    let replacement_host = FakeHost::new(Arc::clone(&registry));
    replacement_host.update(|state| {
        state.session_manager = Arc::new(NullSessionManager);
        state.mode = ExtensionMode::Tui;
    });
    let actions = Arc::new(RecordingActions::default());
    let replaced: Arc<dyn ReplacedSessionContextApi> =
        ExtensionContext::replaced(replacement_host.clone(), actions);
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "__ctx.newSession({ withSession: async (ctx) => { \
               globalThis.__replacedCtx = ctx; \
               globalThis.__replacedSessionId = ctx.sessionManager.getSessionId(); \
               globalThis.__replacedMode = ctx.mode; \
             } });",
        );
        assert!(harness.settle_all().await);
        // 旧 host 的 FakeSessionManager 返回 "session-1"；替换后的 host 返回 "session-null"。
        assert_eq!(
            harness.eval::<String>("globalThis.__replacedSessionId"),
            "session-null"
        );
        assert_eq!(harness.eval::<String>("globalThis.__replacedMode"), "tui");

        // 之后再次替换（invalidate 新 host）→ captured replaced ctx 的成员读取抛同一文案。
        replacement_host.invalidate(STALE);
        let error = harness.expect_error("globalThis.__replacedCtx.sessionManager");
        assert_eq!(error.name, "Error");
        assert_eq!(error.message, STALE);
        let error = harness.expect_error("globalThis.__replacedCtx.modelRegistry");
        assert_eq!(error.message, STALE);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn js_fork_and_switch_session_run_with_session() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let (host, replaced, actions) = replacement_for(&registry);
    let weak = Arc::downgrade(&host);
    host.update(|state| state.replacement = Some((weak, replaced)));

    run_local(async {
        set_js_command_context(&harness, host.clone());
        harness.eval::<()>(
            "globalThis.__events = []; \
             __ctx.fork('entry-1', { position: 'at', withSession: async (ctx) => { \
               globalThis.__events.push('fork'); \
               await ctx.sendMessage({ customType: 'note', content: 'forked', display: true }); \
             } }).then(() => globalThis.__events.push('fork-done'));",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["fork","fork-done"]"#
        );
        assert_eq!(
            actions.calls(),
            vec!["sendMessage:note:forked:display=true:turn=false:deliverAs=none".to_owned()]
        );

        // switchSession 同样支持 withSession（#2860 的第三个流程）。
        let (switch_host, switch_replaced, switch_actions) = replacement_for(&registry);
        let weak = Arc::downgrade(&switch_host);
        switch_host.update(|state| state.replacement = Some((weak, switch_replaced)));
        set_js_command_context(&harness, switch_host.clone());
        harness.eval::<()>(
            "globalThis.__events = []; \
             __ctx.switchSession('/tmp/target.jsonl', { withSession: async (ctx) => { \
               globalThis.__events.push('switch'); \
               await ctx.sendUserMessage('switch callback message'); \
             } }).then(() => globalThis.__events.push('switch-done'));",
        );
        assert!(harness.settle_all().await);
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["switch","switch-done"]"#
        );
        assert_eq!(
            switch_actions.calls(),
            vec!["sendUserMessage:switch callback message:deliverAs=none".to_owned()]
        );
        assert_eq!(
            switch_host.calls(),
            vec!["switchSession:/tmp/target.jsonl".to_owned()]
        );
        assert_eq!(host.calls(), vec!["fork:entry-1:at".to_owned()]);
    })
    .await;
}

// ===========================================================================
// sendMessage：content 缺省/null 归一化为 []
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn send_message_accepts_missing_content_and_display() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let host = FakeHost::new(registry);
    let actions = Arc::new(RecordingActions::default());

    run_local(async {
        set_js_replaced_context(&harness, host.clone(), actions.clone());
        assert_eq!(
            settle_call(&harness, "__ctx.sendMessage({ customType: 'note' })").await,
            Ok("null".to_owned())
        );
        assert_eq!(
            settle_call(
                &harness,
                "__ctx.sendMessage({ customType: 'note', content: null, display: true })"
            )
            .await,
            Ok("null".to_owned())
        );
        // `details: null` 原样保留（TS 的 `details: message.details`）；缺失的 customType
        // 按 `undefined` 处理（Rust 会话条目字段是 String，这里落为空串）。
        assert_eq!(
            settle_call(
                &harness,
                "__ctx.sendMessage({ customType: 'note', content: 'x', display: true, details: null })"
            )
            .await,
            Ok("null".to_owned())
        );
        assert_eq!(
            settle_call(&harness, "__ctx.sendMessage({ content: 'untyped' })").await,
            Ok("null".to_owned())
        );
        assert_eq!(
            actions.calls(),
            vec![
                "sendMessage:note:<blocks>:display=false:turn=false:deliverAs=none".to_owned(),
                "sendMessage:note:<blocks>:display=true:turn=false:deliverAs=none".to_owned(),
                "sendMessage:note:x:display=true:turn=false:deliverAs=none:details=null".to_owned(),
                "sendMessage::untyped:display=false:turn=false:deliverAs=none".to_owned(),
            ]
        );
    })
    .await;
}

// ===========================================================================
// modelRegistry：getProvider 与 refresh 的 Map 结果
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn model_registry_exposes_get_provider_and_a_map_refresh_result() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    let known_provider = registry
        .get_all()
        .first()
        .map(|model| model.provider.clone());
    let host = FakeHost::new(registry);

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        assert_eq!(
            harness.eval::<String>("typeof __ctx.modelRegistry.getProvider"),
            "function"
        );
        assert!(harness.eval::<bool>("__ctx.modelRegistry.getProvider('nope') === undefined"));
        if let Some(provider) = &known_provider {
            let literal = serde_json::to_string(provider).expect("provider literal");
            assert!(harness.eval::<bool>(&format!(
                "(() => {{ const p = __ctx.modelRegistry.getProvider({literal});                   return p !== undefined && p.id === {literal} &&                     Array.isArray(p.getModels()) && p.getModels().length > 0; }})()"
            )));
        }
        // TS 的 `provider?` 参数允许显式 undefined。
        assert!(harness.eval::<bool>(
            "Array.isArray(__ctx.modelRegistry.getModelsOfType('chat', undefined)) &&              __ctx.modelRegistry.getModelsOfType('chat', undefined).length > 0"
        ));
        assert!(harness.eval::<bool>(
            "__ctx.modelRegistry.getAvailableOfType('chat', undefined, {}) instanceof Promise"
        ));

        harness.eval::<()>(
            "globalThis.__refresh = undefined; \
             __ctx.modelRegistry.refresh({ allowNetwork: false }) \
               .then((result) => { globalThis.__refresh = result; });",
        );
        assert!(
            harness
                .drive_until(std::time::Duration::from_secs(5), |harness| {
                    harness.eval::<bool>("globalThis.__refresh !== undefined")
                })
                .await,
            "model registry refresh did not settle"
        );
        assert!(harness.eval::<bool>("globalThis.__refresh.errors instanceof Map"));
        assert_eq!(
            harness.eval::<String>("JSON.stringify(globalThis.__refresh)"),
            r#"{"aborted":false,"errors":{}}"#
        );
    })
    .await;
}

// ===========================================================================
// modelRegistry.stream / streamSimple（8964-extension-provider-streaming）
// ===========================================================================

/// `ctx.modelRegistry.stream` / `streamSimple` 返回 `for await` + `result()` 可用的
/// 事件流（8964 回归：扩展注册的 provider 也能走这两个入口）。
#[tokio::test(flavor = "current_thread")]
async fn model_registry_streams_events_and_the_final_result_to_js() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    install_faux_provider(&registry);
    let host = FakeHost::new(Arc::clone(&registry));

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        harness.eval::<()>(
            "globalThis.__events = []; globalThis.__text = ''; globalThis.__result = undefined; \
             (async () => { \
               const model = __ctx.modelRegistry.find('extension-provider', 'faux-model'); \
               globalThis.__modelFound = model !== undefined && model.id === 'faux-model'; \
               const stream = __ctx.modelRegistry.streamSimple(model, { \
                 messages: [{ role: 'user', content: 'Hello', timestamp: Date.now() }], \
               }); \
               globalThis.__isAsyncIterable = typeof stream[Symbol.asyncIterator] === 'function'; \
               for await (const event of stream) { \
                 __events.push(event.type); \
                 if (event.type === 'text_delta') __text += event.delta; \
               } \
               globalThis.__iterated = true; \
               globalThis.__eventsDone = true; \
             })().catch((error) => { globalThis.__streamError = String((error && error.message) || error); }); \
             (async () => { \
               const model = __ctx.modelRegistry.find('extension-provider', 'faux-model'); \
               const stream = __ctx.modelRegistry.stream(model, { \
                 messages: [{ role: 'user', content: 'Hi', timestamp: Date.now() }], \
               }); \
               globalThis.__result = await stream.result(); \
               globalThis.__resultDone = true; \
             })().catch((error) => { globalThis.__resultError = String((error && error.message) || error); });",
        );
        assert!(
            harness
                .drive_until(std::time::Duration::from_secs(5), |harness| {
                    harness.eval::<String>(
                        "String(globalThis.__eventsDone) + ':' + String(globalThis.__resultDone)",
                    ) == "true:true"
                })
                .await,
            "the model registry streams did not settle"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__streamError)"),
            "undefined"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__resultError)"),
            "undefined"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__modelFound)"),
            "true"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__isAsyncIterable)"),
            "true"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__iterated)"),
            "true"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["start","text_start","text_delta","text_end","done"]"#
        );
        assert_eq!(harness.eval::<String>("globalThis.__text"), "custom provider response");
        assert_eq!(
            harness.eval::<String>("globalThis.__result.stopReason"),
            "stop"
        );
        assert_eq!(
            harness.eval::<String>("globalThis.__result.content[0].text"),
            "custom provider response"
        );
    })
    .await;
}

// ===========================================================================
// Rust 视图
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn rust_view_reads_state_through_the_host() {
    let registry = new_model_registry().await;
    let host = FakeHost::new(Arc::clone(&registry));
    let token = CancellationToken::new();
    host.update(|state| {
        state.signal = Some(token.clone());
        state.pending = true;
        state.is_idle = false;
        state.usage = Some(ContextUsage {
            tokens: Some(1_000),
            context_window: 200_000,
            percent: Some(0.5),
        });
    });

    let context = ExtensionContext::base(host.clone());
    assert_eq!(context.mode(), ExtensionMode::Print);
    assert!(!context.has_ui());
    assert_eq!(context.cwd(), std::path::Path::new("/tmp/kiana-test"));
    assert_eq!(context.session_manager().get_session_id(), "session-1");
    assert!(std::ptr::eq(context.model_registry(), registry.as_ref()));
    assert_eq!(context.model(), None);
    assert_eq!(context.thinking_level(), None);
    assert!(context.scoped_models().is_empty());
    assert_eq!(context.signal(), Some(token));
    assert!(context.has_pending_messages());
    assert!(!context.is_idle());
    assert!(context.is_project_trusted());
    assert_eq!(context.get_system_prompt(), "");
    assert_eq!(
        context.get_context_usage().map(|usage| usage.tokens),
        Some(Some(1_000))
    );
    // 无 UI 时 `ui()` 是进程级 no-op 单例。
    assert_eq!(context.ui().get_editor_text(), "");
    assert!(!context.ui().get_tools_expanded());

    context.abort();
    context.shutdown();
    context.compact(Some(CompactOptions {
        custom_instructions: Some("summarize".to_owned()),
        ..Default::default()
    }));
    assert_eq!(host.calls(), vec!["compact:summarize".to_owned()]);
    assert_eq!(host.state.lock().expect("fake host").aborts, 1);
    assert_eq!(host.state.lock().expect("fake host").shutdowns, 1);
}

// ===========================================================================
// 辅助
// ===========================================================================

/// 启动一个返回 Promise 的 JS 表达式并泵到 settle，返回 `Ok(JSON.stringify(value))` 或
/// `Err(error.message)`（宿主完成事件由 `settle_all` 驱动）。
async fn settle_call(harness: &Harness, expression: &str) -> Result<String, String> {
    harness.eval::<()>(&format!(
        "globalThis.__done = false; globalThis.__result = undefined; globalThis.__error = undefined;\n\
         Promise.resolve({expression}).then(\n\
           (value) => {{ globalThis.__result = JSON.stringify(value === undefined ? null : value); globalThis.__done = true; }},\n\
           (error) => {{ globalThis.__error = String(error && error.message !== undefined ? error.message : error); globalThis.__done = true; }}\n\
         );"
    ));
    assert!(
        harness.settle_all().await,
        "engine did not settle for `{expression}`"
    );
    assert!(
        harness.eval::<bool>("globalThis.__done"),
        "promise did not settle for `{expression}`"
    );
    match harness.eval::<Option<String>>("globalThis.__error") {
        Some(message) => Err(message),
        None => Ok(harness.eval::<String>("globalThis.__result")),
    }
}

// ===========================================================================
// modelRegistry 的注册面（审查 major：ctx.modelRegistry 不再是同步报错桩）
// ===========================================================================

/// 本用例需要登记「当前引擎」（`JsCallback` 的持久槽），因此自建 `Rc<JsEngine>` 并安装
/// loader（`Harness` 持有的是裸 `JsEngine`）。
struct RegisteredHarness {
    engine: std::rc::Rc<JsEngine>,
}

impl RegisteredHarness {
    fn new() -> Self {
        let engine = std::rc::Rc::new(JsEngine::new().expect("create engine"));
        kiana_extensions::engine::install(&engine).expect("install prelude");
        context::install(&engine).expect("install context.js");
        kiana_extensions::loader::install(std::rc::Rc::clone(&engine)).expect("install loader");
        Self { engine }
    }

    fn eval<T>(&self, source: &str) -> T
    where
        T: for<'js> rquickjs::FromJs<'js>,
    {
        self.engine
            .eval_script::<T>(source, "<test>")
            .unwrap_or_else(|error| panic!("eval failed: {error:?}"))
    }

    fn eval_error(&self, source: &str) -> kiana_extensions::engine::JsError {
        self.engine
            .eval_script_error::<()>(source, "<test>")
            .expect_err("expected a JS error")
    }

    fn set_context(&self, context: Arc<ExtensionContext>) {
        self.engine
            .with_js(|ctx| -> rquickjs::Result<()> {
                let value = context::create_context_value(&ctx, Arc::clone(context.host()))?;
                ctx.globals().set("__ctx", value)
            })
            .expect("create context");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn model_registry_registers_providers_and_virtual_models_from_js() {
    let harness = RegisteredHarness::new();
    let registry = new_model_registry().await;
    let host = FakeHost::new(Arc::clone(&registry));

    run_local(async {
        harness.set_context(ExtensionContext::base(host.clone()));
        harness.eval::<()>(
            r#"
            __ctx.modelRegistry.registerProvider("js-provider", {
              name: "JS Provider",
              baseUrl: "https://example.invalid",
              apiKey: "$JS_PROVIDER_KEY",
              api: "openai-completions",
              headers: { "x-js": "1" },
              authHeader: true,
              models: [
                {
                  id: "m1",
                  name: "Model One",
                  input: ["text"],
                  cost: { input: 1, output: 2, cacheRead: 0, cacheWrite: 0 },
                  reasoning: false,
                  contextWindow: 1000,
                  maxTokens: 100,
                },
              ],
            });
            __ctx.modelRegistry.registerVirtualModel({
              provider: "js-provider",
              id: "virtual-1",
              name: "Virtual One",
              route: () => ({ model: undefined, thinkingLevel: "off" }),
            });
            "#,
        );
        let ids = harness.eval::<String>(
            "JSON.stringify(__ctx.modelRegistry.getRegisteredProviderIds().sort())",
        );
        assert!(ids.contains("js-provider"), "ids={ids}");
        assert_eq!(
            harness.eval::<String>(
                "__ctx.modelRegistry.getRegisteredProviderConfig('js-provider').name"
            ),
            "JS Provider"
        );
        assert_eq!(
            harness.eval::<String>(
                "__ctx.modelRegistry.getRegisteredProviderConfig('js-provider').baseUrl"
            ),
            "https://example.invalid"
        );
        assert_eq!(
            harness.eval::<String>(
                "JSON.stringify(__ctx.modelRegistry.getRegisteredProviderConfig('js-provider').models[0].id)"
            ),
            r#""m1""#
        );
        assert!(harness.eval::<bool>(
            "typeof __ctx.modelRegistry.getProvider('js-provider').stream === 'function' && \
             typeof __ctx.modelRegistry.getProvider('js-provider').streamSimple === 'function'"
        ));
        assert!(
            harness.eval::<bool>(
                "__ctx.modelRegistry.find('js-provider', 'virtual-1') !== undefined"
            ),
            "virtual model must be listed"
        );

        // 同名注销后配置消失。
        harness.eval::<()>("__ctx.modelRegistry.unregisterProvider('js-provider');");
        assert!(harness.eval::<bool>(
            "__ctx.modelRegistry.getRegisteredProviderConfig('js-provider') === undefined"
        ));

        // 无名配置的按名注册：逐字错误。
        let error = harness.eval_error(
            "__ctx.modelRegistry.registerProvider('missing-config')",
        );
        assert_eq!(
            error.message,
            "Provider config is required when registering by name"
        );
    })
    .await;
}

/// 审查 minor/major 回归：`ctx.modelRegistry.getProvider(id).stream/streamSimple` 不再是
/// 同步报错桩，返回与 `ctx.modelRegistry.stream*` 相同的事件流外观。
#[tokio::test(flavor = "current_thread")]
async fn model_registry_provider_stream_surface_is_implemented() {
    let harness = new_engine();
    let registry = new_model_registry().await;
    install_faux_provider(&registry);
    let host = FakeHost::new(Arc::clone(&registry));

    run_local(async {
        set_js_context(&harness, ExtensionContext::base(host.clone()));
        harness.eval::<()>(
            r#"
            globalThis.__events = [];
            globalThis.__text = "";
            globalThis.__result = undefined;
            (async () => {
              const provider = __ctx.modelRegistry.getProvider("extension-provider");
              const model = __ctx.modelRegistry.find("extension-provider", "faux-model");
              const stream = provider.streamSimple(model, {
                messages: [{ role: "user", content: "Hello", timestamp: Date.now() }],
              });
              for await (const event of stream) {
                __events.push(event.type);
                if (event.type === "text_delta") __text += event.delta;
              }
              globalThis.__iterated = true;
            })().catch((error) => {
              globalThis.__streamError = String((error && error.message) || error);
            });
            (async () => {
              const provider = __ctx.modelRegistry.getProvider("extension-provider");
              const model = __ctx.modelRegistry.find("extension-provider", "faux-model");
              const stream = provider.stream(model, {
                messages: [{ role: "user", content: "Hi", timestamp: Date.now() }],
              });
              globalThis.__result = await stream.result();
              globalThis.__resultDone = true;
            })().catch((error) => {
              globalThis.__resultError = String((error && error.message) || error);
            });
            "#,
        );
        assert!(
            harness
                .drive_until(std::time::Duration::from_secs(5), |harness| {
                    harness.eval::<String>(
                        "String(globalThis.__iterated) + ':' + String(globalThis.__resultDone)",
                    ) == "true:true"
                })
                .await,
            "provider streams did not settle"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__streamError)"),
            "undefined"
        );
        assert_eq!(
            harness.eval::<String>("String(globalThis.__resultError)"),
            "undefined"
        );
        assert_eq!(
            harness.eval::<String>("JSON.stringify(__events)"),
            r#"["start","text_start","text_delta","text_end","done"]"#
        );
        assert_eq!(
            harness.eval::<String>("globalThis.__text"),
            "custom provider response"
        );
        assert_eq!(
            harness.eval::<String>("globalThis.__result.content[0].text"),
            "custom provider response"
        );
    })
    .await;
}
