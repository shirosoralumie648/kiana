//! 命名轨迹的确定性 mock provider（评测与 loop-guard 测试用）。
//!
//! 与 [`super::faux`] 的区别：
//!
//! - **命名轨迹**：每个轨迹有名字，可以独立注册/重置/读取请求记录；全局注册表
//!   [`MockLlmAdmin::global`] 让不同测试/评测任务共用一套控制面而不互相覆盖（用名字隔离）。
//! - **请求记录**：每次 `stream`/`stream_simple` 都会在消费脚本之前记录
//!   [`MockRequestRecord`]（消息、prompt 文本、工具名、changed-files、affected-tests），
//!   测试可以直接断言「模型看到了什么」，而不是只能断言「模型回了什么」。
//! - **可预测的用量/费用**：没有显式 `usage` 时按 prompt/输出文本估算 token，并用模型目录价
//!   计算 cost；显式 `usage` 原样保留。未知价格仍是 0，但调用方可以从模型费用表判断
//!   `cost_known`。
//! - **changed-files → affected-tests**：[`AffectedTestMap`] 把轨迹里写过的文件映射到应该
//!   运行的测试分片；模拟的「测试命令」轨迹可以据此决定下一轮回复。
//!
//! 本模块不做任何网络请求，也不需要 API key：`create_provider` 注册的 auth 固定返回
//! `ModelAuth::default()`。它可以直接作为 [`ProviderStreams`] 交给上层，也可以被
//! `kiana-agent-core` 的测试包装成 `StreamFn` 跑完整工具循环。
//!
//! # 轨迹模型
//!
//! [`MockTrajectory`] 是一串 [`MockStep`]：普通响应按顺序消费，动态响应用闭包根据当前
//! [`MockRequestRecord`] 生成响应。队列耗尽时返回 `stop_reason = "error"` 的助手消息，
//! 文本是 `No more mock responses queued for trajectory "<name>"`，不会 panic。
//!
//! # 并发
//!
//! 轨迹状态（游标、请求记录、调用计数）经 `Arc<Mutex<_>>` 共享；`MockLlm` 的 clone 看到同一
//! 份状态。取脚本与自增序号在同一临界区内完成，因此并发请求拿到的顺序是确定的锁顺序。
//! 动态响应闭包在锁外调用，避免用户代码重入死锁。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;

use crate::api::lazy::spawn_forward;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use crate::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::models::{CreateProviderOptions, Provider, ProviderStreams, create_provider};
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, DoneReason,
    ErrorReason, Message, Model, ModelCost, SimpleStreamOptions, StopReason, StreamOptions,
    TextContent, TextKind, ThinkingContent, ThinkingKind, ToolCall, ToolCallKind, Usage, UsageCost,
};
use crate::utils::estimate::{estimate_context_tokens, estimate_message_tokens};
use crate::utils::text::{
    content_text_assistant, content_text_user, content_text_user_content, get_system_message_text,
};

// ---------------------------------------------------------------------------
// 公共类型
// ---------------------------------------------------------------------------

/// mock provider 的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MockError {
    /// 参数/注册表错误。
    #[error("{0}")]
    Message(String),
    /// 轨迹不存在。
    #[error("unknown mock trajectory: {0}")]
    UnknownTrajectory(String),
}

impl MockError {
    fn message(message: impl Into<String>) -> Self {
        MockError::Message(message.into())
    }
}

/// 一条被 mock provider 记录的请求。
#[derive(Debug, Clone, PartialEq)]
pub struct MockRequestRecord {
    /// 轨迹名。
    pub trajectory: String,
    /// 该轨迹内的 1-based 请求序号。
    pub sequence: u64,
    /// 请求模型 id。
    pub model: String,
    /// provider id。
    pub provider: String,
    /// api id。
    pub api: String,
    /// 归一化后的消息（prompt 中 system/tools 已折叠进首条 system 消息）。
    pub messages: Vec<Message>,
    /// 所有消息的纯文本拼接（system/user/assistant/toolResult，按顺序）。
    pub prompt_text: String,
    /// 本次请求可见的工具名（顶层 `tools` + system 消息里的 `toolsAdded`）。
    pub tool_names: Vec<String>,
    /// 从 transcript 的 assistant tool call 参数里提取到的文件路径。
    pub changed_files: Vec<String>,
    /// 由 [`AffectedTestMap`] 计算出的受影响测试（没有配置映射时为空）。
    pub affected_tests: Vec<String>,
}

impl MockRequestRecord {
    /// 最后一条 user/toolResult 消息的文本；动态轨迹常用它判断「上一轮失败反馈」。
    pub fn last_user_text(&self) -> String {
        for message in self.messages.iter().rev() {
            match message {
                Message::User(user) => {
                    return content_text_user_content(&user.content, "\n");
                }
                Message::ToolResult(result) => {
                    return content_text_user(&result.content, "\n");
                }
                _ => {}
            }
        }
        String::new()
    }

    /// prompt 中是否包含某段文本（大小写敏感的普通子串匹配）。
    pub fn prompt_contains(&self, needle: &str) -> bool {
        self.prompt_text.contains(needle)
    }
}

/// mock 响应里的一个内容块。
#[derive(Debug, Clone, PartialEq)]
pub enum MockContent {
    /// 文本块。
    Text(String),
    /// 思考块。
    Thinking(String),
    /// 工具调用。
    ToolCall {
        /// 调用 id。
        id: String,
        /// 工具名。
        name: String,
        /// 参数对象。
        arguments: serde_json::Map<String, JsonValue>,
    },
}

/// 一条脚本化的助手响应。
#[derive(Debug, Clone, PartialEq)]
pub struct MockResponse {
    /// 内容块。
    pub content: Vec<MockContent>,
    /// 结束原因；`None` 时按内容推断（有 tool call → `ToolUse`，否则 `Stop`）。
    pub stop_reason: Option<StopReason>,
    /// 显式 usage；`None` 时按 prompt/输出文本估算。
    pub usage: Option<Usage>,
    /// 失败文本（同时把 stop_reason 设为 `Error`）。
    pub error_message: Option<String>,
}

impl Default for MockResponse {
    fn default() -> Self {
        MockResponse {
            content: Vec::new(),
            stop_reason: None,
            usage: None,
            error_message: None,
        }
    }
}

impl MockResponse {
    /// 纯文本响应。
    pub fn text(text: impl Into<String>) -> Self {
        MockResponse {
            content: vec![MockContent::Text(text.into())],
            ..MockResponse::default()
        }
    }

    /// 纯思考响应（`stop_reason = Stop`）。
    pub fn thinking(text: impl Into<String>) -> Self {
        MockResponse {
            content: vec![MockContent::Thinking(text.into())],
            ..MockResponse::default()
        }
    }

    /// 单个工具调用。
    pub fn tool_call(
        name: impl Into<String>,
        arguments: serde_json::Map<String, JsonValue>,
    ) -> Self {
        MockResponse {
            content: vec![MockContent::ToolCall {
                id: "mock-call-1".to_string(),
                name: name.into(),
                arguments,
            }],
            ..MockResponse::default()
        }
    }

    /// 多个工具调用（同一批）。
    pub fn tool_calls(calls: Vec<(&str, serde_json::Map<String, JsonValue>)>) -> Self {
        MockResponse {
            content: calls
                .into_iter()
                .enumerate()
                .map(|(index, (name, arguments))| MockContent::ToolCall {
                    id: format!("mock-call-{}", index + 1),
                    name: name.to_string(),
                    arguments,
                })
                .collect(),
            ..MockResponse::default()
        }
    }

    /// 追加一个文本块。
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.content.push(MockContent::Text(text.into()));
        self
    }

    /// 设置显式 usage。
    pub fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// 设置结束原因。
    pub fn with_stop_reason(mut self, reason: StopReason) -> Self {
        self.stop_reason = Some(reason);
        self
    }

    /// 设置失败文本。
    pub fn with_error(mut self, message: impl Into<String>) -> Self {
        self.error_message = Some(message.into());
        self.stop_reason = Some(StopReason::Error);
        self
    }
}

/// 动态响应闭包。
pub type MockResponder = Arc<dyn Fn(&MockRequestRecord) -> MockResponse + Send + Sync>;

/// 轨迹中的一步。
#[derive(Clone)]
pub enum MockStep {
    /// 固定响应。
    Respond(MockResponse),
    /// 按请求动态生成响应。
    Dynamic(MockResponder),
}

impl From<MockResponse> for MockStep {
    fn from(response: MockResponse) -> Self {
        MockStep::Respond(response)
    }
}

impl MockStep {
    /// 动态步骤。
    pub fn dynamic<F>(responder: F) -> Self
    where
        F: Fn(&MockRequestRecord) -> MockResponse + Send + Sync + 'static,
    {
        MockStep::Dynamic(Arc::new(responder))
    }
}

/// 命名轨迹：一串按顺序消费的步骤。
#[derive(Clone, Default)]
pub struct MockTrajectory {
    /// 步骤列表。
    pub steps: Vec<MockStep>,
}

impl MockTrajectory {
    /// 空轨迹。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用步骤构造。
    pub fn from_steps(steps: Vec<MockStep>) -> Self {
        MockTrajectory { steps }
    }

    /// 追加一步。
    pub fn push(mut self, step: impl Into<MockStep>) -> Self {
        self.steps.push(step.into());
        self
    }

    /// 追加一条文本响应。
    pub fn then_text(self, text: impl Into<String>) -> Self {
        self.push(MockStep::Respond(MockResponse::text(text)))
    }
}

/// changed-files → affected-tests 映射。
///
/// 规则按「文件路径前缀 → 测试路径列表」匹配。行为约定：
///
/// - 命中规则的源文件贡献对应测试分片；所有结果都追加 `always`（通常是回归测试）；
/// - 修改 `tests/...` 目录时，选择它所处的目录分片并追加回归测试；
/// - 有任一源文件未命中任何规则（跨切面改动）时返回 `["__ALL__"]`；
/// - 空输入返回空列表。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AffectedTestMap {
    /// 前缀规则。
    pub rules: Vec<AffectedTestRule>,
    /// 每次都追加的测试（通常是 regressions）。
    pub always: Vec<String>,
    /// 未映射时是否返回 `__ALL__`（默认 true）。
    pub unmapped_runs_all: bool,
}

/// 一条 affected-tests 规则。
#[derive(Debug, Clone, PartialEq)]
pub struct AffectedTestRule {
    /// 文件路径前缀（`/` 分隔）。
    pub prefix: String,
    /// 命中后要运行的测试路径。
    pub tests: Vec<String>,
}

impl AffectedTestMap {
    /// 构造映射（未映射默认跑全量）。
    pub fn new(rules: Vec<AffectedTestRule>, always: Vec<String>) -> Self {
        AffectedTestMap {
            rules,
            always,
            unmapped_runs_all: true,
        }
    }

    /// 从 JSON 读取：
    ///
    /// ```json
    /// {
    ///   "rules": [{"path": "src/settings", "tests": ["tests/e2e/settings"]}],
    ///   "always": ["tests/e2e/regressions"],
    ///   "unmappedRunsAll": true
    /// }
    /// ```
    pub fn from_json(value: &JsonValue) -> Result<Self, MockError> {
        let object = value
            .as_object()
            .ok_or_else(|| MockError::message("affected-test map must be a JSON object"))?;
        let mut rules = Vec::new();
        if let Some(entries) = object.get("rules") {
            let entries = entries
                .as_array()
                .ok_or_else(|| MockError::message("affected-test map rules must be an array"))?;
            for entry in entries {
                let entry = entry.as_object().ok_or_else(|| {
                    MockError::message("affected-test map rule must be an object")
                })?;
                let prefix = entry
                    .get("path")
                    .or_else(|| entry.get("prefix"))
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        MockError::message("affected-test map rule is missing a string path")
                    })?;
                let tests = entry
                    .get("tests")
                    .and_then(JsonValue::as_array)
                    .ok_or_else(|| {
                        MockError::message("affected-test map rule is missing a tests array")
                    })?
                    .iter()
                    .map(|test| {
                        test.as_str().map(str::to_string).ok_or_else(|| {
                            MockError::message("affected-test map test must be a string")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                rules.push(AffectedTestRule {
                    prefix: normalize_path(prefix),
                    tests,
                });
            }
        }
        let always = object
            .get("always")
            .and_then(JsonValue::as_array)
            .map(|tests| {
                tests
                    .iter()
                    .map(|test| {
                        test.as_str().map(str::to_string).ok_or_else(|| {
                            MockError::message("affected-test map always entry must be a string")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let unmapped_runs_all = object
            .get("unmappedRunsAll")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true);
        Ok(AffectedTestMap {
            rules,
            always,
            unmapped_runs_all,
        })
    }

    /// 解析 changed files，返回去重、排序后的测试路径。
    pub fn resolve(&self, changed_files: &[String]) -> Vec<String> {
        if changed_files.is_empty() {
            return Vec::new();
        }
        let mut selected: Vec<String> = Vec::new();
        let mut unmatched = false;
        for file in changed_files {
            let file = normalize_path(file);
            if file.starts_with("tests/") || file.starts_with("test/") {
                if let Some(parent) = file.rsplit_once('/').map(|(parent, _)| parent) {
                    selected.push(parent.to_string());
                }
                continue;
            }
            let matching: Vec<&AffectedTestRule> = self
                .rules
                .iter()
                .filter(|rule| path_has_prefix(&file, &rule.prefix))
                .collect();
            if matching.is_empty() {
                unmatched = true;
            } else {
                for rule in matching {
                    selected.extend(rule.tests.iter().cloned());
                }
            }
        }
        if unmatched && self.unmapped_runs_all {
            return vec!["__ALL__".to_string()];
        }
        selected.extend(self.always.iter().cloned());
        selected.sort();
        selected.dedup();
        selected
    }
}

/// mock 模型选项。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MockModelOptions {
    /// 模型 id；`None` → `mock-<trajectory>`。
    pub id: Option<String>,
    /// 展示名；`None` → id。
    pub name: Option<String>,
    /// 价格表；`None` → 全零（未知价格）。
    pub cost: Option<ModelCost>,
    /// 上下文窗口；`None` → 128000。
    pub context_window: Option<u64>,
    /// 最大输出；`None` → 16384。
    pub max_tokens: Option<u64>,
    /// changed-files 映射。
    pub affected_tests: Option<AffectedTestMap>,
}

/// `ProviderStreams` 实现：按命名轨迹回放响应。
#[derive(Clone)]
pub struct MockLlm {
    name: String,
    api: String,
    provider: String,
    state: Arc<Mutex<TrajectoryState>>,
    model: Model,
    affected_tests: Option<AffectedTestMap>,
}

struct TrajectoryState {
    steps: Vec<MockStep>,
    cursor: usize,
    call_count: AtomicU64,
    requests: Vec<MockRequestRecord>,
}

impl std::fmt::Debug for MockLlm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MockLlm")
            .field("name", &self.name)
            .field("api", &self.api)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// `create_provider` 之后的句柄。
#[derive(Clone)]
pub struct MockProviderHandle {
    /// 可注册进模型运行时的 provider。
    pub provider: Arc<dyn Provider>,
    /// 直接可用的流实现（测试里包装成 `StreamFn` 时不必经 provider 路由）。
    pub core: MockLlm,
    /// 该 provider 的唯一模型。
    pub model: Model,
}

impl MockProviderHandle {
    /// 模型 id。
    pub fn model_id(&self) -> &str {
        &self.model.id
    }
}

/// 命名轨迹的全局/局部控制面。
#[derive(Clone)]
pub struct MockLlmAdmin {
    registry: Arc<Mutex<HashMap<String, Arc<Mutex<TrajectoryState>>>>>,
}

impl std::fmt::Debug for MockLlmAdmin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MockLlmAdmin")
            .field("trajectories", &self.trajectory_names())
            .finish()
    }
}

impl Default for MockLlmAdmin {
    fn default() -> Self {
        Self::new()
    }
}

impl MockLlmAdmin {
    /// 一个新的、隔离的注册表（测试推荐）。
    pub fn new() -> Self {
        MockLlmAdmin {
            registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 进程级共享注册表。
    pub fn global() -> &'static MockLlmAdmin {
        static GLOBAL: OnceLock<MockLlmAdmin> = OnceLock::new();
        GLOBAL.get_or_init(MockLlmAdmin::new)
    }

    /// 注册（或替换）一条命名轨迹；同名旧状态与请求记录被丢弃。
    pub fn register(&self, name: impl Into<String>, trajectory: MockTrajectory) {
        let mut registry = lock(&self.registry);
        registry.insert(
            name.into(),
            Arc::new(Mutex::new(TrajectoryState {
                steps: trajectory.steps,
                cursor: 0,
                call_count: AtomicU64::new(0),
                requests: Vec::new(),
            })),
        );
    }

    /// 重置轨迹：游标归零、清空请求记录、调用计数归零；步骤保留。
    pub fn reset(&self, name: &str) -> Result<(), MockError> {
        let state = self.state(name)?;
        let mut state = lock_state(&state);
        state.cursor = 0;
        state.requests.clear();
        state.call_count.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// 重置所有轨迹。
    pub fn reset_all(&self) {
        let states: Vec<Arc<Mutex<TrajectoryState>>> =
            lock(&self.registry).values().cloned().collect();
        for state in states {
            let mut state = lock_state(&state);
            state.cursor = 0;
            state.requests.clear();
            state.call_count.store(0, Ordering::SeqCst);
        }
    }

    /// 取消注册。
    pub fn unregister(&self, name: &str) -> bool {
        lock(&self.registry).remove(name).is_some()
    }

    /// 注册表里的轨迹名（排序后，便于快照/断言）。
    pub fn trajectory_names(&self) -> Vec<String> {
        let mut names: Vec<String> = lock(&self.registry).keys().cloned().collect();
        names.sort();
        names
    }

    /// 该轨迹已经收到的请求记录（副本）。
    pub fn requests(&self, name: &str) -> Result<Vec<MockRequestRecord>, MockError> {
        let state = self.state(name)?;
        Ok(lock_state(&state).requests.clone())
    }

    /// 清空请求记录（不动游标）。
    pub fn clear_requests(&self, name: &str) -> Result<(), MockError> {
        let state = self.state(name)?;
        lock_state(&state).requests.clear();
        Ok(())
    }

    /// 已经消费的请求数。
    pub fn call_count(&self, name: &str) -> Result<u64, MockError> {
        let state = self.state(name)?;
        let state = lock_state(&state);
        Ok(state.call_count.load(Ordering::SeqCst))
    }

    /// 剩余脚本步数。
    pub fn pending_steps(&self, name: &str) -> Result<usize, MockError> {
        let state = self.state(name)?;
        let state = lock_state(&state);
        Ok(state.steps.len().saturating_sub(state.cursor))
    }

    /// 为一个命名轨迹创建 provider + 模型。
    pub fn provider(
        &self,
        name: &str,
        options: MockModelOptions,
    ) -> Result<MockProviderHandle, MockError> {
        let state = self.state(name)?;
        let api = format!("mock:{name}");
        let provider_id = format!("mock-provider:{name}");
        let model_id = options.id.unwrap_or_else(|| format!("mock-{name}"));
        let model = Model {
            id: model_id.clone(),
            name: options.name.unwrap_or_else(|| model_id.clone()),
            api: api.clone(),
            provider: provider_id.clone(),
            base_url: "mock://localhost".to_string(),
            input: vec![crate::types::ModelInputModality::Text],
            cost: options.cost.unwrap_or_default(),
            context_window: Some(options.context_window.unwrap_or(128_000)),
            max_tokens: Some(options.max_tokens.unwrap_or(16_384)),
            ..Model::default()
        };
        let core = MockLlm {
            name: name.to_string(),
            api,
            provider: provider_id.clone(),
            state,
            model: model.clone(),
            affected_tests: options.affected_tests,
        };
        let streams: Arc<dyn ProviderStreams> = Arc::new(core.clone());
        let provider = create_provider(CreateProviderOptions {
            id: provider_id,
            name: Some(format!("Mock ({name})")),
            auth: ProviderAuth {
                api_key: Some(Arc::new(MockApiKeyAuth)),
                oauth: None,
            },
            models: vec![crate::types::AnyModel::Chat(model.clone())],
            api: Some(streams.into()),
            ..Default::default()
        })
        .map_err(|error| MockError::message(error.to_string()))?;
        Ok(MockProviderHandle {
            provider,
            core,
            model,
        })
    }

    fn state(&self, name: &str) -> Result<Arc<Mutex<TrajectoryState>>, MockError> {
        lock(&self.registry)
            .get(name)
            .cloned()
            .ok_or_else(|| MockError::UnknownTrajectory(name.to_string()))
    }
}

impl MockLlm {
    /// 轨迹名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 该轨迹当前使用的模型。
    pub fn model(&self) -> &Model {
        &self.model
    }

    fn stream_internal(
        &self,
        model: &Model,
        context: &crate::types::TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let stream = create_assistant_message_event_stream();
        // 记录发生在消费脚本之前：同步返回后测试即可断言请求已到达 provider。
        let (record, step) = {
            let mut state = lock_state(&self.state);
            let sequence = state.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            let record = self.record_request(sequence, model, context);
            state.requests.push(record.clone());
            let step = state.steps.get(state.cursor).cloned();
            if step.is_some() {
                state.cursor += 1;
            }
            (record, step)
        };

        let core = self.clone();
        let task_stream = stream.clone();
        let request_model = model.clone();
        let task_context = context.clone();
        let signal = options
            .as_ref()
            .and_then(|options| options.base.signal.clone());
        spawn_forward(async move {
            let response = match step {
                Some(MockStep::Respond(response)) => response,
                Some(MockStep::Dynamic(responder)) => responder(&record),
                None => MockResponse::text(format!(
                    "No more mock responses queued for trajectory \"{}\"",
                    core.name
                ))
                .with_error(format!(
                    "No more mock responses queued for trajectory \"{}\"",
                    core.name
                )),
            };
            core.push_response(
                &task_stream,
                &request_model,
                &task_context,
                response,
                signal.as_ref(),
            )
            .await;
        });
        stream
    }

    fn record_request(
        &self,
        sequence: u64,
        model: &Model,
        context: &crate::types::TranscriptContext,
    ) -> MockRequestRecord {
        let messages = context.as_context().messages.clone();
        let mut prompt_parts = Vec::with_capacity(messages.len());
        let mut tool_names = Vec::new();
        if let Some(tools) = &context.as_context().tools {
            tool_names.extend(tools.iter().map(|tool| tool.name.clone()));
        }
        for message in &messages {
            let text = match message {
                Message::System(system) => {
                    if let Some(tools) = &system.tools_added {
                        tool_names.extend(tools.iter().map(|tool| tool.name.clone()));
                    }
                    get_system_message_text(system)
                }
                Message::User(user) => content_text_user_content(&user.content, "\n"),
                Message::Assistant(assistant) => content_text_assistant(&assistant.content, "\n"),
                Message::ToolResult(result) => content_text_user(&result.content, "\n"),
            };
            prompt_parts.push(text);
        }
        tool_names.sort();
        tool_names.dedup();
        let changed_files = changed_files_from_messages(&messages);
        let affected_tests = self
            .affected_tests
            .as_ref()
            .map(|map| map.resolve(&changed_files))
            .unwrap_or_default();
        MockRequestRecord {
            trajectory: self.name.clone(),
            sequence,
            model: model.id.clone(),
            provider: model.provider.clone(),
            api: model.api.clone(),
            messages,
            prompt_text: prompt_parts.join("\n"),
            tool_names,
            changed_files,
            affected_tests,
        }
    }

    async fn push_response(
        &self,
        stream: &AssistantMessageEventStream,
        model: &Model,
        context: &crate::types::TranscriptContext,
        response: MockResponse,
        signal: Option<&CancellationToken>,
    ) {
        let mut message = self.build_message(model, context, response);
        if signal.is_some_and(CancellationToken::is_cancelled) {
            message.stop_reason = StopReason::Aborted;
            message.error_message = Some("Request aborted".to_string());
            let _ = stream.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Aborted,
                error: Box::new(message.clone()),
            });
            stream.end(message);
            return;
        }
        let reason = done_reason(message.stop_reason);
        let mut partial = AssistantMessage {
            content: Vec::new(),
            stop_reason: StopReason::Pending,
            ..message.clone()
        };
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(partial.clone()),
        });
        for (index, block) in message.content.iter().enumerate() {
            if signal.is_some_and(CancellationToken::is_cancelled) {
                let mut aborted = message.clone();
                aborted.stop_reason = StopReason::Aborted;
                aborted.error_message = Some("Request aborted".to_string());
                let _ = stream.push(AssistantMessageEvent::Error {
                    reason: ErrorReason::Aborted,
                    error: Box::new(aborted.clone()),
                });
                stream.end(aborted);
                return;
            }
            match block {
                AssistantContentBlock::Text(text) => {
                    partial
                        .content
                        .push(AssistantContentBlock::Text(text.clone()));
                    let _ = stream.push(AssistantMessageEvent::TextStart {
                        content_index: index,
                        partial: Box::new(partial.clone()),
                    });
                    let _ = stream.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: text.text.clone(),
                        partial: Box::new(partial.clone()),
                    });
                    let _ = stream.push(AssistantMessageEvent::TextEnd {
                        content_index: index,
                        content: text.text.clone(),
                        partial: Box::new(partial.clone()),
                    });
                }
                AssistantContentBlock::Thinking(thinking) => {
                    partial
                        .content
                        .push(AssistantContentBlock::Thinking(thinking.clone()));
                    let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                        content_index: index,
                        partial: Box::new(partial.clone()),
                    });
                    let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: thinking.thinking.clone(),
                        partial: Box::new(partial.clone()),
                    });
                    let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                        content_index: index,
                        content: thinking.thinking.clone(),
                        partial: Box::new(partial.clone()),
                    });
                }
                AssistantContentBlock::ToolCall(tool_call) => {
                    partial
                        .content
                        .push(AssistantContentBlock::ToolCall(tool_call.clone()));
                    let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                        content_index: index,
                        partial: Box::new(partial.clone()),
                    });
                    let arguments =
                        serde_json::to_value(&tool_call.arguments).unwrap_or(JsonValue::Null);
                    let delta = kiana_json::js_object::stringify(&arguments);
                    let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta,
                        partial: Box::new(partial.clone()),
                    });
                    let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                        content_index: index,
                        tool_call: tool_call.clone(),
                        partial: Box::new(partial.clone()),
                    });
                }
            }
        }
        if message.stop_reason == StopReason::Error {
            let _ = stream.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: Box::new(message.clone()),
            });
        } else {
            let _ = stream.push(AssistantMessageEvent::Done {
                reason,
                message: Box::new(message.clone()),
            });
        }
        stream.end(message);
    }

    fn build_message(
        &self,
        model: &Model,
        context: &crate::types::TranscriptContext,
        response: MockResponse,
    ) -> AssistantMessage {
        let content: Vec<AssistantContentBlock> = response
            .content
            .into_iter()
            .map(|content| match content {
                MockContent::Text(text) => AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text,
                    text_signature: None,
                }),
                MockContent::Thinking(thinking) => {
                    AssistantContentBlock::Thinking(ThinkingContent {
                        kind_thinking: ThinkingKind::Thinking,
                        thinking,
                        thinking_signature: None,
                        redacted: None,
                    })
                }
                MockContent::ToolCall {
                    id,
                    name,
                    arguments,
                } => AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: ToolCallKind::ToolCall,
                    id,
                    name,
                    arguments: arguments.into_iter().collect(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                }),
            })
            .collect();
        let has_tool_call = content
            .iter()
            .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)));
        let stop_reason = response.stop_reason.unwrap_or(if has_tool_call {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        });
        let mut message = AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: Some(model.id.clone()),
            response_id: Some(format!("mock-response-{}", self.sequence_hint())),
            usage: Usage::default(),
            stop_reason,
            error_message: response.error_message,
            timestamp: now_unix_ms(),
            ..Default::default()
        };
        message.usage = match response.usage {
            Some(mut usage) => {
                normalize_usage(&mut usage);
                usage
            }
            None => estimate_usage(model, context, &message),
        };
        message
    }

    fn sequence_hint(&self) -> u64 {
        let state = lock_state(&self.state);
        state.call_count.load(Ordering::SeqCst)
    }
}

impl ProviderStreams for MockLlm {
    fn stream(
        &self,
        model: &Model,
        context: &crate::types::TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // `StreamOptions` 与 `SimpleStreamOptions` 的 base 相同；mock 不解释 provider 专属
        // 字段，因此直接把它作为 simple 的 base。
        self.stream_internal(
            model,
            context,
            Some(SimpleStreamOptions {
                base: options,
                ..Default::default()
            }),
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &crate::types::TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.stream_internal(model, context, Some(options))
    }
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

struct MockApiKeyAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for MockApiKeyAuth {
    fn name(&self) -> &str {
        "Mock"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn lock_state(state: &Mutex<TrajectoryState>) -> MutexGuard<'_, TrajectoryState> {
    lock(state)
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn done_reason(reason: StopReason) -> DoneReason {
    match reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

fn normalize_usage(usage: &mut Usage) {
    if usage.total_tokens == 0 {
        usage.total_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
    }
}

fn estimate_usage(
    model: &Model,
    context: &crate::types::TranscriptContext,
    message: &AssistantMessage,
) -> Usage {
    let input = estimate_context_tokens(context).tokens;
    let output = estimate_message_tokens(&Message::Assistant(message.clone()));
    let mut usage = Usage {
        input,
        output,
        total_tokens: input + output,
        cost: UsageCost::default(),
        ..Usage::default()
    };
    let _ = crate::models::cost::calculate_cost(model, &mut usage);
    usage
}

/// 从 assistant tool call 的参数里提取文件路径。
fn changed_files_from_messages(messages: &[Message]) -> Vec<String> {
    let mut files = Vec::new();
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for block in &assistant.content {
            let AssistantContentBlock::ToolCall(call) = block else {
                continue;
            };
            collect_paths_from_value(
                &JsonValue::Object(call.arguments.clone().into_iter().collect()),
                &mut files,
            );
        }
    }
    files.sort();
    files.dedup();
    files
}

fn collect_paths_from_value(value: &JsonValue, files: &mut Vec<String>) {
    match value {
        JsonValue::Object(object) => {
            for (key, item) in object {
                let lower = key.to_ascii_lowercase();
                let looks_like_path = lower == "path"
                    || lower == "file_path"
                    || lower == "filepath"
                    || lower == "file"
                    || lower.ends_with("_path")
                    || lower.ends_with("_file");
                if looks_like_path && let Some(path) = item.as_str() {
                    files.push(normalize_path(path));
                }
                collect_paths_from_value(item, files);
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                collect_paths_from_value(item, files);
            }
        }
        _ => {}
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessageEvent, Context, ModelInputModality, TextContent, TextKind, ToolCall,
        ToolCallKind, UserContent, UserMessage, UserRole,
    };
    use crate::utils::transcript::normalize_context;

    fn user_context(text: &str) -> crate::types::TranscriptContext {
        normalize_context(&Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text(text.to_string()),
                timestamp: 1,
            })],
            tools: None,
        })
    }

    async fn collect(
        stream: AssistantMessageEventStream,
    ) -> (Vec<AssistantMessageEvent>, AssistantMessage) {
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
        let message = stream.result().await.expect("mock result");
        (events, message)
    }

    #[tokio::test]
    async fn named_trajectory_replays_steps_and_records_requests() {
        let admin = MockLlmAdmin::new();
        admin.register(
            "demo",
            MockTrajectory::new().then_text("first").then_text("second"),
        );
        let handle = admin.provider("demo", MockModelOptions::default()).unwrap();
        let context = user_context("hello");
        let (events, first) = collect(handle.core.stream_simple(
            &handle.model,
            &context,
            SimpleStreamOptions::default(),
        ))
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AssistantMessageEvent::Done { .. }))
        );
        assert_eq!(first.stop_reason, StopReason::Stop);
        assert!(matches!(
            &first.content[0],
            AssistantContentBlock::Text(TextContent { text, .. }) if text == "first"
        ));

        let requests = admin.requests("demo").unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].sequence, 1);
        assert_eq!(requests[0].trajectory, "demo");
        assert!(requests[0].prompt_text.contains("hello"));
        assert_eq!(requests[0].model, handle.model.id);
        assert_eq!(admin.call_count("demo").unwrap(), 1);
        assert_eq!(admin.pending_steps("demo").unwrap(), 1);

        admin.reset("demo").unwrap();
        assert_eq!(admin.requests("demo").unwrap().len(), 0);
        assert_eq!(admin.pending_steps("demo").unwrap(), 2);
        let (_, again) = collect(handle.core.stream_simple(
            &handle.model,
            &context,
            SimpleStreamOptions::default(),
        ))
        .await;
        assert!(matches!(
            &again.content[0],
            AssistantContentBlock::Text(TextContent { text, .. }) if text == "first"
        ));
    }

    #[tokio::test]
    async fn dynamic_responder_sees_previous_tool_result_and_changed_files() {
        let admin = MockLlmAdmin::new();
        admin.register(
            "dynamic",
            MockTrajectory::new()
                .push(MockStep::Respond(MockResponse::text("start")))
                .push(MockStep::dynamic(|request: &MockRequestRecord| {
                    if request.last_user_text().contains("FAILED") {
                        MockResponse::text("fixed")
                    } else {
                        MockResponse::text("retry")
                    }
                })),
        );
        let handle = admin
            .provider("dynamic", MockModelOptions::default())
            .unwrap();

        // 请求 1。
        let (_, first) = collect(handle.core.stream_simple(
            &handle.model,
            &user_context("do the thing"),
            SimpleStreamOptions::default(),
        ))
        .await;
        assert_eq!(first.stop_reason, StopReason::Stop);

        // 请求 2：context 里带一条 FAILED 的 tool result。
        let mut messages = vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("do the thing".to_string()),
                timestamp: 1,
            }),
            Message::ToolResult(crate::types::ToolResultMessage {
                role: crate::types::ToolResultRole::ToolResult,
                tool_call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                content: vec![crate::types::UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "FAILED: expected 42".to_string(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: true,
                timestamp: 2,
                duration_ms: None,
            }),
        ];
        let context = normalize_context(&Context {
            system_prompt: None,
            messages: std::mem::take(&mut messages),
            tools: None,
        });
        let (_, second) = collect(handle.core.stream_simple(
            &handle.model,
            &context,
            SimpleStreamOptions::default(),
        ))
        .await;
        assert!(matches!(
            &second.content[0],
            AssistantContentBlock::Text(TextContent { text, .. }) if text == "fixed"
        ));
        let requests = admin.requests("dynamic").unwrap();
        assert_eq!(requests[1].last_user_text(), "FAILED: expected 42");
    }

    #[tokio::test]
    async fn changed_files_are_mapped_to_affected_tests() {
        let admin = MockLlmAdmin::new();
        admin.register("mapped", MockTrajectory::new().then_text("ok"));
        let handle = admin
            .provider(
                "mapped",
                MockModelOptions {
                    affected_tests: Some(AffectedTestMap::new(
                        vec![AffectedTestRule {
                            prefix: "src/settings".to_string(),
                            tests: vec!["tests/settings".to_string()],
                        }],
                        vec!["tests/regressions".to_string()],
                    )),
                    ..Default::default()
                },
            )
            .unwrap();
        let assistant = AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "call-1".to_string(),
                name: "write".to_string(),
                arguments: [(
                    "path".to_string(),
                    JsonValue::String("src/settings/form.ts".to_string()),
                )]
                .into_iter()
                .collect(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            })],
            api: "mock:mapped".to_string(),
            provider: "mock-provider:mapped".to_string(),
            model: "mock-mapped".to_string(),
            ..AssistantMessage::default()
        };
        let context = normalize_context(&Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text("edit the form".to_string()),
                    timestamp: 1,
                }),
                Message::Assistant(assistant),
            ],
            tools: None,
        });
        collect(
            handle
                .core
                .stream_simple(&handle.model, &context, SimpleStreamOptions::default()),
        )
        .await;
        let requests = admin.requests("mapped").unwrap();
        assert_eq!(requests[0].changed_files, vec!["src/settings/form.ts"]);
        assert_eq!(
            requests[0].affected_tests,
            vec!["tests/regressions", "tests/settings"]
        );
    }

    #[test]
    fn affected_test_map_json_and_unmapped_fallback() {
        let map = AffectedTestMap::from_json(&serde_json::json!({
            "rules": [
                {"path": "src/settings", "tests": ["tests/settings"]},
                {"path": "src/api", "tests": ["tests/api"]}
            ],
            "always": ["tests/regressions"]
        }))
        .unwrap();
        assert_eq!(
            map.resolve(&["src/settings/form.ts".to_string()]),
            vec!["tests/regressions", "tests/settings"]
        );
        assert_eq!(
            map.resolve(&["src/api/client.ts".to_string()]),
            vec!["tests/api", "tests/regressions"]
        );
        assert_eq!(
            map.resolve(&["src/unknown/helper.ts".to_string()]),
            vec!["__ALL__"]
        );
        assert_eq!(
            map.resolve(&["tests/e2e/mcp/new.spec.ts".to_string()]),
            vec!["tests/e2e/mcp", "tests/regressions"]
        );
        assert!(map.resolve(&[]).is_empty());
    }

    #[tokio::test]
    async fn exhausted_queue_returns_error_message_not_panic() {
        let admin = MockLlmAdmin::new();
        admin.register("empty", MockTrajectory::new());
        let handle = admin
            .provider("empty", MockModelOptions::default())
            .unwrap();
        let (_, message) = collect(handle.core.stream_simple(
            &handle.model,
            &user_context("hello"),
            SimpleStreamOptions::default(),
        ))
        .await;
        assert_eq!(message.stop_reason, StopReason::Error);
        assert!(
            message
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains("No more mock responses queued")
        );
    }

    #[tokio::test]
    async fn usage_is_estimated_and_priced_when_not_provided() {
        let admin = MockLlmAdmin::new();
        admin.register(
            "usage",
            MockTrajectory::new().then_text("a fairly long answer"),
        );
        let cost = ModelCost {
            input: 3.0,
            output: 15.0,
            ..Default::default()
        };
        let handle = admin
            .provider(
                "usage",
                MockModelOptions {
                    cost: Some(cost),
                    ..Default::default()
                },
            )
            .unwrap();
        let (_, message) = collect(handle.core.stream_simple(
            &handle.model,
            &user_context("prompt"),
            SimpleStreamOptions::default(),
        ))
        .await;
        assert!(message.usage.input > 0);
        assert!(message.usage.output > 0);
        assert_eq!(
            message.usage.total_tokens,
            message.usage.input
                + message.usage.output
                + message.usage.cache_read
                + message.usage.cache_write
        );
        assert!(message.usage.cost.total > 0.0);
    }

    #[tokio::test]
    async fn explicit_usage_is_preserved() {
        let admin = MockLlmAdmin::new();
        admin.register(
            "explicit",
            MockTrajectory::new().push(MockStep::Respond(MockResponse::text("ok").with_usage(
                Usage {
                    input: 10,
                    output: 5,
                    total_tokens: 15,
                    cost: UsageCost {
                        total: 0.25,
                        ..Default::default()
                    },
                    ..Usage::default()
                },
            ))),
        );
        let handle = admin
            .provider("explicit", MockModelOptions::default())
            .unwrap();
        let (_, message) = collect(handle.core.stream_simple(
            &handle.model,
            &user_context("hello"),
            SimpleStreamOptions::default(),
        ))
        .await;
        assert_eq!(message.usage.total_tokens, 15);
        assert_eq!(message.usage.cost.total, 0.25);
    }

    #[tokio::test]
    async fn models_complete_without_any_api_key() {
        use crate::models::{CreateModelsOptions, ModelsStreamOptions, create_models};

        let admin = MockLlmAdmin::new();
        admin.register(
            "no-key",
            MockTrajectory::new().then_text("answered without a key"),
        );
        let handle = admin
            .provider("no-key", MockModelOptions::default())
            .unwrap();
        let models = create_models(CreateModelsOptions::default());
        models.set_provider(handle.provider.clone());
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hello".to_string()),
                timestamp: 1,
            })],
            tools: None,
        };
        let message = models
            .complete(
                &handle.model,
                &context,
                ModelsStreamOptions::from(crate::types::StreamOptions::default()),
            )
            .await;
        assert!(matches!(
            &message.content[0],
            AssistantContentBlock::Text(TextContent { text, .. })
                if text == "answered without a key"
        ));
        assert_eq!(admin.requests("no-key").unwrap().len(), 1);
    }

    #[test]
    fn model_options_have_sane_defaults() {
        let admin = MockLlmAdmin::new();
        admin.register("defaults", MockTrajectory::new().then_text("ok"));
        let handle = admin
            .provider("defaults", MockModelOptions::default())
            .unwrap();
        assert_eq!(handle.model.id, "mock-defaults");
        assert_eq!(handle.model.api, "mock:defaults");
        assert_eq!(handle.model.provider, "mock-provider:defaults");
        assert_eq!(handle.model.input, vec![ModelInputModality::Text]);
        assert_eq!(handle.model.context_window, Some(128_000));
    }
}
