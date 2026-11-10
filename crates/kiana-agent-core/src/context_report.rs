//! 下一次请求的上下文报告，以及手工压缩的收敛判定。
//!
//! # 上下文报告
//!
//! [`build_context_report`] 用一条 stand-in user prompt 重建「下一次请求」：
//!
//! 1. 复制当前 [`AgentContext`] 的消息；
//! 2. 追加 stand-in prompt（模拟用户的下一条输入）；
//! 3. 用 `convert_to_llm`（缺省只保留标准消息）投影成 provider 可消费的消息；
//! 4. 用 `kiana_ai::utils::estimate` 的同一套估算（有 assistant usage 时以 usage 为锚、
//!    只补算其后的消息）计算总 token；
//! 5. 按 `tool` / `prompt` / `turn-context` / `segment` 分类，给出每段 token 与预览。
//!
//! 因此 `total_tokens` 与真实 provider 上报的 input 使用同一口径（误差来自估算器本身），
//! 而不是把每段单独 `ceil` 后相加。
//!
//! # 手工压缩
//!
//! [`observe_condensation`] 以「token 数下降并稳定」作为压缩成功的判据：先看到 token 低于
//! 基线，再要求同一数值保持 `settle`（默认 2.5s）；整个过程超过 `timeout`（默认 90s）仍未
//! 稳定则返回 [`CondensationPhase::Failed`]。这样既不会把「压缩刚开始、token 还没降」误判
//! 成成功，也不会在异步压缩任务卡死时永远等下去。

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, SystemMessage, UserContent,
    UserMessage, UserRole,
};
use kiana_ai::utils::estimate::{
    estimate_context_tokens_for_messages, estimate_message_tokens, estimate_text_tokens,
};
use kiana_ai::utils::text::{
    content_text_assistant, content_text_user, content_text_user_content, get_system_message_text,
};

use crate::types::{AgentContext, AgentMessage};

/// 报告分段类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextSegmentKind {
    /// 工具声明（system 消息里的 `toolsAdded`/`toolsRemoved` 或顶层 tools）。
    Tool,
    /// system prompt / 命名 section。
    Prompt,
    /// 本轮新增的上下文（assistant/toolResult/user 等）。
    TurnContext,
    /// transcript 中的普通消息。
    Segment,
}

impl ContextSegmentKind {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ContextSegmentKind::Tool => "tool",
            ContextSegmentKind::Prompt => "prompt",
            ContextSegmentKind::TurnContext => "turnContext",
            ContextSegmentKind::Segment => "segment",
        }
    }
}

/// 报告中的一段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSegment {
    /// 类别。
    pub kind: ContextSegmentKind,
    /// 展示名（如 `tool:bash`、`assistant`、`system`）。
    pub label: String,
    /// 角色（消息类分段）。
    pub role: String,
    /// token 估算。
    pub tokens: u64,
    /// 截断后的文本预览。
    pub preview: String,
    /// 原始字符数（截断前）。
    pub chars: usize,
}

/// 报告选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextReportOptions {
    /// 模拟用户下一条输入的 stand-in prompt。
    pub stand_in_prompt: String,
    /// 上下文窗口（缺省取模型字段）。
    pub context_window: Option<u64>,
    /// 每个分段的预览字符上限（默认 200）。
    pub preview_chars: usize,
}

impl Default for ContextReportOptions {
    fn default() -> Self {
        ContextReportOptions {
            stand_in_prompt: "[next user prompt]".to_string(),
            context_window: None,
            preview_chars: 200,
        }
    }
}

impl ContextReportOptions {
    /// 构造。
    pub fn new(stand_in_prompt: impl Into<String>) -> Self {
        ContextReportOptions {
            stand_in_prompt: stand_in_prompt.into(),
            ..Default::default()
        }
    }
}

/// 上下文报告。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextReport {
    /// 模型 id。
    pub model: String,
    /// provider id。
    pub provider: String,
    /// 重建后的下一次请求 token 总量。
    pub total_tokens: u64,
    /// 最后一条 assistant usage 提供的锚点 token（没有则为 0）。
    pub usage_tokens: u64,
    /// usage 之后追加消息的估算 token。
    pub trailing_tokens: u64,
    /// 上下文窗口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 使用率（`total_tokens / context_window`）。
    pub utilization: f64,
    /// 工具类合计。
    pub tool_tokens: u64,
    /// prompt 类合计。
    pub prompt_tokens: u64,
    /// turn-context 类合计。
    pub turn_context_tokens: u64,
    /// 普通消息类合计。
    pub segment_tokens: u64,
    /// 工具分段。
    pub tools: Vec<ContextSegment>,
    /// prompt 分段。
    pub prompt: Vec<ContextSegment>,
    /// turn-context 分段。
    pub turn_context: Vec<ContextSegment>,
    /// 普通消息分段。
    pub segments: Vec<ContextSegment>,
    /// 重建后的消息数量（含 stand-in）。
    pub request_message_count: usize,
}

impl ContextReport {
    /// 所有分段。
    pub fn all_segments(&self) -> Vec<&ContextSegment> {
        self.tools
            .iter()
            .chain(self.prompt.iter())
            .chain(self.turn_context.iter())
            .chain(self.segments.iter())
            .collect()
    }
}

/// 用 stand-in prompt 重建下一次请求并统计 token。
///
/// 缺省投影是 [`crate::agent::custom_message_projection`]（agent 的
/// `default_convert_to_llm`）。会话层有自己的 `convert_to_llm`（把
/// `bashExecution` / `custom` / `branchSummary` / `compactionSummary` 映射成 user 文本），
/// 应改用 [`build_context_report_with`] 传入同一个函数，否则报告会低估真实请求。
pub fn build_context_report(
    context: &AgentContext,
    model: &Model,
    options: &ContextReportOptions,
) -> ContextReport {
    build_context_report_with(context, model, options, |message| match message {
        AgentMessage::Standard(message) => Some(message.clone()),
        AgentMessage::Custom(object) => crate::agent::custom_message_projection(object),
    })
}

/// 与 [`build_context_report`] 相同，但用调用方的 `convert_to_llm` 投影消息。
///
/// 投影对每条消息独立调用；返回 `None` 表示该消息不会进入真实请求。
pub fn build_context_report_with<F>(
    context: &AgentContext,
    model: &Model,
    options: &ContextReportOptions,
    project: F,
) -> ContextReport
where
    F: Fn(&AgentMessage) -> Option<Message>,
{
    let mut messages = context.messages.clone();
    messages.push(AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(options.stand_in_prompt.clone()),
        timestamp: now_unix_ms(),
    })));

    // 与 `default_convert_to_llm` 保持一致：标准消息原样保留，自定义消息在 role 是
    // 标准四类之一时也会投影进真实请求（漏掉它们会低估 total_tokens）。
    let mut request_messages: Vec<Message> = Vec::new();
    // 与 `request_messages` 对齐：`true` 表示该条目来自 Custom（token 已计入
    // turn_context，分类循环不再重复统计）。
    let mut request_from_custom: Vec<bool> = Vec::new();
    let mut turn_context: Vec<ContextSegment> = Vec::new();
    for message in &messages {
        match message {
            AgentMessage::Standard(_) => {
                if let Some(projected) = project(message) {
                    request_messages.push(projected);
                    request_from_custom.push(false);
                }
            }
            AgentMessage::Custom(object) => {
                if let Some(projected) = project(message) {
                    request_messages.push(projected);
                    request_from_custom.push(true);
                }
                let value = serde_json::Value::Object(object.clone().into_iter().collect());
                let text = kiana_json::js_object::stringify(&value);
                turn_context.push(segment(
                    ContextSegmentKind::TurnContext,
                    "custom",
                    object
                        .get("role")
                        .and_then(|role| role.as_str())
                        .unwrap_or("custom"),
                    estimate_text_tokens(&text),
                    &text,
                    options.preview_chars,
                ));
            }
        }
    }

    let estimate = estimate_context_tokens_for_messages(&request_messages);
    let mut tools = Vec::new();
    let mut prompt = Vec::new();
    let mut segments = Vec::new();

    // 顶层可执行工具（agent loop 通常把它们投影进 system 消息）。已经由
    // `toolsAdded` 声明的工具不能重复统计，否则 `tool_tokens` 会把同一份声明算两次。
    let declared_tools: std::collections::HashSet<&str> = request_messages
        .iter()
        .filter_map(|message| match message {
            Message::System(system) => Some(system),
            _ => None,
        })
        .flat_map(|system| system.tools_added.iter().flatten())
        .map(|tool| tool.name.as_str())
        .collect();
    if let Some(context_tools) = &context.tools {
        for tool in context_tools {
            if declared_tools.contains(tool.name.as_str()) {
                continue;
            }
            let text = kiana_json::js_object::stringify(&serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }));
            tools.push(segment(
                ContextSegmentKind::Tool,
                &format!("tool:{}", tool.name),
                "tool",
                estimate_text_tokens(&text),
                &text,
                options.preview_chars,
            ));
        }
    }

    for (message, from_custom) in request_messages.iter().zip(&request_from_custom) {
        if *from_custom {
            // 已在 turn_context 中统计，避免分类合计重复。
            continue;
        }
        match message {
            Message::System(system) => {
                let text = get_system_message_text(system);
                let text_tokens = estimate_text_tokens(&text);
                if text_tokens > 0 || system.sections.is_some() {
                    prompt.push(segment(
                        ContextSegmentKind::Prompt,
                        "system",
                        "system",
                        text_tokens,
                        &text,
                        options.preview_chars,
                    ));
                }
                let tool_tokens = system_tool_tokens(system);
                if tool_tokens > 0 {
                    tools.push(segment(
                        ContextSegmentKind::Tool,
                        "system:tools",
                        "system",
                        tool_tokens,
                        &system_tools_text(system),
                        options.preview_chars,
                    ));
                }
            }
            Message::User(user) => {
                let text = content_text_user_content(&user.content, "\n");
                segments.push(segment(
                    ContextSegmentKind::Segment,
                    "user",
                    "user",
                    estimate_message_tokens(message),
                    &text,
                    options.preview_chars,
                ));
            }
            Message::Assistant(assistant) => {
                let text = content_text_assistant(&assistant.content, "\n");
                // 工具调用的参数在下面单独计入 `tools`；估算消息本身时去掉这些块，
                // 否则同一份字符会被统计两次（分类合计超过 total_tokens）。
                let text_only = AssistantMessage {
                    content: assistant
                        .content
                        .iter()
                        .filter(|block| !matches!(block, AssistantContentBlock::ToolCall(_)))
                        .cloned()
                        .collect(),
                    ..assistant.clone()
                };
                let segment_tokens = estimate_message_tokens(&Message::Assistant(text_only));
                // 流式中的 Pending assistant 属于「本轮上下文」，放进 turn_context，
                // 保持在 segments 里会让 kind 与所属数组不一致（合计对不上）。
                if assistant.stop_reason == kiana_ai::types::StopReason::Pending {
                    turn_context.push(segment(
                        ContextSegmentKind::TurnContext,
                        "assistant",
                        "assistant",
                        segment_tokens,
                        &text,
                        options.preview_chars,
                    ));
                } else {
                    segments.push(segment(
                        ContextSegmentKind::Segment,
                        "assistant",
                        "assistant",
                        segment_tokens,
                        &text,
                        options.preview_chars,
                    ));
                }
                // tool call 的参数也归入工具类，便于定位「哪个工具吃了 token」。
                for block in &assistant.content {
                    let AssistantContentBlock::ToolCall(call) = block else {
                        continue;
                    };
                    let text = format!(
                        "{} {}",
                        call.name,
                        kiana_json::js_object::stringify(&serde_json::Value::Object(
                            call.arguments.clone().into_iter().collect()
                        ))
                    );
                    tools.push(segment(
                        ContextSegmentKind::Tool,
                        &format!("toolCall:{}", call.name),
                        "assistant",
                        estimate_text_tokens(&text),
                        &text,
                        options.preview_chars,
                    ));
                }
            }
            Message::ToolResult(result) => {
                let text = content_text_user(&result.content, "\n");
                segments.push(segment(
                    ContextSegmentKind::Segment,
                    "toolResult",
                    "toolResult",
                    estimate_message_tokens(message),
                    &text,
                    options.preview_chars,
                ));
            }
        }
    }

    let attributed: u64 = tools
        .iter()
        .chain(prompt.iter())
        .chain(turn_context.iter())
        .chain(segments.iter())
        .map(|segment| segment.tokens)
        .sum();
    // usage 锚点与逐段估算不可能完全可加：把残差记成 prompt overhead，保证分段和覆盖总量。
    let overhead = estimate.tokens.saturating_sub(attributed);
    if overhead > 0 {
        prompt.push(ContextSegment {
            kind: ContextSegmentKind::Prompt,
            label: "prompt:overhead".to_string(),
            role: "system".to_string(),
            tokens: overhead,
            preview: String::new(),
            chars: 0,
        });
    }

    let context_window = options
        .context_window
        .or(model.context_window)
        .filter(|window| *window > 0);
    let utilization = context_window
        .map(|window| estimate.tokens as f64 / window as f64)
        .unwrap_or(0.0);

    ContextReport {
        model: model.id.clone(),
        provider: model.provider.clone(),
        total_tokens: estimate.tokens,
        usage_tokens: estimate.usage_tokens,
        trailing_tokens: estimate.trailing_tokens,
        context_window,
        utilization,
        tool_tokens: tools.iter().map(|segment| segment.tokens).sum(),
        prompt_tokens: prompt.iter().map(|segment| segment.tokens).sum(),
        turn_context_tokens: turn_context.iter().map(|segment| segment.tokens).sum(),
        segment_tokens: segments.iter().map(|segment| segment.tokens).sum(),
        tools,
        prompt,
        turn_context,
        segments,
        request_message_count: request_messages.len(),
    }
}

/// 手工压缩的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CondensationPhase {
    /// 已触发，等待 token 下降。
    Started,
    /// token 已下降并稳定。
    Settled,
    /// 超时或失败。
    Failed,
}

/// 一次手工压缩的观测事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CondensationEvent {
    /// 阶段。
    pub phase: CondensationPhase,
    /// 压缩前 token。
    pub before_tokens: u64,
    /// 稳定后的 token（未完成时是最后一次观测值）。
    pub after_tokens: u64,
    /// 节省的 token。
    pub saved_tokens: u64,
    /// 从开始到结束的毫秒数。
    pub elapsed_ms: u64,
    /// 失败原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CondensationEvent {
    /// 是否成功。
    pub fn is_settled(&self) -> bool {
        self.phase == CondensationPhase::Settled
    }
}

/// 手工压缩观测配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CondensationConfig {
    /// token 稳定多久算收敛（默认 2.5s）。
    pub settle_ms: u64,
    /// 总超时（默认 90s）。
    pub timeout_ms: u64,
    /// 轮询间隔（默认 100ms）。
    pub poll_interval_ms: u64,
    /// 至少下降多少 token 才算有效压缩（默认 1）。
    pub min_saved_tokens: u64,
}

impl Default for CondensationConfig {
    fn default() -> Self {
        CondensationConfig {
            settle_ms: 2_500,
            timeout_ms: 90_000,
            poll_interval_ms: 100,
            min_saved_tokens: 1,
        }
    }
}

impl CondensationConfig {
    /// 设置 settle。
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle_ms = settle.as_millis() as u64;
        self
    }

    /// 设置 timeout。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = timeout.as_millis() as u64;
        self
    }
}

/// 等待一次手工压缩收敛。
///
/// `current_tokens` 每次轮询都会被调用一次；返回 `None` 表示 token 数暂时不可用（继续等）。
pub async fn observe_condensation<F, Fut>(
    baseline_tokens: u64,
    mut current_tokens: F,
    config: CondensationConfig,
) -> CondensationEvent
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<u64>>,
{
    let started = tokio::time::Instant::now();
    let timeout = Duration::from_millis(config.timeout_ms.max(1));
    let settle = Duration::from_millis(config.settle_ms);
    let poll = Duration::from_millis(config.poll_interval_ms.max(1));
    let mut lowest = baseline_tokens;
    let mut stable_since: Option<tokio::time::Instant> = None;

    loop {
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let saved = baseline_tokens.saturating_sub(lowest);
            return CondensationEvent {
                phase: CondensationPhase::Failed,
                before_tokens: baseline_tokens,
                after_tokens: lowest,
                saved_tokens: saved,
                elapsed_ms: elapsed.as_millis() as u64,
                error: Some(format!(
                    "condensation did not settle within {} ms",
                    config.timeout_ms
                )),
            };
        }

        // 只有「当前观测 == 历史最低」才算稳定；从最低点回升说明还在变化，必须重新计时。
        // 读取失败（None）不改变稳定状态，也不伪造一次稳定观测。
        let mut at_lowest = false;
        if let Some(observed) = current_tokens().await {
            if observed < lowest {
                lowest = observed;
                stable_since = None;
                at_lowest = true;
            } else if observed == lowest {
                at_lowest = true;
            } else {
                stable_since = None;
            }
        }
        let saved = baseline_tokens.saturating_sub(lowest);
        if saved >= config.min_saved_tokens && at_lowest {
            match stable_since {
                None => stable_since = Some(tokio::time::Instant::now()),
                Some(since) if since.elapsed() >= settle => {
                    let elapsed = started.elapsed();
                    return CondensationEvent {
                        phase: CondensationPhase::Settled,
                        before_tokens: baseline_tokens,
                        after_tokens: lowest,
                        saved_tokens: saved,
                        elapsed_ms: elapsed.as_millis() as u64,
                        error: None,
                    };
                }
                Some(_) => {}
            }
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            continue;
        }
        tokio::time::sleep(poll.min(remaining)).await;
    }
}

fn segment(
    kind: ContextSegmentKind,
    label: &str,
    role: &str,
    tokens: u64,
    text: &str,
    preview_chars: usize,
) -> ContextSegment {
    let chars = text.chars().count();
    let preview = if chars <= preview_chars {
        text.to_string()
    } else {
        let kept: String = text.chars().take(preview_chars).collect();
        format!("{kept}… (+{} more chars)", chars - preview_chars)
    };
    ContextSegment {
        kind,
        label: label.to_string(),
        role: role.to_string(),
        tokens,
        preview,
        chars,
    }
}

fn system_tool_tokens(system: &SystemMessage) -> u64 {
    let added = system
        .tools_added
        .as_ref()
        .map(|tools| {
            estimate_text_tokens(&kiana_json::js_object::stringify(
                &serde_json::to_value(tools).unwrap_or(serde_json::Value::Null),
            ))
        })
        .unwrap_or(0);
    let removed = system
        .tools_removed
        .as_ref()
        .map(|tools| {
            estimate_text_tokens(&kiana_json::js_object::stringify(
                &serde_json::to_value(tools).unwrap_or(serde_json::Value::Null),
            ))
        })
        .unwrap_or(0);
    added + removed
}

fn system_tools_text(system: &SystemMessage) -> String {
    let mut parts = Vec::new();
    if let Some(tools) = &system.tools_added {
        parts.push(format!(
            "toolsAdded: {}",
            kiana_json::js_object::stringify(
                &serde_json::to_value(tools).unwrap_or(serde_json::Value::Null)
            )
        ));
    }
    if let Some(tools) = &system.tools_removed {
        parts.push(format!(
            "toolsRemoved: {}",
            kiana_json::js_object::stringify(
                &serde_json::to_value(tools).unwrap_or(serde_json::Value::Null)
            )
        ));
    }
    parts.join("\n")
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JsonObject;
    use kiana_ai::types::{
        AssistantMessage, AssistantRole, StopReason, SystemContent, SystemMessage, TextContent,
        TextKind, Tool,
    };

    fn model() -> Model {
        Model {
            id: "test-model".to_string(),
            provider: "test".to_string(),
            context_window: Some(100_000),
            ..Model::default()
        }
    }

    fn custom_message(role: &str, text: &str) -> AgentMessage {
        let object: JsonObject = serde_json::json!({ "role": role, "content": text })
            .as_object()
            .expect("object")
            .clone()
            .into_iter()
            .collect();
        AgentMessage::Custom(Box::new(object))
    }

    #[test]
    fn projectable_custom_messages_are_counted_in_the_request() {
        let options = ContextReportOptions::default();
        let base = AgentContext {
            messages: Vec::new(),
            tools: None,
        };
        let with_custom = AgentContext {
            messages: vec![custom_message(
                "system",
                "a long custom system prompt that the real request includes",
            )],
            tools: None,
        };
        let without = build_context_report(&base, &model(), &options);
        let report = build_context_report(&with_custom, &model(), &options);
        assert!(
            report.total_tokens > without.total_tokens,
            "custom system prompt 会进入真实请求，total_tokens 不能漏算"
        );
        assert_eq!(
            report.request_message_count,
            without.request_message_count + 1
        );
        assert_eq!(report.turn_context.len(), 1);
        let turn_context_tokens: u64 = report
            .turn_context
            .iter()
            .map(|segment| segment.tokens)
            .sum();
        assert_eq!(report.turn_context_tokens, turn_context_tokens);
    }

    #[test]
    fn caller_projection_controls_which_custom_messages_reach_the_request() {
        let options = ContextReportOptions::default();
        let context = AgentContext {
            messages: vec![custom_message("compactionSummary", "summary text")],
            tools: None,
        };
        let baseline = build_context_report(&context, &model(), &options);
        let projected =
            build_context_report_with(&context, &model(), &options, |message| match message {
                AgentMessage::Standard(message) => Some(message.clone()),
                AgentMessage::Custom(object) => {
                    (object.get("role").and_then(serde_json::Value::as_str)
                        == Some("compactionSummary"))
                    .then(|| {
                        Message::User(UserMessage {
                            role: UserRole::User,
                            content: UserContent::Text("summary text".to_string()),
                            timestamp: 0,
                        })
                    })
                }
            });
        assert!(
            projected.total_tokens > baseline.total_tokens,
            "会话层投影会把 compactionSummary 变成 user 文本，报告必须跟随"
        );
        assert_eq!(
            projected.request_message_count,
            baseline.request_message_count + 1
        );
    }

    #[test]
    fn tools_declared_in_system_messages_are_counted_once() {
        let tool = Tool {
            name: "bash".to_string(),
            description: "run commands".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
        };
        let system = AgentMessage::Standard(Message::System(SystemMessage {
            role: Default::default(),
            content: SystemContent::Text("system".to_string()),
            sections: None,
            tools_added: Some(vec![tool.clone()]),
            tools_removed: None,
            timestamp: 1,
        }));
        struct NoopTool;
        impl crate::types::AgentToolExecute for NoopTool {
            fn execute<'a>(
                &'a self,
                _tool_call_id: &'a str,
                _params: serde_json::Value,
                _signal: Option<&'a tokio_util::sync::CancellationToken>,
                _on_update: Option<crate::types::AgentToolUpdateCallback>,
            ) -> crate::types::BoxFuture<
                'a,
                Result<
                    crate::types::AgentToolResult<serde_json::Value>,
                    crate::types::AgentToolError,
                >,
            > {
                Box::pin(async {
                    Err(crate::types::AgentToolError {
                        message: "unused".to_string(),
                        name: None,
                        stack: None,
                    })
                })
            }
        }
        let agent_tool = crate::types::AgentTool {
            name: "bash".to_string(),
            label: "bash".to_string(),
            description: "run commands".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: std::sync::Arc::new(NoopTool),
        };
        let context = AgentContext {
            messages: vec![system],
            tools: Some(vec![agent_tool]),
        };
        let report = build_context_report(&context, &model(), &ContextReportOptions::default());
        assert!(
            report
                .tools
                .iter()
                .all(|segment| segment.label != "tool:bash"),
            "toolsAdded 已经统计过的工具不能重复计入顶层 tools"
        );
    }

    #[test]
    fn pending_assistant_messages_live_in_turn_context() {
        let assistant = AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "streaming…".to_string(),
                text_signature: None,
            })],
            stop_reason: StopReason::Pending,
            ..AssistantMessage::default()
        };
        let context = AgentContext {
            messages: vec![AgentMessage::Standard(Message::Assistant(assistant))],
            tools: None,
        };
        let report = build_context_report(&context, &model(), &ContextReportOptions::default());
        assert!(
            report
                .turn_context
                .iter()
                .any(|segment| segment.role == "assistant"),
            "Pending assistant 应出现在 turn_context"
        );
        assert!(
            report
                .segments
                .iter()
                .all(|segment| segment.role != "assistant"),
            "segments 里不能出现 kind 为 TurnContext 的分段"
        );
    }
}
