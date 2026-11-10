//! RPC 服务端分派对拍测试（spec `ca-rpc-print.md` §7.1 / §7.4 第 3 项）。
//!
//! 逐条转写 TS 的 `rpc-prompt-response-semantics.test.ts`、`5868-rpc-unknown-command-id.test.ts`、
//! `7150-rpc-prompt-during-compaction.test.ts`、`9068-user-bash-fail-closed.test.ts`（RPC 部分），
//! 并补充分派表、关停与双 rebind 的边界。
//!
//! 所有测试都在 `tokio::task::LocalSet` 上运行（扩展派发/UI future 是 `!Send`），
//! 通过 `MockHost` / `MockFacade` + `CaptureStdout` 观测 stdout，避免任何进程级全局状态。

#![allow(clippy::arc_with_non_send_sync)]

use std::collections::VecDeque;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};

use kiana_agent_core::QueueMode;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, ImageContent, Message, Model, ModelCost,
    ModelInputModality, ModelThinkingLevel, UserContent, UserMessage, UserRole,
};
use kiana_compaction::compaction::CompactionResult;
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::events::{ForkPosition, InputSource, UserBashEvent, UserBashEventResult};
use kiana_ext_api::tool_result::BashResult;
use kiana_ext_api::types::{
    ExtensionError, ExtensionMode, ForkOptions, NewSessionOptions, ResolvedCommand,
    SwitchSessionOptions,
};
use kiana_ext_api::{BoxFuture, LocalBoxFuture};
use kiana_rpc_print::RawStdout;
use kiana_rpc_print::rpc_mode::{AgentSessionFacade, RpcServer, RpcSessionHost, RunSignal};
use kiana_session::agent_session::{
    AgentSession, AgentSessionError, AgentSessionEvent, AgentSessionEventListener,
    AgentSessionUnsubscribe, ExecuteBashOptions, ExtensionBindings as SessionExtensionBindings,
    ModelCycleResult, ModelMutationOptions, NavigateTreeOptions, PromptDisposition, PromptOptions,
    QueuedInputDisposition, SessionStats, SessionStatsTokens, TreeNavigationResult,
};
use kiana_session::agent_session_runtime::{ForkSessionResult, SessionReplacementResult};
use kiana_session::sdk::{PromptTemplate, Skill};
use kiana_session_core::session_manager::{
    SessionEntry, SessionHeader, SessionInfoEntry, SessionInfoType, SessionTreeNode,
};

// ============================================================================
// stdout 捕获
// ============================================================================

/// 记录写入的 [`RawStdout`] 替身（不触碰进程级 stdout）。
#[derive(Default)]
struct CaptureStdout {
    lines: Mutex<Vec<String>>,
    flushes: AtomicUsize,
    take_overs: AtomicUsize,
    backpressure_calls: AtomicUsize,
    backpressure_gate: Mutex<Option<Arc<BackpressureGate>>>,
}

/// 可释放的背压闸门：`wait_for_backpressure` 在它释放前一直挂起。
#[derive(Default)]
struct BackpressureGate {
    released: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl BackpressureGate {
    async fn wait(&self) {
        if self.released.load(Ordering::SeqCst) {
            return;
        }
        self.notify.notified().await;
    }

    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl CaptureStdout {
    fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }

    fn records(&self) -> Vec<Value> {
        self.lines()
            .iter()
            .flat_map(|line| line.split('\n'))
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("valid JSON record"))
            .collect()
    }

    fn responses(&self) -> Vec<Value> {
        self.records()
            .into_iter()
            .filter(|record| record.get("type").and_then(Value::as_str) == Some("response"))
            .collect()
    }

    fn response_for(&self, id: &str) -> Option<Value> {
        self.responses()
            .into_iter()
            .find(|record| record.get("id").and_then(Value::as_str) == Some(id))
    }

    fn flush_count(&self) -> usize {
        self.flushes.load(Ordering::SeqCst)
    }

    fn take_over_count(&self) -> usize {
        self.take_overs.load(Ordering::SeqCst)
    }

    fn block_backpressure(&self, gate: Arc<BackpressureGate>) {
        *lock(&self.backpressure_gate) = Some(gate);
    }
}

impl RawStdout for CaptureStdout {
    fn write_raw(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        lock(&self.lines).push(text.to_string());
    }

    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()> {
        self.backpressure_calls.fetch_add(1, Ordering::SeqCst);
        let gate = lock(&self.backpressure_gate).clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
        })
    }

    fn flush(&self) -> BoxFuture<'_, ()> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn take_over(&self) {
        self.take_overs.fetch_add(1, Ordering::SeqCst);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ============================================================================
// Mock 会话门面
// ============================================================================

/// `user_bash` 的 mock 行为（对应 #9068 的三类 handler 结果）。
#[derive(Clone)]
enum UserBashBehavior {
    /// handler 全部返回 `undefined`。
    Local,
    /// handler 抛错：先 `emitError` 再 rethrow。
    Throw(String),
    /// handler 返回非法结果。
    Invalid,
    /// handler 返回完整结果。
    Result(BashResult),
}

struct MockState {
    model: Option<Model>,
    thinking_level: ModelThinkingLevel,
    is_streaming: bool,
    is_compacting: bool,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
    session_file: Option<String>,
    session_id: String,
    session_name: Option<String>,
    auto_compaction_enabled: bool,
    messages: Vec<AgentMessage>,
    pending_message_count: usize,
    available_models: Vec<Model>,
    available_thinking_levels: Vec<ModelThinkingLevel>,
    /// `session.agent`（`Some` 时 RPC 模式会建立 stdout 背压订阅）。
    agent: Option<kiana_agent_core::Agent>,
    entries: Vec<SessionEntry>,
    tree: Vec<SessionTreeNode>,
    leaf_id: Option<String>,
    cwd: String,
    prompt_templates: Vec<PromptTemplate>,
    skills: Vec<Skill>,
    registered_commands: Vec<ResolvedCommand>,
    last_assistant_text: Option<String>,
    fork_messages: Vec<(String, String)>,
    session_stats: SessionStats,
    // 行为开关
    prompt_error: Option<AgentSessionError>,
    prompt_disposition: Option<PromptDisposition>,
    steer_disposition: QueuedInputDisposition,
    follow_up_disposition: QueuedInputDisposition,
    cycle_model_result: Option<ModelCycleResult>,
    cycle_thinking_result: Option<ModelThinkingLevel>,
    compact_result: Option<Result<CompactionResult, AgentSessionError>>,
    export_path: Option<String>,
    compact_instructions: Vec<Option<String>>,
    bash_result: BashResult,
    user_bash_behavior: UserBashBehavior,
    set_model_error: Option<AgentSessionError>,
    /// 设置后 `prompt` 永远挂起（模拟一个卡在授权检查/扩展处理里的命令）。
    prompt_gate: Option<Arc<tokio::sync::Notify>>,
    /// `bind_extensions` 返回前执行的钩子（模拟扩展在 `session_start` 里触发嵌套 rebind）。
    on_bind: Option<Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>>,
    // 记录
    bindings: Option<SessionExtensionBindings>,
    bind_count: usize,
    listener: Option<AgentSessionEventListener>,
    calls: Vec<String>,
    execute_bash_count: usize,
    recorded_bash: Option<(String, BashResult, Option<bool>)>,
    set_thinking_levels: Vec<ModelThinkingLevel>,
    set_session_names: Vec<String>,
    prompt_images: Vec<Option<Vec<ImageContent>>>,
    steer_images: Vec<Option<Vec<ImageContent>>>,
    follow_up_images: Vec<Option<Vec<ImageContent>>>,
    steering_modes: Vec<QueueMode>,
    follow_up_modes: Vec<QueueMode>,
    abort_count: usize,
    abort_retry_count: usize,
    abort_bash_count: usize,
    auto_compaction: Vec<bool>,
    auto_retry: Vec<bool>,
    clear_queue_result: (Vec<String>, Vec<String>),
    /// 在 `steer`/`follow_up`/`clear_queue` 内部经订阅写出的 `queue_update`（模拟会话行为）。
    queue_update_on_steer: Option<(Vec<String>, Vec<String>)>,
    queue_update_on_follow_up: Option<(Vec<String>, Vec<String>)>,
    queue_update_on_clear: Option<(Vec<String>, Vec<String>)>,
}

impl MockState {
    fn new() -> Self {
        Self {
            model: Some(fake_model("fake", "fake-model")),
            thinking_level: ModelThinkingLevel::High,
            is_streaming: false,
            is_compacting: false,
            steering_mode: QueueMode::All,
            follow_up_mode: QueueMode::OneAtATime,
            session_file: Some("/tmp/session.jsonl".to_string()),
            session_id: "session-id".to_string(),
            session_name: Some("my session".to_string()),
            auto_compaction_enabled: true,
            messages: vec![custom_message("one"), custom_message("two")],
            pending_message_count: 1,
            available_models: vec![fake_model("fake", "fake-model")],
            available_thinking_levels: vec![ModelThinkingLevel::Off, ModelThinkingLevel::High],
            agent: None,
            entries: vec![
                session_info_entry("e1"),
                session_info_entry("e2"),
                session_info_entry("e3"),
            ],
            tree: vec![SessionTreeNode {
                entry: session_info_entry("e1"),
                children: vec![SessionTreeNode {
                    entry: session_info_entry("e2"),
                    children: Vec::new(),
                    label: Some("branch".to_string()),
                    label_timestamp: Some("2024-01-01T00:00:00.000Z".to_string()),
                }],
                label: None,
                label_timestamp: None,
            }],
            leaf_id: Some("e3".to_string()),
            cwd: "/work".to_string(),
            prompt_templates: vec![prompt_template("template")],
            skills: vec![skill("myskill")],
            registered_commands: vec![resolved_command("cmd")],
            last_assistant_text: None,
            fork_messages: vec![
                ("e1".to_string(), "hello".to_string()),
                ("e2".to_string(), "world".to_string()),
            ],
            session_stats: SessionStats {
                session_file: Some("/tmp/session.jsonl".to_string()),
                session_id: "session-id".to_string(),
                user_messages: 1,
                assistant_messages: 1,
                tool_calls: 2,
                tool_results: 2,
                total_messages: 4,
                tokens: SessionStatsTokens {
                    input: 10.0,
                    output: 20.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 30.0,
                },
                cost: 0.5,
                context_usage: None,
            },
            prompt_error: None,
            prompt_disposition: Some(PromptDisposition::Started),
            steer_disposition: QueuedInputDisposition::Queued,
            follow_up_disposition: QueuedInputDisposition::Handled,
            cycle_model_result: None,
            cycle_thinking_result: None,
            compact_result: Some(Ok(CompactionResult {
                summary: "summary".to_string(),
                first_kept_entry_id: "e2".to_string(),
                tokens_before: 100,
                estimated_tokens_after: None,
                usage: None,
                details: None,
            })),
            export_path: Some("/tmp/export.html".to_string()),
            bash_result: BashResult {
                output: "local output".to_string(),
                exit_code: Some(0),
                cancelled: false,
                truncated: false,
                full_output_path: None,
                execution_status: None,
            },
            user_bash_behavior: UserBashBehavior::Local,
            set_model_error: None,
            prompt_gate: None,
            on_bind: None,
            bindings: None,
            bind_count: 0,
            listener: None,
            calls: Vec::new(),
            execute_bash_count: 0,
            recorded_bash: None,
            set_thinking_levels: Vec::new(),
            set_session_names: Vec::new(),
            prompt_images: Vec::new(),
            steer_images: Vec::new(),
            follow_up_images: Vec::new(),
            steering_modes: Vec::new(),
            follow_up_modes: Vec::new(),
            abort_count: 0,
            abort_retry_count: 0,
            abort_bash_count: 0,
            auto_compaction: Vec::new(),
            auto_retry: Vec::new(),
            clear_queue_result: (Vec::new(), Vec::new()),
            queue_update_on_steer: None,
            queue_update_on_follow_up: None,
            queue_update_on_clear: None,
            compact_instructions: Vec::new(),
        }
    }

    fn records(&mut self, call: &str) {
        self.calls.push(call.to_string());
    }
}

struct MockFacade {
    state: Mutex<MockState>,
}

impl MockFacade {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(MockState::new()),
        })
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        lock(&self.state)
    }

    fn on_error(&self, error: ExtensionError) {
        let listener = self
            .state()
            .bindings
            .as_ref()
            .and_then(|bindings| bindings.on_error.clone());
        if let Some(listener) = listener {
            listener(error);
        }
    }

    /// 让下一次/所有 `prompt` 调用永远挂起。
    fn block_prompt(&self, gate: Arc<tokio::sync::Notify>) {
        self.state().prompt_gate = Some(gate);
    }
}

#[async_trait(?Send)]
impl AgentSessionFacade for MockFacade {
    fn agent_session(&self) -> AgentSession {
        panic!("MockFacade overrides every method used by the RPC server")
    }

    fn model(&self) -> Option<Model> {
        self.state().model.clone()
    }

    fn thinking_level(&self) -> ModelThinkingLevel {
        self.state().thinking_level
    }

    fn is_streaming(&self) -> bool {
        self.state().is_streaming
    }

    fn is_compacting(&self) -> bool {
        self.state().is_compacting
    }

    fn steering_mode(&self) -> QueueMode {
        self.state().steering_mode
    }

    fn follow_up_mode(&self) -> QueueMode {
        self.state().follow_up_mode
    }

    fn session_file(&self) -> Option<String> {
        self.state().session_file.clone()
    }

    fn session_id(&self) -> String {
        self.state().session_id.clone()
    }

    fn session_name(&self) -> Option<String> {
        self.state().session_name.clone()
    }

    fn auto_compaction_enabled(&self) -> bool {
        self.state().auto_compaction_enabled
    }

    fn messages(&self) -> Vec<AgentMessage> {
        self.state().messages.clone()
    }

    fn message_count(&self) -> usize {
        self.state().messages.len()
    }

    fn pending_message_count(&self) -> usize {
        self.state().pending_message_count
    }

    fn available_models(&self) -> Vec<Model> {
        self.state().available_models.clone()
    }

    async fn set_model(
        &self,
        model: Model,
        _options: ModelMutationOptions,
    ) -> Result<(), AgentSessionError> {
        let mut state = self.state();
        state.records("set_model");
        if let Some(error) = state.set_model_error.clone() {
            return Err(error);
        }
        state.model = Some(model);
        Ok(())
    }

    async fn cycle_model(&self) -> Result<Option<ModelCycleResult>, AgentSessionError> {
        let mut state = self.state();
        state.records("cycle_model");
        Ok(state.cycle_model_result.clone())
    }

    fn set_thinking_level(&self, level: ModelThinkingLevel, _options: ModelMutationOptions) {
        let mut state = self.state();
        state.records("set_thinking_level");
        state.set_thinking_levels.push(level);
    }

    fn cycle_thinking_level(&self, _options: ModelMutationOptions) -> Option<ModelThinkingLevel> {
        self.state().cycle_thinking_result
    }

    fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        self.state().available_thinking_levels.clone()
    }

    fn set_steering_mode(&self, mode: QueueMode) {
        let mut state = self.state();
        state.records("set_steering_mode");
        state.steering_modes.push(mode);
    }

    fn set_follow_up_mode(&self, mode: QueueMode) {
        let mut state = self.state();
        state.records("set_follow_up_mode");
        state.follow_up_modes.push(mode);
    }

    async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<CompactionResult, AgentSessionError> {
        let mut state = self.state();
        state.records("compact");
        state.compact_instructions.push(custom_instructions);
        match state.compact_result.clone() {
            Some(result) => result,
            None => Ok(CompactionResult {
                summary: "summary".to_string(),
                first_kept_entry_id: "e2".to_string(),
                tokens_before: 100,
                estimated_tokens_after: None,
                usage: None,
                details: None,
            }),
        }
    }

    fn set_auto_compaction_enabled(&self, enabled: bool) {
        let mut state = self.state();
        state.records("set_auto_compaction");
        state.auto_compaction.push(enabled);
    }

    fn set_auto_retry_enabled(&self, enabled: bool) {
        let mut state = self.state();
        state.records("set_auto_retry");
        state.auto_retry.push(enabled);
    }

    fn abort_retry(&self) {
        let mut state = self.state();
        state.records("abort_retry");
        state.abort_retry_count += 1;
    }

    async fn prompt(&self, text: String, options: PromptOptions) -> Result<(), AgentSessionError> {
        let (error, disposition, gate) = {
            let mut state = self.state();
            state.records(&format!("prompt:{text}"));
            state.prompt_images.push(options.images.clone());
            (
                state.prompt_error.clone(),
                state.prompt_disposition,
                state.prompt_gate.clone(),
            )
        };
        if let Some(gate) = gate {
            gate.notified().await;
        }
        if let Some(error) = error {
            return Err(error);
        }
        if let Some(disposition) = disposition
            && let Some(callback) = options.preflight_result.as_ref()
        {
            callback(disposition);
        }
        Ok(())
    }

    async fn steer(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        _source: Option<InputSource>,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        let mut state = self.state();
        state.records(&format!("steer:{text}"));
        state.steer_images.push(images);
        let disposition = state.steer_disposition;
        let queue_update = state.queue_update_on_steer.clone();
        let listener = state.listener.clone();
        drop(state);
        if let (Some(queue_update), Some(listener)) = (queue_update, listener) {
            listener(&AgentSessionEvent::QueueUpdate {
                steering: queue_update.0,
                follow_up: queue_update.1,
            });
        }
        Ok(disposition)
    }

    async fn follow_up(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        _source: Option<InputSource>,
    ) -> Result<QueuedInputDisposition, AgentSessionError> {
        let mut state = self.state();
        state.records(&format!("follow_up:{text}"));
        state.follow_up_images.push(images);
        let disposition = state.follow_up_disposition;
        let queue_update = state.queue_update_on_follow_up.clone();
        let listener = state.listener.clone();
        drop(state);
        if let (Some(queue_update), Some(listener)) = (queue_update, listener) {
            listener(&AgentSessionEvent::QueueUpdate {
                steering: queue_update.0,
                follow_up: queue_update.1,
            });
        }
        Ok(disposition)
    }

    async fn abort(&self) {
        let mut state = self.state();
        state.records("abort");
        state.abort_count += 1;
    }

    async fn wait_for_idle(&self) {
        self.state().records("wait_for_idle");
    }

    fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        let mut state = self.state();
        state.records("clear_queue");
        let result = state.clear_queue_result.clone();
        let queue_update = state.queue_update_on_clear.clone();
        let listener = state.listener.clone();
        drop(state);
        if let (Some(queue_update), Some(listener)) = (queue_update, listener) {
            listener(&AgentSessionEvent::QueueUpdate {
                steering: queue_update.0,
                follow_up: queue_update.1,
            });
        }
        result
    }

    async fn execute_bash(
        &self,
        command: String,
        options: ExecuteBashOptions,
    ) -> Result<BashResult, AgentSessionError> {
        let mut state = self.state();
        state.records(&format!("execute_bash:{command}"));
        state.execute_bash_count += 1;
        let result = state.bash_result.clone();
        // 真实 `AgentSession::execute_bash` 内部会 `recordBashResult`。
        state.recorded_bash = Some((command, result.clone(), options.exclude_from_context));
        Ok(result)
    }

    fn record_bash_result(
        &self,
        command: String,
        result: &BashResult,
        exclude_from_context: Option<bool>,
    ) {
        let mut state = self.state();
        state.records(&format!("record_bash_result:{command}"));
        state.recorded_bash = Some((command, result.clone(), exclude_from_context));
    }

    fn abort_bash(&self) {
        let mut state = self.state();
        state.records("abort_bash");
        state.abort_bash_count += 1;
    }

    fn set_session_name(&self, name: &str) {
        let mut state = self.state();
        state.records(&format!("set_session_name:{name}"));
        state.set_session_names.push(name.to_string());
    }

    fn get_session_stats(&self) -> SessionStats {
        self.state().records("get_session_stats");
        self.state().session_stats.clone()
    }

    async fn export_html(&self, _output_path: Option<String>) -> Result<String, AgentSessionError> {
        self.state().records("export_html");
        Ok(self
            .state()
            .export_path
            .clone()
            .unwrap_or_else(|| "/tmp/export.html".to_string()))
    }

    fn get_user_messages_for_forking(&self) -> Vec<(String, String)> {
        self.state().fork_messages.clone()
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        self.state().last_assistant_text.clone()
    }

    async fn bind_extensions(&self, bindings: SessionExtensionBindings) {
        let hook = {
            let mut state = self.state();
            state.bind_count += 1;
            state.bindings = Some(bindings);
            state.on_bind.clone()
        };
        if let Some(hook) = hook {
            hook().await;
        }
    }

    fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        let mut state = self.state();
        state.records("subscribe");
        state.listener = Some(listener);
        Box::new(|| {})
    }

    fn agent(&self) -> Option<kiana_agent_core::Agent> {
        self.state().agent.clone()
    }

    async fn reload(&self) -> Result<(), AgentSessionError> {
        self.state().records("reload");
        Ok(())
    }

    async fn navigate_tree(
        &self,
        _target_id: String,
        _options: NavigateTreeOptions,
    ) -> Result<TreeNavigationResult, AgentSessionError> {
        self.state().records("navigate_tree");
        Ok(TreeNavigationResult::default())
    }

    fn registered_commands(&self) -> Vec<ResolvedCommand> {
        self.state().registered_commands.clone()
    }

    fn prompt_templates(&self) -> Vec<PromptTemplate> {
        self.state().prompt_templates.clone()
    }

    fn skills(&self) -> Vec<Skill> {
        self.state().skills.clone()
    }

    fn header(&self) -> Option<SessionHeader> {
        None
    }

    fn entries(&self) -> Vec<SessionEntry> {
        self.state().entries.clone()
    }

    fn tree(&self) -> Vec<SessionTreeNode> {
        self.state().tree.clone()
    }

    fn leaf_id(&self) -> Option<String> {
        self.state().leaf_id.clone()
    }

    fn cwd(&self) -> String {
        self.state().cwd.clone()
    }

    async fn emit_user_bash(
        &self,
        event: UserBashEvent,
    ) -> Result<Option<UserBashEventResult>, String> {
        let mut state = self.state();
        state.records(&format!("emit_user_bash:{}", event.command));
        let behavior = state.user_bash_behavior.clone();
        drop(state);
        match behavior {
            UserBashBehavior::Local => Ok(None),
            UserBashBehavior::Throw(message) => {
                self.on_error(ExtensionError {
                    extension_path: "/ext/route.ts".to_string(),
                    event: "user_bash".to_string(),
                    error: message.clone(),
                    stack: Some("stack".to_string()),
                });
                Err(message)
            }
            UserBashBehavior::Invalid => {
                let message = "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object".to_string();
                self.on_error(ExtensionError {
                    extension_path: "/ext/invalid.ts".to_string(),
                    event: "user_bash".to_string(),
                    error: message.clone(),
                    stack: None,
                });
                Err(message)
            }
            UserBashBehavior::Result(result) => Ok(Some(UserBashEventResult::Result(result))),
        }
    }
}

// ============================================================================
// Mock 运行时宿主
// ============================================================================

#[derive(Default)]
struct HostState {
    rebind: Option<Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>>,
    rebind_registrations: usize,
    dispose_count: usize,
    new_session_calls: Vec<Option<String>>,
    switch_session_calls: Vec<String>,
    fork_calls: Vec<(String, Option<ForkPosition>)>,
    new_session_results: VecDeque<Result<SessionReplacementResult, String>>,
    switch_session_results: VecDeque<Result<SessionReplacementResult, String>>,
    fork_results: VecDeque<Result<ForkSessionResult, String>>,
    /// 模拟 runtime 的 `finishSessionReplacement`（非取消时调用 rebind 回调）。
    runtime_calls_rebind: bool,
    /// 设置后 `dispose()` 挂起（模拟卡在 `session_shutdown` 处理里的关停）。
    dispose_gate: Option<Arc<tokio::sync::Notify>>,
}

struct MockHost {
    facade: Arc<dyn AgentSessionFacade>,
    state: Mutex<HostState>,
}

impl MockHost {
    fn new(facade: Arc<dyn AgentSessionFacade>) -> Arc<Self> {
        Arc::new(Self {
            facade,
            state: Mutex::new(HostState {
                runtime_calls_rebind: true,
                ..HostState::default()
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, HostState> {
        lock(&self.state)
    }

    fn queue_new_session(&self, result: Result<SessionReplacementResult, String>) {
        self.state().new_session_results.push_back(result);
    }

    fn queue_fork(&self, result: Result<ForkSessionResult, String>) {
        self.state().fork_results.push_back(result);
    }

    fn queue_switch_session(&self, result: Result<SessionReplacementResult, String>) {
        self.state().switch_session_results.push_back(result);
    }

    /// 让 `dispose()` 挂起。
    fn block_dispose(&self, gate: Arc<tokio::sync::Notify>) {
        self.state().dispose_gate = Some(gate);
    }
}

#[async_trait(?Send)]
impl RpcSessionHost for MockHost {
    fn session(&self) -> Arc<dyn AgentSessionFacade> {
        Arc::clone(&self.facade)
    }

    fn set_rebind_session(
        &self,
        callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    ) {
        let mut state = self.state();
        state.rebind_registrations += 1;
        state.rebind = Some(callback);
    }

    async fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let (result, rebind, runtime_calls_rebind) = {
            let mut state = self.state();
            state
                .new_session_calls
                .push(options.and_then(|options| options.parent_session));
            let result = state
                .new_session_results
                .pop_front()
                .unwrap_or(Ok(SessionReplacementResult { cancelled: false }));
            (result, state.rebind.clone(), state.runtime_calls_rebind)
        };
        if let Ok(result) = &result
            && !result.cancelled
            && runtime_calls_rebind
            && let Some(rebind) = rebind
        {
            rebind().await;
        }
        result
    }

    async fn switch_session(
        &self,
        session_path: String,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let (result, rebind, runtime_calls_rebind) = {
            let mut state = self.state();
            state.switch_session_calls.push(session_path);
            let result = state
                .switch_session_results
                .pop_front()
                .unwrap_or(Ok(SessionReplacementResult { cancelled: false }));
            (result, state.rebind.clone(), state.runtime_calls_rebind)
        };
        if let Ok(result) = &result
            && !result.cancelled
            && runtime_calls_rebind
            && let Some(rebind) = rebind
        {
            rebind().await;
        }
        result
    }

    async fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        let (result, rebind, runtime_calls_rebind) = {
            let mut state = self.state();
            state
                .fork_calls
                .push((entry_id, options.and_then(|options| options.position)));
            let result = state
                .fork_results
                .pop_front()
                .unwrap_or(Ok(ForkSessionResult {
                    cancelled: false,
                    selected_text: None,
                }));
            (result, state.rebind.clone(), state.runtime_calls_rebind)
        };
        if let Ok(result) = &result
            && !result.cancelled
            && runtime_calls_rebind
            && let Some(rebind) = rebind
        {
            rebind().await;
        }
        result
    }

    async fn dispose(&self) {
        self.state().dispose_count += 1;
        let gate = self.state().dispose_gate.clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
    }
}

// ============================================================================
// 测试夹具
// ============================================================================

fn fake_model(provider: &str, id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: "Fake Model".to_string(),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: "https://example.invalid".to_string(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        model_type: None,
        reasoning: Some(false),
        thinking_level_map: None,
        prompt_cache: None,
        context_window: Some(128000),
        max_tokens: Some(4096),
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
        output: None,
    }
}

fn custom_message(text: &str) -> AgentMessage {
    let mut object = indexmap::IndexMap::new();
    object.insert("role".to_string(), Value::String("user".to_string()));
    object.insert("content".to_string(), Value::String(text.to_string()));
    AgentMessage::Custom(Box::new(object))
}

fn source_info(name: &str) -> SourceInfo {
    SourceInfo {
        path: format!("/tmp/{name}"),
        source: "local".to_string(),
        scope: SourceScope::User,
        origin: SourceOrigin::TopLevel,
        base_dir: None,
    }
}

fn session_info_entry(id: &str) -> SessionEntry {
    SessionEntry::SessionInfo(SessionInfoEntry {
        kind: SessionInfoType::Marker,
        id: id.to_string(),
        parent_id: None,
        timestamp: "2024-01-01T00:00:00.000Z".to_string(),
        name: None,
    })
}

fn prompt_template(name: &str) -> PromptTemplate {
    PromptTemplate {
        name: name.to_string(),
        description: "template description".to_string(),
        argument_hint: None,
        content: "body".to_string(),
        source_info: source_info(&format!("{name}.md")),
        file_path: format!("/tmp/{name}.md"),
    }
}

fn skill(name: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: "skill description".to_string(),
        file_path: format!("/tmp/{name}/SKILL.md"),
        base_dir: format!("/tmp/{name}"),
        source_info: source_info(&format!("{name}/SKILL.md")),
        disable_model_invocation: false,
    }
}

fn resolved_command(name: &str) -> ResolvedCommand {
    ResolvedCommand {
        name: name.to_string(),
        source_info: source_info(&format!("{name}.ts")),
        description: Some("command description".to_string()),
        get_argument_completions: None,
        handler: Arc::new(|_args, _ctx| Box::pin(async {})),
        invocation_name: name.to_string(),
    }
}

struct Harness {
    server: Arc<RpcServer>,
    host: Arc<MockHost>,
    facade: Arc<MockFacade>,
    stdout: Arc<CaptureStdout>,
}

fn harness() -> Harness {
    let facade = MockFacade::new();
    let host = MockHost::new(Arc::clone(&facade) as Arc<dyn AgentSessionFacade>);
    let stdout = Arc::new(CaptureStdout::default());
    let server = RpcServer::new(
        Arc::clone(&host) as Arc<dyn RpcSessionHost>,
        Arc::clone(&stdout) as Arc<dyn RawStdout>,
    );
    Harness {
        server,
        host,
        facade,
        stdout,
    }
}

async fn start(harness: &Harness) {
    harness.server.rebind_session().await;
}

/// 让本地任务运行到谓词为真（`LocalSet` 上 `spawn_local` 的任务在挂起点被调度）。
async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for condition"
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/rpc_server/records.json")).expect("fixture JSON")
}

fn fixture_line(name: &str) -> String {
    fixture()[name]
        .as_str()
        .unwrap_or_else(|| panic!("fixture {name} missing"))
        .to_string()
}

async fn send(server: &Arc<RpcServer>, line: &str) {
    server.handle_input_line(line).await;
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("string")
}

// ============================================================================
// 未知命令与解析错误（#5868）
// ============================================================================

#[tokio::test]
async fn unknown_command_preserves_request_id() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"test","type":"foobar"}"#).await;
            let lines = harness.stdout.lines();
            assert_eq!(
                lines,
                vec![format!("{}\n", fixture_line("response_unknown_command"))]
            );
        })
        .await;
}

#[tokio::test]
async fn unknown_command_preserves_non_string_ids() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":7,"type":"nope"}"#).await;
            let record = harness.stdout.responses().pop().expect("response");
            assert_eq!(record["id"], json!(7));
            assert_eq!(record["command"], json!("nope"));
            assert_eq!(record["error"], json!("Unknown command: nope"));

            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":null,"type":"nope"}"#).await;
            let record = harness.stdout.responses().pop().expect("response");
            assert_eq!(record["id"], Value::Null);
            assert_eq!(record["command"], json!("nope"));
        })
        .await;
}

#[tokio::test]
async fn missing_type_omits_command_and_null_type_keeps_it() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"test"}"#).await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!(
                    "{}\n",
                    fixture_line("response_unknown_command_no_type")
                )]
            );

            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"test","type":null}"#).await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!(
                    "{}\n",
                    fixture_line("response_unknown_command_null_type")
                )]
            );
        })
        .await;
}

#[tokio::test]
async fn top_level_primitives_and_null() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            for line in ["42", "\"str\"", "[1,2]", "true"] {
                harness.stdout.lines.lock().unwrap().clear();
                send(&harness.server, line).await;
                assert_eq!(
                    harness.stdout.lines(),
                    vec![format!(
                        "{}\n",
                        fixture_line("response_unknown_command_no_id_no_type")
                    )],
                    "input {line}"
                );
            }

            // 顶层 null：TS 是 unhandled rejection，不写任何记录。
            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, "null").await;
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

#[tokio::test]
async fn parse_errors_match_v8_messages() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let fixture = fixture();
            let cases: Value = serde_json::from_str(
                fixture["parse_error_messages"]
                    .as_str()
                    .expect("parse fixture string"),
            )
            .expect("parse fixture JSON");
            for (input, expected) in cases.as_object().expect("parse cases") {
                let Some(expected) = expected.as_str() else {
                    continue;
                };
                harness.stdout.lines.lock().unwrap().clear();
                // 直接把原始输入作为一行（fixture 的键就是该输入）。
                harness.server.handle_input_line(input).await;
                let record = harness.stdout.records().pop().expect("parse record");
                assert_eq!(record["command"], json!("parse"), "input {input:?}");
                assert_eq!(record.get("id"), None, "input {input:?}");
                assert_eq!(record["error"], json!(expected), "input {input:?}");
            }
        })
        .await;
}

// ============================================================================
// get_state / 模型
// ============================================================================

#[tokio::test]
async fn get_state_uses_documented_field_order() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"s","type":"get_state"}"#).await;
            let record = harness.stdout.response_for("s").expect("response");
            assert_eq!(record["success"], json!(true));
            let data = record["data"].as_object().expect("data object");
            let keys: Vec<&str> = data.keys().map(String::as_str).collect();
            assert_eq!(
                keys,
                vec![
                    "model",
                    "thinkingLevel",
                    "isStreaming",
                    "isCompacting",
                    "steeringMode",
                    "followUpMode",
                    "sessionFile",
                    "sessionId",
                    "sessionName",
                    "autoCompactionEnabled",
                    "messageCount",
                    "pendingMessageCount",
                ]
            );
            assert_eq!(data["thinkingLevel"], json!("high"));
            assert_eq!(data["steeringMode"], json!("all"));
            assert_eq!(data["followUpMode"], json!("one-at-a-time"));
            assert_eq!(data["messageCount"], json!(2));
            assert_eq!(data["pendingMessageCount"], json!(1));
            assert_eq!(data["model"]["id"], json!("fake-model"));
        })
        .await;
}

#[tokio::test]
async fn get_state_omits_undefined_optional_fields() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.model = None;
                state.session_file = None;
                state.session_name = None;
            }
            start(&harness).await;
            send(&harness.server, r#"{"id":"s","type":"get_state"}"#).await;
            let record = harness.stdout.response_for("s").expect("response");
            let data = record["data"].as_object().expect("data object");
            assert!(!data.contains_key("model"));
            assert!(!data.contains_key("sessionFile"));
            assert!(!data.contains_key("sessionName"));
        })
        .await;
}

/// 评审 finding 4：TS 的 setter 不校验，`get_state` 原样回显最近一次收到的值
/// （`"bogus"`、数字、`"false"`、0；`undefined` 时省略 steering/followUp 键）。
/// 队列行为仍按 `mode === "all"` 判定（其它值等价 one-at-a-time）。
#[tokio::test]
async fn get_state_echoes_raw_setter_values_like_ts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"m1","type":"set_steering_mode","mode":"bogus"}"#,
            )
            .await;
            assert_eq!(
                harness.facade.state().steering_modes.last().copied(),
                Some(QueueMode::OneAtATime),
                "only \"all\" selects All"
            );
            send(&harness.server, r#"{"id":"g1","type":"get_state"}"#).await;
            let data = harness.stdout.response_for("g1").unwrap()["data"].clone();
            assert_eq!(data["steeringMode"], json!("bogus"));

            send(
                &harness.server,
                r#"{"id":"m2","type":"set_follow_up_mode","mode":5}"#,
            )
            .await;
            assert_eq!(
                harness.facade.state().follow_up_modes.last().copied(),
                Some(QueueMode::OneAtATime)
            );
            send(&harness.server, r#"{"id":"g2","type":"get_state"}"#).await;
            let data = harness.stdout.response_for("g2").unwrap()["data"].clone();
            assert_eq!(data["followUpMode"], json!(5));

            // 缺失 mode -> TS 存 undefined，`JSON.stringify` 省略该键。
            send(&harness.server, r#"{"id":"m3","type":"set_steering_mode"}"#).await;
            send(&harness.server, r#"{"id":"g3","type":"get_state"}"#).await;
            let data = harness.stdout.response_for("g3").unwrap()["data"].clone();
            assert!(data.as_object().unwrap().get("steeringMode").is_none());
            assert_eq!(data["followUpMode"], json!(5));

            // autoCompactionEnabled：getter 是 `raw ?? true`。
            send(
                &harness.server,
                r#"{"id":"a1","type":"set_auto_compaction","enabled":0}"#,
            )
            .await;
            send(&harness.server, r#"{"id":"g4","type":"get_state"}"#).await;
            let data = harness.stdout.response_for("g4").unwrap()["data"].clone();
            assert_eq!(data["autoCompactionEnabled"], json!(0));
            assert_eq!(harness.facade.state().auto_compaction.last(), Some(&false));

            send(
                &harness.server,
                r#"{"id":"a2","type":"set_auto_compaction","enabled":"false"}"#,
            )
            .await;
            send(&harness.server, r#"{"id":"g5","type":"get_state"}"#).await;
            let data = harness.stdout.response_for("g5").unwrap()["data"].clone();
            assert_eq!(data["autoCompactionEnabled"], json!("false"));
            assert_eq!(
                harness.facade.state().auto_compaction.last(),
                Some(&true),
                "\"false\" is truthy in JS"
            );

            for (id, enabled, expected) in [("a3", "null", true), ("a4", "null", true)] {
                harness.stdout.lines.lock().unwrap().clear();
                let body = if id == "a3" {
                    r#"{"id":"a3","type":"set_auto_compaction"}"#.to_string()
                } else {
                    format!(r#"{{"id":"{id}","type":"set_auto_compaction","enabled":{enabled}}}"#)
                };
                send(&harness.server, &body).await;
                send(
                    &harness.server,
                    &format!(r#"{{"id":"g-{id}","type":"get_state"}}"#),
                )
                .await;
                let data = harness.stdout.response_for(&format!("g-{id}")).unwrap()["data"].clone();
                assert_eq!(data["autoCompactionEnabled"], json!(expected), "case {id}");
            }
        })
        .await;
}

#[tokio::test]
async fn set_model_reports_command_and_session_level_errors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"m","type":"set_model","provider":"invalid","modelId":"x"}"#,
            )
            .await;
            let record = harness.stdout.response_for("m").expect("response");
            assert_eq!(record["success"], json!(false));
            assert_eq!(record["error"], json!("Model not found: invalid/x"));

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.set_model_error = Some(AgentSessionError::NoApiKey {
                    provider: "fake".to_string(),
                });
            }
            send(
                &harness.server,
                r#"{"id":"m2","type":"set_model","provider":"fake","modelId":"fake-model"}"#,
            )
            .await;
            let record = harness.stdout.response_for("m2").expect("response");
            assert_eq!(record["success"], json!(false));
            assert_eq!(
                record["error"],
                json!(
                    AgentSessionError::NoApiKey {
                        provider: "fake".to_string()
                    }
                    .to_string()
                )
            );

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.set_model_error = None;
            }
            send(
                &harness.server,
                r#"{"id":"m3","type":"set_model","provider":"fake","modelId":"fake-model"}"#,
            )
            .await;
            let record = harness.stdout.response_for("m3").expect("response");
            assert_eq!(record["success"], json!(true));
            assert_eq!(record["data"]["id"], json!("fake-model"));
        })
        .await;
}

#[tokio::test]
async fn set_model_missing_fields_use_js_string_in_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"m","type":"set_model"}"#).await;
            let record = harness.stdout.response_for("m").expect("response");
            assert_eq!(
                record["error"],
                json!("Model not found: undefined/undefined")
            );
        })
        .await;
}

#[tokio::test]
async fn cycle_model_and_available_models() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(&harness.server, r#"{"id":"c","type":"cycle_model"}"#).await;
            let record = harness.stdout.response_for("c").expect("response");
            assert_eq!(record["success"], json!(true));
            assert_eq!(record["data"], Value::Null);

            // `data:null` 必须**逐字节**存在（不是“data 键缺失”）。
            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"req-1","type":"cycle_model"}"#).await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("response_success_data_null"))]
            );

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.cycle_model_result = Some(ModelCycleResult {
                    model: fake_model("fake", "next-model"),
                    thinking_level: ModelThinkingLevel::Low,
                    is_scoped: true,
                });
            }
            send(&harness.server, r#"{"id":"c2","type":"cycle_model"}"#).await;
            let record = harness.stdout.response_for("c2").expect("response");
            let data = record["data"].as_object().expect("data");
            let keys: Vec<&str> = data.keys().map(String::as_str).collect();
            assert_eq!(keys, vec!["model", "thinkingLevel", "isScoped"]);
            assert_eq!(data["model"]["id"], json!("next-model"));
            assert_eq!(data["thinkingLevel"], json!("low"));
            assert_eq!(data["isScoped"], json!(true));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"a","type":"get_available_models"}"#,
            )
            .await;
            let record = harness.stdout.response_for("a").expect("response");
            assert_eq!(record["data"]["models"][0]["provider"], json!("fake"));
        })
        .await;
}

// ============================================================================
// thinking / queue / compaction / retry
// ============================================================================

#[tokio::test]
async fn thinking_level_commands() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"1","type":"set_thinking_level","level":"max"}"#,
            )
            .await;
            let record = harness.stdout.response_for("1").expect("response");
            assert_eq!(record["success"], json!(true));
            assert_eq!(record.get("data"), None);
            assert_eq!(
                harness.facade.state().set_thinking_levels,
                vec![ModelThinkingLevel::Max]
            );

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"2","type":"set_thinking_level","level":5}"#,
            )
            .await;
            assert_eq!(
                harness.facade.state().set_thinking_levels,
                vec![ModelThinkingLevel::Max, ModelThinkingLevel::Off]
            );

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"3","type":"cycle_thinking_level"}"#,
            )
            .await;
            let record = harness.stdout.response_for("3").expect("response");
            assert_eq!(record["data"], Value::Null);

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.cycle_thinking_result = Some(ModelThinkingLevel::Medium);
            }
            send(
                &harness.server,
                r#"{"id":"4","type":"cycle_thinking_level"}"#,
            )
            .await;
            let record = harness.stdout.response_for("4").expect("response");
            assert_eq!(record["data"], json!({"level": "medium"}));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"5","type":"get_available_thinking_levels"}"#,
            )
            .await;
            let record = harness.stdout.response_for("5").expect("response");
            assert_eq!(record["data"], json!({"levels": ["off", "high"]}));
        })
        .await;
}

#[tokio::test]
async fn queue_mode_compaction_and_retry_commands() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"q1","type":"set_steering_mode","mode":"one-at-a-time"}"#,
            )
            .await;
            send(
                &harness.server,
                r#"{"id":"q2","type":"set_follow_up_mode","mode":"all"}"#,
            )
            .await;
            {
                let state = harness.facade.state();
                assert_eq!(state.steering_modes, vec![QueueMode::OneAtATime]);
                assert_eq!(state.follow_up_modes, vec![QueueMode::All]);
            }

            send(&harness.server, r#"{"id":"c","type":"compact"}"#).await;
            let record = harness.stdout.response_for("c").expect("response");
            assert_eq!(
                record["data"],
                json!({
                    "summary": "summary",
                    "firstKeptEntryId": "e2",
                    "tokensBefore": 100
                })
            );

            send(
                &harness.server,
                r#"{"id":"ac","type":"set_auto_compaction","enabled":true}"#,
            )
            .await;
            send(
                &harness.server,
                r#"{"id":"ar","type":"set_auto_retry","enabled":false}"#,
            )
            .await;
            send(&harness.server, r#"{"id":"br","type":"abort_retry"}"#).await;
            let state = harness.facade.state();
            assert_eq!(state.auto_compaction, vec![true]);
            assert_eq!(state.auto_retry, vec![false]);
            assert_eq!(state.abort_retry_count, 1);
        })
        .await;
}

#[tokio::test]
async fn clear_queue_returns_both_queues() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.clear_queue_result = (
                    vec!["Change direction".to_string()],
                    vec!["Summarize when finished".to_string()],
                );
            }
            start(&harness).await;
            send(&harness.server, r#"{"id":"c","type":"clear_queue"}"#).await;
            let record = harness.stdout.response_for("c").expect("response");
            assert_eq!(
                record["data"],
                json!({
                    "steering": ["Change direction"],
                    "followUp": ["Summarize when finished"]
                })
            );
            let data = record["data"].as_object().expect("data");
            let keys: Vec<&str> = data.keys().map(String::as_str).collect();
            assert_eq!(keys, vec!["steering", "followUp"]);
        })
        .await;
}

// ============================================================================
// prompt / steer / follow_up
// ============================================================================

#[tokio::test]
async fn prompt_preflight_success_emits_one_started_response() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"b2","type":"prompt","message":"Hello"}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("b2").is_some()).await;
            let responses: Vec<Value> = harness
                .stdout
                .responses()
                .into_iter()
                .filter(|record| record["id"] == json!("b2"))
                .collect();
            assert_eq!(responses.len(), 1);
            assert_eq!(
                responses[0],
                json!({
                    "id": "b2",
                    "type": "response",
                    "command": "prompt",
                    "success": true,
                    "data": {"disposition": "started"}
                })
            );
        })
        .await;
}

#[tokio::test]
async fn prompt_preflight_failure_emits_one_error_response() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.prompt_disposition = None;
                state.prompt_error = Some(AgentSessionError::NoApiKey {
                    provider: "fake-provider".to_string(),
                });
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"b1","type":"prompt","message":"Hello"}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("b1").is_some()).await;
            let responses: Vec<Value> = harness
                .stdout
                .responses()
                .into_iter()
                .filter(|record| record["id"] == json!("b1"))
                .collect();
            assert_eq!(responses.len(), 1);
            assert_eq!(responses[0]["success"], json!(false));
            let error = responses[0]["error"].as_str().expect("error text");
            assert!(error.contains(
                "No API key found for fake-provider.\n\nUse /login to log into a provider via OAuth or API key. See:"
            ));
        })
        .await;
}

#[tokio::test]
async fn prompt_failure_after_preflight_emits_no_second_response() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.prompt_disposition = Some(PromptDisposition::Started);
                state.prompt_error = Some(AgentSessionError::other("late failure"));
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"b","type":"prompt","message":"Hello"}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("b").is_some()).await;
            // 让可能的第二个响应有机会写出。
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            let responses = harness
                .stdout
                .responses()
                .into_iter()
                .filter(|record| record["id"] == json!("b"))
                .count();
            assert_eq!(responses, 1);
        })
        .await;
}

#[tokio::test]
async fn prompt_during_compaction_reports_error_without_preflight() {
    // #7150
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.prompt_disposition = None;
                state.prompt_error = Some(AgentSessionError::CompactionInProgress);
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"probe","type":"prompt","message":"PROBE-7150"}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("probe").is_some()).await;
            let record = harness.stdout.response_for("probe").expect("response");
            assert_eq!(record["success"], json!(false));
            let error = record["error"].as_str().expect("error");
            assert!(error.contains("compaction is in progress"), "{error}");
        })
        .await;
}

#[tokio::test]
async fn prompt_handled_and_queued_dispositions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.prompt_disposition = Some(PromptDisposition::Handled);
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"h","type":"prompt","message":"/handled"}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("h").is_some()).await;
            assert_eq!(
                harness.stdout.response_for("h").unwrap()["data"],
                json!({"disposition": "handled"})
            );
        })
        .await;
}

/// TS `rpc-prompt-response-semantics.test.ts`：streaming 期间排队的 prompt 只发一个
/// 成功响应（`data.disposition === "queued"`），preflight 成功后不再发第二个错误。
#[tokio::test]
async fn prompt_queued_during_streaming_emits_one_success_response() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.is_streaming = true;
                state.prompt_disposition = Some(PromptDisposition::Queued);
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"b3","type":"prompt","message":"Queue this","streamingBehavior":"followUp"}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("b3").is_some()).await;
            // 让可能的第二个响应有机会写出。
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            let responses: Vec<Value> = harness
                .stdout
                .responses()
                .into_iter()
                .filter(|record| record["id"] == json!("b3"))
                .collect();
            assert_eq!(responses.len(), 1, "{responses:?}");
            assert_eq!(responses[0]["type"], json!("response"));
            assert_eq!(responses[0]["command"], json!("prompt"));
            assert_eq!(responses[0]["success"], json!(true));
            assert_eq!(responses[0]["data"], json!({"disposition": "queued"}));
        })
        .await;
}

#[tokio::test]
async fn steer_and_follow_up_report_dispositions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.steer_disposition = QueuedInputDisposition::Handled;
                state.follow_up_disposition = QueuedInputDisposition::Queued;
            }
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"A","type":"steer","message":"A"}"#,
            )
            .await;
            let record = harness.stdout.response_for("A").expect("response");
            assert_eq!(record["command"], json!("steer"));
            assert_eq!(record["data"], json!({"disposition": "handled"}));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"B","type":"follow_up","message":"B"}"#,
            )
            .await;
            let record = harness.stdout.response_for("B").expect("response");
            assert_eq!(record["command"], json!("follow_up"));
            assert_eq!(record["data"], json!({"disposition": "queued"}));
        })
        .await;
}

#[tokio::test]
async fn steer_queue_update_is_written_before_the_response() {
    // #9803：input handler 消费 A 的同时排队 B；`queue_update` 先于 response 写出。
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.steer_disposition = QueuedInputDisposition::Handled;
                state.queue_update_on_steer = Some((vec!["B".to_string()], Vec::new()));
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"A","type":"steer","message":"A"}"#,
            )
            .await;

            let lines = harness.stdout.lines();
            let records: Vec<Value> = lines
                .iter()
                .flat_map(|line| line.split('\n'))
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("record"))
                .collect();
            let queue_index = records
                .iter()
                .position(|record| record["type"] == json!("queue_update"))
                .expect("queue_update");
            let response_index = records
                .iter()
                .position(|record| record["id"] == json!("A"))
                .expect("response");
            assert!(queue_index < response_index);
            assert_eq!(
                records[queue_index],
                json!({"type": "queue_update", "steering": ["B"], "followUp": []})
            );
            assert_eq!(
                records[response_index]["data"],
                json!({"disposition": "handled"})
            );
            assert_eq!(
                records
                    .iter()
                    .filter(
                        |record| record["type"] == json!("response") && record["id"] == json!("A")
                    )
                    .count(),
                1
            );
        })
        .await;
}

#[tokio::test]
async fn follow_up_queue_update_is_written_before_the_response() {
    // #9803 的 `it.each(["steer", "follow_up"])`：follow_up 分支同样先写 `queue_update`。
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.follow_up_disposition = QueuedInputDisposition::Handled;
                state.queue_update_on_follow_up = Some((Vec::new(), vec!["B".to_string()]));
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"A","type":"follow_up","message":"A"}"#,
            )
            .await;

            let lines = harness.stdout.lines();
            let records: Vec<Value> = lines
                .iter()
                .flat_map(|line| line.split('\n'))
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("record"))
                .collect();
            let queue_index = records
                .iter()
                .position(|record| record["type"] == json!("queue_update"))
                .expect("queue_update");
            let response_index = records
                .iter()
                .position(|record| record["id"] == json!("A"))
                .expect("response");
            assert!(queue_index < response_index);
            assert_eq!(
                records[queue_index],
                json!({"type": "queue_update", "steering": [], "followUp": ["B"]})
            );
            assert_eq!(
                records[response_index]["data"],
                json!({"disposition": "handled"})
            );
            assert_eq!(
                records
                    .iter()
                    .filter(
                        |record| record["type"] == json!("response") && record["id"] == json!("A")
                    )
                    .count(),
                1
            );
        })
        .await;
}

#[tokio::test]
async fn steer_missing_message_reports_v8_type_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"A","type":"steer"}"#).await;
            let record = harness.stdout.response_for("A").expect("response");
            assert_eq!(record["success"], json!(false));
            assert_eq!(
                record["error"],
                json!("Cannot read properties of undefined (reading 'startsWith')")
            );
        })
        .await;
}

// ============================================================================
// 会话命令
// ============================================================================

#[tokio::test]
async fn get_entries_since_edge_cases_and_leaf_id() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(&harness.server, r#"{"id":"e","type":"get_entries"}"#).await;
            let record = harness.stdout.response_for("e").expect("response");
            let data = record["data"].as_object().expect("data");
            let keys: Vec<&str> = data.keys().map(String::as_str).collect();
            assert_eq!(keys, vec!["entries", "leafId"]);
            assert_eq!(data["entries"].as_array().unwrap().len(), 3);
            assert_eq!(data["leafId"], json!("e3"));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"e2","type":"get_entries","since":"e1"}"#,
            )
            .await;
            let record = harness.stdout.response_for("e2").expect("response");
            let entries = record["data"]["entries"].as_array().expect("entries");
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0]["id"], json!("e2"));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"e3","type":"get_entries","since":"missing"}"#,
            )
            .await;
            let record = harness.stdout.response_for("e3").expect("response");
            assert_eq!(record["success"], json!(false));
            assert_eq!(record["error"], json!("Entry not found: missing"));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"e4","type":"get_entries","since":null}"#,
            )
            .await;
            let record = harness.stdout.response_for("e4").expect("response");
            assert_eq!(record["error"], json!("Entry not found: null"));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"e5","type":"get_entries","since":""}"#,
            )
            .await;
            let record = harness.stdout.response_for("e5").expect("response");
            assert_eq!(record["error"], json!("Entry not found: "));

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.leaf_id = None;
            }
            send(&harness.server, r#"{"id":"e6","type":"get_entries"}"#).await;
            let record = harness.stdout.response_for("e6").expect("response");
            assert_eq!(record["data"]["leafId"], Value::Null);
            assert!(record["data"].as_object().unwrap().contains_key("leafId"));
        })
        .await;
}

#[tokio::test]
async fn get_tree_nests_children_with_label_fields() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"t","type":"get_tree"}"#).await;
            let record = harness.stdout.response_for("t").expect("response");
            let data = record["data"].as_object().expect("data");
            let keys: Vec<&str> = data.keys().map(String::as_str).collect();
            assert_eq!(keys, vec!["tree", "leafId"]);
            assert_eq!(data["leafId"], json!("e3"));
            let node = &data["tree"][0];
            let node_keys: Vec<&str> = node
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(node_keys, vec!["entry", "children"]);
            assert_eq!(node["children"][0]["label"], json!("branch"));
            assert_eq!(
                node["children"][0]["labelTimestamp"],
                json!("2024-01-01T00:00:00.000Z")
            );
        })
        .await;
}

#[tokio::test]
async fn get_last_assistant_text_data_shapes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"a","type":"get_last_assistant_text"}"#,
            )
            .await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("last_assistant_text_empty"))]
            );

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.last_assistant_text = Some("hi".to_string());
            }
            send(
                &harness.server,
                r#"{"id":"a","type":"get_last_assistant_text"}"#,
            )
            .await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("last_assistant_text_value"))]
            );
        })
        .await;
}

#[tokio::test]
async fn set_session_name_trims_and_rejects_empty() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                &format!(
                    "{{\"id\":\"n\",\"type\":\"set_session_name\",\"name\":{}}}",
                    json_string("\u{feff}  \t\n")
                ),
            )
            .await;
            let record = harness.stdout.response_for("n").expect("response");
            assert_eq!(record["error"], json!("Session name cannot be empty"));

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                &format!(
                    "{{\"id\":\"n2\",\"type\":\"set_session_name\",\"name\":{}}}",
                    json_string("  hello \u{feff}")
                ),
            )
            .await;
            let record = harness.stdout.response_for("n2").expect("response");
            assert_eq!(record["success"], json!(true));
            assert_eq!(
                harness.facade.state().set_session_names,
                vec!["hello".to_string()]
            );

            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"n3","type":"set_session_name"}"#).await;
            let record = harness.stdout.response_for("n3").expect("response");
            assert_eq!(
                record["error"],
                json!("Cannot read properties of undefined (reading 'trim')")
            );
        })
        .await;
}

#[tokio::test]
async fn get_messages_and_fork_messages() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(&harness.server, r#"{"id":"m","type":"get_messages"}"#).await;
            let record = harness.stdout.response_for("m").expect("response");
            assert_eq!(record["data"]["messages"].as_array().unwrap().len(), 2);

            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"f","type":"get_fork_messages"}"#).await;
            let record = harness.stdout.response_for("f").expect("response");
            let messages = record["data"]["messages"].as_array().expect("messages");
            let keys: Vec<&str> = messages[0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, vec!["entryId", "text"]);
            assert_eq!(messages[0]["entryId"], json!("e1"));
            assert_eq!(messages[1]["text"], json!("world"));
        })
        .await;
}

#[tokio::test]
async fn get_commands_orders_extension_prompt_skill() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(&harness.server, r#"{"id":"c","type":"get_commands"}"#).await;
            let record = harness.stdout.response_for("c").expect("response");
            let commands = record["data"]["commands"].as_array().expect("commands");
            assert_eq!(commands.len(), 3);
            assert_eq!(commands[0]["name"], json!("cmd"));
            assert_eq!(commands[0]["source"], json!("extension"));
            assert_eq!(commands[1]["name"], json!("template"));
            assert_eq!(commands[1]["source"], json!("prompt"));
            assert_eq!(commands[2]["name"], json!("skill:myskill"));
            assert_eq!(commands[2]["source"], json!("skill"));
            let keys: Vec<&str> = commands[0]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, vec!["name", "description", "source", "sourceInfo"]);
            let source_keys: Vec<&str> = commands[0]["sourceInfo"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(source_keys, vec!["path", "source", "scope", "origin"]);
        })
        .await;
}

#[tokio::test]
async fn session_stats_export_html_and_abort_commands() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(&harness.server, r#"{"id":"s","type":"get_session_stats"}"#).await;
            let record = harness.stdout.response_for("s").expect("response");
            assert_eq!(record["data"]["userMessages"], json!(1));
            assert_eq!(record["data"]["tokens"]["total"], json!(30));

            send(&harness.server, r#"{"id":"e","type":"export_html"}"#).await;
            let record = harness.stdout.response_for("e").expect("response");
            assert_eq!(record["data"], json!({"path": "/tmp/export.html"}));

            send(&harness.server, r#"{"id":"a","type":"abort"}"#).await;
            let record = harness.stdout.response_for("a").expect("response");
            assert_eq!(record.get("data"), None);
            assert_eq!(harness.facade.state().abort_count, 1);

            send(&harness.server, r#"{"id":"b","type":"abort_bash"}"#).await;
            assert_eq!(harness.facade.state().abort_bash_count, 1);
        })
        .await;
}

// ============================================================================
// 会话替换（双 rebind）
// ============================================================================

#[tokio::test]
async fn new_session_rebinds_twice() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            assert_eq!(harness.facade.state().bind_count, 1);

            send(
                &harness.server,
                r#"{"id":"n","type":"new_session","parentSession":"/tmp/parent.jsonl"}"#,
            )
            .await;
            let record = harness.stdout.response_for("n").expect("response");
            assert_eq!(record["data"], json!({"cancelled": false}));
            // 初始 1 次 + runtime 内部 1 次 + 服务端 1 次。
            assert_eq!(harness.facade.state().bind_count, 3);
            assert_eq!(
                harness.host.state().new_session_calls,
                vec![Some("/tmp/parent.jsonl".to_string())]
            );
        })
        .await;
}

#[tokio::test]
async fn cancelled_session_operations_do_not_rebind() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            harness
                .host
                .queue_new_session(Ok(SessionReplacementResult { cancelled: true }));
            harness.host.queue_fork(Ok(ForkSessionResult {
                cancelled: true,
                selected_text: None,
            }));
            harness
                .host
                .queue_switch_session(Ok(SessionReplacementResult { cancelled: true }));

            send(&harness.server, r#"{"id":"n","type":"new_session"}"#).await;
            let record = harness.stdout.response_for("n").expect("response");
            assert_eq!(record["data"], json!({"cancelled": true}));
            assert_eq!(harness.facade.state().bind_count, 1);

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"s","type":"switch_session","sessionPath":"/tmp/s.jsonl"}"#,
            )
            .await;
            let record = harness.stdout.response_for("s").expect("response");
            assert_eq!(record["data"], json!({"cancelled": true}));
            assert_eq!(
                harness.host.state().switch_session_calls,
                vec!["/tmp/s.jsonl".to_string()]
            );
            assert_eq!(harness.facade.state().bind_count, 1);
        })
        .await;
}

#[tokio::test]
async fn new_session_empty_parent_is_treated_as_absent() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"n","type":"new_session","parentSession":""}"#,
            )
            .await;
            assert_eq!(harness.host.state().new_session_calls, vec![None]);
        })
        .await;
}

#[tokio::test]
async fn fork_text_before_cancelled_and_omission() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            harness.host.queue_fork(Ok(ForkSessionResult {
                cancelled: false,
                selected_text: Some("hello".to_string()),
            }));
            send(
                &harness.server,
                r#"{"id":"f","type":"fork","entryId":"e1"}"#,
            )
            .await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("fork_with_text"))]
            );

            harness.stdout.lines.lock().unwrap().clear();
            harness.host.queue_fork(Ok(ForkSessionResult {
                cancelled: true,
                selected_text: None,
            }));
            send(
                &harness.server,
                r#"{"id":"f","type":"fork","entryId":"e1"}"#,
            )
            .await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("fork_without_text"))]
            );

            harness.stdout.lines.lock().unwrap().clear();
            harness.host.queue_fork(Ok(ForkSessionResult {
                cancelled: false,
                selected_text: Some(String::new()),
            }));
            send(
                &harness.server,
                r#"{"id":"f","type":"fork","entryId":"e1"}"#,
            )
            .await;
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("fork_empty_text"))]
            );
        })
        .await;
}

#[tokio::test]
async fn clone_without_leaf_fails_and_with_leaf_uses_at_position() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.leaf_id = None;
            }
            start(&harness).await;
            send(&harness.server, r#"{"id":"c","type":"clone"}"#).await;
            let record = harness.stdout.response_for("c").expect("response");
            assert_eq!(
                record["error"],
                json!("Cannot clone session: no current entry selected")
            );

            harness.stdout.lines.lock().unwrap().clear();
            {
                let mut state = harness.facade.state();
                state.leaf_id = Some("e2".to_string());
            }
            harness.host.queue_fork(Ok(ForkSessionResult {
                cancelled: false,
                selected_text: None,
            }));
            send(&harness.server, r#"{"id":"c2","type":"clone"}"#).await;
            let record = harness.stdout.response_for("c2").expect("response");
            assert_eq!(record["data"], json!({"cancelled": false}));
            assert_eq!(
                harness.host.state().fork_calls,
                vec![("e2".to_string(), Some(ForkPosition::At))]
            );
        })
        .await;
}

#[tokio::test]
async fn fork_error_texts_are_passed_through() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            harness
                .host
                .queue_fork(Err("Invalid entry ID for forking".to_string()));
            send(
                &harness.server,
                r#"{"id":"f","type":"fork","entryId":"bad"}"#,
            )
            .await;
            let record = harness.stdout.response_for("f").expect("response");
            assert_eq!(record["command"], json!("fork"));
            assert_eq!(record["error"], json!("Invalid entry ID for forking"));
        })
        .await;
}

// ============================================================================
// bash（#9068）
// ============================================================================

#[tokio::test]
async fn bash_fails_closed_when_user_bash_handler_throws() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.user_bash_behavior = UserBashBehavior::Throw("Routing failed".to_string());
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"bash-request","type":"bash","command":"pwd"}"#,
            )
            .await;
            let record = harness
                .stdout
                .response_for("bash-request")
                .expect("response");
            assert_eq!(record["success"], json!(false));
            assert!(record["error"].as_str().unwrap().contains("Routing failed"));
            let records = harness.stdout.records();
            let extension_error = records
                .iter()
                .find(|record| record["type"] == json!("extension_error"))
                .expect("extension_error record");
            assert_eq!(extension_error["event"], json!("user_bash"));
            assert!(
                extension_error["error"]
                    .as_str()
                    .unwrap()
                    .contains("Routing failed")
            );
            assert_eq!(extension_error.get("stack"), None);
            assert_eq!(harness.facade.state().execute_bash_count, 0);
        })
        .await;
}

#[tokio::test]
async fn bash_fails_closed_on_invalid_user_bash_result() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.user_bash_behavior = UserBashBehavior::Invalid;
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"bash-request","type":"bash","command":"pwd"}"#,
            )
            .await;
            let record = harness
                .stdout
                .response_for("bash-request")
                .expect("response");
            assert_eq!(record["success"], json!(false));
            assert!(
                record["error"]
                    .as_str()
                    .unwrap()
                    .contains("Invalid user_bash handler result")
            );
            // #9068 的 RPC 用例对两行失败都断言 extension_error 记录。
            let records = harness.stdout.records();
            let extension_error = records
                .iter()
                .find(|record| record["type"] == json!("extension_error"))
                .expect("extension_error record");
            assert_eq!(extension_error["event"], json!("user_bash"));
            assert!(
                extension_error["error"]
                    .as_str()
                    .unwrap()
                    .contains("Invalid user_bash handler result")
            );
            assert_eq!(extension_error.get("stack"), None);
            assert_eq!(harness.facade.state().execute_bash_count, 0);
        })
        .await;
}

#[tokio::test]
async fn bash_executes_locally_when_user_bash_returns_undefined() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.user_bash_behavior = UserBashBehavior::Local;
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"bash-request","type":"bash","command":"pwd"}"#,
            )
            .await;
            let record = harness
                .stdout
                .response_for("bash-request")
                .expect("response");
            assert_eq!(record["success"], json!(true));
            assert_eq!(
                record["data"],
                json!({
                    "output": "local output",
                    "exitCode": 0,
                    "cancelled": false,
                    "truncated": false
                })
            );
            assert_eq!(harness.facade.state().execute_bash_count, 1);
            assert_eq!(
                harness.facade.state().recorded_bash.as_ref().unwrap().0,
                "pwd"
            );
        })
        .await;
}

#[tokio::test]
async fn bash_uses_extension_result_without_executing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.user_bash_behavior = UserBashBehavior::Result(BashResult {
                    output: "handled output".to_string(),
                    exit_code: None,
                    cancelled: true,
                    truncated: false,
                    full_output_path: Some("/tmp/full.txt".to_string()),
                    execution_status: None,
                });
            }
            start(&harness).await;
            send(
                &harness.server,
                r#"{"id":"b","type":"bash","command":"pwd","excludeFromContext":true}"#,
            )
            .await;
            let record = harness.stdout.response_for("b").expect("response");
            assert_eq!(
                record["data"],
                json!({
                    "output": "handled output",
                    "cancelled": true,
                    "truncated": false,
                    "fullOutputPath": "/tmp/full.txt"
                })
            );
            assert_eq!(record["data"].get("exitCode"), None);
            assert_eq!(harness.facade.state().execute_bash_count, 0);
            let state = harness.facade.state();
            let (command, _, exclude) = state.recorded_bash.as_ref().expect("recorded");
            assert_eq!(command, "pwd");
            assert_eq!(*exclude, Some(true));
        })
        .await;
}

// ============================================================================
// 生命周期 / 关停
// ============================================================================

#[tokio::test]
async fn extension_error_listener_writes_record() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let bindings = harness.facade.state().bindings.clone().expect("bindings");
            let listener = bindings.on_error.clone().expect("on_error");
            listener(ExtensionError {
                extension_path: "/ext.ts".to_string(),
                event: "user_bash".to_string(),
                error: "boom".to_string(),
                stack: Some("ignored".to_string()),
            });
            assert_eq!(
                harness.stdout.lines(),
                vec![format!("{}\n", fixture_line("extension_error"))]
            );
        })
        .await;
}

#[tokio::test]
async fn shutdown_handler_is_gated_on_agent_settled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let bindings = harness.facade.state().bindings.clone().expect("bindings");
            let handler = bindings.shutdown_handler.clone().expect("shutdown handler");
            handler();

            assert!(harness.server.check_shutdown_requested().await.is_some());
            assert_eq!(harness.host.state().dispose_count, 1);
            assert_eq!(harness.stdout.flush_count(), 1);
        })
        .await;
}

#[tokio::test]
async fn agent_settled_event_triggers_shutdown_check() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let bindings = harness.facade.state().bindings.clone().expect("bindings");
            let shutdown = bindings.shutdown_handler.clone().expect("handler");
            let listener = harness.facade.state().listener.clone().expect("listener");
            shutdown();
            listener(&AgentSessionEvent::AgentSettled { aborted: false });
            wait_until(|| harness.host.state().dispose_count == 1).await;
            assert_eq!(harness.stdout.flush_count(), 1);
            assert!(
                harness
                    .stdout
                    .records()
                    .iter()
                    .any(|record| record["type"] == json!("agent_settled"))
            );
        })
        .await;
}

/// 评审 finding 2（RPC 侧）：投影失败后该 run 的后续事件不再写出（TS 的 run 已失败结束），
/// 到真实的 `agent_settled` 恢复并照常检查 `shutdownHandler`。
#[tokio::test]
async fn projection_failure_suppresses_run_events_until_agent_settled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let bindings = harness.facade.state().bindings.clone().expect("bindings");
            let shutdown = bindings.shutdown_handler.clone().expect("handler");
            let listener = harness.facade.state().listener.clone().expect("listener");
            shutdown();

            listener(&AgentSessionEvent::AgentStart);
            // `message_update` 的 message 不是 assistant -> `toJsonEvent` 抛错。
            listener(&AgentSessionEvent::MessageUpdate {
                message: AgentMessage::Standard(Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text("hi".to_string()),
                    timestamp: 1,
                })),
                assistant_message_event: AssistantMessageEvent::Start {
                    partial: Box::new(AssistantMessage::default()),
                },
            });
            listener(&AgentSessionEvent::QueueUpdate {
                steering: vec!["steer".to_string()],
                follow_up: Vec::new(),
            });
            listener(&AgentSessionEvent::AgentSettled { aborted: false });

            wait_until(|| harness.host.state().dispose_count == 1).await;
            let types: Vec<String> = harness
                .stdout
                .records()
                .iter()
                .filter_map(|record| record["type"].as_str().map(str::to_string))
                .collect();
            assert_eq!(types, vec!["agent_start", "agent_settled"]);
            // 恢复后后续 run 的事件重新写出。
            listener(&AgentSessionEvent::AgentStart);
            let types: Vec<String> = harness
                .stdout
                .records()
                .iter()
                .filter_map(|record| record["type"].as_str().map(str::to_string))
                .collect();
            assert_eq!(types, vec!["agent_start", "agent_settled", "agent_start"]);
        })
        .await;
}

#[tokio::test]
async fn run_with_input_eof_exits_zero_and_disposes() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let code = Arc::clone(&harness.server)
                .run_with_input(Cursor::new(
                    b"{\"id\":\"a\",\"type\":\"abort\"}\n{\"id\":\"b\",\"type\":\"abort_bash\"}"
                        .to_vec(),
                ))
                .await;
            assert_eq!(code, 0);
            assert_eq!(harness.host.state().dispose_count, 1);
            assert_eq!(harness.stdout.flush_count(), 1);
            assert_eq!(harness.facade.state().abort_count, 1);
            assert_eq!(harness.facade.state().abort_bash_count, 1);
            // `run_with_input` 不接管 stdout（生产入口才调用 take_over）。
            assert_eq!(harness.stdout.take_over_count(), 0);
        })
        .await;
}

#[tokio::test]
async fn sigterm_exits_143_without_flush() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let code = harness
                .server
                .shutdown_for_signal(Some(RunSignal::Term))
                .await;
            assert_eq!(code, 143);
            assert_eq!(harness.host.state().dispose_count, 1);
            assert_eq!(harness.stdout.flush_count(), 0);
        })
        .await;
}

#[tokio::test]
async fn sighup_exits_129_with_flush_and_second_signal_keeps_own_code() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let code = harness
                .server
                .shutdown_for_signal(Some(RunSignal::Hup))
                .await;
            assert_eq!(code, 129);
            assert_eq!(harness.stdout.flush_count(), 1);

            // 重入：第二个 SIGHUP 返回 129，且不重复 dispose / flush。
            let code = harness
                .server
                .shutdown_for_signal(Some(RunSignal::Hup))
                .await;
            assert_eq!(code, 129);
            assert_eq!(harness.host.state().dispose_count, 1);
            assert_eq!(harness.stdout.flush_count(), 1);
        })
        .await;
}

#[tokio::test]
async fn check_shutdown_requested_returns_none_without_request() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            assert_eq!(harness.server.check_shutdown_requested().await, None);
            assert_eq!(harness.host.state().dispose_count, 0);
        })
        .await;
}

#[tokio::test]
async fn extension_ui_response_is_resolved_and_ignored_when_unknown() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            // 未知 id：静默忽略，不写任何记录。
            send(
                &harness.server,
                r#"{"type":"extension_ui_response","id":"nope","value":"x"}"#,
            )
            .await;
            assert!(harness.stdout.lines().is_empty());
        })
        .await;
}

#[tokio::test]
async fn rebind_registers_session_subscription_and_mode() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            assert_eq!(harness.host.state().rebind_registrations, 1);
            let actions = {
                let state = harness.facade.state();
                let bindings = state.bindings.as_ref().expect("bindings");
                assert!(bindings.ui_context.is_some());
                assert_eq!(bindings.mode, Some(ExtensionMode::Rpc));
                assert!(bindings.command_context_actions.is_some());
                assert!(bindings.shutdown_handler.is_some());
                assert!(bindings.on_error.is_some());
                assert!(state.listener.is_some());
                bindings.command_context_actions.clone().expect("actions")
            };
            // `waitForIdle` / `reload` 动作可用。
            (actions.wait_for_idle)().await;
            (actions.reload)().await;
            assert!(
                harness
                    .facade
                    .state()
                    .calls
                    .contains(&"wait_for_idle".to_string())
            );
            assert!(harness.facade.state().calls.contains(&"reload".to_string()));
        })
        .await;
}

/// `setRebindSession` 注册的回调类型。
type RebindCallback = Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>;

/// 模拟“`session_start` 扩展在 `bindExtensions` 期间替换会话”的宿主：`current` 由测试的
/// `on_bind` 钩子推进，`session()` 每次返回当时的最新会话。
struct NestedRebindHost {
    facades: Vec<Arc<dyn AgentSessionFacade>>,
    current: Arc<AtomicUsize>,
    rebind: Arc<Mutex<Option<RebindCallback>>>,
}

#[async_trait(?Send)]
impl RpcSessionHost for NestedRebindHost {
    fn session(&self) -> Arc<dyn AgentSessionFacade> {
        let index = self
            .current
            .load(Ordering::SeqCst)
            .min(self.facades.len() - 1);
        Arc::clone(&self.facades[index])
    }

    fn set_rebind_session(
        &self,
        callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    ) {
        *lock(&self.rebind) = Some(callback);
    }

    async fn new_session(
        &self,
        _options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Ok(SessionReplacementResult { cancelled: false })
    }

    async fn switch_session(
        &self,
        _session_path: String,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Ok(SessionReplacementResult { cancelled: false })
    }

    async fn fork(
        &self,
        _entry_id: String,
        _options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        Ok(ForkSessionResult {
            cancelled: false,
            selected_text: None,
        })
    }

    async fn dispose(&self) {}
}

/// 回归：`bindExtensions` 期间发生嵌套 rebind 后，外层 rebind 必须订阅**最新**会话。
///
/// TS 的 `session` 是 `runRpcMode` 闭包里的可变变量；嵌套 rebind（扩展在 `session_start`
/// 中调用 newSession，runtime 替换会话后回调 rebindSession）会把它改写成新会话，外层恢复
/// 执行时的 `session.subscribe(...)` 因而订阅新会话。Rust 若订阅本函数开始时捕获的旧会话，
/// 事件流会停留在已替换的会话上。
#[tokio::test]
async fn nested_rebind_during_bind_subscribes_to_the_latest_session() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let first = MockFacade::new();
            let second = MockFacade::new();
            let current = Arc::new(AtomicUsize::new(0));
            let rebind: Arc<Mutex<Option<RebindCallback>>> = Arc::new(Mutex::new(None));

            // 第一次 bindExtensions 内：切到 second 并调用嵌套 rebind。
            {
                let current = Arc::clone(&current);
                let rebind = Arc::clone(&rebind);
                first.state().on_bind = Some(Arc::new(move || {
                    let current = Arc::clone(&current);
                    let rebind = Arc::clone(&rebind);
                    Box::pin(async move {
                        current.store(1, Ordering::SeqCst);
                        let callback = lock(&rebind).clone();
                        if let Some(callback) = callback {
                            callback().await;
                        }
                    }) as LocalBoxFuture<'static, ()>
                }));
            }

            let host = Arc::new(NestedRebindHost {
                facades: vec![
                    Arc::clone(&first) as Arc<dyn AgentSessionFacade>,
                    Arc::clone(&second) as Arc<dyn AgentSessionFacade>,
                ],
                current,
                rebind,
            });
            let server = RpcServer::new(host, Arc::new(CaptureStdout::default()));
            server.rebind_session().await;

            assert_eq!(first.state().bind_count, 1);
            assert_eq!(
                second.state().bind_count,
                1,
                "nested rebind must bind the replacement session"
            );
            assert!(
                !first.state().calls.iter().any(|call| call == "subscribe"),
                "outer rebind must not subscribe to the replaced session"
            );
            assert_eq!(
                second
                    .state()
                    .calls
                    .iter()
                    .filter(|call| *call == "subscribe")
                    .count(),
                2,
                "inner and outer rebinds both subscribe to the latest session"
            );
        })
        .await;
}

// ============================================================================
// 回归：V8 TypeError 文本与字段原样转发（评审发现 4 / 6）
// ============================================================================

/// 先交付一次给定字节，然后永远 pending 的 stdin 替身（“stdin 打开但没有更多输入”）。
struct PendingReader {
    chunk: Option<Vec<u8>>,
    delivered: Arc<AtomicUsize>,
}

impl PendingReader {
    fn new(chunk: Vec<u8>) -> (Self, Arc<AtomicUsize>) {
        let delivered = Arc::new(AtomicUsize::new(0));
        (
            Self {
                chunk: Some(chunk),
                delivered: Arc::clone(&delivered),
            },
            delivered,
        )
    }

    fn idle() -> Self {
        Self {
            chunk: None,
            delivered: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl AsyncRead for PendingReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        match this.chunk.take() {
            Some(chunk) => {
                let length = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..length]);
                this.delivered.fetch_add(1, Ordering::SeqCst);
                Poll::Ready(Ok(()))
            }
            None => Poll::Pending,
        }
    }
}

fn response_error(harness: &Harness, id: &str) -> String {
    harness.stdout.response_for(id).expect("response")["error"]
        .as_str()
        .unwrap_or_else(|| panic!("response {id} has no error"))
        .to_string()
}

#[tokio::test]
async fn non_string_text_fields_raise_v8_type_errors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            // prompt: TS 在 `text.startsWith(...)` 上抛 TypeError（异步、由 .catch 写响应）。
            send(
                &harness.server,
                r#"{"id":"p","type":"prompt","message":42}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("p").is_some()).await;
            assert_eq!(
                response_error(&harness, "p"),
                "text.startsWith is not a function"
            );
            assert_eq!(
                harness.stdout.response_for("p").unwrap()["command"],
                json!("prompt")
            );
            assert!(
                !harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .any(|call| call.starts_with("prompt:")),
                "coerced text must never reach session.prompt"
            );

            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"p2","type":"prompt"}"#).await;
            wait_until(|| harness.stdout.response_for("p2").is_some()).await;
            assert_eq!(
                response_error(&harness, "p2"),
                "Cannot read properties of undefined (reading 'startsWith')"
            );

            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"p3","type":"prompt","message":null}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("p3").is_some()).await;
            assert_eq!(
                response_error(&harness, "p3"),
                "Cannot read properties of null (reading 'startsWith')"
            );

            // steer / follow_up：同一个 `_queueUserInput` 的 `text.startsWith`。
            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"s","type":"steer","message":{}}"#).await;
            assert_eq!(
                response_error(&harness, "s"),
                "text.startsWith is not a function"
            );
            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"f","type":"follow_up","message":[]}"#,
            )
            .await;
            assert_eq!(
                response_error(&harness, "f"),
                "text.startsWith is not a function"
            );
            assert!(
                !harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .any(|call| call.starts_with("steer:") || call.starts_with("follow_up:"))
            );

            // set_session_name: `command.name.trim()`。
            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"n","type":"set_session_name","name":42}"#,
            )
            .await;
            assert_eq!(
                response_error(&harness, "n"),
                "command.name.trim is not a function"
            );
            harness.stdout.lines.lock().unwrap().clear();
            send(&harness.server, r#"{"id":"n2","type":"set_session_name"}"#).await;
            assert_eq!(
                response_error(&harness, "n2"),
                "Cannot read properties of undefined (reading 'trim')"
            );
            assert!(harness.facade.state().set_session_names.is_empty());
        })
        .await;
}

/// 评审 finding 1：`images` 原样透传（合法值交给 session.prompt），畸形值复刻 TS 在
/// `_normalizePromptImages` 里抛出的 TypeError 文本，并且**不启动 run**。
#[tokio::test]
async fn prompt_images_pass_through_and_malformed_values_raise_ts_errors() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            // 合法图片：`type` 在 TS 里从不校验，data/mimeType 原样进入 ImageContent。
            send(
                &harness.server,
                r#"{"id":"p","type":"prompt","message":"hi","images":[{"type":"image","data":"YWJj","mimeType":"image/png"}]}"#,
            )
            .await;
            wait_until(|| harness.stdout.response_for("p").is_some()).await;
            assert_eq!(
                harness.stdout.response_for("p").unwrap()["success"],
                json!(true)
            );
            {
                let state = harness.facade.state();
                let images = state
                    .prompt_images
                    .last()
                    .expect("prompt recorded")
                    .clone()
                    .expect("images forwarded");
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].data, "YWJj");
                assert_eq!(images[0].mime_type, "image/png");
            }

            let prompt_calls = |harness: &Harness| {
                harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .filter(|call| call.starts_with("prompt:"))
                    .count()
            };

            // 真值但不可迭代：V8 的 for-of 文本使用变量名 `images`。
            for (id, images) in [("p_obj", "{}"), ("p_num", "5"), ("p_true", "true")] {
                let before = prompt_calls(&harness);
                harness.stdout.lines.lock().unwrap().clear();
                send(
                    &harness.server,
                    &format!(
                        r#"{{"id":"{id}","type":"prompt","message":"hi","images":{images}}}"#
                    ),
                )
                .await;
                wait_until(|| harness.stdout.response_for(id).is_some()).await;
                assert_eq!(response_error(&harness, id), "images is not iterable");
                assert_eq!(prompt_calls(&harness), before, "no run is started");
            }

            // 元素级错误：`Buffer.from(image.data, "base64")` 与 `mimeType.split(...)`。
            let buffer_prefix = "The first argument must be of type string or an instance of \
Buffer, ArrayBuffer, or Array or an Array-like Object. ";
            for (id, images, expected) in [
                (
                    "p_undef",
                    "[{}]",
                    format!("{buffer_prefix}Received undefined"),
                ),
                (
                    "p_numdata",
                    "[{\"data\":5}]",
                    format!("{buffer_prefix}Received type number (5)"),
                ),
                (
                    "p_nulldata",
                    "[{\"data\":null}]",
                    format!("{buffer_prefix}Received null"),
                ),
                (
                    "p_objdata",
                    "[{\"data\":{}}]",
                    format!("{buffer_prefix}Received an instance of Object"),
                ),
                (
                    "p_nullitem",
                    "[null]",
                    "Cannot read properties of null (reading 'data')".to_string(),
                ),
                (
                    "p_missingmime",
                    "[{\"data\":\"YWJj\"}]",
                    "Cannot read properties of undefined (reading 'split')".to_string(),
                ),
                (
                    "p_nullmime",
                    "[{\"data\":\"YWJj\",\"mimeType\":null}]",
                    "Cannot read properties of null (reading 'split')".to_string(),
                ),
                (
                    "p_nummime",
                    "[{\"data\":\"YWJj\",\"mimeType\":5}]",
                    "mimeType.split is not a function".to_string(),
                ),
                (
                    "p_str",
                    "\"abc\"",
                    format!("{buffer_prefix}Received undefined"),
                ),
            ] {
                let before = prompt_calls(&harness);
                harness.stdout.lines.lock().unwrap().clear();
                send(
                    &harness.server,
                    &format!(
                        r#"{{"id":"{id}","type":"prompt","message":"hi","images":{images}}}"#
                    ),
                )
                .await;
                wait_until(|| harness.stdout.response_for(id).is_some()).await;
                assert_eq!(response_error(&harness, id), expected, "case {id}");
                assert_eq!(prompt_calls(&harness), before, "case {id} starts no run");
            }
        })
        .await;
}

/// 评审 finding 1：`steer` / `follow_up` 只做 `content.push(...images)`。
/// 真值不可迭代 -> spread TypeError；数组元素 TS 不校验，Rust 保留可表示的图片。
#[tokio::test]
async fn steer_and_follow_up_images_follow_spread_semantics() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"s","type":"steer","message":"hi","images":[{"data":"YWJj","mimeType":"image/png"}]}"#,
            )
            .await;
            assert_eq!(harness.stdout.response_for("s").unwrap()["success"], json!(true));
            {
                let state = harness.facade.state();
                let images = state
                    .steer_images
                    .last()
                    .expect("steer recorded")
                    .clone()
                    .expect("images forwarded");
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].data, "YWJj");
            }

            let steer_calls = |harness: &Harness| {
                harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .filter(|call| call.starts_with("steer:"))
                    .count()
            };
            let follow_calls = |harness: &Harness| {
                harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .filter(|call| call.starts_with("follow_up:"))
                    .count()
            };
            let spread_error =
                "Spread syntax requires ...iterable[Symbol.iterator] to be a function";

            for (id, command, images) in [
                ("s_obj", "steer", "{}"),
                ("s_num", "steer", "5"),
                ("f_true", "follow_up", "true"),
            ] {
                let before = if command == "steer" {
                    steer_calls(&harness)
                } else {
                    follow_calls(&harness)
                };
                harness.stdout.lines.lock().unwrap().clear();
                send(
                    &harness.server,
                    &format!(
                        r#"{{"id":"{id}","type":"{command}","message":"hi","images":{images}}}"#
                    ),
                )
                .await;
                assert_eq!(response_error(&harness, id), spread_error);
                let after = if command == "steer" {
                    steer_calls(&harness)
                } else {
                    follow_calls(&harness)
                };
                assert_eq!(after, before, "no queue call for {id}");
            }

            // 数组里的非图片元素在 TS 里被原样 spread（不报错）；Rust 丢弃无法表示的元素。
            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"s2","type":"steer","message":"hi","images":[{"data":"YWJj","mimeType":"image/png"},{},"text",null]}"#,
            )
            .await;
            assert_eq!(harness.stdout.response_for("s2").unwrap()["success"], json!(true));
            let state = harness.facade.state();
            let images = state
                .steer_images
                .last()
                .expect("steer recorded")
                .clone()
                .expect("valid images kept");
            assert_eq!(images.len(), 1, "valid element survives invalid siblings");
            assert_eq!(images[0].data, "YWJj");
        })
        .await;
}

/// 评审发现 4 的副作用：TS 的 prompt 校验发生在异步调用内部，`handleCommand` 正常返回，
/// 因此 `checkShutdownRequested()` 仍会执行；校验失败不是“跳过关停检查”的理由。
/// 评审发现 2 的其余两处：`switch_session` 与 `export_html` 的原样值会走到
/// `normalizePath(input).startsWith`，非字符串抛 V8 TypeError；`compact` 的真值
/// `customInstructions` 则被模板字符串强转（`String(v)`）。
#[tokio::test]
async fn non_string_paths_and_compaction_instructions_follow_js_coercion() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            // switch_session: SessionManager.open -> resolvePath -> normalizePath.
            for (id, line, expected) in [
                (
                    "ss1",
                    r#"{"id":"ss1","type":"switch_session"}"#,
                    "Cannot read properties of undefined (reading 'startsWith')",
                ),
                (
                    "ss2",
                    r#"{"id":"ss2","type":"switch_session","sessionPath":null}"#,
                    "Cannot read properties of null (reading 'startsWith')",
                ),
                (
                    "ss3",
                    r#"{"id":"ss3","type":"switch_session","sessionPath":42}"#,
                    "normalized.startsWith is not a function",
                ),
                (
                    "ss4",
                    r#"{"id":"ss4","type":"switch_session","sessionPath":{}}"#,
                    "normalized.startsWith is not a function",
                ),
            ] {
                send(&harness.server, line).await;
                let record = harness.stdout.response_for(id).expect("response");
                assert_eq!(record["success"], json!(false));
                assert_eq!(record["command"], json!("switch_session"));
                assert_eq!(record["error"], json!(expected));
            }
            assert!(
                harness.host.state().switch_session_calls.is_empty(),
                "non-string sessionPath must never reach the host"
            );

            // export_html: `session.exportToHtml(outputPath)` -> `exportSessionToHtml` 先检查
            // 会话文件（内存会话 / 文件不存在），之后才 `normalizePath(outputPath)`。
            // 真值非字符串的类型错误必须让位给两个 session 级错误。
            harness.stdout.lines.lock().unwrap().clear();
            send(
                &harness.server,
                r#"{"id":"eh1","type":"export_html","outputPath":42}"#,
            )
            .await;
            let record = harness.stdout.response_for("eh1").expect("response");
            assert_eq!(record["success"], json!(false));
            assert_eq!(record["command"], json!("export_html"));
            assert_eq!(
                record["error"],
                json!("Nothing to export yet - start a conversation first"),
                "existing check precedes normalizePath (session file is missing)"
            );
            assert!(
                !harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .any(|call| call == "export_html"),
                "truthy non-string outputPath must not export"
            );

            // 内存会话：`getSessionFile()` 为空 -> "Cannot export in-memory session to HTML"
            // 先于 normalizePath。
            harness.facade.state().session_file = None;
            send(
                &harness.server,
                r#"{"id":"eh_in_memory","type":"export_html","outputPath":42}"#,
            )
            .await;
            let record = harness
                .stdout
                .response_for("eh_in_memory")
                .expect("response");
            assert_eq!(
                record["error"],
                json!("Cannot export in-memory session to HTML")
            );

            // 会话文件存在时才轮到 normalizePath 的 TypeError。
            let existing = std::env::temp_dir().join(format!(
                "kiana-rpc-print-export-{}-{}.jsonl",
                std::process::id(),
                harness.stdout.records().len()
            ));
            std::fs::write(&existing, b"").expect("write session file");
            harness.facade.state().session_file = Some(existing.to_string_lossy().into_owned());
            send(
                &harness.server,
                r#"{"id":"eh_type","type":"export_html","outputPath":42}"#,
            )
            .await;
            let record = harness.stdout.response_for("eh_type").expect("response");
            assert_eq!(
                record["error"],
                json!("normalized.startsWith is not a function")
            );
            let _ = std::fs::remove_file(&existing);
            harness.facade.state().session_file = Some("/tmp/session.jsonl".to_string());

            // 空串/false/0 为 falsy，走默认路径。
            harness.stdout.lines.lock().unwrap().clear();
            for (id, value) in [("eh2", json!("")), ("eh3", json!(false)), ("eh4", json!(0))] {
                send(
                    &harness.server,
                    &format!(r#"{{"id":"{id}","type":"export_html","outputPath":{value}}}"#),
                )
                .await;
                let record = harness.stdout.response_for(id).expect("response");
                assert_eq!(record["success"], json!(true));
                assert_eq!(record["data"], json!({"path": "/tmp/export.html"}));
            }

            // compact: `if (customInstructions)` + 模板字符串。
            harness.stdout.lines.lock().unwrap().clear();
            for (id, value) in [
                ("cp1", json!(42)),
                ("cp2", json!({"a": 1})),
                ("cp3", json!(["a", "b"])),
                ("cp4", json!(false)),
                ("cp5", json!("")),
                ("cp6", json!("focus on tests")),
            ] {
                send(
                    &harness.server,
                    &format!(r#"{{"id":"{id}","type":"compact","customInstructions":{value}}}"#),
                )
                .await;
                let record = harness.stdout.response_for(id).expect("response");
                assert_eq!(record["success"], json!(true));
            }
            send(&harness.server, r#"{"id":"cp7","type":"compact"}"#).await;
            assert_eq!(
                harness.facade.state().compact_instructions,
                vec![
                    Some("42".to_string()),
                    Some("[object Object]".to_string()),
                    Some("a,b".to_string()),
                    None,
                    None,
                    Some("focus on tests".to_string()),
                    None,
                ]
            );
        })
        .await;
}

/// 评审发现 2(a)：Node 的 `spawn` 会把 `args` 里的非字符串按 JS `String(v)` 强转
/// （node v26.12.1 实测），因此 `executeBash(undefined)` 执行的是字面命令
/// `undefined` 并返回 success，而不是 ERR_INVALID_ARG_TYPE。这里锁定该行为。
#[tokio::test]
async fn bash_missing_command_is_stringified_like_node_spawn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            {
                let mut state = harness.facade.state();
                state.user_bash_behavior = UserBashBehavior::Local;
            }
            start(&harness).await;
            send(&harness.server, r#"{"id":"b1","type":"bash"}"#).await;
            send(&harness.server, r#"{"id":"b2","type":"bash","command":42}"#).await;
            send(
                &harness.server,
                r#"{"id":"b3","type":"bash","command":{"x":1}}"#,
            )
            .await;
            assert_eq!(harness.facade.state().execute_bash_count, 3);
            let record = harness.stdout.response_for("b1").expect("response");
            assert_eq!(record["success"], json!(true));
            assert_eq!(
                harness.facade.state().recorded_bash.as_ref().unwrap().0,
                "[object Object]"
            );
            let state = harness.facade.state();
            let executed: Vec<&str> = state
                .calls
                .iter()
                .filter_map(|call| call.strip_prefix("execute_bash:"))
                .collect();
            assert_eq!(executed, vec!["undefined", "42", "[object Object]"]);
        })
        .await;
}

#[tokio::test]
async fn prompt_type_error_still_checks_shutdown_requested() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;
            let bindings = harness.facade.state().bindings.clone().expect("bindings");
            let shutdown = bindings.shutdown_handler.clone().expect("handler");
            shutdown();

            send(
                &harness.server,
                r#"{"id":"p","type":"prompt","message":42}"#,
            )
            .await;
            wait_until(|| {
                harness.stdout.response_for("p").is_some()
                    && harness.host.state().dispose_count == 1
            })
            .await;
            assert_eq!(
                response_error(&harness, "p"),
                "text.startsWith is not a function"
            );
        })
        .await;
}

#[tokio::test]
async fn auto_compaction_and_retry_default_to_true_when_enabled_is_missing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            start(&harness).await;

            send(
                &harness.server,
                r#"{"id":"ac","type":"set_auto_compaction"}"#,
            )
            .await;
            send(
                &harness.server,
                r#"{"id":"ar","type":"set_auto_retry","enabled":null}"#,
            )
            .await;
            send(
                &harness.server,
                r#"{"id":"ac2","type":"set_auto_compaction","enabled":false}"#,
            )
            .await;
            let state = harness.facade.state();
            // settings-defaults: compaction.enabled / retry.enabled 都是 true；
            // `null`/缺失经 `?? 默认值` 落回 true。
            assert_eq!(state.auto_compaction, vec![true, false]);
            assert_eq!(state.auto_retry, vec![true]);
        })
        .await;
}

// ============================================================================
// 回归：关停抢占（评审发现 5）
// ============================================================================

/// SIGTERM/SIGHUP 或扩展关停后，JSONL 缓冲区里未成行的字节必须丢弃：TS 的
/// `detachInput()` 之后直接 `process.exit()`，不会把半行当成命令执行。
#[tokio::test]
async fn signal_shutdown_discards_buffered_partial_input() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let (reader, delivered) =
                PendingReader::new(b"{\"id\":\"x\",\"type\":\"get_state\"}".to_vec());
            let run = tokio::task::spawn_local(Arc::clone(&harness.server).run_with_input(reader));
            wait_until(|| delivered.load(Ordering::SeqCst) == 1).await;

            let code = harness
                .server
                .shutdown_for_signal(Some(RunSignal::Term))
                .await;
            assert_eq!(code, 143);
            let run_code = tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("run_with_input must return after the signal shutdown")
                .expect("run task");
            assert_eq!(run_code, 143);
            assert!(
                harness.stdout.responses().is_empty(),
                "buffered partial line must not be dispatched: {:?}",
                harness.stdout.lines()
            );
        })
        .await;
}

/// SIGTERM 不能等一个卡住的命令处理：TS 在 dispose/flush 后直接 `process.exit()`。
#[tokio::test]
async fn signal_shutdown_preempts_stuck_in_flight_command() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let gate = Arc::new(tokio::sync::Notify::new());
            harness.facade.block_prompt(Arc::clone(&gate));
            let (reader, delivered) = PendingReader::new(
                b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"x\"}\n".to_vec(),
            );
            let run = tokio::task::spawn_local(Arc::clone(&harness.server).run_with_input(reader));
            wait_until(|| {
                delivered.load(Ordering::SeqCst) == 1
                    && harness
                        .facade
                        .state()
                        .calls
                        .iter()
                        .any(|call| call == "prompt:x")
            })
            .await;

            let run_code = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let code = harness
                    .server
                    .shutdown_for_signal(Some(RunSignal::Term))
                    .await;
                assert_eq!(code, 143);
                run.await
            })
            .await
            .expect("signal shutdown must not wait for in-flight commands")
            .expect("run task");
            assert_eq!(run_code, 143);
            gate.notify_waiters();
        })
        .await;
}

/// 评审 finding 9：stdin EOF 是 TS 的 `void shutdown()`——即使另一个 shutdown（SIGTERM）
/// 已开始、`shuttingDown` 为 true，重入分支也立即 `process.exit(0)`。
#[tokio::test]
async fn eof_during_in_flight_signal_shutdown_exits_zero() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let gate = Arc::new(tokio::sync::Notify::new());
            harness.host.block_dispose(Arc::clone(&gate));
            let (writer, reader) = tokio::io::duplex(4096);
            let run = tokio::task::spawn_local(Arc::clone(&harness.server).run_with_input(reader));
            wait_until(|| harness.facade.state().bind_count >= 1).await;

            let first = tokio::task::spawn_local({
                let server = Arc::clone(&harness.server);
                async move { server.shutdown_for_signal(Some(RunSignal::Term)).await }
            });
            wait_until(|| harness.host.state().dispose_count == 1).await;

            // 父进程在 SIGTERM 的 dispose 还卡着时关闭 stdin。
            drop(writer);
            let run_code = tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("EOF must not wait for the in-flight shutdown")
                .expect("run task");
            assert_eq!(run_code, 0, "TS re-enters shutdown and exits 0");

            gate.notify_waiters();
            let first_code = tokio::time::timeout(std::time::Duration::from_secs(2), first)
                .await
                .expect("first shutdown completes after dispose is released")
                .expect("first shutdown task");
            assert_eq!(first_code, 143);
        })
        .await;
}

/// 第二个信号在第一个 `shutdown` 还卡在 `dispose()` 时立即强杀，并保持自己的退出码。
#[tokio::test]
async fn second_signal_force_exits_with_its_own_code_during_dispose() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let gate = Arc::new(tokio::sync::Notify::new());
            harness.host.block_dispose(Arc::clone(&gate));
            let run = tokio::task::spawn_local(
                Arc::clone(&harness.server).run_with_input(PendingReader::idle()),
            );
            // 等 `run_with_input` 完成 rebind（进入读取循环）。
            wait_until(|| harness.facade.state().bind_count >= 1).await;

            let first = tokio::task::spawn_local({
                let server = Arc::clone(&harness.server);
                async move { server.shutdown_for_signal(Some(RunSignal::Term)).await }
            });
            wait_until(|| harness.host.state().dispose_count == 1).await;

            let second = harness
                .server
                .shutdown_for_signal(Some(RunSignal::Hup))
                .await;
            assert_eq!(second, 129);

            let run_code = tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the second signal must preempt the stuck dispose")
                .expect("run task");
            assert_eq!(run_code, 129);

            // 放开 dispose，让第一个 shutdown 收尾（退出码仍是 143）。
            gate.notify_waiters();
            let first_code = tokio::time::timeout(std::time::Duration::from_secs(2), first)
                .await
                .expect("first shutdown completes after dispose is released")
                .expect("first shutdown task");
            assert_eq!(first_code, 143);
            assert_eq!(harness.host.state().dispose_count, 1);
        })
        .await;
}

/// 扩展 `shutdownHandler` 触发的关停同样不能等在途命令（TS 在 dispose/flush 后
/// `process.exit()`；卡住的 prompt 任务被进程退出直接丢弃）。
#[tokio::test]
async fn extension_shutdown_preempts_stuck_in_flight_command() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let gate = Arc::new(tokio::sync::Notify::new());
            harness.facade.block_prompt(Arc::clone(&gate));
            // 用 duplex 精确控制投递时机：先让 prompt 真正卡住，再请求关停，最后用一条
            // 正常命令触发 `checkShutdownRequested()`。
            let (mut writer, reader) = tokio::io::duplex(4096);
            let run = tokio::task::spawn_local(Arc::clone(&harness.server).run_with_input(reader));
            wait_until(|| harness.facade.state().bindings.is_some()).await;
            let handler = harness
                .facade
                .state()
                .bindings
                .as_ref()
                .and_then(|bindings| bindings.shutdown_handler.clone())
                .expect("shutdown handler");
            // 投递 prompt 行并等它真正卡住（`session.prompt()` 永不返回）。
            writer
                .write_all(b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"x\"}\n")
                .await
                .expect("write prompt line");
            wait_until(|| {
                harness
                    .facade
                    .state()
                    .calls
                    .iter()
                    .any(|call| call == "prompt:x")
            })
            .await;
            // 请求关停，再用一条正常命令触发 `checkShutdownRequested()`。
            handler();
            writer
                .write_all(b"{\"id\":\"a\",\"type\":\"abort\"}\n")
                .await
                .expect("write abort line");

            let run_code = tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("extension shutdown must not wait for in-flight commands")
                .expect("run task");
            assert_eq!(run_code, 0);
            assert_eq!(harness.host.state().dispose_count, 1);
            drop(writer);
            gate.notify_waiters();
        })
        .await;
}

// ============================================================================
// 回归（评审发现 1/2）：agent 背压订阅与 EOF 立即关停
// ============================================================================

/// 构造一个立即完成一次 assistant 消息的真实 [`kiana_agent_core::Agent`]。
///
/// 用于验证 RPC 模式的 `session.agent.subscribe` 背压订阅：mock 会话通过
/// [`AgentSessionFacade::agent`] 把它交给服务端。
fn backpressure_test_agent() -> kiana_agent_core::Agent {
    use kiana_ai::event_stream::AssistantMessageEventStream;
    use kiana_ai::types::{
        AssistantMessage, AssistantMessageEvent, AssistantRole, DoneReason, StopReason,
    };

    let stream_fn: kiana_agent_core::types::StreamFn = Arc::new(|_model, _context, _options| {
        Box::pin(async move {
            let message = AssistantMessage {
                role: AssistantRole::Assistant,
                stop_reason: StopReason::Stop,
                ..Default::default()
            };
            let stream = AssistantMessageEventStream::new();
            let _ = stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(message),
            });
            Ok(stream)
        })
    });
    kiana_agent_core::Agent::new(kiana_agent_core::AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .expect("construct the backpressure test agent")
}

/// 评审发现 1：TS 在 RPC 模式**无条件**订阅 `session.agent`，每个 agent 事件后
/// `await waitForRawStdoutBackpressure()`，agent loop 因此被 stdout 写链节流。
#[tokio::test]
async fn agent_events_await_stdout_backpressure() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let agent = backpressure_test_agent();
            harness.facade.state().agent = Some(agent.clone());
            start(&harness).await;

            let gate = Arc::new(BackpressureGate::default());
            harness.stdout.block_backpressure(Arc::clone(&gate));

            let run = tokio::task::spawn_local(async move { agent.prompt_text("hi").await });
            // 第一个 agent 事件（agent_start）的派发必须已经卡在背压等待上。
            wait_until(|| harness.stdout.backpressure_calls.load(Ordering::SeqCst) >= 1).await;
            assert!(
                !run.is_finished(),
                "agent events must await stdout backpressure while it is blocked"
            );

            gate.release();
            let joined = tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the agent run completes once backpressure is released");
            let outcome = joined.expect("join task");
            assert!(
                outcome.is_ok(),
                "the agent run must finish without an error"
            );
            assert!(
                harness.stdout.backpressure_calls.load(Ordering::SeqCst) >= 2,
                "every agent event awaits the stdout write chain"
            );
        })
        .await;
}

/// 评审发现 2：stdin EOF 立即 `void shutdown()`（TS 的 `onInputEnd`）。在途命令不等待：
/// `dispose()` 中止它们，flush 后进程退出；不会卡到命令自己完成。
#[tokio::test]
async fn stdin_eof_does_not_wait_for_in_flight_commands() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let harness = harness();
            let gate = Arc::new(tokio::sync::Notify::new());
            harness.facade.block_prompt(Arc::clone(&gate));

            let started = std::time::Instant::now();
            let code = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                Arc::clone(&harness.server).run_with_input(Cursor::new(
                    b"{\"id\":\"p\",\"type\":\"prompt\",\"message\":\"x\"}".to_vec(),
                )),
            )
            .await
            .expect("EOF must not wait for the in-flight prompt");
            assert_eq!(code, 0);
            assert_eq!(harness.host.state().dispose_count, 1);
            assert_eq!(harness.stdout.flush_count(), 1);
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "EOF shutdown must not wait for command completion"
            );
            gate.notify_waiters();
        })
        .await;
}
