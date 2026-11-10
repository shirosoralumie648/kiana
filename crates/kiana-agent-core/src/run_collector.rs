//! Agent run 汇总：`AgentRunCollector` → `AgentRunSummary` / `AgentRunCoverage`。
//!
//! 一次 `agentLoop` 可以产生多轮 chat 与多次工具调用；TUI 状态栏、evals 与日志需要的不是
//! 一串 OTEL span，而是一个**可持久化、可 diff、无引用的汇总值**：
//!
//! - [`AgentRunSummary`]：chat 数/stop reason 分布/延迟、工具状态计数与按工具名计数、token
//!   用量分解、费用与不可用原因、错误分类、step 数；
//! - [`AgentRunCoverage`]：注册工具 vs 实际调用工具、使用过的模型/provider（排序去重）。
//!
//! [`AgentRunCollector`] 既可以直接调用 `begin_*`/`end_*`，也可以用
//! [`AgentRunCollector::observe_event`] 消费 [`AgentEvent`] 自动累积（`turn_start` 开 chat、
//! `turn_end` 收 chat、`tool_execution_start/end` 收工具）。直接 API 覆盖事件流里没有的
//! 状态（blocked / skipped / timeout / aborted）与从未产生 span 的孤儿工具调用。
//!
//! 设计参考 oh-my-pi `packages/agent/src/run-collector.ts`（字段形状、inputTokens 含 cache、
//! cost unavailable reasons 排序、coverage 排序去重）；Kiana 版用 `Mutex` 内部状态 + 可注入
//! 时钟重写，并额外提供 `AgentEvent` 驱动。
//!
//! # 不变量
//!
//! - 方法不 panic：内部锁中毒时恢复数据，记 `latency_ms = 0`；
//! - `by_name` / `by_stop_reason` / `by_type` 的键在快照时按字典序排序；
//! - `input_tokens` 是**含缓存**的总输入（`input + cache_read + cache_write`）；分桶字段
//!   单独保留，聚合时逐项相加，不再从分桶反推总量；
//! - `snapshot` 是纯值快照，之后继续记录不影响已返回的结果；`mark_run_ended` 首次返回
//!   `true`，之后 `false`。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use kiana_ai::types::{AssistantMessage, StopReason};

use crate::types::{AgentEvent, AgentMessage};

/// 工具调用的终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    /// 成功。
    Ok,
    /// 执行失败。
    Error,
    /// 被跳过（例如达到工具预算）。
    Skipped,
    /// 被 `beforeToolCall`/权限层拦截。
    Blocked,
    /// 超时。
    Timeout,
    /// 调用方取消。
    Aborted,
}

impl ToolStatus {
    /// 稳定字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ToolStatus::Ok => "ok",
            ToolStatus::Error => "error",
            ToolStatus::Skipped => "skipped",
            ToolStatus::Blocked => "blocked",
            ToolStatus::Timeout => "timeout",
            ToolStatus::Aborted => "aborted",
        }
    }
}

/// 可注入时钟（毫秒，单调递增即可）。测试用假时钟锁定 latency。
pub trait RunClock: Send + Sync {
    /// 当前毫秒。
    fn now_ms(&self) -> f64;
}

/// 系统时钟（UNIX 纪元毫秒）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunClock;

impl RunClock for SystemRunClock {
    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .unwrap_or(0.0)
    }
}

/// 单次 chat 的原始记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRecord {
    /// 第几轮（从 0 开始；事件驱动时由 `turn_start` 计数）。
    pub step_number: u64,
    /// 模型 id。
    pub model: String,
    /// provider id。
    pub provider: String,
    /// stop reason（`None` 表示未结束/未知）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// 延迟（毫秒）。
    pub latency_ms: f64,
    /// 含缓存的总输入 token。
    pub input_tokens: u64,
    /// 输出 token。
    pub output_tokens: u64,
    /// 缓存命中输入。
    pub cached_input_tokens: u64,
    /// 缓存写入输入。
    pub cache_write_tokens: u64,
    /// reasoning token（output 的子集，单独展示）。
    pub reasoning_output_tokens: u64,
    /// 总 token（provider 上报优先，否则 input+output）。
    pub total_tokens: u64,
    /// 费用（USD）；`None` 表示没有可用价格。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// 费用不可用的原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_unavailable_reason: Option<String>,
    /// 错误类型（仅 error/aborted 轮）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

/// 单次工具调用的原始记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRecord {
    /// tool call id。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 终态。
    pub status: ToolStatus,
    /// 延迟（毫秒）。
    pub latency_ms: f64,
    /// 错误类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

/// 按工具名的计数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCounters {
    /// 总调用数。
    pub total: u64,
    /// 成功。
    pub ok: u64,
    /// 失败。
    pub error: u64,
    /// 跳过。
    pub skipped: u64,
    /// 拦截。
    pub blocked: u64,
    /// 超时。
    pub timeout: u64,
    /// 取消。
    pub aborted: u64,
    /// 累计延迟。
    pub total_latency_ms: f64,
}

impl ToolCounters {
    fn add(&mut self, status: ToolStatus, latency_ms: f64) {
        self.total += 1;
        self.total_latency_ms += latency_ms;
        match status {
            ToolStatus::Ok => self.ok += 1,
            ToolStatus::Error => self.error += 1,
            ToolStatus::Skipped => self.skipped += 1,
            ToolStatus::Blocked => self.blocked += 1,
            ToolStatus::Timeout => self.timeout += 1,
            ToolStatus::Aborted => self.aborted += 1,
        }
    }
}

/// chat 汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummary {
    /// chat 数。
    pub total: u64,
    /// stop reason → 次数（键排序）。
    pub by_stop_reason: BTreeMap<String, u64>,
    /// 累计延迟。
    pub total_latency_ms: f64,
}

/// 工具汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSummary {
    /// 总调用数。
    pub total: u64,
    /// 成功。
    pub ok: u64,
    /// 失败。
    pub error: u64,
    /// 跳过。
    pub skipped: u64,
    /// 拦截。
    pub blocked: u64,
    /// 超时。
    pub timeout: u64,
    /// 取消。
    pub aborted: u64,
    /// 累计延迟。
    pub total_latency_ms: f64,
    /// 按工具名（键排序）。
    pub by_name: BTreeMap<String, ToolCounters>,
}

/// token 用量汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    /// 含缓存的总输入。
    pub input_tokens: u64,
    /// 输出。
    pub output_tokens: u64,
    /// 缓存命中。
    pub cached_input_tokens: u64,
    /// 缓存写入。
    pub cache_write_tokens: u64,
    /// reasoning。
    pub reasoning_output_tokens: u64,
    /// 总 token。
    pub total_tokens: u64,
}

/// 费用汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostSummary {
    /// 估算/上报费用之和。
    pub estimated_usd: f64,
    /// 不可用原因（排序去重）。
    pub unavailable_reasons: Vec<String>,
}

/// 错误汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSummary {
    /// 错误总数。
    pub total: u64,
    /// 错误类型 → 次数。
    pub by_type: BTreeMap<String, u64>,
}

/// 一次 run 的纯值汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunSummary {
    /// chat 汇总。
    pub chats: ChatSummary,
    /// 工具汇总。
    pub tools: ToolSummary,
    /// token 汇总。
    pub usage: UsageTotals,
    /// 费用汇总。
    pub cost: CostSummary,
    /// 错误汇总。
    pub errors: ErrorSummary,
    /// step 数（通常等于 chat 数）。
    pub step_count: u64,
}

impl AgentRunSummary {
    /// 空汇总。
    pub fn empty() -> Self {
        Self::default()
    }
}

/// 覆盖率汇总：注册 vs 实际调用。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunCoverage {
    /// 注册过的工具名（排序）。
    pub tools_available: Vec<String>,
    /// 实际调用过的工具名（排序）。
    pub tools_invoked: Vec<String>,
    /// 注册但未调用（`tools_available - tools_invoked`）。
    pub tools_unused: Vec<String>,
    /// 使用过的模型（排序）。
    pub models_used: Vec<String>,
    /// 使用过的 provider（排序）。
    pub providers_used: Vec<String>,
}

impl AgentRunCoverage {
    /// 空覆盖。
    pub fn empty() -> Self {
        Self::default()
    }
}

/// 快照。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunSnapshot {
    /// 汇总。
    pub summary: AgentRunSummary,
    /// 覆盖。
    pub coverage: AgentRunCoverage,
}

/// chat 开始时返回的 token。
#[derive(Debug, Clone, PartialEq)]
pub struct ChatToken {
    /// step 序号。
    pub step_number: u64,
    /// 模型。
    pub model: String,
    /// provider。
    pub provider: String,
    /// 起始毫秒。
    pub started_ms: f64,
}

/// 工具开始时返回的 token。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolToken {
    /// tool call id。
    pub tool_call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 起始毫秒。
    pub started_ms: f64,
}

#[derive(Debug, Default)]
struct EventState {
    pending_chat: Option<ChatToken>,
    pending_tools: BTreeMap<String, ToolToken>,
    steps: u64,
}

#[derive(Debug, Default)]
struct Inner {
    chats: Vec<ChatRecord>,
    tools: Vec<ToolRecord>,
    available_tools: BTreeSet<String>,
    invoked_tools: BTreeSet<String>,
    models_used: BTreeSet<String>,
    providers_used: BTreeSet<String>,
    run_ended: bool,
}

/// 一次 run 的收集器。可 clone（共享同一份状态）。
#[derive(Clone)]
pub struct AgentRunCollector {
    inner: Arc<Mutex<Inner>>,
    events: Arc<Mutex<EventState>>,
    clock: Arc<dyn RunClock>,
}

impl std::fmt::Debug for AgentRunCollector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (chats, tools) = self.counts();
        formatter
            .debug_struct("AgentRunCollector")
            .field("chats", &chats)
            .field("tools", &tools)
            .finish()
    }
}

impl Default for AgentRunCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRunCollector {
    /// 用系统时钟构造。
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemRunClock))
    }

    /// 用自定义时钟构造（测试）。
    pub fn with_clock(clock: Arc<dyn RunClock>) -> Self {
        AgentRunCollector {
            inner: Arc::new(Mutex::new(Inner::default())),
            events: Arc::new(Mutex::new(EventState::default())),
            clock,
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_events(&self) -> std::sync::MutexGuard<'_, EventState> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn counts(&self) -> (usize, usize) {
        let inner = self.lock_inner();
        (inner.chats.len(), inner.tools.len())
    }

    /// 标记 run 结束；首次返回 `true`。
    pub fn mark_run_ended(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.run_ended {
            false
        } else {
            inner.run_ended = true;
            true
        }
    }

    /// run 是否已结束。
    pub fn run_ended(&self) -> bool {
        self.lock_inner().run_ended
    }

    /// 记录某一轮请求中暴露的工具名。
    pub fn note_available_tools<'a>(&self, tools: impl IntoIterator<Item = &'a str>) {
        let mut inner = self.lock_inner();
        inner
            .available_tools
            .extend(tools.into_iter().map(str::to_string));
    }

    /// 开始一次 chat。
    pub fn begin_chat(
        &self,
        step_number: u64,
        model: impl Into<String>,
        provider: impl Into<String>,
    ) -> ChatToken {
        ChatToken {
            step_number,
            model: model.into(),
            provider: provider.into(),
            started_ms: self.clock.now_ms(),
        }
    }

    /// 收尾一次 chat。`latency` 优先用 provider 的 `duration_ms`，否则用时钟差。
    ///
    /// usage 取 assistant 消息自带的 provider 上报值；需要 canonical 记账时用
    /// [`Self::end_chat_with_usage`] 传入会话语义下重算后的 usage。
    pub fn end_chat(
        &self,
        token: &ChatToken,
        message: &AssistantMessage,
        cost_usd: Option<f64>,
        cost_unavailable_reason: Option<&str>,
    ) {
        self.end_chat_inner(
            token,
            message,
            &message.usage,
            cost_usd,
            cost_unavailable_reason,
        );
    }

    /// 用显式 usage 收尾 chat（会话层 `account_provider_usage` 的 canonical 结果）。
    ///
    /// 这让 run summary 的费用与 `SessionTelemetry::record_api_call` 记的 cost 同源：
    /// provider 上报 `cost.total = 0` 但模型目录有价格时，两边都用 canonical 金额。
    pub fn end_chat_with_usage(
        &self,
        token: &ChatToken,
        message: &AssistantMessage,
        usage: &kiana_ai::types::Usage,
        cost_usd: Option<f64>,
        cost_unavailable_reason: Option<&str>,
    ) {
        self.end_chat_inner(token, message, usage, cost_usd, cost_unavailable_reason);
    }

    fn end_chat_inner(
        &self,
        token: &ChatToken,
        message: &AssistantMessage,
        usage: &kiana_ai::types::Usage,
        cost_usd: Option<f64>,
        cost_unavailable_reason: Option<&str>,
    ) {
        let latency_ms = message
            .duration_ms
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or_else(|| (self.clock.now_ms() - token.started_ms).max(0.0));
        let input_tokens = usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write);
        let total_tokens = if usage.total_tokens != 0 {
            usage.total_tokens
        } else {
            input_tokens.saturating_add(usage.output)
        };
        let stop_reason = Some(stop_reason_str(message.stop_reason));
        let error_type = matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
            .then(|| stop_reason_str(message.stop_reason));
        let cost = cost_usd.or(Some(usage.cost.total));
        let cost = cost.filter(|value| value.is_finite());
        let unavailable = cost_unavailable_reason
            .map(ToString::to_string)
            .or_else(|| {
                (cost.is_none() || cost == Some(0.0) && total_tokens > 0)
                    .then(|| "cost_unknown".to_string())
            });

        let record = ChatRecord {
            step_number: token.step_number,
            model: if message.model.is_empty() {
                token.model.clone()
            } else {
                message.model.clone()
            },
            provider: if message.provider.is_empty() {
                token.provider.clone()
            } else {
                message.provider.clone()
            },
            stop_reason,
            latency_ms,
            input_tokens,
            output_tokens: usage.output,
            cached_input_tokens: usage.cache_read,
            cache_write_tokens: usage.cache_write,
            reasoning_output_tokens: usage.reasoning.unwrap_or(0),
            total_tokens,
            cost_usd: cost,
            cost_unavailable_reason: unavailable,
            error_type,
        };
        let mut inner = self.lock_inner();
        if !record.model.is_empty() {
            inner.models_used.insert(record.model.clone());
        }
        if !record.provider.is_empty() {
            inner.providers_used.insert(record.provider.clone());
        }
        inner.chats.push(record);
    }

    /// 无 assistant 消息的失败 chat（流函数直接报错）。
    pub fn fail_chat(&self, token: &ChatToken, error_type: impl Into<String>) {
        let record = ChatRecord {
            step_number: token.step_number,
            model: token.model.clone(),
            provider: token.provider.clone(),
            stop_reason: Some("error".to_string()),
            latency_ms: (self.clock.now_ms() - token.started_ms).max(0.0),
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            cost_usd: None,
            cost_unavailable_reason: None,
            error_type: Some(error_type.into()),
        };
        let mut inner = self.lock_inner();
        if !record.model.is_empty() {
            inner.models_used.insert(record.model.clone());
        }
        if !record.provider.is_empty() {
            inner.providers_used.insert(record.provider.clone());
        }
        inner.chats.push(record);
    }

    /// 开始一次工具调用。
    pub fn begin_tool(
        &self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> ToolToken {
        let token = ToolToken {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            started_ms: self.clock.now_ms(),
        };
        self.lock_inner()
            .invoked_tools
            .insert(token.tool_name.clone());
        token
    }

    /// 收尾一次工具调用。
    pub fn end_tool(&self, token: &ToolToken, status: ToolStatus, error_type: Option<&str>) {
        let latency_ms = (self.clock.now_ms() - token.started_ms).max(0.0);
        self.end_tool_with_latency(token, status, error_type, latency_ms);
    }

    /// 收尾工具调用并显式给出延迟（事件里的 `durationMs`）。
    pub fn end_tool_with_latency(
        &self,
        token: &ToolToken,
        status: ToolStatus,
        error_type: Option<&str>,
        latency_ms: f64,
    ) {
        let record = ToolRecord {
            tool_call_id: token.tool_call_id.clone(),
            tool_name: token.tool_name.clone(),
            status,
            latency_ms: latency_ms.max(0.0),
            error_type: error_type.map(ToString::to_string),
        };
        let mut inner = self.lock_inner();
        inner.invoked_tools.insert(record.tool_name.clone());
        inner.tools.push(record);
    }

    /// 记录一个从未产生工具事件的调用（pre-run 中断、尾部清理）。
    pub fn record_orphan_tool(
        &self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        status: ToolStatus,
    ) {
        let record = ToolRecord {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            status,
            latency_ms: 0.0,
            error_type: None,
        };
        let mut inner = self.lock_inner();
        inner.invoked_tools.insert(record.tool_name.clone());
        inner.tools.push(record);
    }

    /// 消费一个 [`AgentEvent`]：
    ///
    /// - `turn_start` 开 chat（模型/provider 在 `turn_end` 从消息补全）；
    /// - `turn_end` 收 chat；
    /// - `tool_execution_start/end` 收工具（按 `duration_ms`/结果文本区分
    ///   blocked / skipped / timeout / aborted，见 [`tool_status_from_event`]）；
    /// - `agent_end` 标记 run 结束。
    pub fn observe_event(&self, event: &AgentEvent) {
        self.observe_event_with_usage(event, None);
    }

    /// [`Self::observe_event`] 的 canonical usage 版本。
    ///
    /// 会话层在 `turn_end` 传入 `account_provider_usage` 的结果，使 summary 的 token/cost
    /// 与遥测 API call 记录一致；其余事件忽略 `usage`。
    pub fn observe_event_with_usage(
        &self,
        event: &AgentEvent,
        usage: Option<&kiana_ai::types::Usage>,
    ) {
        match event {
            AgentEvent::TurnEnd { message, .. } => {
                self.observe_turn_end(message, usage);
            }
            _ => self.observe_event_inner(event),
        }
    }

    fn observe_turn_end(&self, message: &AgentMessage, usage: Option<&kiana_ai::types::Usage>) {
        let token = self.lock_events().pending_chat.take();
        let Some(token) = token else { return };
        if let AgentMessage::Standard(kiana_ai::types::Message::Assistant(assistant)) = message {
            match usage {
                Some(usage) => self.end_chat_with_usage(&token, assistant, usage, None, None),
                None => self.end_chat(&token, assistant, None, None),
            }
        } else {
            self.fail_chat(&token, "missing_assistant_message");
        }
    }

    fn observe_event_inner(&self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStart => {
                let mut state = self.lock_events();
                let step = state.steps;
                state.steps += 1;
                state.pending_chat = Some(ChatToken {
                    step_number: step,
                    model: String::new(),
                    provider: String::new(),
                    started_ms: self.clock.now_ms(),
                });
            }
            AgentEvent::TurnEnd { message, .. } => {
                self.observe_turn_end(message, None);
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                ..
            } => {
                let token = self.begin_tool(tool_call_id.clone(), tool_name.clone());
                self.lock_events()
                    .pending_tools
                    .insert(tool_call_id.clone(), token);
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
            } => {
                let token = self
                    .lock_events()
                    .pending_tools
                    .remove(tool_call_id)
                    .unwrap_or_else(|| self.begin_tool(tool_call_id.clone(), tool_name.clone()));
                let status = tool_status_from_event(result, *is_error, *duration_ms);
                let latency = duration_ms
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .unwrap_or_else(|| (self.clock.now_ms() - token.started_ms).max(0.0));
                self.end_tool_with_latency(&token, status, tool_status_error_type(status), latency);
            }
            AgentEvent::AgentEnd { .. } => {
                self.mark_run_ended();
            }
            AgentEvent::AgentStart
            | AgentEvent::MessageStart { .. }
            | AgentEvent::MessageUpdate { .. }
            | AgentEvent::MessageEnd { .. }
            | AgentEvent::ToolExecutionUpdate { .. } => {}
        }
    }

    /// 构建快照；`step_count` 取自 `turn_start` 计数。
    pub fn snapshot(&self) -> AgentRunSnapshot {
        let steps = self.lock_events().steps;
        self.snapshot_with_step_count(steps)
    }

    /// 构建快照并显式指定 step 数。
    pub fn snapshot_with_step_count(&self, step_count: u64) -> AgentRunSnapshot {
        let inner = self.lock_inner();
        let (summary, coverage) = build_snapshot(&inner, step_count);
        AgentRunSnapshot { summary, coverage }
    }

    /// 只取汇总。
    pub fn summary(&self) -> AgentRunSummary {
        self.snapshot().summary
    }

    /// 只取覆盖率。
    pub fn coverage(&self) -> AgentRunCoverage {
        self.snapshot().coverage
    }
}

/// 从 `ToolExecutionEnd.result`（`AgentToolResult` 的 JSON 形态）提取文本内容。
fn tool_result_text(result: &serde_json::Value) -> String {
    result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// 把 [`AgentEvent::ToolExecutionEnd`] 映射为完整终态。
///
/// 事件本身只有 `is_error` 与 `duration_ms`，但这两个字段足以区分四类非成功终态：
///
/// - `duration_ms == None` 表示工具**没有真正执行**（未知工具、参数校验失败、
///   `beforeToolCall` 拦截、abort、length 截断）。其中 `is_error` 为真的归为
///   [`ToolStatus::Blocked`]（被前置门禁拦下），为假的归为 [`ToolStatus::Skipped`]；
/// - 结果文本是 `"Operation aborted"`（事件取消）时归为 [`ToolStatus::Aborted`]；
/// - 已执行（`duration_ms` 有值）且失败、错误文本含 `timed out`/`timeout` 的归为
///   [`ToolStatus::Timeout`]（bash 等工具的超时错误）；
/// - 其余按 `is_error` 落 `Ok`/`Error`。
fn tool_status_from_event(
    result: &serde_json::Value,
    is_error: bool,
    duration_ms: Option<f64>,
) -> ToolStatus {
    let text = tool_result_text(result);
    let lower = text.to_ascii_lowercase();
    if lower.contains("operation aborted") {
        return ToolStatus::Aborted;
    }
    match duration_ms {
        None if is_error => ToolStatus::Blocked,
        None => ToolStatus::Skipped,
        Some(_) if is_error && (lower.contains("timed out") || lower.contains("timeout")) => {
            ToolStatus::Timeout
        }
        Some(_) if is_error => ToolStatus::Error,
        Some(_) => ToolStatus::Ok,
    }
}

/// 终态对应的 `error_type` 字面量（成功为 `None`）。
fn tool_status_error_type(status: ToolStatus) -> Option<&'static str> {
    match status {
        ToolStatus::Ok => None,
        ToolStatus::Error => Some("tool_error"),
        ToolStatus::Skipped => Some("tool_skipped"),
        ToolStatus::Blocked => Some("tool_blocked"),
        ToolStatus::Timeout => Some("tool_timeout"),
        ToolStatus::Aborted => Some("tool_aborted"),
    }
}

fn stop_reason_str(stop_reason: StopReason) -> String {
    match stop_reason {
        StopReason::Pending => "pending",
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
        StopReason::Deferred => "deferred",
        StopReason::TurnCapped => "turnCapped",
        StopReason::TokenCapped => "tokenCapped",
        StopReason::CostCapped => "costCapped",
        StopReason::TimeCapped => "timeCapped",
    }
    .to_string()
}

fn build_snapshot(inner: &Inner, step_count: u64) -> (AgentRunSummary, AgentRunCoverage) {
    let mut chats = ChatSummary {
        total: inner.chats.len() as u64,
        ..ChatSummary::default()
    };
    let mut usage = UsageTotals::default();
    let mut cost = CostSummary::default();
    let mut unavailable = BTreeSet::new();
    let mut errors_by_type: BTreeMap<String, u64> = BTreeMap::new();

    for chat in &inner.chats {
        chats.total_latency_ms += chat.latency_ms;
        usage.input_tokens = usage.input_tokens.saturating_add(chat.input_tokens);
        usage.output_tokens = usage.output_tokens.saturating_add(chat.output_tokens);
        usage.cached_input_tokens = usage
            .cached_input_tokens
            .saturating_add(chat.cached_input_tokens);
        usage.cache_write_tokens = usage
            .cache_write_tokens
            .saturating_add(chat.cache_write_tokens);
        usage.reasoning_output_tokens = usage
            .reasoning_output_tokens
            .saturating_add(chat.reasoning_output_tokens);
        usage.total_tokens = usage.total_tokens.saturating_add(chat.total_tokens);
        if let Some(reason) = &chat.stop_reason {
            *chats.by_stop_reason.entry(reason.clone()).or_default() += 1;
        }
        if let Some(value) = chat.cost_usd {
            cost.estimated_usd += value;
        }
        if let Some(reason) = &chat.cost_unavailable_reason {
            unavailable.insert(reason.clone());
        }
        if let Some(error_type) = &chat.error_type {
            *errors_by_type.entry(error_type.clone()).or_default() += 1;
        }
    }
    chats.total = inner.chats.len() as u64;
    cost.unavailable_reasons = unavailable.into_iter().collect();

    let mut tools = ToolSummary {
        total: inner.tools.len() as u64,
        ..ToolSummary::default()
    };
    for tool in &inner.tools {
        tools.total_latency_ms += tool.latency_ms;
        match tool.status {
            ToolStatus::Ok => tools.ok += 1,
            ToolStatus::Error => tools.error += 1,
            ToolStatus::Skipped => tools.skipped += 1,
            ToolStatus::Blocked => tools.blocked += 1,
            ToolStatus::Timeout => tools.timeout += 1,
            ToolStatus::Aborted => tools.aborted += 1,
        }
        tools
            .by_name
            .entry(tool.tool_name.clone())
            .or_default()
            .add(tool.status, tool.latency_ms);
        if let Some(error_type) = &tool.error_type {
            *errors_by_type.entry(error_type.clone()).or_default() += 1;
        }
    }

    let error_total: u64 = errors_by_type.values().sum();
    let summary = AgentRunSummary {
        chats,
        tools,
        usage,
        cost,
        errors: ErrorSummary {
            total: error_total,
            by_type: errors_by_type,
        },
        step_count,
    };

    let tools_available: Vec<String> = inner.available_tools.iter().cloned().collect();
    let tools_invoked: Vec<String> = inner.invoked_tools.iter().cloned().collect();
    let tools_unused = tools_available
        .iter()
        .filter(|name| !inner.invoked_tools.contains(*name))
        .cloned()
        .collect();
    let coverage = AgentRunCoverage {
        tools_available,
        tools_invoked,
        tools_unused,
        models_used: inner.models_used.iter().cloned().collect(),
        providers_used: inner.providers_used.iter().cloned().collect(),
    };
    (summary, coverage)
}

/// 合并多个 run 的 summary（逐项相加、按 key 合并、集合排序去重）。
pub fn aggregate_agent_run_summaries(summaries: &[AgentRunSummary]) -> AgentRunSummary {
    let mut merged = AgentRunSummary::empty();
    let mut unavailable = BTreeSet::new();
    for summary in summaries {
        merged.chats.total += summary.chats.total;
        merged.chats.total_latency_ms += summary.chats.total_latency_ms;
        for (reason, count) in &summary.chats.by_stop_reason {
            *merged
                .chats
                .by_stop_reason
                .entry(reason.clone())
                .or_default() += count;
        }
        merged.tools.total += summary.tools.total;
        merged.tools.ok += summary.tools.ok;
        merged.tools.error += summary.tools.error;
        merged.tools.skipped += summary.tools.skipped;
        merged.tools.blocked += summary.tools.blocked;
        merged.tools.timeout += summary.tools.timeout;
        merged.tools.aborted += summary.tools.aborted;
        merged.tools.total_latency_ms += summary.tools.total_latency_ms;
        for (name, counters) in &summary.tools.by_name {
            let entry = merged.tools.by_name.entry(name.clone()).or_default();
            entry.total += counters.total;
            entry.ok += counters.ok;
            entry.error += counters.error;
            entry.skipped += counters.skipped;
            entry.blocked += counters.blocked;
            entry.timeout += counters.timeout;
            entry.aborted += counters.aborted;
            entry.total_latency_ms += counters.total_latency_ms;
        }
        merged.usage.input_tokens += summary.usage.input_tokens;
        merged.usage.output_tokens += summary.usage.output_tokens;
        merged.usage.cached_input_tokens += summary.usage.cached_input_tokens;
        merged.usage.cache_write_tokens += summary.usage.cache_write_tokens;
        merged.usage.reasoning_output_tokens += summary.usage.reasoning_output_tokens;
        merged.usage.total_tokens += summary.usage.total_tokens;
        merged.cost.estimated_usd += summary.cost.estimated_usd;
        unavailable.extend(summary.cost.unavailable_reasons.iter().cloned());
        merged.errors.total += summary.errors.total;
        for (error_type, count) in &summary.errors.by_type {
            *merged.errors.by_type.entry(error_type.clone()).or_default() += count;
        }
        merged.step_count += summary.step_count;
    }
    merged.cost.unavailable_reasons = unavailable.into_iter().collect();
    merged
}

/// 合并多个 run 的 coverage（并集、排序去重）。
pub fn aggregate_agent_run_coverage(coverages: &[AgentRunCoverage]) -> AgentRunCoverage {
    let mut available = BTreeSet::new();
    let mut invoked = BTreeSet::new();
    let mut models = BTreeSet::new();
    let mut providers = BTreeSet::new();
    for coverage in coverages {
        available.extend(coverage.tools_available.iter().cloned());
        invoked.extend(coverage.tools_invoked.iter().cloned());
        models.extend(coverage.models_used.iter().cloned());
        providers.extend(coverage.providers_used.iter().cloned());
    }
    let tools_available: Vec<String> = available.into_iter().collect();
    let tools_invoked: Vec<String> = invoked.into_iter().collect();
    let tools_unused = tools_available
        .iter()
        .filter(|name| !tools_invoked.contains(name))
        .cloned()
        .collect();
    AgentRunCoverage {
        tools_available,
        tools_invoked,
        tools_unused,
        models_used: models.into_iter().collect(),
        providers_used: providers.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use kiana_ai::types::{
        AssistantRole, Message, Usage, UsageCost, UserContent, UserMessage, UserRole,
    };

    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn new() -> Self {
            FakeClock(AtomicU64::new(0))
        }
        fn advance(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl RunClock for FakeClock {
        fn now_ms(&self) -> f64 {
            self.0.load(Ordering::SeqCst) as f64
        }
    }

    fn collector() -> (AgentRunCollector, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::new());
        (AgentRunCollector::with_clock(clock.clone()), clock)
    }

    fn assistant(model: &str, input: u64, output: u64, cost: f64) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            model: model.into(),
            provider: "openai".into(),
            usage: Usage {
                input,
                output,
                cache_read: 10,
                cache_write: 5,
                total_tokens: input + output + 15,
                cost: UsageCost {
                    total: cost,
                    ..UsageCost::default()
                },
                ..Usage::default()
            },
            stop_reason: StopReason::ToolUse,
            duration_ms: Some(250.0),
            ..AssistantMessage::default()
        }
    }

    #[test]
    fn direct_recording_matches_the_messages() {
        let (collector, clock) = collector();
        collector.note_available_tools(["bash", "read"]);
        let chat = collector.begin_chat(0, "gpt-5", "openai");
        clock.advance(250);
        collector.end_chat(&chat, &assistant("gpt-5", 100, 40, 0.5), None, None);

        let tool = collector.begin_tool("call_1", "bash");
        clock.advance(30);
        collector.end_tool(&tool, ToolStatus::Error, Some("exit_code_1"));
        collector.record_orphan_tool("call_2", "read", ToolStatus::Blocked);

        let snapshot = collector.snapshot_with_step_count(1);
        let summary = &snapshot.summary;
        assert_eq!(summary.chats.total, 1);
        assert_eq!(summary.chats.by_stop_reason.get("toolUse"), Some(&1));
        assert_eq!(summary.chats.total_latency_ms, 250.0);
        assert_eq!(summary.usage.input_tokens, 115); // 100 + 10 + 5
        assert_eq!(summary.usage.output_tokens, 40);
        assert_eq!(summary.usage.cached_input_tokens, 10);
        assert_eq!(summary.usage.cache_write_tokens, 5);
        assert_eq!(summary.usage.total_tokens, 155);
        assert_eq!(summary.usage.reasoning_output_tokens, 0);
        assert!((summary.cost.estimated_usd - 0.5).abs() < 1e-9);
        assert!(summary.cost.unavailable_reasons.is_empty());
        assert_eq!(summary.tools.total, 2);
        assert_eq!(summary.tools.ok, 0);
        assert_eq!(summary.tools.error, 1);
        assert_eq!(summary.tools.blocked, 1);
        assert_eq!(summary.tools.by_name["bash"].error, 1);
        assert_eq!(summary.tools.by_name["read"].blocked, 1);
        assert_eq!(summary.errors.by_type.get("exit_code_1"), Some(&1));
        assert_eq!(summary.step_count, 1);

        assert_eq!(
            snapshot.coverage.tools_available,
            vec!["bash".to_string(), "read".to_string()]
        );
        assert_eq!(snapshot.coverage.tools_unused, Vec::<String>::new());
        assert_eq!(snapshot.coverage.models_used, vec!["gpt-5".to_string()]);
        assert_eq!(snapshot.coverage.providers_used, vec!["openai".to_string()]);
    }

    #[test]
    fn event_driven_collection_matches_the_events() {
        let (collector, clock) = collector();
        collector.observe_event(&AgentEvent::AgentStart);
        collector.observe_event(&AgentEvent::TurnStart);
        clock.advance(100);
        let message = AgentMessage::Standard(Message::Assistant(assistant("gpt-5", 50, 20, 0.25)));
        collector.observe_event(&AgentEvent::TurnEnd {
            message: message.clone(),
            tool_results: Vec::new(),
        });
        collector.observe_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "call_1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!({}),
        });
        clock.advance(10);
        collector.observe_event(&AgentEvent::ToolExecutionEnd {
            tool_call_id: "call_1".into(),
            tool_name: "bash".into(),
            result: serde_json::json!({"content": []}),
            is_error: false,
            duration_ms: Some(10.0),
        });
        collector.observe_event(&AgentEvent::AgentEnd {
            messages: vec![message],
        });

        assert!(collector.run_ended());
        let snapshot = collector.snapshot();
        assert_eq!(snapshot.summary.chats.total, 1);
        assert_eq!(snapshot.summary.step_count, 1);
        // duration_ms 优先于时钟差。
        assert_eq!(snapshot.summary.chats.total_latency_ms, 250.0);
        assert_eq!(snapshot.summary.tools.ok, 1);
        assert_eq!(
            snapshot.summary.tools.by_name["bash"].total_latency_ms,
            10.0
        );
        assert!(
            snapshot
                .coverage
                .tools_invoked
                .contains(&"bash".to_string())
        );
        assert!(!collector.mark_run_ended(), "agent_end already marked it");
    }

    #[test]
    fn fail_chat_records_a_zero_latency_style_failure() {
        let (collector, clock) = collector();
        let chat = collector.begin_chat(3, "gpt-5", "openai");
        clock.advance(15);
        collector.fail_chat(&chat, "stream_error");
        let summary = collector.summary();
        assert_eq!(summary.chats.total, 1);
        assert_eq!(summary.chats.by_stop_reason.get("error"), Some(&1));
        assert_eq!(summary.chats.total_latency_ms, 15.0);
        assert_eq!(summary.errors.by_type.get("stream_error"), Some(&1));
    }

    #[test]
    fn cost_unknown_is_recorded_when_zero_usage_cost_has_tokens() {
        let (collector, _) = collector();
        let chat = collector.begin_chat(0, "free-model", "custom");
        let mut message = assistant("free-model", 10, 10, 0.0);
        message.usage.cost = UsageCost::default();
        collector.end_chat(&chat, &message, None, None);
        let summary = collector.summary();
        assert_eq!(
            summary.cost.unavailable_reasons,
            vec!["cost_unknown".to_string()]
        );
    }

    #[test]
    fn aggregates_summaries_and_coverage() {
        let a = AgentRunSummary {
            chats: ChatSummary {
                total: 1,
                by_stop_reason: BTreeMap::from([("stop".to_string(), 1)]),
                total_latency_ms: 10.0,
            },
            tools: ToolSummary {
                total: 1,
                ok: 1,
                total_latency_ms: 5.0,
                by_name: BTreeMap::from([(
                    "bash".to_string(),
                    ToolCounters {
                        total: 1,
                        ok: 1,
                        total_latency_ms: 5.0,
                        ..ToolCounters::default()
                    },
                )]),
                ..ToolSummary::default()
            },
            usage: UsageTotals {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
                ..UsageTotals::default()
            },
            cost: CostSummary {
                estimated_usd: 0.1,
                unavailable_reasons: vec!["cost_unknown".to_string()],
            },
            errors: ErrorSummary::default(),
            step_count: 1,
        };
        let b = AgentRunSummary {
            chats: ChatSummary {
                total: 2,
                by_stop_reason: BTreeMap::from([("stop".to_string(), 1), ("error".to_string(), 1)]),
                total_latency_ms: 20.0,
            },
            tools: ToolSummary {
                total: 2,
                error: 2,
                total_latency_ms: 7.0,
                by_name: BTreeMap::from([(
                    "read".to_string(),
                    ToolCounters {
                        total: 2,
                        error: 2,
                        total_latency_ms: 7.0,
                        ..ToolCounters::default()
                    },
                )]),
                ..ToolSummary::default()
            },
            usage: UsageTotals {
                input_tokens: 50,
                output_tokens: 25,
                total_tokens: 75,
                ..UsageTotals::default()
            },
            cost: CostSummary {
                estimated_usd: 0.2,
                unavailable_reasons: vec![
                    "cost_unknown".to_string(),
                    "pricing_missing".to_string(),
                ],
            },
            errors: ErrorSummary {
                total: 1,
                by_type: BTreeMap::from([("http_500".to_string(), 1)]),
            },
            step_count: 2,
        };
        let merged = aggregate_agent_run_summaries(&[a, b]);
        assert_eq!(merged.chats.total, 3);
        assert_eq!(merged.chats.by_stop_reason.get("stop"), Some(&2));
        assert_eq!(merged.chats.by_stop_reason.get("error"), Some(&1));
        assert_eq!(merged.tools.total, 3);
        assert_eq!(merged.tools.ok, 1);
        assert_eq!(merged.tools.error, 2);
        assert_eq!(merged.usage.total_tokens, 225);
        assert!((merged.cost.estimated_usd - 0.3).abs() < 1e-9);
        assert_eq!(
            merged.cost.unavailable_reasons,
            vec!["cost_unknown".to_string(), "pricing_missing".to_string()]
        );
        assert_eq!(merged.errors.total, 1);
        assert_eq!(merged.step_count, 3);

        let coverage = aggregate_agent_run_coverage(&[
            AgentRunCoverage {
                tools_available: vec!["bash".into(), "read".into()],
                tools_invoked: vec!["bash".into()],
                tools_unused: vec!["read".into()],
                models_used: vec!["gpt-5".into()],
                providers_used: vec!["openai".into()],
            },
            AgentRunCoverage {
                tools_available: vec!["read".into(), "grep".into()],
                tools_invoked: vec!["grep".into()],
                tools_unused: vec!["read".into()],
                models_used: vec!["gpt-5".into()],
                providers_used: vec!["openai".into()],
            },
        ]);
        assert_eq!(
            coverage.tools_available,
            vec!["bash".to_string(), "grep".to_string(), "read".to_string()]
        );
        assert_eq!(
            coverage.tools_invoked,
            vec!["bash".to_string(), "grep".to_string()]
        );
        assert_eq!(coverage.tools_unused, vec!["read".to_string()]);
        assert_eq!(coverage.models_used, vec!["gpt-5".to_string()]);
    }

    #[test]
    fn a_non_assistant_turn_end_fails_the_chat() {
        let (collector, _) = collector();
        collector.observe_event(&AgentEvent::TurnStart);
        collector.observe_event(&AgentEvent::TurnEnd {
            message: AgentMessage::Standard(Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("hi".into()),
                timestamp: 0,
            })),
            tool_results: Vec::new(),
        });
        let summary = collector.summary();
        assert_eq!(
            summary.errors.by_type.get("missing_assistant_message"),
            Some(&1)
        );
    }

    fn tool_end(id: &str, text: &str, is_error: bool, duration_ms: Option<f64>) -> AgentEvent {
        AgentEvent::ToolExecutionEnd {
            tool_call_id: id.into(),
            tool_name: "bash".into(),
            result: serde_json::json!({
                "content": [{ "type": "text", "text": text }],
            }),
            is_error,
            duration_ms,
        }
    }

    fn start_tool(collector: &AgentRunCollector, id: &str) {
        collector.observe_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: id.into(),
            tool_name: "bash".into(),
            args: serde_json::json!({}),
        });
    }

    #[test]
    fn event_driven_tools_distinguish_blocked_skipped_timeout_aborted() {
        let (collector, _) = collector();

        start_tool(&collector, "blocked");
        collector.observe_event(&tool_end(
            "blocked",
            "Tool execution was blocked",
            true,
            None,
        ));

        start_tool(&collector, "skipped");
        collector.observe_event(&tool_end(
            "skipped",
            "skipped because of length",
            false,
            None,
        ));

        start_tool(&collector, "timeout");
        collector.observe_event(&tool_end(
            "timeout",
            "Command timed out after 1 seconds",
            true,
            Some(1000.0),
        ));

        start_tool(&collector, "aborted");
        collector.observe_event(&tool_end("aborted", "Operation aborted", true, None));

        start_tool(&collector, "ok");
        collector.observe_event(&tool_end("ok", "fine", false, Some(5.0)));

        let summary = collector.summary().tools;
        assert_eq!(summary.blocked, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.timeout, 1);
        // aborted 优先于 `duration_ms == None` 的 blocked 归类。
        assert_eq!(summary.aborted, 1);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.error, 0);
        assert_eq!(summary.total, 5);
        assert_eq!(summary.by_name["bash"].blocked, 1);
        assert_eq!(summary.by_name["bash"].aborted, 1);
    }

    #[test]
    fn canonical_usage_override_drives_tokens_and_cost() {
        let (canonical_collector, _) = collector();
        let chat = canonical_collector.begin_chat(0, "gpt-5", "openai");
        let mut message = assistant("gpt-5", 100, 40, 0.0);
        // provider 上报 cost 为 0（例如 faux provider）；canonical 重算后有金额。
        let mut canonical = message.usage.clone();
        canonical.cost.total = 1.25;

        canonical_collector.end_chat_with_usage(&chat, &message, &canonical, None, None);
        let summary = canonical_collector.summary();
        assert!((summary.cost.estimated_usd - 1.25).abs() < 1e-9);
        assert_eq!(summary.usage.input_tokens, 115);
        assert!(summary.cost.unavailable_reasons.is_empty());

        // 对照组：不传 canonical 时仍使用 provider 上报的 0。
        let (control, _) = collector();
        message.usage.cost.total = 0.0;
        let chat = control.begin_chat(0, "gpt-5", "openai");
        control.end_chat(&chat, &message, None, None);
        assert_eq!(control.summary().cost.estimated_usd, 0.0);
    }
}
