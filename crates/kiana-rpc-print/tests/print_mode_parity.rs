//! `modes/print-mode.ts` 的移植测试。
//!
//! - `runPrintMode` 的 4 个用例标题沿用 `packages/coding-agent/test/print-mode.test.ts` 的原始标题。
//! - spec §2.3.3 的观测点表逐条覆盖：text/json 的 stdout、stderr、退出码，prompt 参数与图片，
//!   text 模式也订阅、dispose 恰好一次、JSON 头先于 rebind、头缺失不写记录、事件经
//!   `serialize_event_line` 的键序、`length` 先文本后警告、空 `errorMessage` 回退 `Request error`。
//! - 事件行的黄金样本复用 `fixtures/json_event/runtime_order_cases.json`（Node v26.12.1 生成）。
//! - 信号语义（`process.on`/`process.off`）在**子进程**里验证：真实 `sigaction` 处理器触发时
//!   会 `process::exit`，不能在父测试进程里安装。子进程场景见文件末尾。
//!
//! TS 测试用 mock（`sessionManager.getHeader()` 返回 `undefined`），因此这里同样用
//! `MockHost`/`MockSession`，不触碰进程级 stdout/stderr。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, ImageContent, ImageKind, Message,
    StopReason, TextContent, TextKind, ThinkingContent, ThinkingKind, ToolCall, ToolCallKind,
    Usage,
};
use kiana_ext_api::types::{
    ExtensionCommandContextActions, ExtensionMode, ForkOptions,
    NavigateTreeOptions as ExtNavigateTreeOptions, NewSessionOptions,
    SwitchSessionOptions as ExtSwitchSessionOptions,
};
use kiana_ext_api::{BoxFuture, LocalBoxFuture};
use kiana_rpc_print::{
    PrintErrorSink, PrintModeOptions, PrintOutputMode, PrintRuntimeHost, PrintSession, PrintSignal,
    RawStdout, print_signal_exit_code, print_signal_skips_flush, run_print_mode_signal_for_test,
    run_print_mode_with_host, run_print_mode_with_host_and_error_sink,
    run_print_mode_with_host_and_signal_handlers,
};
use kiana_session::AgentSessionEvent;
use kiana_session::agent_session::{
    AgentSession, AgentSessionError, AgentSessionEventListener, AgentSessionUnsubscribe,
    ExtensionBindings, NavigateTreeOptions, PromptOptions, TreeNavigationResult,
};
use kiana_session::agent_session_runtime::{ForkSessionResult, SessionReplacementResult};
use kiana_session_core::session_manager::SessionHeader;

const RUNTIME_ORDER_FIXTURE: &str = include_str!("fixtures/json_event/runtime_order_cases.json");
const HEADER_FIXTURE: &str = include_str!("fixtures/print_mode/header.json");
const HEADER_RAW_FIXTURE: &str = include_str!("fixtures/print_mode/header_raw.json");

// ============================================================================
// 测试夹具
// ============================================================================

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 在 `LocalSet` 上驱动 `!Send` future（与生产调用方一致）。
fn run<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(tokio::task::LocalSet::new().run_until(future))
}

/// 记录写入的 [`RawStdout`] 替身（spec §2.8 的测试纪律：不触进程级 stdout）。
#[derive(Default)]
struct CaptureStdout {
    writes: Mutex<Vec<String>>,
    flush_calls: Mutex<usize>,
    wait_calls: Mutex<usize>,
    take_overs: Mutex<usize>,
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
        if self.released.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.notify.notified().await;
    }

    fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl CaptureStdout {
    fn writes(&self) -> Vec<String> {
        lock(&self.writes).clone()
    }

    fn flush_calls(&self) -> usize {
        *lock(&self.flush_calls)
    }

    fn take_overs(&self) -> usize {
        *lock(&self.take_overs)
    }
}

impl RawStdout for CaptureStdout {
    fn write_raw(&self, text: &str) {
        lock(&self.writes).push(text.to_string());
    }

    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()> {
        *lock(&self.wait_calls) += 1;
        let gate = lock(&self.backpressure_gate).clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
        })
    }

    fn flush(&self) -> BoxFuture<'_, ()> {
        *lock(&self.flush_calls) += 1;
        Box::pin(async {})
    }

    fn take_over(&self) {
        *lock(&self.take_overs) += 1;
    }
}

/// 记录 `console.error` 行的替身。
#[derive(Default)]
struct CaptureErrors {
    lines: Mutex<Vec<String>>,
}

impl CaptureErrors {
    fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }
}

impl PrintErrorSink for CaptureErrors {
    fn write_line(&self, text: &str) {
        lock(&self.lines).push(text.to_string());
    }
}

struct PromptRecord {
    text: String,
    options: PromptOptions,
}

#[derive(Default)]
struct MockSessionState {
    header: Option<SessionHeader>,
    /// `sessionManager.getHeader()` 的原始 JSON（TS JSON.stringify 的真实输入）。
    header_json: serde_json::Value,
    messages: Vec<AgentMessage>,
    order: Vec<&'static str>,
    header_calls: usize,
    header_json_calls: usize,
    messages_calls: usize,
    /// `prompt` 在返回前等待信号处理器的 dispose 已经开始（确定性替代 sleep 竞态）。
    wait_for_dispose_started: Option<Arc<std::sync::atomic::AtomicUsize>>,
    bindings: Vec<ExtensionBindings>,
    subscribe_calls: usize,
    unsubscribe_calls: usize,
    listener: Option<AgentSessionEventListener>,
    prompts: Vec<PromptRecord>,
    prompt_results: VecDeque<Result<(), String>>,
    queued_events: Vec<AgentSessionEvent>,
    wait_for_idle_calls: usize,
    navigate_calls: Vec<(String, NavigateTreeOptions)>,
    navigate_results: VecDeque<Result<TreeNavigationResult, String>>,
    reload_calls: usize,
    reload_error: Option<String>,
    /// `session.agent`（`Some` 时 JSON 模式会建立 stdout 背压订阅）。
    agent: Option<kiana_agent_core::Agent>,
}

/// [`PrintSession`] 的 mock：覆盖流程会调用的每个方法（`agent_session` 必须 panic）。
#[derive(Clone, Default)]
struct MockSession {
    state: Arc<Mutex<MockSessionState>>,
}

impl MockSession {
    fn new(header: Option<SessionHeader>, messages: Vec<AgentMessage>) -> Self {
        let session = MockSession::default();
        {
            let mut state = lock(&session.state);
            state.header_json = header
                .as_ref()
                .map(|header| serde_json::to_value(header).expect("typed header json"))
                .unwrap_or(serde_json::Value::Null);
            state.header = header;
            state.messages = messages;
        }
        session
    }

    /// 直接设置 `getHeader()` 的原始 JSON（模拟从会话文件解析出的对象：键序/未知字段）。
    fn set_header_json(&self, header: serde_json::Value) {
        self.state().header_json = header;
    }

    fn state(&self) -> MutexGuard<'_, MockSessionState> {
        lock(&self.state)
    }

    fn queue_event(&self, event: AgentSessionEvent) {
        self.state().queued_events.push(event);
    }

    fn push_prompt_error(&self, message: &str) {
        self.state()
            .prompt_results
            .push_back(Err(message.to_string()));
    }
}

#[async_trait(?Send)]
impl PrintSession for MockSession {
    fn agent_session(&self) -> AgentSession {
        panic!("MockSession::agent_session must not be called");
    }

    fn messages(&self) -> Vec<AgentMessage> {
        let mut state = lock(&self.state);
        state.messages_calls += 1;
        state.order.push("messages");
        state.messages.clone()
    }

    fn header(&self) -> Option<SessionHeader> {
        let mut state = lock(&self.state);
        state.header_calls += 1;
        state.order.push("header");
        state.header.clone()
    }

    fn header_json(&self) -> serde_json::Value {
        let mut state = lock(&self.state);
        state.header_json_calls += 1;
        state.order.push("header_json");
        state.header_json.clone()
    }

    fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        {
            let mut state = lock(&self.state);
            state.subscribe_calls += 1;
            state.order.push("subscribe");
            state.listener = Some(listener);
        }
        let state = Arc::clone(&self.state);
        Box::new(move || {
            lock(&state).unsubscribe_calls += 1;
        })
    }

    fn agent(&self) -> Option<kiana_agent_core::Agent> {
        self.state().agent.clone()
    }

    async fn bind_extensions(&self, bindings: ExtensionBindings) {
        let mut state = lock(&self.state);
        state.bindings.push(bindings);
        state.order.push("bind");
    }

    async fn prompt(&self, text: String, options: PromptOptions) -> Result<(), AgentSessionError> {
        let (result, listener, events, wait_for_dispose_started, agent) = {
            let mut state = lock(&self.state);
            state.order.push("prompt");
            state.prompts.push(PromptRecord {
                text: text.clone(),
                options,
            });
            let result = state.prompt_results.pop_front().unwrap_or(Ok(()));
            let listener = state.listener.clone();
            let events = std::mem::take(&mut state.queued_events);
            let wait_for_dispose_started = state.wait_for_dispose_started.clone();
            let agent = state.agent.clone();
            (result, listener, events, wait_for_dispose_started, agent)
        };
        // 子进程信号场景：等信号处理器真正进入 dispose（最多 5s，避免无信号时永久挂起），
        // 这样「main 在处理器完成前返回」是确定性的，不依赖机器负载。
        if let Some(counter) = wait_for_dispose_started {
            for _ in 0..500 {
                if counter.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        if let Some(listener) = listener {
            for event in &events {
                listener(event);
            }
        }
        // 背压订阅测试：让真实 Agent 在 prompt 期间发事件（JSON 模式已订阅
        // `session.agent`，每个 agent 事件都会等 stdout 写链）。
        if let Some(agent) = agent {
            let _ = agent.prompt_text(text).await;
        }
        result.map_err(AgentSessionError::Other)
    }

    async fn wait_for_idle(&self) {
        let mut state = lock(&self.state);
        state.wait_for_idle_calls += 1;
    }

    async fn navigate_tree(
        &self,
        target_id: String,
        options: NavigateTreeOptions,
    ) -> Result<TreeNavigationResult, AgentSessionError> {
        let result = {
            let mut state = lock(&self.state);
            state.navigate_calls.push((target_id, options));
            state
                .navigate_results
                .pop_front()
                .unwrap_or_else(|| Ok(TreeNavigationResult::default()))
        };
        result.map_err(AgentSessionError::Other)
    }

    async fn reload(&self) -> Result<(), AgentSessionError> {
        let error = {
            let mut state = lock(&self.state);
            state.reload_calls += 1;
            state.reload_error.clone()
        };
        match error {
            Some(message) => Err(AgentSessionError::Other(message)),
            None => Ok(()),
        }
    }
}

type RebindCallback = Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>;

#[derive(Default)]
struct MockHostState {
    sessions: Vec<Arc<dyn PrintSession>>,
    rebind: Option<RebindCallback>,
    set_rebind_calls: usize,
    dispose_calls: usize,
    new_session_calls: Vec<Option<NewSessionOptions>>,
    fork_calls: Vec<(String, Option<ForkOptions>)>,
    switch_session_calls: Vec<(String, Option<ExtSwitchSessionOptions>)>,
    new_session_outcome: Option<Result<SessionReplacementResult, String>>,
    fork_outcome: Option<Result<ForkSessionResult, String>>,
    switch_session_outcome: Option<Result<SessionReplacementResult, String>>,
    /// `dispose` 在返回前按 10ms 片睡眠的次数（信号子进程场景需要可抢占的 dispose）。
    dispose_delay_chunks: usize,
    /// dispose 被调用的计数（信号子进程场景与 `prompt` 协调）。
    dispose_started: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

/// [`PrintRuntimeHost`] 的 mock。
#[derive(Clone, Default)]
struct MockHost {
    state: Arc<Mutex<MockHostState>>,
}

impl MockHost {
    fn new(session: MockSession) -> Self {
        let host = MockHost::default();
        host.push_session(session);
        host
    }

    fn state(&self) -> MutexGuard<'_, MockHostState> {
        lock(&self.state)
    }

    fn push_session(&self, session: MockSession) {
        self.state().sessions.push(Arc::new(session));
    }

    fn set_new_session_outcome(&self, outcome: Result<SessionReplacementResult, String>) {
        self.state().new_session_outcome = Some(outcome);
    }

    fn set_fork_outcome(&self, outcome: Result<ForkSessionResult, String>) {
        self.state().fork_outcome = Some(outcome);
    }

    fn set_switch_session_outcome(&self, outcome: Result<SessionReplacementResult, String>) {
        self.state().switch_session_outcome = Some(outcome);
    }

    async fn trigger_rebind(&self) {
        let callback = self
            .state()
            .rebind
            .clone()
            .expect("rebind callback installed");
        callback().await;
    }
}

#[async_trait(?Send)]
impl PrintRuntimeHost for MockHost {
    fn session(&self) -> Arc<dyn PrintSession> {
        self.state()
            .sessions
            .last()
            .cloned()
            .expect("mock host has a session")
    }

    fn set_rebind_session(&self, callback: RebindCallback) {
        let mut state = lock(&self.state);
        state.set_rebind_calls += 1;
        state.rebind = Some(callback);
    }

    async fn new_session(
        &self,
        options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let outcome = {
            let mut state = lock(&self.state);
            state.new_session_calls.push(options);
            state.new_session_outcome.clone()
        };
        outcome.unwrap_or(Ok(SessionReplacementResult { cancelled: false }))
    }

    async fn fork(
        &self,
        entry_id: String,
        options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        let outcome = {
            let mut state = lock(&self.state);
            state.fork_calls.push((entry_id, options));
            state.fork_outcome.clone()
        };
        outcome.unwrap_or(Ok(ForkSessionResult {
            cancelled: false,
            selected_text: None,
        }))
    }

    async fn switch_session(
        &self,
        session_path: String,
        options: Option<ExtSwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        let outcome = {
            let mut state = lock(&self.state);
            state.switch_session_calls.push((session_path, options));
            state.switch_session_outcome.clone()
        };
        outcome.unwrap_or(Ok(SessionReplacementResult { cancelled: false }))
    }

    async fn dispose(&self) {
        let (delay, started) = {
            let mut state = self.state();
            state.dispose_calls += 1;
            (state.dispose_delay_chunks, state.dispose_started.clone())
        };
        if let Some(counter) = started {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        // 可被 LocalSet 抢占的 dispose：模拟 extension session_shutdown 的异步工作。
        for _ in 0..delay {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

// ----------------------------------------------------------------------------
// 消息/头夹具
// ----------------------------------------------------------------------------

fn session_header() -> SessionHeader {
    SessionHeader {
        id: "session-1".to_string(),
        timestamp: Some("2024-12-03T14:00:00.000Z".to_string()),
        cwd: Some("/tmp/project".to_string()),
        ..Default::default()
    }
}

fn text_block(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn assistant_with(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
    error_message: Option<&str>,
) -> AgentMessage {
    AgentMessage::Standard(Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "faux".to_string(),
        provider: "faux".to_string(),
        model: "faux".to_string(),
        usage: Usage::default(),
        stop_reason,
        error_message: error_message.map(str::to_string),
        timestamp: 1_700_000_000_000,
        ..AssistantMessage::default()
    }))
}

/// TS `createAssistantMessage({ text, stopReason, errorMessage })`。
fn assistant_message(
    text: Option<&str>,
    stop_reason: StopReason,
    error_message: Option<&str>,
) -> AgentMessage {
    let content = text.map(|text| vec![text_block(text)]).unwrap_or_default();
    assistant_with(content, stop_reason, error_message)
}

/// `{ role: "assistant", ... }` 但不符合标准 `AssistantMessage` 形状的自定义 JSON 消息。
fn custom_message(value: serde_json::Value) -> AgentMessage {
    serde_json::from_value(value).expect("custom agent message")
}

fn image() -> ImageContent {
    ImageContent {
        kind_image: ImageKind::Image,
        data: "abc".to_string(),
        mime_type: "image/png".to_string(),
    }
}

fn text_options(initial_message: Option<&str>) -> PrintModeOptions {
    PrintModeOptions {
        mode: PrintOutputMode::Text,
        initial_message: initial_message.map(str::to_string),
        ..Default::default()
    }
}

fn json_options(messages: Vec<&str>) -> PrintModeOptions {
    PrintModeOptions {
        mode: PrintOutputMode::Json,
        messages: messages.into_iter().map(str::to_string).collect(),
        ..Default::default()
    }
}

// ============================================================================
// describe: "runPrintMode"（TS 用例逐字移植）
// ============================================================================

/// it: "emits session_shutdown in text mode"
#[test]
fn emits_session_shutdown_in_text_mode() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());
        let images = vec![image()];

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            PrintModeOptions {
                mode: PrintOutputMode::Text,
                initial_message: Some("Say done".to_string()),
                initial_images: Some(images.clone()),
                ..Default::default()
            },
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        {
            let state = session.state();
            assert_eq!(state.prompts.len(), 1);
            assert_eq!(state.prompts[0].text, "Say done");
            assert_eq!(
                state.prompts[0].options.images.as_deref(),
                Some(images.as_slice())
            );
            assert_eq!(state.subscribe_calls, 1, "text mode also subscribes");
            assert_eq!(state.unsubscribe_calls, 1, "dispose() unsubscribes");
            assert_eq!(state.bindings.len(), 1);
            assert_eq!(state.bindings[0].mode, Some(ExtensionMode::Print));
            assert!(state.bindings[0].command_context_actions.is_some());
            assert!(state.bindings[0].on_error.is_some());
            assert!(state.bindings[0].ui_context.is_none());
        }
        assert_eq!(
            host.state().dispose_calls,
            1,
            "session_shutdown emitted exactly once"
        );
        assert_eq!(host.state().set_rebind_calls, 1);
        assert_eq!(io.writes(), vec!["done\n"]);
        assert!(errors.lines().is_empty());
        assert_eq!(io.flush_calls(), 1);
        assert_eq!(io.take_overs(), 0, "print mode never takes over stdout");
    });
}

/// it: "emits session_shutdown in json mode"
#[test]
fn emits_session_shutdown_in_json_mode() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        {
            let state = session.state();
            assert_eq!(state.prompts.len(), 1);
            assert_eq!(state.prompts[0].text, "hello");
            assert!(state.prompts[0].options.images.is_none());
            assert_eq!(state.subscribe_calls, 1);
            assert_eq!(state.bindings[0].mode, Some(ExtensionMode::Json));
        }
        assert_eq!(host.state().dispose_calls, 1);
        assert!(
            io.writes().is_empty(),
            "header is absent and no events were emitted"
        );
        assert!(errors.lines().is_empty());
    });
}

/// it: "emits session_shutdown and returns non-zero on assistant error"
#[test]
fn emits_session_shutdown_and_returns_non_zero_on_assistant_error() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(
                None,
                StopReason::Error,
                Some("provider failure"),
            )],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(errors.lines(), vec!["provider failure"]);
        assert!(
            io.writes().is_empty(),
            "error stop reason writes no stdout text"
        );
        assert_eq!(host.state().dispose_calls, 1);
    });
}

/// it: "warns on stderr and returns non-zero when the response was truncated"（#10799）
#[test]
fn warns_on_stderr_and_returns_non_zero_when_the_response_was_truncated() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(None, StopReason::Length, None)],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(
            errors.lines(),
            vec!["Response was truncated: the model reached its output token limit."]
        );
        assert!(io.writes().is_empty());
        assert_eq!(host.state().dispose_calls, 1);
    });
}

// ============================================================================
// spec §2.3.3 观测点表
// ============================================================================

/// §2.3.3 行 1：text 模式 stop 带 1 个 text 块。
#[test]
fn text_mode_writes_each_text_block_with_a_trailing_newline() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec!["done\n"]);
        assert!(errors.lines().is_empty());
    });
}

/// §2.3.3 行 2：text 模式无消息。
#[test]
fn text_mode_without_messages_writes_nothing() {
    run(async {
        let session = MockSession::new(None, Vec::new());
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert!(io.writes().is_empty());
        assert!(errors.lines().is_empty());
        assert_eq!(session.state().messages_calls, 1);

        assert_eq!(io.flush_calls(), 1);
    });
}

/// §2.3.3 行 3：text 模式最后一条不是 assistant。
#[test]
fn text_mode_ignores_a_last_message_that_is_not_an_assistant_message() {
    run(async {
        let user_message = AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text("hi".to_string()),
            timestamp: 1,
        }));
        let session = MockSession::new(None, vec![user_message]);
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert!(io.writes().is_empty());
        assert!(errors.lines().is_empty());
    });
}

/// 回归（review finding 5）：自定义 `role: "assistant"` 消息也进入文本分支；`content`
/// 数组里的 text 块照常写出（TS 只按 `lastMessage?.role === "assistant"` 判定）。
#[test]
fn text_mode_custom_assistant_role_with_content_array_writes_text_blocks() {
    run(async {
        let session = MockSession::new(
            None,
            vec![custom_message(serde_json::json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": "custom text" }],
            }))],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec!["custom text\n"]);
        assert!(errors.lines().is_empty());
    });
}

/// 回归（review finding 5）：自定义 assistant 缺 `content` 时 TS 抛
/// `assistantMsg.content is not iterable` -> catch 输出、返回 1。
#[test]
fn text_mode_custom_assistant_without_content_fails_like_ts() {
    run(async {
        let session = MockSession::new(
            None,
            vec![custom_message(serde_json::json!({ "role": "assistant" }))],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(errors.lines(), vec!["assistantMsg.content is not iterable"]);
        assert!(io.writes().is_empty());
    });
}

/// 回归（review finding 5）：自定义 assistant 的 stopReason/errorMessage 同样被读取。
#[test]
fn text_mode_custom_assistant_error_stop_reason_uses_error_message() {
    run(async {
        let session = MockSession::new(
            None,
            vec![custom_message(serde_json::json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": "ignored" }],
                "stopReason": "error",
                "errorMessage": "custom fail",
            }))],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(errors.lines(), vec!["custom fail"]);
        assert!(io.writes().is_empty(), "error stop reason suppresses text");
    });
}

/// 回归（review finding 5）：`text` 缺失时模板字符串写出 `"undefined"`（JS `String(undefined)`）。
#[test]
fn text_mode_custom_assistant_missing_text_renders_undefined() {
    run(async {
        let session = MockSession::new(
            None,
            vec![custom_message(serde_json::json!({
                "role": "assistant",
                "content": [{ "type": "text" }],
            }))],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec!["undefined\n"]);
        assert!(errors.lines().is_empty());
    });
}

/// §2.3.3 行 5：text 模式 aborted 且无 errorMessage -> `Request aborted`。
#[test]
fn text_mode_aborted_without_error_message_falls_back_to_request_aborted() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(None, StopReason::Aborted, None)],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(errors.lines(), vec!["Request aborted"]);
        assert!(io.writes().is_empty());
    });
}

/// 空 `errorMessage` 是 JS falsy -> `Request error`（不能用 Option::or_else）。
#[test]
fn text_mode_error_with_empty_error_message_falls_back_to_request_error() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(None, StopReason::Error, Some(""))],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(errors.lines(), vec!["Request error"]);
    });
}

/// §2.3.3 行 6：text 模式 length 带文本 `partial`：先写文本再写警告。
#[test]
fn text_mode_length_writes_the_text_before_the_truncation_warning() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("partial"), StopReason::Length, None)],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(io.writes(), vec!["partial\n"]);
        assert_eq!(
            errors.lines(),
            vec!["Response was truncated: the model reached its output token limit."]
        );
    });
}

/// 多个 text 块与 thinking/toolCall 混排：只写 text 块，保持顺序。
#[test]
fn text_mode_writes_only_text_blocks_in_order() {
    run(async {
        let content = vec![
            text_block("first"),
            AssistantContentBlock::Thinking(ThinkingContent {
                kind_thinking: ThinkingKind::Thinking,
                thinking: "hmm".to_string(),
                ..Default::default()
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "call_1".to_string(),
                name: "write".to_string(),
                arguments: serde_json::from_value(serde_json::json!({ "path": "a.txt" }))
                    .expect("tool arguments object"),
                ..Default::default()
            }),
            text_block("second"),
        ];
        let session = MockSession::new(
            None,
            vec![assistant_with(content, StopReason::ToolUse, None)],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec!["first\n", "second\n"]);
        assert!(errors.lines().is_empty());
    });
}

/// §2.3.3 行 7：prompt 抛错 -> stderr 为 `error.message`，返回 1（仍会 dispose + flush）。
#[test]
fn prompt_errors_are_reported_and_return_one() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        session.push_prompt_error("boom");
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            text_options(Some("explode")),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        assert_eq!(errors.lines(), vec!["boom"]);
        assert!(
            io.writes().is_empty(),
            "no text branch after a prompt failure"
        );
        assert_eq!(host.state().dispose_calls, 1);
        assert_eq!(io.flush_calls(), 1);
    });
}

/// prompt 串行：initial 之后依次发送 messages；任一失败后续不再发送。
#[test]
fn prompts_run_serially_and_stop_after_the_first_failure() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        {
            let mut state = session.state();
            state.prompt_results.push_back(Ok(()));
            state
                .prompt_results
                .push_back(Err("second failed".to_string()));
        }
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let options = PrintModeOptions {
            mode: PrintOutputMode::Text,
            messages: vec!["two".to_string(), "three".to_string()],
            initial_message: Some("one".to_string()),
            ..Default::default()
        };
        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            options,
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 1);
        let state = session.state();
        let texts: Vec<&str> = state
            .prompts
            .iter()
            .map(|prompt| prompt.text.as_str())
            .collect();
        assert_eq!(texts, vec!["one", "two"], "the third message is never sent");
        assert!(state.prompts[0].options.images.is_none());
        assert!(state.prompts[1].options.images.is_none());
        assert_eq!(errors.lines(), vec!["second failed"]);
    });
}

/// initial message 为空字符串时跳过（TS 的 `if (initialMessage)`）。
#[test]
fn empty_initial_message_is_skipped() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let options = PrintModeOptions {
            mode: PrintOutputMode::Text,
            messages: vec!["only".to_string()],
            initial_message: Some(String::new()),
            initial_images: Some(vec![image()]),
        };
        let exit_code =
            run_print_mode_with_host_and_error_sink(Arc::new(host), options, io, errors.clone())
                .await;

        assert_eq!(exit_code, 0);
        let state = session.state();
        assert_eq!(state.prompts.len(), 1);
        assert_eq!(state.prompts[0].text, "only");
        assert!(state.prompts[0].options.images.is_none());
        assert!(errors.lines().is_empty());
    });
}

/// text 模式的订阅是 no-op：事件不会写到 stdout。
#[test]
fn text_mode_subscribes_but_discards_events() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        session.queue_event(AgentSessionEvent::AgentStart);
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(Some("go")),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(session.state().subscribe_calls, 1);
        assert_eq!(io.writes(), vec!["done\n"]);
    });
}

// ============================================================================
// JSON 模式
// ============================================================================

/// JSON 头只写一次，且在 rebindSession() 之前。
#[test]
fn json_mode_writes_the_header_before_rebinding() {
    run(async {
        let session = MockSession::new(
            Some(session_header()),
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        // 黄金样本由 Node v26.12.1 的 `JSON.stringify(header) + "\n"` 生成。
        let fixture: serde_json::Value =
            serde_json::from_str(HEADER_FIXTURE).expect("header fixture");
        session.set_header_json(fixture["header"].clone());
        let expected_line = fixture["line"].as_str().expect("golden header line");
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec![expected_line]);
        assert_eq!(
            session.state().order,
            vec!["header_json", "bind", "subscribe", "prompt"],
            "header must be written before bindExtensions/subscribe"
        );
        assert_eq!(session.state().header_json_calls, 1);
        assert_eq!(
            session.state().header_calls,
            0,
            "JSON mode must serialize the raw header object, not the typed view"
        );
        assert!(errors.lines().is_empty());
    });
}

/// 回归（review finding 4）：JSON 头是 `getHeader()` 的**原始对象**——保留文件键序与未知字段，
/// 包括 v1 迁移在末尾追加的 `version`。typed `SessionHeader` 会归一化顺序并丢弃未知字段。
#[test]
fn json_mode_writes_the_raw_header_object_with_its_file_key_order() {
    let fixture: serde_json::Value =
        serde_json::from_str(HEADER_RAW_FIXTURE).expect("raw header fixture");
    for name in ["v1_migrated", "extra_keys", "fresh"] {
        let case = fixture["cases"]
            .get(name)
            .unwrap_or_else(|| panic!("case {name}"));
        let header = case["header"].clone();
        let expected_line = case["line"].as_str().expect("golden line").to_string();

        run(async {
            let session = MockSession::new(None, Vec::new());
            session.set_header_json(header);
            let host = MockHost::new(session);
            let io = Arc::new(CaptureStdout::default());
            let errors = Arc::new(CaptureErrors::default());

            let exit_code = run_print_mode_with_host_and_error_sink(
                Arc::new(host),
                json_options(vec!["hello"]),
                io.clone(),
                errors.clone(),
            )
            .await;

            assert_eq!(exit_code, 0, "case {name}");
            assert_eq!(io.writes(), vec![expected_line], "case {name}");
            assert!(errors.lines().is_empty(), "case {name}");
        });
    }
}

/// 头为 None：不写 header 记录，事件流从第一条事件开始。
#[test]
fn json_mode_without_a_header_writes_only_events() {
    run(async {
        let session = MockSession::new(None, Vec::new());
        session.queue_event(AgentSessionEvent::AgentStart);
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec!["{\"type\":\"agent_start\"}\n"]);
        assert_eq!(session.state().header_json_calls, 1);
        assert!(errors.lines().is_empty());
    });
}

/// 事件必须经 `serialize_event_line`：输出键序是当前稳定契约（改动需同步测试）。
#[test]
fn json_mode_serializes_events_through_the_wire_serializer() {
    let fixture: serde_json::Value =
        serde_json::from_str(RUNTIME_ORDER_FIXTURE).expect("runtime order fixture");
    let case = fixture["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|case| case["name"] == "message_end_anthropic")
        .expect("message_end_anthropic fixture");
    let event: AgentSessionEvent =
        serde_json::from_value(case["event"].clone()).expect("deserialize AgentSessionEvent");
    let expected_line = case["line"].as_str().expect("golden line").to_string();

    run(async {
        let session = MockSession::new(None, Vec::new());
        session.queue_event(event);
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.writes(), vec![expected_line]);
        assert!(errors.lines().is_empty());
    });
}

/// TS `message_update` 投影错误：`message.role !== "assistant"`。
fn message_update_with_non_assistant_message() -> AgentSessionEvent {
    AgentSessionEvent::MessageUpdate {
        message: AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text("hi".to_string()),
            timestamp: 1,
        })),
        assistant_message_event: kiana_ai::types::AssistantMessageEvent::Start {
            partial: Box::new(AssistantMessage::default()),
        },
    }
}

/// 投影失败（`message_update` 的 message 不是 assistant）：TS 的 listener throw 被
/// `Agent.runWithLifecycle` 吞掉（prompt 正常 resolve、`--mode json` 退出码 0、不写
/// stderr）。Rust 无法合成失败消息事件（见模块文档第 3 条），但同样的可观察结果是
/// **不写半条记录、不写 stderr、退出码 0**。
#[test]
fn json_mode_projection_errors_skip_the_event_and_keep_exit_zero() {
    run(async {
        let session = MockSession::new(None, Vec::new());
        session.queue_event(message_update_with_non_assistant_message());
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert!(errors.lines().is_empty(), "TS writes nothing to stderr");
        assert!(io.writes().is_empty(), "no half record is written");
    });
}

/// 回归（review finding 2）：投影错误之后该 run 的事件不再写出（TS 的 run 已失败结束），
/// 但 prompt 正常 resolve，后续 prompt 仍会发送；退出码 0、无 stderr。
#[test]
fn json_mode_projection_error_suppresses_later_records_but_not_prompts() {
    run(async {
        let session = MockSession::new(None, Vec::new());
        session.queue_event(AgentSessionEvent::AgentStart);
        session.queue_event(message_update_with_non_assistant_message());
        session.queue_event(AgentSessionEvent::AgentEnd {
            messages: Vec::new(),
            will_retry: false,
        });
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let options = PrintModeOptions {
            mode: PrintOutputMode::Json,
            messages: vec!["hello".to_string(), "second".to_string()],
            ..Default::default()
        };
        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            options,
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert_eq!(
            io.writes(),
            vec!["{\"type\":\"agent_start\"}\n"],
            "records before the failure are kept, later ones are not written"
        );
        assert!(errors.lines().is_empty(), "TS writes nothing to stderr");
        let state = session.state();
        assert_eq!(
            state.prompts.len(),
            2,
            "TS resolves the failed prompt and sends the second message"
        );
        assert_eq!(state.prompts[0].text, "hello");
        assert_eq!(host.state().dispose_calls, 1);
        assert_eq!(io.flush_calls(), 1);
    });
}

/// 回归（review finding 2）：`toolcall_start` 的 content 下标不是 toolCall 时同样跳过该
/// 事件、不写 stderr、退出码 0。
#[test]
fn json_mode_toolcall_start_projection_error_is_skipped() {
    run(async {
        let message = assistant_with(Vec::new(), StopReason::Stop, None);
        let session = MockSession::new(None, Vec::new());
        session.queue_event(AgentSessionEvent::MessageUpdate {
            message,
            assistant_message_event: kiana_ai::types::AssistantMessageEvent::ToolCallStart {
                content_index: 0,
                partial: Box::new(AssistantMessage::default()),
            },
        });
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert!(errors.lines().is_empty());
        assert!(io.writes().is_empty());
    });
}

/// JSON 模式完全不看 stopReason：assistant error 正常返回 0。
#[test]
fn json_mode_ignores_the_assistant_stop_reason() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(
                None,
                StopReason::Error,
                Some("provider failure"),
            )],
        );
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            json_options(vec!["hello"]),
            io.clone(),
            errors.clone(),
        )
        .await;

        assert_eq!(exit_code, 0);
        assert!(io.writes().is_empty());
        assert!(errors.lines().is_empty());
    });
}

// ============================================================================
// 订阅 / dispose / 重绑 / 命令动作
// ============================================================================

/// 重绑回调忽略传入的 session，重新读取 `host.session()`。
#[test]
fn rebind_callback_rereads_the_host_session() {
    run(async {
        let first = MockSession::new(None, Vec::new());
        let host = MockHost::new(first.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            text_options(None),
            io,
            errors,
        )
        .await;
        assert_eq!(exit_code, 0);
        assert_eq!(host.state().set_rebind_calls, 1);

        let second = MockSession::new(None, Vec::new());
        host.push_session(second.clone());
        host.trigger_rebind().await;

        assert_eq!(second.state().bindings.len(), 1);
        assert_eq!(second.state().subscribe_calls, 1);
        assert_eq!(first.state().subscribe_calls, 1);
        // 第一次运行结束时已退订；重绑时旧订阅句柄已被 dispose 取走，不会二次退订。
        assert_eq!(first.state().unsubscribe_calls, 1);
        assert_eq!(second.state().unsubscribe_calls, 0);
    });
}

/// 命令上下文动作：wait/new/fork/navigate/switch/reload 全部转发（选项不丢失）。
#[test]
fn command_context_actions_forward_every_operation() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session.clone());
        host.set_new_session_outcome(Ok(SessionReplacementResult { cancelled: true }));
        host.set_fork_outcome(Ok(ForkSessionResult {
            cancelled: true,
            selected_text: Some("picked".to_string()),
        }));
        host.set_switch_session_outcome(Ok(SessionReplacementResult { cancelled: true }));
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            text_options(None),
            io,
            errors.clone(),
        )
        .await;
        assert_eq!(exit_code, 0);

        let actions: ExtensionCommandContextActions = {
            let state = session.state();
            state.bindings[0]
                .command_context_actions
                .clone()
                .expect("command context actions bound")
        };

        (actions.wait_for_idle)().await;
        assert_eq!(session.state().wait_for_idle_calls, 1);

        let new_session_result = (actions.new_session)(None).await;
        assert!(new_session_result.cancelled);
        assert!(host.state().new_session_calls[0].is_none());

        let fork_result = (actions.fork)(
            "entry-1".to_string(),
            Some(ForkOptions {
                position: Some(kiana_ext_api::events::ForkPosition::At),
                with_session: None,
            }),
        )
        .await;
        assert!(fork_result.cancelled);
        let fork_calls = host.state().fork_calls.clone();
        assert_eq!(fork_calls.len(), 1);
        assert_eq!(fork_calls[0].0, "entry-1");
        assert_eq!(
            fork_calls[0]
                .1
                .as_ref()
                .and_then(|options| options.position),
            Some(kiana_ext_api::events::ForkPosition::At)
        );

        let navigate_result = (actions.navigate_tree)(
            "target-1".to_string(),
            Some(ExtNavigateTreeOptions {
                summarize: Some(true),
                custom_instructions: Some("keep it short".to_string()),
                replace_instructions: Some(false),
                label: Some("branch".to_string()),
            }),
        )
        .await;
        assert!(!navigate_result.cancelled);
        let navigate_calls = session.state().navigate_calls.clone();
        assert_eq!(navigate_calls.len(), 1);
        assert_eq!(navigate_calls[0].0, "target-1");
        assert_eq!(navigate_calls[0].1.summarize, Some(true));
        assert_eq!(
            navigate_calls[0].1.custom_instructions.as_deref(),
            Some("keep it short")
        );
        assert_eq!(navigate_calls[0].1.replace_instructions, Some(false));
        assert_eq!(navigate_calls[0].1.label.as_deref(), Some("branch"));

        let switch_result = (actions.switch_session)("/tmp/other.jsonl".to_string(), None).await;
        assert!(switch_result.cancelled);
        let switch_calls = host.state().switch_session_calls.clone();
        assert_eq!(switch_calls.len(), 1);
        assert_eq!(switch_calls[0].0, "/tmp/other.jsonl");
        assert!(switch_calls[0].1.is_none());

        (actions.reload)().await;
        assert_eq!(session.state().reload_calls, 1);
        assert!(errors.lines().is_empty());
    });
}

/// 命令动作的错误收窄：返回 `{cancelled: true}` 且**不写 stderr**（TS 里这些错误 reject
/// 回扩展，同样不写 stderr；Rust 的动作签名没有错误通道）。
#[test]
fn command_context_action_errors_report_cancelled_without_stderr() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session.clone());
        host.set_new_session_outcome(Err("no session".to_string()));
        host.set_fork_outcome(Err("bad entry".to_string()));
        host.set_switch_session_outcome(Err("missing file".to_string()));
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code = run_print_mode_with_host_and_error_sink(
            Arc::new(host.clone()),
            text_options(None),
            io,
            errors.clone(),
        )
        .await;
        assert_eq!(exit_code, 0);

        let actions = {
            let state = session.state();
            state.bindings[0]
                .command_context_actions
                .clone()
                .expect("command context actions bound")
        };
        assert!((actions.new_session)(None).await.cancelled);
        assert!((actions.fork)("e".to_string(), None).await.cancelled);
        assert!(
            (actions.switch_session)("/p".to_string(), None)
                .await
                .cancelled
        );
        assert!(errors.lines().is_empty());
    });
}

/// 扩展 `onError` 写入逐字 `Extension error (path): message`。
#[test]
fn extension_errors_are_logged_verbatim() {
    run(async {
        let session = MockSession::new(
            None,
            vec![assistant_message(Some("done"), StopReason::Stop, None)],
        );
        let host = MockHost::new(session.clone());
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        run_print_mode_with_host_and_error_sink(
            Arc::new(host),
            text_options(None),
            io,
            errors.clone(),
        )
        .await;

        let listener = {
            let state = session.state();
            state.bindings[0].on_error.clone().expect("onError bound")
        };
        listener(kiana_ext_api::types::ExtensionError {
            extension_path: "/ext/a.ts".to_string(),
            event: "tool_call".to_string(),
            error: "kaput".to_string(),
            stack: None,
        });
        assert_eq!(errors.lines(), vec!["Extension error (/ext/a.ts): kaput"]);
    });
}

/// dispose 恰好一次，即便信号路径与 finally 竞争（信号 body 先执行）。
///
/// 两条信号路径都不 flush：TS `print-mode.ts` 的 handler 只做
/// `killTrackedDetachedChildren()` -> `disposeRuntime()` -> `process.exit(129|143)`，
/// 从不调用 `flushRawStdout()`（spec §2.3.2、§4.7）。
#[test]
fn dispose_runs_exactly_once_and_signal_paths_skip_flush() {
    run(async {
        for (signal, expected_code, expected_flushes) in [
            (PrintSignal::Term, 143, 0usize),
            (PrintSignal::Hup, 129, 0usize),
        ] {
            let session = MockSession::new(None, Vec::new());
            let host = MockHost::new(session);
            let io = Arc::new(CaptureStdout::default());

            let exit_code = run_print_mode_signal_for_test(
                Arc::new(host.clone()),
                PrintModeOptions::default(),
                io.clone(),
                signal,
            )
            .await;

            assert_eq!(exit_code, print_signal_exit_code(signal));
            assert_eq!(exit_code, expected_code);
            assert!(print_signal_skips_flush(signal));
            assert_eq!(host.state().dispose_calls, 1, "dispose once per signal");
            assert_eq!(io.flush_calls(), expected_flushes);
            assert_eq!(io.take_overs(), 0);
        }
    });
}

/// 正常路径的 finally 会 flush（与信号路径相反：两条信号路径都不 flush）。
#[test]
fn normal_completion_flushes_stdout() {
    run(async {
        let session = MockSession::new(None, Vec::new());
        let host = MockHost::new(session);
        let io = Arc::new(CaptureStdout::default());
        let errors = Arc::new(CaptureErrors::default());

        let exit_code =
            run_print_mode_with_host(Arc::new(host), text_options(None), io.clone()).await;

        assert_eq!(exit_code, 0);
        assert_eq!(io.flush_calls(), 1);
        assert!(errors.lines().is_empty());
    });
}

// ============================================================================
// 信号（子进程；真实 sigaction/process.exit 只能在子进程里验证）
// ============================================================================

/// 子进程场景选择器（父进程通过环境变量传入）。
#[cfg(unix)]
const SIGNAL_CHILD_ENV: &str = "KIANA_RPC_PRINT_SIGNAL_CHILD";

/// 子进程测试名（父进程用 `--exact` 只跑它）。
#[cfg(unix)]
const SIGNAL_CHILD_TEST: &str = "signal_child_scenario";

/// 子进程入口。普通测试运行时环境变量缺失，立即返回。
///
/// 信号处理器触发时会 `process::exit`，因此**绝不能**在父测试进程里安装真实处理器。
#[cfg(unix)]
#[test]
fn signal_child_scenario() {
    let Ok(scenario) = std::env::var(SIGNAL_CHILD_ENV) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    let local = tokio::task::LocalSet::new();
    match scenario.as_str() {
        "sigterm_inflight" => run_inflight_signal_scenario(&runtime, &local, PrintSignal::Term),
        "sighup_inflight" => run_inflight_signal_scenario(&runtime, &local, PrintSignal::Hup),
        "restore_default" => run_restore_default_scenario(&runtime, &local),
        other => panic!("unknown signal child scenario: {other}"),
    }
}

/// 在子进程里跑一次完整的 print 模式（真实信号处理器 + mock 宿主）。
#[cfg(unix)]
fn run_signal_flow(
    runtime: &tokio::runtime::Runtime,
    local: &tokio::task::LocalSet,
    host: MockHost,
) -> i32 {
    runtime.block_on(
        local.run_until(run_print_mode_with_host_and_signal_handlers(
            Arc::new(host),
            text_options(Some("hello")),
            Arc::new(CaptureStdout::default()),
            Arc::new(CaptureErrors::default()),
        )),
    )
}

/// 先跑一次正常流程（注册 -> 清理 -> 还原处置），再跑一次“信号在 dispose 期间到达”的流程。
///
/// TS 语义：已经触发的处理器不可被 finally 的 process.off 取消，它的 dispose 链最终调用
/// `process.exit(143|129)`。修复前 `JoinHandle::abort()` 会在 dispose 的 await 点取消处理器，
/// 进程以正常退出码结束。第二次运行同时验证清理后仍能重新注册处理器（默认处置已还原）。
#[cfg(unix)]
fn run_inflight_signal_scenario(
    runtime: &tokio::runtime::Runtime,
    local: &tokio::task::LocalSet,
    signal: PrintSignal,
) {
    // 第一次运行：正常完成，并让第二次运行面对“已还原为默认处置”的进程。
    let first = run_signal_flow(
        runtime,
        local,
        MockHost::new(MockSession::new(None, Vec::new())),
    );
    assert_eq!(first, 0, "initial run must complete normally");

    // 让 prompt 等到信号处理器真正进入 dispose 再返回：main 必然在处理器完成前到达
    // finally（不依赖线程调度速度）。
    let dispose_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let session = MockSession::new(None, Vec::new());
    session.state().wait_for_dispose_started = Some(Arc::clone(&dispose_started));
    let host = MockHost::new(session);
    {
        let mut state = host.state();
        state.dispose_delay_chunks = 50; // ~500ms：main 会在处理器完成前返回
        state.dispose_started = Some(dispose_started);
    }

    let os_signal = match signal {
        PrintSignal::Term => libc::SIGTERM,
        PrintSignal::Hup => libc::SIGHUP,
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        unsafe {
            libc::kill(libc::getpid(), os_signal);
        }
    });

    let code = run_signal_flow(runtime, local, host);
    // LocalSet 仍存活：继续驱动它，让已经触发的处理器完成 dispose -> process::exit。
    runtime.block_on(local.run_until(async {
        tokio::time::sleep(std::time::Duration::from_millis(3_000)).await;
    }));
    // 修复前才会到这里（处理器被 abort，正常退出）。
    std::process::exit(code);
}

/// 清理必须等价 `process.off`：移除监听器后 SIGTERM 恢复默认处置（直接终止进程），
/// 而不是被“永不注销”的全局处理器吞掉。
#[cfg(unix)]
fn run_restore_default_scenario(runtime: &tokio::runtime::Runtime, local: &tokio::task::LocalSet) {
    // 多次运行：每次清理都必须把 SIGTERM 的处置还原为 SIG_DFL（也验证可重复注册）。
    for _ in 0..2 {
        let host = MockHost::new(MockSession::new(None, Vec::new()));
        assert_eq!(run_signal_flow(runtime, local, host), 0);
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(50));
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    });
    runtime.block_on(local.run_until(async {
        tokio::time::sleep(std::time::Duration::from_millis(2_000)).await;
    }));
    // 信号被吞（没有恢复默认处置）时才会到这里。
    std::process::exit(97);
}

/// 重新执行当前测试二进制，只跑 [`signal_child_scenario`]。
#[cfg(unix)]
fn run_signal_child(scenario: &str) -> std::process::Output {
    let executable = std::env::current_exe().expect("current test executable");
    std::process::Command::new(executable)
        .arg("--exact")
        .arg(SIGNAL_CHILD_TEST)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(SIGNAL_CHILD_ENV, scenario)
        .output()
        .expect("spawn signal child")
}

#[cfg(unix)]
fn child_stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// 回归（review finding 1，major）：SIGTERM 在 dispose 期间到达时，finally 的清理不得
/// 取消已经触发的处理器；dispose 完成后必须 `process::exit(143)`。
#[cfg(unix)]
#[test]
fn sigterm_during_inflight_dispose_still_exits_143() {
    let output = run_signal_child("sigterm_inflight");
    assert_eq!(
        output.status.code(),
        Some(print_signal_exit_code(PrintSignal::Term)),
        "stderr: {}",
        child_stderr(&output)
    );
}

/// 同一语义的 SIGHUP 分支：`process.exit(129)`。
#[cfg(unix)]
#[test]
fn sighup_during_inflight_dispose_still_exits_129() {
    let output = run_signal_child("sighup_inflight");
    assert_eq!(
        output.status.code(),
        Some(print_signal_exit_code(PrintSignal::Hup)),
        "stderr: {}",
        child_stderr(&output)
    );
}

/// 回归（review finding 2）：清理等价 `process.off`——移除监听器后恢复 SIGTERM 的默认处置。
/// 修复前 tokio 的全局 libc 处理器永不注销，SIGTERM 被吞掉（进程正常退出 97）。
#[cfg(unix)]
#[test]
fn removing_the_signal_handler_restores_the_default_disposition() {
    use std::os::unix::process::ExitStatusExt;

    let output = run_signal_child("restore_default");
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGTERM),
        "expected default SIGTERM termination, status {:?}, stderr: {}",
        output.status.code(),
        child_stderr(&output)
    );
}

// ============================================================================
// 回归（评审发现 1）：JSON 模式的 agent 背压订阅
// ============================================================================

/// 构造一个立即完成一次 assistant 消息的真实 [`kiana_agent_core::Agent`]。
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

/// TS 的 `mode === "json"` 分支：`session.agent.subscribe(async () => await
/// waitForRawStdoutBackpressure())`。agent 事件派发必须等 stdout 写链。
#[test]
fn json_mode_awaits_stdout_backpressure_for_agent_events() {
    run(async {
        let session = MockSession::new(None, Vec::new());
        session.state().agent = Some(backpressure_test_agent());
        let host = Arc::new(MockHost::new(session));
        let io = Arc::new(CaptureStdout::default());
        let gate = Arc::new(BackpressureGate::default());
        *lock(&io.backpressure_gate) = Some(Arc::clone(&gate));

        let run_future = tokio::task::spawn_local(run_print_mode_with_host(
            Arc::clone(&host) as Arc<dyn PrintRuntimeHost>,
            json_options(vec!["hi"]),
            Arc::clone(&io) as Arc<dyn RawStdout>,
        ));
        wait_until(|| *lock(&io.wait_calls) >= 1).await;
        assert!(
            !run_future.is_finished(),
            "agent events must await stdout backpressure while it is blocked"
        );

        gate.release();
        let exit_code = tokio::time::timeout(std::time::Duration::from_secs(2), run_future)
            .await
            .expect("the print run completes once backpressure is released")
            .expect("run future");
        assert_eq!(exit_code, 0);
        assert_eq!(host.state().dispose_calls, 1);
    });
}
