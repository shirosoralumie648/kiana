//! agent.ts：`Agent` 状态机、队列与 run 生命周期。
//!
//! TS 源：`packages/agent/src/agent.ts`。`Agent` 是低层 agent loop 的有状态包装：
//! 拥有当前 transcript、发出生命周期事件、执行工具，并提供 steering / follow-up 队列。
//!
//! 调用方：`kiana-session` 的 `AgentSession` 与 SDK 构造并驱动 [`Agent`]；[`Agent`] 内部
//! 调用 `crate::agent_loop` 的 `run_agent_loop` / `run_agent_loop_continue` 执行 turn 循环，
//! 后者通过事件 sink 回调 [`Agent`] 的私有 `process_events`。本文件只负责状态、队列、
//! run 生命周期与事件转发，不考虑 provider 细节（那是 `kiana-ai` 的职责）。
//!
//! # 与 TS 的形态差异（行为等价）
//!
//! - TS 的 `state` 是带 getter/setter 的**活对象**（`state.messages.push(...)` 直接改内部
//!   数组）；Rust 的 [`AgentState`] 是冻结的普通结构体，[`Agent::state`] 返回**快照**。
//!   等价的写入口是 [`Agent::set_tools`]/[`Agent::set_messages`]/[`Agent::push_message`]
//!   等（赋值语义与 TS setter 一样复制顶层数组）。
//! - TS 的 listener 可以抛错中止 run；Rust listener 返回 `()`，不存在这条失败路径。
//! - TS 的 listener 注册表是 `Set`：[`AgentListener`] 的**引用身份**等价于 JS 的函数身份
//!   （同一个 `Arc` 克隆订阅两次只保留一个条目，`Arc::ptr_eq` 判定）。派发时按 TS 的**活
//!   Set 迭代**语义进行：listener 在某个事件派发中途新订阅的会收到**当前**事件；在轮到自己
//!   之前退订的会被跳过；`delete` + `add` 会被再次访问。返回的 [`Subscription`] **不会**在
//!   drop 时自动退订（TS 丢弃返回的闭包时 listener 仍然有效），必须显式
//!   [`Subscription::unsubscribe`]。
//! - TS 的 `processEvents` 每次为 `pendingToolCalls` 新建 `Set` 实例（`ReadonlySet` 消费者
//!   能看到新引用）；Rust 的 [`AgentState::pending_tool_calls`] 是冻结的 `BTreeSet`，
//!   只有值语义（插入/删除后就地生效），没有引用身份语义。
//! - TS 的 `AbortSignal` 由 `AbortController` 产生；Rust 用 [`CancellationToken`]（§4.2），
//!   [`Agent::signal`] 返回当前 run 的令牌克隆（取消状态共享）。
//! - TS 的 `prompt(input: string, images?)` / `prompt(message)` / `prompt(messages[])` 是
//!   重载；Rust 拆成 [`Agent::prompt_text`]/[`Agent::prompt_text_with_images`]/
//!   [`Agent::prompt`]/[`Agent::prompt_messages`]（`prompt` 对应单条消息，`prompt_messages`
//!   对应数组，两者都不复制、不加时间戳，与 TS 的 `normalizePromptInput` 一致）。
//! - `continue()` 是 Rust 关键字，方法名为 [`Agent::continue_`]。
//! - TS 的 `prompt()`/`continue()` 是 JS promise：调用即同步进入 `runWithLifecycle`（置
//!   `activeRun`/`isStreaming`）并开始运行；调用方不 `await`、丢弃 promise 甚至只看一眼
//!   都不影响 run 跑完。Rust 的对应方法**不是** `async fn`：它们同步完成前置检查、登记
//!   `activeRun`，并在调用线程上把 run 执行到 TS 的第一个 `await`（`agent_start` 派发完成、
//!   listener 的同步部分已运行），再把 run 交给 tokio 任务继续，返回一个等待 run 结束的
//!   future（`.await` 用法不变）。因此 (a) 丢弃返回的 future 不会中断 run，
//!   `finally { finishRun() }` 的等价清理一定会发生（run 的 Drop guard 兜底，包括任务在
//!   首次被 poll 前就被运行时丢弃的情形）；(b) fire-and-forget 调用也会真正开跑。
//!   唯一残留差异：不在 tokio 运行时上下文里调用时无法 spawn，run 会退化为在返回的 future
//!   首次被 poll 时执行（Rust async 的惰性），这是“没有运行时就无法运行”的退化路径。
//! - JS 是单线程 + 微任务模型：`prompt()` 返回后，调用方的**同步代码**先于任何微任务执行，
//!   因此紧随其后的 `steer()`/`followUp()` 一定会被 run 的初始 steering 轮询取到。Rust 的
//!   run 在第一个挂起点之后由多线程运行时调度，调用方的同步代码与 run 的继续之间没有
//!   happens-before 关系：紧跟在 `prompt()` 之后的同步 `steer()` 可能晚于初始 steering
//!   轮询，从而在下一轮 turn 边界才被取走（消息不会丢失，仍留在队列里；若本次只有一轮且
//!   不轮询 steering，则留到下一次 `prompt()`/`continue()`）。这是调度模型差异，无法在不
//!   引入额外同步屏障（会破坏 fire-and-forget 语义）的前提下消除。
//!
//! # 一次 run 的执行流程
//!
//! 1. 入口方法（[`Agent::prompt`] / [`Agent::prompt_messages`] / [`Agent::prompt_text`] /
//!    [`Agent::continue_`]）先在锁内检查 `active_run`，冲突时返回各自对应的
//!    [`AgentError`]（不产生事件、不登记 run）。
//! 2. `create_context_snapshot` 复制 `state.messages` / `state.tools`，`create_loop_config`
//!    把当前配置整体克隆进 [`AgentLoopConfig`]；本次 run 从此使用自己的快照。
//! 3. `begin_run` → `claim_run` 登记 `active_run`、置 `is_streaming = true`，然后把 run
//!    同步轮询到 `agent_start` 派发的挂起点（listener 的同步部分已运行；见下方 `begin_run`
//!    的“同步前缀”说明），再 `tokio::spawn` 交给运行时；返回的 future 只等待 run 结束。
//! 4. 低层循环（`crate::agent_loop`）发出 `turn_start` / `message_*` /
//!    `tool_execution_*` / `turn_end` / `agent_end`；每个事件先由 `process_events` 写入
//!    [`AgentState`]，再按订阅顺序串行 `await` listener。
//! 5. run 结束（成功、失败合成、panic 或任务被运行时丢弃）时 `finish_run` 清除
//!    `active_run` 并唤醒 [`Agent::wait_for_idle`] 的等待者。
//!
//! # 并发与锁模型
//!
//! - 内部状态按字段分锁（见 `AgentInner`），没有一把全局大锁；锁只在同步代码块内持有，
//!   **不跨 `.await`**（返回的 future 要求 `Send`，而 `std::sync::MutexGuard` 不是 `Send`）。
//! - “同一时刻至多一个 run”由 `active_run` 这个单槽位加 `claim_run` 的同临界区检查保证；
//!   所有 [`Agent`] 克隆共享同一个 `AgentInner`，因此跨任务调用也受同一道闸门约束。
//! - listener 的派发是串行的：一个事件的 listener 依次被 `await`。`Agent::steer` /
//!   `Agent::follow_up` / 各 `set_*` 可以在 run 进行中从其他任务调用，由各自的锁保护。
//!
//! # 事件到状态的映射
//!
//! `process_events` 对每个事件先更新 [`AgentState`]，再派发 listener：
//!
//! | 事件 | 状态变化 |
//! |---|---|
//! | `message_start` / `message_update` | `streaming_message = Some(message)` |
//! | `message_end` | `streaming_message = None`；`messages.push(message)` |
//! | `tool_execution_start` | `pending_tool_calls.insert(toolCallId)` |
//! | `tool_execution_end` | `pending_tool_calls.remove(toolCallId)` |
//! | `turn_end` | 若 assistant 消息带非空 `errorMessage`，写入 `error_message` |
//! | `agent_end` | `streaming_message = None` |
//!
//! `is_streaming` 与 `active_run` 不由事件维护，而由 `claim_run` / `finish_run` 维护；
//! `system_prompt` 在读取时才从 transcript 重放。
//!
//! # 失败模式
//!
//! - 前置检查失败：从返回的 future 里以 [`AgentError`] 给出（future 立即就绪）。
//! - run 内部失败：由 `handle_run_failure` 合成一条 assistant 失败消息，发出
//!   `message_start` → `message_end` → `turn_end` → `agent_end`；正常情况下调用方的 future
//!   返回 `Ok`（失败信息在消息与事件里，而不是返回值里），只有收尾事件派发本身出错时才
//!   返回 `Err`。
//! - listener 的 future 返回 `()`，不存在“listener 抛错中止 run”的路径；
//!   `process_events` 唯一的错误是防御性的 `ListenerOutsideActiveRun`。
//! - run 任务 panic 时 `RunGuard` 的 `Drop` 仍会执行 `finish_run`，不会把 Agent 卡在
//!   processing 状态。
//!
//! # 阅读顺序
//!
//! `AgentOptions` / `AgentInitialState` → `AgentInner` → [`Agent::new`] →
//! [`Agent::prompt`] / [`Agent::continue_`] → `begin_run` / `claim_run` → `event_sink` /
//! `process_events` → `finish_run`。理解与 TS 的差异先读上面的“与 TS 的形态差异”。
//!
//! 错误文本是用户可见的稳定契约（见 `spec/agent-codemode.md` §3.2），改动需同步测试。

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{SystemTime, UNIX_EPOCH};

use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, ClientIdentity, ImageContent, Message,
    Model, ModelThinkingLevel, OnPayloadAsyncFn, OnPayloadFn, OnProviderStreamEventFn,
    OnResponseFn, SimpleStreamOptions, StopReason, StreamOptions, SystemContent, SystemMessage,
    SystemRole, TextContent, TextKind, ThinkingBudgets, ThinkingLevel as AiThinkingLevel, Tool,
    Transport, Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_ai::utils::text::get_system_message_text;
use kiana_ai::utils::transcript::{
    create_initial_system_message, get_current_system_message, to_tool_declaration,
};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{AgentEventSink, AgentLoopError, run_agent_loop, run_agent_loop_continue};
use crate::stream_fn::{DefaultStreamFnError, require_default_stream_fn};
use crate::types::{
    AfterToolCallFn, AgentContext, AgentEvent, AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage,
    AgentState, AgentTool, BeforeToolCallFn, BoxFuture, FinishTurnFn, GetApiKeyFn, GetMessagesFn,
    JsonValue, PrepareNextTurnContext, PrepareNextTurnFn, PrepareRequestFn, StreamFn,
    ThinkingLevel, ToolExecutionMode, TransformContextFn,
};

// TS: `export type { QueueMode } from "./types.ts";`
pub use crate::types::QueueMode;

// ---------------------------------------------------------------------------
// 默认值
// ---------------------------------------------------------------------------

/// `EMPTY_USAGE`：失败合成消息的用量。
fn empty_usage() -> Usage {
    Usage::default()
}

/// `DEFAULT_MODEL`：`initialState.model` 缺省时的状态模型。
///
/// TS 是对象字面量常量；Rust 的 [`Model`] 带 `Option` 字段（`reasoning`/`contextWindow`/
/// `maxTokens`），TS 里这几个字段存在且为 `false`/`0`/`0`，因此用 `Some(..)` 表达。
fn default_model() -> Model {
    Model {
        id: "unknown".to_string(),
        name: "unknown".to_string(),
        api: "unknown".to_string(),
        provider: "unknown".to_string(),
        base_url: String::new(),
        input: Vec::new(),
        reasoning: Some(false),
        cost: Default::default(),
        context_window: Some(0),
        max_tokens: Some(0),
        ..Default::default()
    }
}

/// 当前 Unix 毫秒（`Date.now()`）。
///
/// 系统时钟早于 Unix 纪元导致 `duration_since` 失败时返回 0；`as_millis()` 结果按 `i64`
/// 截断（当前时间尺度下不会溢出）。
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 取锁并忽略 poisoning（`into_inner`）。
///
/// 某个线程在持锁期间 panic 会把 `Mutex` 标记为 poisoned；这里仍取用内部值，使 Agent 在
/// 一次 listener/钩子 panic 之后依然可读、可清理（run 的 panic 由 `RunGuard` 兜底）。
/// 代价是：若 panic 恰好发生在多字段更新中途，读到的状态可能是部分更新的，调用方不能
/// 假设 panic 后各字段互相一致。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `thinkingLevel === "off"` → `undefined`，其余原样映射到 kiana-ai 的 [`AiThinkingLevel`]。
fn to_ai_thinking_level(level: ThinkingLevel) -> Option<AiThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(AiThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(AiThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(AiThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(AiThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(AiThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(AiThinkingLevel::Max),
    }
}

/// `defaultConvertToLlm(messages)`：只保留 `role` ∈ {system,user,assistant,toolResult} 的消息。
///
/// TS 的实现按 **role** 过滤（`message.role === "system" || ...`），自定义 role
/// （coding-agent 的 `bashExecution`/`custom`/`branchSummary`/`compactionSummary`）被丢弃；
/// 但 `role` 是四种标准之一的普通对象（coding-agent 用 `{role:"system", sections}` 承载
/// prompt 的命名 section）会**保留**并原样交给 provider。Rust 用 [`AgentMessage::Custom`]
/// 携带这类对象，这里按 role 投影回标准 [`Message`]：
/// - `system`：`content`/`sections`/`toolsAdded`/`toolsRemoved`/`timestamp` 逐字段读取；
/// - `user`：`content` 按 `string | (TextContent | ImageContent)[]` 解析，`timestamp` 取整；
/// - `assistant` / `toolResult`：按标准结构反序列化（字段不全时丢弃，Rust 的封闭联合无法
///   表达 TS 的结构化宽类型）。
fn default_convert_to_llm(messages: Vec<AgentMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(message) => Some(message),
            AgentMessage::Custom(object) => custom_message_projection(&object),
        })
        .collect()
}

/// 把 role 为标准四类之一的自定义消息投影成 [`Message`]（见 [`default_convert_to_llm`]）。
pub(crate) fn custom_message_projection(object: &crate::types::JsonObject) -> Option<Message> {
    let role = object.get("role").and_then(serde_json::Value::as_str)?;
    match role {
        "system" => Some(Message::System(custom_system_projection(object))),
        "user" => {
            let content = object
                .get("content")
                .and_then(|value| serde_json::from_value::<UserContent>(value.clone()).ok())
                .unwrap_or_default();
            let timestamp = object
                .get("timestamp")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            Some(Message::User(UserMessage {
                role: UserRole::User,
                content,
                timestamp,
            }))
        }
        "assistant" => serde_json::from_value(JsonValue::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ))
        .ok()
        .map(Message::Assistant),
        "toolResult" => serde_json::from_value(JsonValue::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ))
        .ok()
        .map(Message::ToolResult),
        _ => None,
    }
}

/// `toToolDeclaration(agentTool)`：把 [`AgentTool`] 投影成模型可见的工具声明。
fn agent_tool_declaration(tool: &AgentTool) -> Tool {
    to_tool_declaration(&Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
        constrained_sampling: tool.constrained_sampling.clone(),
    })
}

/// 把 `AgentMessage[]` 投影成 transcript 回放能消费的 `Message[]`。
///
/// TS 的 `getCurrentSystemMessage` / `getCurrentSystemPrompt` 接受 `readonly { role }[]`，
/// 按 `role === "system"` 取消息。自定义 system 消息（coding-agent 的 sections 载体）同样参与
/// 回放：这里投影成 [`SystemMessage`]（`content`、`sections`、`toolsAdded`、`toolsRemoved`、
/// `timestamp` 按字段读取，缺省/类型不符时取空，与 TS 的属性访问一致）。
fn project_system_messages(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::System(system)) => {
                Some(Message::System(system.clone()))
            }
            AgentMessage::Custom(object) if message.role() == "system" => {
                Some(Message::System(custom_system_projection(object)))
            }
            _ => None,
        })
        .collect()
}

/// 自定义 `role: "system"` 消息 → [`SystemMessage`]。
fn custom_system_projection(object: &crate::types::JsonObject) -> SystemMessage {
    SystemMessage {
        role: SystemRole::System,
        content: object
            .get("content")
            .and_then(|value| serde_json::from_value::<SystemContent>(value.clone()).ok())
            .unwrap_or_else(|| SystemContent::Text(String::new())),
        sections: object
            .get("sections")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tools_added: object
            .get("toolsAdded")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        tools_removed: object
            .get("toolsRemoved")
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        timestamp: object
            .get("timestamp")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
    }
}

/// TS `let timestamp; … timestamp ??= message.timestamp` 的变量状态。
///
/// `??=` 只判断 nullish：已经是非 nullish 的值不会再被覆盖；`null` 会被后续消息再次覆盖。
/// 因此最终值要么是**第一条非 nullish** 的 `timestamp`，要么是“全部 nullish”时最后一条
/// system 消息的值（`null` 或 `undefined`）。后者决定 TS 的
/// `timestamp === undefined && tools.length === 0 → undefined`。
enum JsTimestamp {
    /// JS `undefined`：属性缺失，或被后续缺失属性的消息覆盖。
    Undefined,
    /// JS `null`。
    Null,
    /// 非 nullish 的值（Rust 侧 i64 化；非数值按 0，见 [`system_message_timestamp`]）。
    Number(i64),
}

/// 读取一条 system 消息的 `timestamp` 属性。
///
/// 外层 `None` = 属性缺失（JS `undefined`）；`Some(None)` = `null`；`Some(Some(v))` =
/// 非 nullish 值。JSON 没有 `undefined`，因此“没有该键”就是属性缺失（未定义）的自然表示。
/// 非整数数值截断，`null` 以外的非数值（字符串/对象等）按 0 处理——TS 会原样保留，
/// 但 Rust 的 [`SystemMessage::timestamp`] 是 `i64`。
fn system_message_timestamp(message: &AgentMessage) -> Option<Option<i64>> {
    match message {
        AgentMessage::Standard(Message::System(system)) => Some(Some(system.timestamp)),
        AgentMessage::Custom(object) => match object.get("timestamp") {
            None => None,
            Some(value) if value.is_null() => Some(None),
            Some(value) => Some(Some(
                value
                    .as_i64()
                    .or_else(|| value.as_f64().map(|value| value as i64))
                    .unwrap_or(0),
            )),
        },
        _ => None,
    }
}

/// `getCurrentSystemMessage(messages)` 作用于 `AgentMessage[]` 的完整语义。
///
/// [`project_system_messages`] 把自定义 system 消息投影成 [`SystemMessage`]，而该类型的
/// `timestamp` 是必需的 `i64`，所以 replay 结果总是“带时间戳”。TS 的
/// `timestamp ??= message.timestamp` 允许一条**没有** `timestamp` 的 system 消息（coding-agent
/// 的 sections 载体）不设置时间戳，于是 `timestamp === undefined && tools.length === 0` 时返回
/// `undefined`（`state.systemPrompt === ""`、`reset()` 清空 transcript）。这里补回该判定，
/// 并用 `??=` 求出的时间戳修正 replay 结果（最终 `timestamp ?? 0`）。
fn current_system_message(messages: &[AgentMessage]) -> Option<SystemMessage> {
    let mut replayed = get_current_system_message(&project_system_messages(messages))?;

    // TS: `for (const message of messages) { if (!isSystemMessage(message)) continue;
    //        timestamp ??= message.timestamp; }`
    let mut timestamp = JsTimestamp::Undefined;
    for message in messages.iter().filter(|message| message.role() == "system") {
        if !matches!(timestamp, JsTimestamp::Undefined | JsTimestamp::Null) {
            break;
        }
        timestamp = match system_message_timestamp(message) {
            None => JsTimestamp::Undefined,
            Some(None) => JsTimestamp::Null,
            Some(Some(value)) => JsTimestamp::Number(value),
        };
    }

    // TS: `if (timestamp === undefined && tools.length === 0) return undefined;`
    if matches!(timestamp, JsTimestamp::Undefined)
        && replayed.tools_added.as_ref().is_none_or(Vec::is_empty)
    {
        return None;
    }
    // TS: `timestamp ?? 0`
    replayed.timestamp = match timestamp {
        JsTimestamp::Number(value) => value,
        JsTimestamp::Null | JsTimestamp::Undefined => 0,
    };
    Some(replayed)
}

/// `getCurrentSystemPrompt(messages)`（基于 [`current_system_message`]）。
fn current_system_prompt(messages: &[AgentMessage]) -> String {
    match current_system_message(messages) {
        Some(message) => get_system_message_text(&message),
        None => String::new(),
    }
}

/// `AgentMessage.errorMessage`（只对 `role === "assistant"` 有意义）。
///
/// TS 的 `event.message.errorMessage` 对标准与自定义 assistant 消息都会读取；空字符串按
/// JS 假值处理（`if (... && event.message.errorMessage)`），因此这里返回 `None`。
fn assistant_error_message(message: &AgentMessage) -> Option<&str> {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => assistant
            .error_message
            .as_deref()
            .filter(|text| !text.is_empty()),
        AgentMessage::Custom(object) if message.role() == "assistant" => object
            .get("errorMessage")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// AgentInitialState / AgentOptions
// ---------------------------------------------------------------------------

/// `AgentInitialState = Partial<Omit<AgentState, "pendingToolCalls" | "isStreaming" |
/// "streamingMessage" | "errorMessage">>`。
///
/// `systemPrompt` 与 `tools` 会变成首条 system 消息，除非 `messages` 已经以 system 开头。
#[derive(Debug, Clone, Default)]
pub struct AgentInitialState {
    /// `systemPrompt`：转成首条 system 消息的 `content`。
    pub system_prompt: Option<String>,
    /// 初始模型；缺省为 `default_model()`（TS 的 `DEFAULT_MODEL`）。
    pub model: Option<Model>,
    /// 初始思考等级；缺省 `"off"`。
    pub thinking_level: Option<ThinkingLevel>,
    /// 初始可执行工具（会被复制）。
    pub tools: Option<Vec<AgentTool>>,
    /// 初始 transcript（会被复制）。
    pub messages: Option<Vec<AgentMessage>>,
}

/// `convertToLlm`：`AgentMessage[] → Message[]`（TS 允许返回 Promise，Rust 冻结契约为同步）。
pub type ConvertToLlmFn = Arc<dyn Fn(Vec<AgentMessage>) -> Vec<Message> + Send + Sync>;

/// `AgentOptions.prepareNextTurn`（旧形态：只收 signal）。
pub type AgentPrepareNextTurnFn = Arc<
    dyn Fn(Option<CancellationToken>) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// `AgentOptions.prepareNextTurnWithContext`：收 context + signal，返回下一轮替换状态。
pub type AgentPrepareNextTurnWithContextFn = Arc<
    dyn Fn(
            PrepareNextTurnContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// `Agent.subscribe` 的 listener。
///
/// TS: `(event: AgentEvent, signal: AbortSignal) => Promise<void> | void`。Rust 的 listener
/// 总是异步（同步 listener 用 `Box::pin(async {})` 包一层），第二个参数是本次 run 的
/// [`CancellationToken`]（借用，避免每次事件克隆句柄）。
pub type AgentListener =
    Arc<dyn for<'a> Fn(AgentEvent, &'a CancellationToken) -> BoxFuture<'a, ()> + Send + Sync>;

/// `AgentOptions`：构造 [`Agent`] 的选项。
///
/// TS 的 `options` 与 `streamFn` 在类型上必填、运行期容错（旧编译产物可能省略）。Rust 用
/// `Option` 表达同样的容错：`stream_fn` 为 `None` 时在构造期急切调用
/// [`require_default_stream_fn`]（与 TS 的 `runtimeOptions.streamFn ?? getDefaultStreamFn()`
/// 相同：未配置默认值时 `new` 立即失败）。
#[derive(Clone, Default)]
pub struct AgentOptions {
    /// 初始状态；缺省等价于空 transcript、`DEFAULT_MODEL`、`"off"`、无工具。
    ///
    /// `system_prompt` 与 `tools` 只有在 `messages` 不以 system 消息开头时才会被折叠成
    /// 首条 system 消息（见 `create_mutable_agent_state`）。
    pub initial_state: Option<AgentInitialState>,
    /// `convertToLlm`；缺省 `default_convert_to_llm`（本文件的私有函数，按 role 过滤）。
    pub convert_to_llm: Option<ConvertToLlmFn>,
    /// `transformContext`。
    pub transform_context: Option<TransformContextFn>,
    /// `streamFn`；缺省用进程级默认流函数（急切求值）。
    pub stream_fn: Option<StreamFn>,
    /// `getApiKey(provider)`。
    pub get_api_key: Option<GetApiKeyFn>,
    /// `onPayload`。
    pub on_payload: Option<OnPayloadFn>,
    /// 异步版 `onPayload`（扩展桥）。
    pub on_payload_async: Option<OnPayloadAsyncFn>,
    /// `onResponse`。
    pub on_response: Option<OnResponseFn>,
    /// `onProviderStreamEvent`。
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    /// `beforeToolCall`。
    pub before_tool_call: Option<BeforeToolCallFn>,
    /// `afterToolCall`。
    pub after_tool_call: Option<AfterToolCallFn>,
    /// `finishTurn`。
    pub finish_turn: Option<FinishTurnFn>,
    /// `prepareRequest`。
    pub prepare_request: Option<PrepareRequestFn>,
    /// `prepareNextTurn`（旧形态）。
    pub prepare_next_turn: Option<AgentPrepareNextTurnFn>,
    /// `prepareNextTurnWithContext`（优先于旧形态）。
    pub prepare_next_turn_with_context: Option<AgentPrepareNextTurnWithContextFn>,
    /// `steeringMode`；缺省 `"one-at-a-time"`。
    pub steering_mode: Option<QueueMode>,
    /// `followUpMode`；缺省 `"one-at-a-time"`。
    pub follow_up_mode: Option<QueueMode>,
    /// `sessionId`：转发给 provider（缓存感知后端）。
    pub session_id: Option<String>,
    /// `thinkingBudgets`：转发给流函数。
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// `transport`；缺省 `"auto"`。
    pub transport: Option<Transport>,
    /// `maxRetryDelayMs`。
    pub max_retry_delay_ms: Option<u64>,
    /// `toolExecution`；缺省 `"parallel"`。
    pub tool_execution: Option<ToolExecutionMode>,
    /// `identity`：请求身份，逐字段覆盖 `DEFAULT_AGENT_IDENTITY`。
    pub identity: Option<ClientIdentity>,
    /// 失控保护（loop guard）；`None` 表示不启用。
    pub loop_guard: Option<crate::loop_guard::LoopGuardHandle>,
    /// turn / token / cost / wall-clock 预算；`None` 表示不启用。
    pub budget: Option<crate::budget::BudgetHandle>,
}

// ---------------------------------------------------------------------------
// AgentError
// ---------------------------------------------------------------------------

/// [`Agent`] 的同步前置错误（错误文本是 `spec/agent-codemode.md` §3.2 的稳定契约，改动需同步测试）。
///
/// run 内部失败（流函数报错等）**不**从 [`Agent::prompt`] 返回：与 TS 一样，它会被合成为一条
/// assistant 失败消息并经事件发出。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AgentError {
    /// `reset()` 运行中。
    #[error("Agent is already processing. Wait for completion before resetting.")]
    ResetWhileProcessing,
    /// `prompt()` 运行中。
    #[error(
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    )]
    PromptWhileProcessing,
    /// `continue()` 运行中。
    #[error("Agent is already processing. Wait for completion before continuing.")]
    ContinueWhileProcessing,
    /// `continue()` 没有可继续的对话。
    #[error("No messages to continue from")]
    NoMessagesToContinue,
    /// `continue()` 尾巴是 assistant 且两个队列都空。
    #[error("Cannot continue from message role: assistant")]
    CannotContinueFromAssistant,
    /// `processEvents` 在 run 之外被调用（防御性；loop 不会触发）。
    #[error("Agent listener invoked outside active run")]
    ListenerOutsideActiveRun,
    /// `runWithLifecycle` 的防御性冲突检查。
    #[error("Agent is already processing.")]
    AlreadyProcessing,
    /// 构造时未配置默认流函数（`getDefaultStreamFn()` 的逐字错误）。
    #[error("{0}")]
    DefaultStreamFn(#[from] DefaultStreamFnError),
    /// 低层循环的前置校验错误（`runAgentLoopContinue` 的同步抛出）。
    #[error("{0}")]
    Loop(#[from] AgentLoopError),
}

// ---------------------------------------------------------------------------
// PendingMessageQueue
// ---------------------------------------------------------------------------

/// `PendingMessageQueue`：steering / follow-up 队列。
///
/// 先进先出，不做去重或合并。`mode` 只在 `peek` / `drain` 时生效：`"all"` 一次取走全部，
/// `"one-at-a-time"` 只取队首一条并把其余留在队列里；修改 mode 会立刻改变之后每次
/// `drain` 的批量大小（已入队的消息不受影响）。
#[derive(Debug, Clone)]
struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        PendingMessageQueue {
            messages: Vec::new(),
            mode,
        }
    }

    /// `enqueue(message)`。
    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    /// `hasItems()`。
    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    /// `peek()`：`"all"` 返回全部；`"one-at-a-time"` 只返回第一条。
    fn peek(&self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => self.messages.clone(),
            QueueMode::OneAtATime => self.messages.first().cloned().into_iter().collect(),
        }
    }

    /// `drain()`：取走 `peek()` 选中的消息，其余保留。
    fn drain(&mut self) -> Vec<AgentMessage> {
        let drained = self.peek();
        self.messages.drain(..drained.len());
        drained
    }

    /// `clear()`。
    fn clear(&mut self) {
        self.messages.clear();
    }
}

/// [`Agent::run_prompt_messages_with`] 的消息来源。
///
/// `Fixed` 是 `prompt()` 的显式输入；`Steering`/`FollowUp` 在 executor 启动（即
/// `claim_run` 成功）之后才从队列 `drain()`。`continue_` 若提前出队再 claim 失败，这批
/// 用户消息会随 executor 一起被丢弃，且调用方只看到 `ContinueWhileProcessing`。
enum PromptMessages {
    Fixed(Vec<AgentMessage>),
    Steering(Arc<AgentInner>),
    FollowUp(Arc<AgentInner>),
}

impl PromptMessages {
    fn take(self) -> Vec<AgentMessage> {
        match self {
            PromptMessages::Fixed(messages) => messages,
            PromptMessages::Steering(inner) => lock(&inner.steering_queue).drain(),
            PromptMessages::FollowUp(inner) => lock(&inner.follow_up_queue).drain(),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentInner / ActiveRun
// ---------------------------------------------------------------------------

/// `ActiveRun = { promise, resolve, abortController }` 的 Rust 形态。
struct ActiveRun {
    /// 本次 run 的唯一编号：`finishRun` 只能清理自己那次 run（见 [`Agent::finish_run`]）。
    id: u64,
    /// `abortController.signal`。
    signal: CancellationToken,
    /// `promise`：`finishRun` 里 resolve（多等待者共享）。
    done: tokio::sync::watch::Sender<bool>,
}

/// listener 注册表条目（TS 的 `Set` 条目）。
///
/// `seq` 是单调递增的订阅序号：`Set` 语义是插入序，删除后重新 `add` 会追加到末尾，
/// 因此每次真正插入都分配新序号；派发时用“已访问到的最大序号”模拟活迭代。
struct ListenerEntry {
    seq: u64,
    listener: AgentListener,
}

/// 构造期固定的 Agent 配置（TS 的公开可写字段）。
///
/// 与 TS 一样，这些字段在 run 开始时被快照进 `AgentLoopConfig`；run 中修改只影响下一次 run。
#[derive(Clone)]
struct AgentConfig {
    convert_to_llm: ConvertToLlmFn,
    transform_context: Option<TransformContextFn>,
    stream_function: StreamFn,
    get_api_key: Option<GetApiKeyFn>,
    on_payload: Option<OnPayloadFn>,
    on_payload_async: Option<OnPayloadAsyncFn>,
    on_response: Option<OnResponseFn>,
    on_provider_stream_event: Option<OnProviderStreamEventFn>,
    before_tool_call: Option<BeforeToolCallFn>,
    after_tool_call: Option<AfterToolCallFn>,
    finish_turn: Option<FinishTurnFn>,
    prepare_request: Option<PrepareRequestFn>,
    prepare_next_turn: Option<AgentPrepareNextTurnFn>,
    prepare_next_turn_with_context: Option<AgentPrepareNextTurnWithContextFn>,
    session_id: Option<String>,
    thinking_budgets: Option<ThinkingBudgets>,
    transport: Transport,
    max_retry_delay_ms: Option<u64>,
    tool_execution: ToolExecutionMode,
    identity: Option<ClientIdentity>,
    loop_guard: Option<crate::loop_guard::LoopGuardHandle>,
    budget: Option<crate::budget::BudgetHandle>,
}

/// `Agent` 的全部共享可变状态与同步原语。
///
/// 按字段分锁而不是一把大锁：读取状态快照、队列操作、listener 注册与配置读写互不阻塞。
/// 约束：
/// - `active_run` 是“同时至多一个 run”的闸门；`claim_run` 在同一临界区内检查并占位。
/// - `listeners` 配合 `next_listener_seq` 实现 TS `Set` 的插入序与引用去重（同一个
///   `Arc` 只保留一个条目，退订后重新订阅会分配新序号、排到末尾）。
/// - `config` 在每次 run 启动时被 `create_loop_config` 整体克隆；run 中修改只影响下一次
///   run（`prepareNextTurn*` 钩子例外，其实现每次调用都重新读取配置）。
/// - 所有 `MutexGuard` 都在同步块内释放，不跨 `.await`。
struct AgentInner {
    state: Mutex<AgentState>,
    listeners: Mutex<Vec<ListenerEntry>>,
    next_listener_seq: AtomicU64,
    next_run_id: AtomicU64,
    steering_queue: Mutex<PendingMessageQueue>,
    follow_up_queue: Mutex<PendingMessageQueue>,
    active_run: Mutex<Option<ActiveRun>>,
    config: Mutex<AgentConfig>,
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// `Agent`：低层 agent loop 的有状态包装。
///
/// `Agent` 拥有当前 transcript、发出生命周期事件、执行工具，并提供 steering / follow-up
/// 队列。克隆 `Agent` 只复制句柄（内部状态共享）。
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Clone for Agent {
    fn clone(&self) -> Self {
        Agent {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Agent {
    /// `new Agent(options)`。
    ///
    /// `streamFn` 的求值是**急切**的（TS 构造函数里 `runtimeOptions.streamFn ??
    /// getDefaultStreamFn()`）：未显式传入且未配置默认流函数时立即返回
    /// [`DefaultStreamFnError`]（逐字消息见 [`crate::stream_fn`]）。
    ///
    /// 构造只做同步工作：生成初始状态、按选项创建队列与配置、初始化锁和计数器；不启动
    /// 任务、不发事件。未给出的选项落到以下缺省：`convertToLlm` = `default_convert_to_llm`，
    /// `steeringMode`/`followUpMode` = `"one-at-a-time"`，`transport` = `"auto"`，
    /// `toolExecution` = `"parallel"`，`model` = `DEFAULT_MODEL`，`thinkingLevel` = `"off"`。
    pub fn new(options: AgentOptions) -> Result<Self, AgentError> {
        let stream_function = match options.stream_fn.clone() {
            Some(stream_function) => stream_function,
            None => require_default_stream_fn()?,
        };
        let convert_to_llm = options
            .convert_to_llm
            .clone()
            .unwrap_or_else(|| Arc::new(default_convert_to_llm));
        let state = create_mutable_agent_state(options.initial_state.clone());
        let steering_queue =
            PendingMessageQueue::new(options.steering_mode.unwrap_or(QueueMode::OneAtATime));
        let follow_up_queue =
            PendingMessageQueue::new(options.follow_up_mode.unwrap_or(QueueMode::OneAtATime));
        Ok(Agent {
            inner: Arc::new(AgentInner {
                state: Mutex::new(state),
                listeners: Mutex::new(Vec::new()),
                next_listener_seq: AtomicU64::new(0),
                next_run_id: AtomicU64::new(0),
                steering_queue: Mutex::new(steering_queue),
                follow_up_queue: Mutex::new(follow_up_queue),
                active_run: Mutex::new(None),
                config: Mutex::new(AgentConfig {
                    convert_to_llm,
                    transform_context: options.transform_context,
                    stream_function,
                    get_api_key: options.get_api_key,
                    on_payload: options.on_payload,
                    on_payload_async: options.on_payload_async,
                    on_response: options.on_response,
                    on_provider_stream_event: options.on_provider_stream_event,
                    before_tool_call: options.before_tool_call,
                    after_tool_call: options.after_tool_call,
                    finish_turn: options.finish_turn,
                    prepare_request: options.prepare_request,
                    prepare_next_turn: options.prepare_next_turn,
                    prepare_next_turn_with_context: options.prepare_next_turn_with_context,
                    session_id: options.session_id,
                    thinking_budgets: options.thinking_budgets,
                    transport: options.transport.unwrap_or(Transport::Auto),
                    max_retry_delay_ms: options.max_retry_delay_ms,
                    tool_execution: options
                        .tool_execution
                        .unwrap_or(ToolExecutionMode::Parallel),
                    identity: options.identity,
                    loop_guard: options.loop_guard,
                    budget: options.budget,
                }),
            }),
        })
    }

    /// 仅测试/内部用：当前 `Weak` 句柄（不延长 `Agent` 生命周期）。
    pub fn downgrade(&self) -> WeakAgent {
        WeakAgent {
            inner: Arc::downgrade(&self.inner),
        }
    }

    // -- 订阅 ---------------------------------------------------------------

    /// `subscribe(listener) → unsubscribe`。
    ///
    /// TS 的 `listeners` 是 `Set`：同一个 listener（Rust 里是同一个 `Arc` 指针，对应 JS 的
    /// 函数引用身份）重复订阅只保留一个条目；listener 按订阅顺序**串行 await**；派发使用 TS
    /// 的活 Set 迭代语义（见 `processEvents`）。订阅本身不发事件。
    ///
    /// 返回的 [`Subscription`] **不会**在 drop 时退订——TS 丢弃 `subscribe` 的返回值时
    /// listener 仍然有效；退订必须显式调用 [`Subscription::unsubscribe`]。
    pub fn subscribe(&self, listener: AgentListener) -> Subscription {
        let mut listeners = lock(&self.inner.listeners);
        if !listeners
            .iter()
            .any(|entry| Arc::ptr_eq(&entry.listener, &listener))
        {
            let seq = self.inner.next_listener_seq.fetch_add(1, Ordering::SeqCst);
            listeners.push(ListenerEntry {
                seq,
                listener: Arc::clone(&listener),
            });
        }
        drop(listeners);
        Subscription {
            inner: Arc::downgrade(&self.inner),
            listener,
        }
    }

    // -- 状态（TS 的 getter/setter 复制语义） --------------------------------

    /// `get state()` 的快照：`systemPrompt` 由 transcript 现算，数组字段是副本。
    ///
    /// `system_prompt` 每次都从 `messages` 重放得出（线性扫描 system 消息），而不是直接
    /// 返回内部缓存字段；因此即使事件追加消息时没有重算该字段，快照仍与 transcript 一致。
    /// 其余字段（`model`/`thinkingLevel`/`tools`/`isStreaming`/...）都是克隆。
    pub fn state(&self) -> AgentState {
        let state = lock(&self.inner.state);
        let mut snapshot = state.clone();
        snapshot.system_prompt = current_system_prompt(&state.messages);
        snapshot
    }

    /// `state.systemPrompt`（只读，由 transcript 的 system 消息回放得到）。
    pub fn system_prompt(&self) -> String {
        let state = lock(&self.inner.state);
        current_system_prompt(&state.messages)
    }

    /// `state.model`（缺省时为 `DEFAULT_MODEL`）。
    pub fn model(&self) -> Model {
        let state = lock(&self.inner.state);
        state.model.clone().unwrap_or_else(default_model)
    }

    /// `state.model = model`。
    pub fn set_model(&self, model: Model) {
        lock(&self.inner.state).model = Some(model);
    }

    /// `state.thinkingLevel`。
    pub fn thinking_level(&self) -> ThinkingLevel {
        lock(&self.inner.state).thinking_level
    }

    /// `state.thinkingLevel = level`。
    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        lock(&self.inner.state).thinking_level = level;
    }

    /// `state.tools`（返回副本；TS getter 返回活数组）。
    pub fn tools(&self) -> Vec<AgentTool> {
        lock(&self.inner.state).tools.clone()
    }

    /// `state.tools = tools`（复制顶层数组，与 TS setter 一致）。
    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        lock(&self.inner.state).tools = tools;
    }

    /// `state.messages`（返回副本；TS getter 返回活数组）。
    pub fn messages(&self) -> Vec<AgentMessage> {
        lock(&self.inner.state).messages.clone()
    }

    /// 在 `state` 锁内运行一次只读检查，不克隆 transcript。
    ///
    /// 供“只需要看当前 model/messages”的回调使用（例如 cache-warmer 的 `isCurrent`，
    /// 每次状态查询都 `state()` 会复制整条 transcript）。回调里不能再调用 Agent 的其他
    /// 方法（`std::sync::Mutex` 不可重入）。
    pub fn with_state<R>(&self, f: impl FnOnce(&AgentState) -> R) -> R {
        let state = lock(&self.inner.state);
        f(&state)
    }

    /// 当前 transcript 的消息条数（O(1)，不克隆）。
    pub fn message_count(&self) -> usize {
        lock(&self.inner.state).messages.len()
    }

    /// `state.messages = messages`（复制顶层数组，与 TS setter 一致）。
    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        let mut state = lock(&self.inner.state);
        state.messages = messages;
        state.system_prompt = current_system_prompt(&state.messages);
    }

    /// `state.messages.push(message)`（TS 对活数组的直接追加）。
    pub fn push_message(&self, message: AgentMessage) {
        let mut state = lock(&self.inner.state);
        state.messages.push(message);
        state.system_prompt = current_system_prompt(&state.messages);
    }

    /// `state.isStreaming`。
    ///
    /// 从 `claim_run` 置为 `true` 起，到 `finish_run` 置回 `false` 为止；`reset` 也会清零。
    /// 它表示 run 生命周期，而不是“某个流正在产出 chunk”；并发观察者在 `claim_run` /
    /// `finish_run` 分两段取锁的极短窗口里，可能看到它与 `active_run` 短暂不一致。
    pub fn is_streaming(&self) -> bool {
        lock(&self.inner.state).is_streaming
    }

    /// `state.streamingMessage`。
    ///
    /// 由 `message_start` / `message_update` 事件写入当前 partial 消息；`claim_run`、
    /// `message_end`、`agent_end`、`finish_run` 与 `reset` 都会清空。`None` 表示当前没有
    /// 正在流式接收的消息。
    pub fn streaming_message(&self) -> Option<AgentMessage> {
        lock(&self.inner.state).streaming_message.clone()
    }

    /// `state.pendingToolCalls`（`ReadonlySet<string>` 的副本）。
    ///
    /// TS 每次事件都新建 `Set` 实例以便消费者观察引用变化；Rust 的 `BTreeSet` 只有值语义，
    /// 这里返回克隆（消费者按值比较）。
    pub fn pending_tool_calls(&self) -> BTreeSet<String> {
        lock(&self.inner.state).pending_tool_calls.clone()
    }

    /// `state.errorMessage`。
    ///
    /// 由 `turn_end` 事件从 assistant 消息的 `errorMessage` 写入（空字符串按假值忽略），
    /// 保留到下一次 run 的 `claim_run` 或 `reset`；成功结束的 run 不会主动清除它。
    pub fn error_message(&self) -> Option<String> {
        lock(&self.inner.state).error_message.clone()
    }

    // -- 模式 ---------------------------------------------------------------

    /// `steeringMode`。
    pub fn steering_mode(&self) -> QueueMode {
        lock(&self.inner.steering_queue).mode
    }

    /// `steeringMode = mode`（只改队列模式）。
    pub fn set_steering_mode(&self, mode: QueueMode) {
        lock(&self.inner.steering_queue).mode = mode;
    }

    /// `followUpMode`。
    pub fn follow_up_mode(&self) -> QueueMode {
        lock(&self.inner.follow_up_queue).mode
    }

    /// `followUpMode = mode`（只改队列模式）。
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        lock(&self.inner.follow_up_queue).mode = mode;
    }

    // -- 配置字段（TS 的公开可写属性） ---------------------------------------

    // 每次 get/set 单独加锁。run 启动时 `create_loop_config` 会整体克隆这些字段，因此 run
    // 进行中调用 `set_*` 只影响下一次 run；`prepareNextTurn*` 钩子是例外，其实现每次调用都
    // 重新读取配置（见 `create_loop_config`）。

    /// `convertToLlm`。
    pub fn convert_to_llm(&self) -> ConvertToLlmFn {
        lock(&self.inner.config).convert_to_llm.clone()
    }

    /// `convertToLlm = f`。
    pub fn set_convert_to_llm(&self, convert_to_llm: ConvertToLlmFn) {
        lock(&self.inner.config).convert_to_llm = convert_to_llm;
    }

    /// `transformContext`。
    pub fn transform_context(&self) -> Option<TransformContextFn> {
        lock(&self.inner.config).transform_context.clone()
    }

    /// `transformContext = f`。
    pub fn set_transform_context(&self, transform_context: Option<TransformContextFn>) {
        lock(&self.inner.config).transform_context = transform_context;
    }

    /// `streamFunction`。
    pub fn stream_function(&self) -> StreamFn {
        lock(&self.inner.config).stream_function.clone()
    }

    /// `streamFunction = f`。
    pub fn set_stream_function(&self, stream_function: StreamFn) {
        lock(&self.inner.config).stream_function = stream_function;
    }

    /// `getApiKey`。
    pub fn get_api_key(&self) -> Option<GetApiKeyFn> {
        lock(&self.inner.config).get_api_key.clone()
    }

    /// `getApiKey = f`。
    pub fn set_get_api_key(&self, get_api_key: Option<GetApiKeyFn>) {
        lock(&self.inner.config).get_api_key = get_api_key;
    }

    /// `onPayload`。
    pub fn on_payload(&self) -> Option<OnPayloadFn> {
        lock(&self.inner.config).on_payload.clone()
    }

    /// `onPayload = f`。
    pub fn set_on_payload(&self, on_payload: Option<OnPayloadFn>) {
        lock(&self.inner.config).on_payload = on_payload;
    }

    /// `onResponse`。
    pub fn on_response(&self) -> Option<OnResponseFn> {
        lock(&self.inner.config).on_response.clone()
    }

    /// `onResponse = f`。
    pub fn set_on_response(&self, on_response: Option<OnResponseFn>) {
        lock(&self.inner.config).on_response = on_response;
    }

    /// `onProviderStreamEvent`。
    pub fn on_provider_stream_event(&self) -> Option<OnProviderStreamEventFn> {
        lock(&self.inner.config).on_provider_stream_event.clone()
    }

    /// `onProviderStreamEvent = f`。
    pub fn set_on_provider_stream_event(
        &self,
        on_provider_stream_event: Option<OnProviderStreamEventFn>,
    ) {
        lock(&self.inner.config).on_provider_stream_event = on_provider_stream_event;
    }

    /// `beforeToolCall`。
    pub fn before_tool_call(&self) -> Option<BeforeToolCallFn> {
        lock(&self.inner.config).before_tool_call.clone()
    }

    /// `beforeToolCall = f`。
    pub fn set_before_tool_call(&self, before_tool_call: Option<BeforeToolCallFn>) {
        lock(&self.inner.config).before_tool_call = before_tool_call;
    }

    /// `afterToolCall`。
    pub fn after_tool_call(&self) -> Option<AfterToolCallFn> {
        lock(&self.inner.config).after_tool_call.clone()
    }

    /// `afterToolCall = f`。
    pub fn set_after_tool_call(&self, after_tool_call: Option<AfterToolCallFn>) {
        lock(&self.inner.config).after_tool_call = after_tool_call;
    }

    /// `finishTurn`。
    pub fn finish_turn(&self) -> Option<FinishTurnFn> {
        lock(&self.inner.config).finish_turn.clone()
    }

    /// `finishTurn = f`。
    pub fn set_finish_turn(&self, finish_turn: Option<FinishTurnFn>) {
        lock(&self.inner.config).finish_turn = finish_turn;
    }

    /// `prepareRequest`。
    pub fn prepare_request(&self) -> Option<PrepareRequestFn> {
        lock(&self.inner.config).prepare_request.clone()
    }

    /// `prepareRequest = f`。
    pub fn set_prepare_request(&self, prepare_request: Option<PrepareRequestFn>) {
        lock(&self.inner.config).prepare_request = prepare_request;
    }

    /// `prepareNextTurn`（旧形态）。
    pub fn prepare_next_turn(&self) -> Option<AgentPrepareNextTurnFn> {
        lock(&self.inner.config).prepare_next_turn.clone()
    }

    /// `prepareNextTurn = f`。
    pub fn set_prepare_next_turn(&self, prepare_next_turn: Option<AgentPrepareNextTurnFn>) {
        lock(&self.inner.config).prepare_next_turn = prepare_next_turn;
    }

    /// `prepareNextTurnWithContext`。
    pub fn prepare_next_turn_with_context(&self) -> Option<AgentPrepareNextTurnWithContextFn> {
        lock(&self.inner.config)
            .prepare_next_turn_with_context
            .clone()
    }

    /// `prepareNextTurnWithContext = f`。
    pub fn set_prepare_next_turn_with_context(
        &self,
        prepare_next_turn_with_context: Option<AgentPrepareNextTurnWithContextFn>,
    ) {
        lock(&self.inner.config).prepare_next_turn_with_context = prepare_next_turn_with_context;
    }

    /// `sessionId`。
    pub fn session_id(&self) -> Option<String> {
        lock(&self.inner.config).session_id.clone()
    }

    /// `sessionId = id`。
    pub fn set_session_id(&self, session_id: Option<String>) {
        lock(&self.inner.config).session_id = session_id;
    }

    /// `thinkingBudgets`。
    pub fn thinking_budgets(&self) -> Option<ThinkingBudgets> {
        lock(&self.inner.config).thinking_budgets.clone()
    }

    /// `thinkingBudgets = budgets`。
    pub fn set_thinking_budgets(&self, thinking_budgets: Option<ThinkingBudgets>) {
        lock(&self.inner.config).thinking_budgets = thinking_budgets;
    }

    /// 当前使用的 loop guard 句柄（`None` 表示未启用）。
    ///
    /// 返回的是共享 `Arc`，调用方可用它读取 `snapshot()` 或临时 `clear_history()`。
    pub fn loop_guard(&self) -> Option<crate::loop_guard::LoopGuardHandle> {
        lock(&self.inner.config).loop_guard.clone()
    }

    /// 替换 loop guard 句柄；只影响之后开始的 run。
    pub fn set_loop_guard(&self, loop_guard: Option<crate::loop_guard::LoopGuardHandle>) {
        lock(&self.inner.config).loop_guard = loop_guard;
    }

    /// 当前使用的预算句柄（`None` 表示未启用）。
    pub fn budget(&self) -> Option<crate::budget::BudgetHandle> {
        lock(&self.inner.config).budget.clone()
    }

    /// 替换预算句柄；只影响之后开始的 run。
    pub fn set_budget(&self, budget: Option<crate::budget::BudgetHandle>) {
        lock(&self.inner.config).budget = budget;
    }

    /// `transport`。
    pub fn transport(&self) -> Transport {
        lock(&self.inner.config).transport
    }

    /// `transport = transport`。
    pub fn set_transport(&self, transport: Transport) {
        lock(&self.inner.config).transport = transport;
    }

    /// `maxRetryDelayMs`。
    pub fn max_retry_delay_ms(&self) -> Option<u64> {
        lock(&self.inner.config).max_retry_delay_ms
    }

    /// `maxRetryDelayMs = ms`。
    pub fn set_max_retry_delay_ms(&self, max_retry_delay_ms: Option<u64>) {
        lock(&self.inner.config).max_retry_delay_ms = max_retry_delay_ms;
    }

    /// `toolExecution`。
    pub fn tool_execution(&self) -> ToolExecutionMode {
        lock(&self.inner.config).tool_execution
    }

    /// `toolExecution = mode`。
    pub fn set_tool_execution(&self, tool_execution: ToolExecutionMode) {
        lock(&self.inner.config).tool_execution = tool_execution;
    }

    /// `identity`。
    pub fn identity(&self) -> Option<ClientIdentity> {
        lock(&self.inner.config).identity.clone()
    }

    /// `identity = identity`。
    pub fn set_identity(&self, identity: Option<ClientIdentity>) {
        lock(&self.inner.config).identity = identity;
    }

    // -- 队列 ---------------------------------------------------------------

    /// `steer(message)`：在当前 assistant turn 结束后注入。
    pub fn steer(&self, message: AgentMessage) {
        lock(&self.inner.steering_queue).enqueue(message);
    }

    /// `followUp(message)`：只在 agent 本应停止后运行。
    pub fn follow_up(&self, message: AgentMessage) {
        lock(&self.inner.follow_up_queue).enqueue(message);
    }

    /// `clearSteeringQueue()`。
    pub fn clear_steering_queue(&self) {
        lock(&self.inner.steering_queue).clear();
    }

    /// `clearFollowUpQueue()`。
    pub fn clear_follow_up_queue(&self) {
        lock(&self.inner.follow_up_queue).clear();
    }

    /// `clearAllQueues()`。
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    /// `hasQueuedMessages()`。
    ///
    /// 按 `||` 短路、至多两次独立加锁读取：steering 队列非空时只看它，根本不会读取
    /// follow-up。两次读取不是原子快照，并发 `steer` / `follow_up` / `drain` 时，返回值
    /// 只反映读取瞬间的状态，可能落后于实际队列。
    pub fn has_queued_messages(&self) -> bool {
        lock(&self.inner.steering_queue).has_items()
            || lock(&self.inner.follow_up_queue).has_items()
    }

    /// `peekQueuedMessages()`：steering 非空时返回它的 `peek()`，否则返回 follow-up 的；不消费。
    ///
    /// 返回的是消息克隆；steering 非空时不会继续查看 follow-up。返回条数由各队列的
    /// `mode` 决定（`"one-at-a-time"` 只有队首一条）。
    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let steering = lock(&self.inner.steering_queue).peek();
        if !steering.is_empty() {
            return steering;
        }
        lock(&self.inner.follow_up_queue).peek()
    }

    // -- run 控制 -----------------------------------------------------------

    // `active_run` 从 `claim_run` 登记到 `finish_run`（或 RunGuard 兜底）清除；`signal()` /
    // `abort()` / `wait_for_idle()` 都以它作为“是否有 run 在进行”的唯一判据。

    /// `signal`：当前 run 的取消令牌（TS 的 `activeRun?.abortController.signal`）。
    ///
    /// 没有进行中的 run 时返回 `None`。有 run 时返回的是同一个 `CancellationToken` 的克隆，
    /// 取消状态在克隆之间共享——任何持有者调用 `cancel()`，run 内的流函数与工具都会观察到。
    pub fn signal(&self) -> Option<CancellationToken> {
        lock(&self.inner.active_run)
            .as_ref()
            .map(|run| run.signal.clone())
    }

    /// `abort()`：中止当前 run（没有运行时是 no-op）。
    ///
    /// 这里的“没有运行时”指没有进行中的 run；每次 run 都在 `claim_run` 里新建取消令牌，
    /// 所以 `abort()` 不会给之后的 run 留下取消状态，重复调用也无副作用。
    pub fn abort(&self) {
        if let Some(run) = lock(&self.inner.active_run).as_ref() {
            run.signal.cancel();
        }
    }

    /// `waitForIdle()`：当前 run 与所有 awaited listener 都结束后 resolve。
    ///
    /// 没有 run 时立即完成；等待者在 `finishRun()`（`agent_end` 的 listener 结算之后）被唤醒。
    ///
    /// 实现上在 `active_run` 锁内完成 `done.subscribe()`，因此不会漏掉通知：`finish_run`
    /// 必须先取得同一把锁，才能移除 `active_run` 并发送结束信号。多个等待者各自订阅同一个
    /// `watch` 通道，都会被唤醒；发送端先于接收端 drop 时也按“已结束”处理。
    pub async fn wait_for_idle(&self) {
        let receiver = lock(&self.inner.active_run)
            .as_ref()
            .map(|run| run.done.subscribe());
        let Some(mut receiver) = receiver else {
            return;
        };
        // 发送端在 `finishRun` 里写 `true`；发送端先于接收端 drop 时同样视为结束。
        let _ = receiver.wait_for(|done| *done).await;
    }

    /// `reset()`：清空对话与两个队列，保留回放出来的 prompt/tool 基线。
    ///
    /// 保留：`model`、`thinking_level`、`tools`（以及配置字段）。
    /// 清空：transcript（重建为 `current_system_message` 回放出的 system 基线，可能为空）、
    /// `is_streaming`、`streaming_message`、`pending_tool_calls`、`error_message`，以及
    /// steering / follow-up 两个队列（丢弃其中未消费的消息）。
    /// 有进行中的 run 时返回 [`AgentError::ResetWhileProcessing`]，且在返回前不会改动状态。
    ///
    /// 检查与清空在同一个 `active_run` 临界区内完成（`claim_run` 也在这把锁内占位），
    /// 因此检查通过后不会被并发启动的 run 打断。锁顺序固定为
    /// `active_run` → `state` → 队列，与 `claim_run` / `finish_run` 一致。
    pub fn reset(&self) -> Result<(), AgentError> {
        let active_run = lock(&self.inner.active_run);
        if active_run.is_some() {
            return Err(AgentError::ResetWhileProcessing);
        }

        {
            let mut state = lock(&self.inner.state);
            let baseline = current_system_message(&state.messages);
            state.messages = match baseline {
                Some(baseline) => vec![AgentMessage::Standard(Message::System(baseline))],
                None => Vec::new(),
            };
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
            state.error_message = None;
            state.system_prompt = current_system_prompt(&state.messages);
        }
        self.clear_follow_up_queue();
        self.clear_steering_queue();
        drop(active_run);
        Ok(())
    }

    // -- prompt / continue --------------------------------------------------

    /// `prompt(message)`：从单条消息开始新一轮（数组不复制、不加时间戳）。
    ///
    /// 与 TS 的 promise 语义一致：**调用即开始**运行（同步完成前置检查、登记 `activeRun`、
    /// 置 `isStreaming`，并把 run 交给 tokio 任务）；返回的 future 只负责等待 run 结束，
    /// 丢弃它不会中断 run。
    ///
    /// 入口处不复制、不加时间戳（与 TS 的 `normalizePromptInput` 一致）；消息会经过低层
    /// 循环的事件（`message_start`/`message_end`）写回 `state.messages`。已有 run 时返回的
    /// future 立即以 [`AgentError::PromptWhileProcessing`] 就绪，不产生任何事件。
    pub fn prompt(&self, message: AgentMessage) -> BoxFuture<'static, Result<(), AgentError>> {
        if lock(&self.inner.active_run).is_some() {
            return ready_error(AgentError::PromptWhileProcessing);
        }
        self.run_prompt_messages(vec![message], false, AgentError::PromptWhileProcessing)
    }

    /// `prompt(messages[])`：整批消息原样开始新一轮（不复制、不加时间戳）。
    ///
    /// 冲突时与 [`Agent::prompt`] 一样返回 [`AgentError::PromptWhileProcessing`]；成功时
    /// 这批消息按给定顺序成为本轮的初始输入。
    pub fn prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
    ) -> BoxFuture<'static, Result<(), AgentError>> {
        if lock(&self.inner.active_run).is_some() {
            return ready_error(AgentError::PromptWhileProcessing);
        }
        self.run_prompt_messages(messages, false, AgentError::PromptWhileProcessing)
    }

    /// `prompt(input: string, images?)`：构造 `[{role:"user", content:[text, ...images],
    /// timestamp: Date.now()}]`。
    pub fn prompt_text(
        &self,
        text: impl Into<String>,
    ) -> BoxFuture<'static, Result<(), AgentError>> {
        self.prompt_text_with_images(text, Vec::new())
    }

    /// `prompt(input: string, images)`：同上并追加图片块。
    ///
    /// `content` 始终是块数组：文本块在最前，图片按传入顺序追加；`timestamp` 取调用时刻的
    /// Unix 毫秒（`Date.now()`）。
    pub fn prompt_text_with_images(
        &self,
        text: impl Into<String>,
        images: Vec<ImageContent>,
    ) -> BoxFuture<'static, Result<(), AgentError>> {
        if lock(&self.inner.active_run).is_some() {
            return ready_error(AgentError::PromptWhileProcessing);
        }
        let message = normalize_prompt_input(text.into(), images);
        self.run_prompt_messages(vec![message], false, AgentError::PromptWhileProcessing)
    }

    /// `continue()`：从当前 transcript 继续；最后一条必须是 user / tool-result。
    ///
    /// 回退阶梯（§8.3）：尾部不是 assistant → 直接用既有 context；尾部是 assistant →
    /// steering 队列（`skipInitialSteeringPoll: true`）→ follow-up 队列 → 抛错。
    ///
    /// 细节与副作用：
    /// - 空 transcript 或全部是 system 消息时返回 [`AgentError::NoMessagesToContinue`]，
    ///   这两种检查在任何出队之前完成，队列内容原样保留；
    /// - 尾部是 assistant 时先尝试 steering：取出的消息作为本轮输入，并跳过启动时的那次
    ///   steering 轮询，避免同一批消息被再取一次（之后新入队的 steering 仍会在后续轮次被
    ///   轮询取走）；steering 为空才尝试 follow-up；
    /// - 尾部是 user / tool-result 时用既有 transcript 继续，启动轮询照常消费 steering；
    /// - 队列的 `mode` 决定一次取出一条还是全部（见 `PendingMessageQueue`）。
    pub fn continue_(&self) -> BoxFuture<'static, Result<(), AgentError>> {
        if lock(&self.inner.active_run).is_some() {
            return ready_error(AgentError::ContinueWhileProcessing);
        }

        let (last_role, all_system) = {
            let state = lock(&self.inner.state);
            (
                state
                    .messages
                    .last()
                    .map(|message| message.role().to_string()),
                state
                    .messages
                    .iter()
                    .all(|message| message.role() == "system"),
            )
        };
        let Some(last_role) = last_role else {
            return ready_error(AgentError::NoMessagesToContinue);
        };
        if all_system {
            return ready_error(AgentError::NoMessagesToContinue);
        }

        if last_role == "assistant" {
            // 只在占住 run 槽位之后才真的出队（见 `PromptMessages`）。这里只探测是否有消息：
            // 先 `drain()` 再 `begin_run` 的话，并发 claim 失败时这批用户消息会被永久丢弃。
            if lock(&self.inner.steering_queue).has_items() {
                return self.run_prompt_messages_with(
                    PromptMessages::Steering(Arc::clone(&self.inner)),
                    true,
                    AgentError::ContinueWhileProcessing,
                );
            }

            if lock(&self.inner.follow_up_queue).has_items() {
                return self.run_prompt_messages_with(
                    PromptMessages::FollowUp(Arc::clone(&self.inner)),
                    false,
                    AgentError::ContinueWhileProcessing,
                );
            }

            return ready_error(AgentError::CannotContinueFromAssistant);
        }

        self.run_continuation()
    }

    // -- 内部实现 -----------------------------------------------------------

    /// `runPromptMessages(messages, { skipInitialSteeringPoll })`。
    ///
    /// `conflict` 是 `begin_run` 竞争失败时返回的错误（各入口返回各自对应的错误文本）。
    fn run_prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
        conflict: AgentError,
    ) -> BoxFuture<'static, Result<(), AgentError>> {
        self.run_prompt_messages_with(
            PromptMessages::Fixed(messages),
            skip_initial_steering_poll,
            conflict,
        )
    }

    /// 同 [`Agent::run_prompt_messages`]，但输入可以延迟到 executor 启动时再取。
    ///
    /// executor 在 `claim_run` 成功之后才被调用；把出队放在 executor 里，`continue_`
    /// 因并发 run 而 claim 失败时就不会丢掉队列里的消息。
    fn run_prompt_messages_with(
        &self,
        messages: PromptMessages,
        skip_initial_steering_poll: bool,
        conflict: AgentError,
    ) -> BoxFuture<'static, Result<(), AgentError>> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config(skip_initial_steering_poll);
        let stream_function = self.stream_function();
        let error_slot: Arc<Mutex<Option<AgentError>>> = Arc::new(Mutex::new(None));
        let sink = self.event_sink(error_slot.clone());
        let executor: RunExecutor = Box::new(move |signal| {
            Box::pin(async move {
                let messages = messages.take();
                let outcome = run_agent_loop(
                    messages,
                    context,
                    config,
                    sink,
                    Some(signal),
                    Some(stream_function),
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
                finish_executor(outcome, &error_slot)
            })
        });
        self.begin_run(executor, conflict)
    }

    /// `runContinuation()`。
    fn run_continuation(&self) -> BoxFuture<'static, Result<(), AgentError>> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config(false);
        let stream_function = self.stream_function();
        let error_slot: Arc<Mutex<Option<AgentError>>> = Arc::new(Mutex::new(None));
        let sink = self.event_sink(error_slot.clone());
        let executor: RunExecutor = Box::new(move |signal| {
            Box::pin(async move {
                let outcome = run_agent_loop_continue(
                    context,
                    config,
                    sink,
                    Some(signal),
                    Some(stream_function),
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
                finish_executor(outcome, &error_slot)
            })
        });
        self.begin_run(executor, AgentError::ContinueWhileProcessing)
    }

    /// `createContextSnapshot()`：`{ messages: state.messages.slice(), tools: state.tools.slice() }`。
    ///
    /// 在 `begin_run` 之前调用。本次 run 使用独立的副本，run 期间调用 `Agent::set_messages` /
    /// `Agent::set_tools` 不会改变正在执行的循环已经拿到的输入；循环自己的 `message_end`
    /// 仍会经事件写回 `state.messages`。
    fn create_context_snapshot(&self) -> AgentContext {
        let state = lock(&self.inner.state);
        AgentContext {
            messages: state.messages.clone(),
            tools: Some(state.tools.clone()),
        }
    }

    /// 事件 sink：`(event) => this.processEvents(event)`。
    ///
    /// 首个事件的派发完成后让出一次（[`YieldOnce`]）：TS 的 `await emit(...)` 即使对已完成的
    /// promise 也总是把后续代码排进微任务，因此 `prompt()`/`continue()` 的同步前缀只到
    /// `agent_start` 的派发为止。Rust 侧由 [`Agent::begin_run`] 在调用线程上同步轮询到同一个
    /// 挂起点（见 [`Agent::begin_run`]）。若首个事件的派发本身就挂起（某个 listener 尚未完成），
    /// 轮询已停在 listener 上，无需再让出。
    ///
    /// `error_slot` 只保留第一个 `processEvents` 错误（后续错误被丢弃），并且**不会**中断
    /// 循环：sink 始终返回 `()`，错误留给 `finish_executor` 在循环返回后统一处理。
    fn event_sink(&self, error_slot: Arc<Mutex<Option<AgentError>>>) -> AgentEventSink {
        let agent = self.clone();
        let first_event = Arc::new(AtomicBool::new(true));
        Arc::new(move |event| {
            let agent = agent.clone();
            let error_slot = error_slot.clone();
            let first_event = Arc::clone(&first_event);
            Box::pin(async move {
                let is_first = first_event.swap(false, Ordering::SeqCst);
                let dispatch = agent.process_events(event);
                let mut dispatch = std::pin::pin!(dispatch);
                // TS 的 `await emit(event)`：先同步执行派发（到第一个内部 await），再挂起一次。
                let (result, completed_synchronously) = match poll_once(dispatch.as_mut()) {
                    Poll::Ready(result) => (result, true),
                    Poll::Pending => (dispatch.await, false),
                };
                if let Err(error) = result {
                    let mut slot = lock(&error_slot);
                    if slot.is_none() {
                        *slot = Some(error);
                    }
                }
                if is_first && completed_synchronously {
                    YieldOnce::new().await;
                }
            })
        })
    }

    /// `createLoopConfig(options)`：逐字段转发到 [`AgentLoopConfig`]。
    ///
    /// 在 run 启动时调用一次：配置与模型在这里被快照，因此 run 中的 `set_*` 不影响本轮
    /// （`prepareNextTurn*` 的实现另行重读配置）。`getSteeringMessages` /
    /// `getFollowUpMessages` 闭包持有 `AgentInner`，每次被循环调用时都读取**当前**队列，
    /// 所以 run 中 `steer` / `follow_up` 入队的消息仍会被后续轮次取到。
    fn create_loop_config(&self, skip_initial_steering_poll: bool) -> AgentLoopConfig {
        let config = lock(&self.inner.config).clone();
        let state = lock(&self.inner.state);
        let reasoning = to_ai_thinking_level(state.thinking_level);
        let model = state.model.clone().unwrap_or_else(default_model);
        drop(state);
        // 之后不再持有 state 锁：队列闭包与 prepareNextTurn 闭包都在被调用时才各自加锁。

        // TS: `{...config, apiKey, identity, signal}` 的“config 侧”字段。流选项分两层：
        // SimpleStreamOptions 承载 reasoning/thinkingBudgets，嵌套的 base 承载身份、会话、
        // 传输、重试上限与三类 provider 回调。
        let mut options = SimpleStreamOptions {
            reasoning,
            thinking_budgets: config.thinking_budgets.clone(),
            ..Default::default()
        };
        options.base = StreamOptions {
            identity: config.identity.clone(),
            session_id: config.session_id.clone(),
            transport: Some(config.transport),
            max_retry_delay_ms: config.max_retry_delay_ms,
            on_payload: config.on_payload.clone(),
            on_payload_async: config.on_payload_async.clone(),
            on_response: config.on_response.clone(),
            on_provider_stream_event: config.on_provider_stream_event.clone(),
            ..Default::default()
        };

        // `getSteeringMessages`：首个轮询在 `skipInitialSteeringPoll` 时返回空。
        let skip = Arc::new(AtomicBool::new(skip_initial_steering_poll));
        let steering_inner = Arc::clone(&self.inner);
        let get_steering_messages: GetMessagesFn = Arc::new(move || {
            let inner = Arc::clone(&steering_inner);
            let skip = Arc::clone(&skip);
            Box::pin(async move {
                if skip.swap(false, Ordering::SeqCst) {
                    return Vec::new();
                }
                lock(&inner.steering_queue).drain()
            })
        });
        let follow_up_inner = Arc::clone(&self.inner);
        let get_follow_up_messages: GetMessagesFn = Arc::new(move || {
            let inner = Arc::clone(&follow_up_inner);
            Box::pin(async move { lock(&inner.follow_up_queue).drain() })
        });

        // `prepareNextTurn`：是否提供闭包在 config 创建时决定（TS 的三元条件），但闭包内部
        // **每次调用**都重新读取 `this.prepareNextTurnWithContext` / `this.prepareNextTurn`
        // （优先 withContext），因此在 run 中替换/清除钩子会在**下一轮**生效。两者都收到
        // 当前 run 的 signal（TS 的 `this.signal`）。
        let prepare_next_turn = if config.prepare_next_turn_with_context.is_some()
            || config.prepare_next_turn.is_some()
        {
            let inner = Arc::clone(&self.inner);
            let prepare: PrepareNextTurnFn = Arc::new(move |context: PrepareNextTurnContext| {
                let inner = Arc::clone(&inner);
                Box::pin(async move {
                    let signal = lock(&inner.active_run)
                        .as_ref()
                        .map(|run| run.signal.clone());
                    let (with_context, legacy) = {
                        let config = lock(&inner.config);
                        (
                            config.prepare_next_turn_with_context.clone(),
                            config.prepare_next_turn.clone(),
                        )
                    };
                    match with_context {
                        Some(with_context) => with_context(context, signal).await,
                        None => match legacy {
                            Some(legacy) => legacy(signal).await,
                            None => None,
                        },
                    }
                })
            });
            Some(prepare)
        } else {
            None
        };

        AgentLoopConfig {
            options,
            model,
            convert_to_llm: config.convert_to_llm,
            transform_context: config.transform_context,
            get_api_key: config.get_api_key,
            finish_turn: config.finish_turn,
            prepare_request: config.prepare_request,
            prepare_next_turn,
            get_steering_messages: Some(get_steering_messages),
            get_follow_up_messages: Some(get_follow_up_messages),
            tool_execution: Some(config.tool_execution),
            before_tool_call: config.before_tool_call,
            after_tool_call: config.after_tool_call,
            loop_guard: config.loop_guard.clone(),
            budget: config.budget.clone(),
        }
    }

    /// `runWithLifecycle(executor)` 的同步前半段。
    ///
    /// 顺序固定（并发观察者不会先看到 `isStreaming` 而没看到已登记的 run）：先登记 `activeRun`，再置 `isStreaming = true` 并清空
    /// `streamingMessage`/`errorMessage`，然后执行 executor。
    ///
    /// TS 的 async 函数在调用后立即同步运行到第一个 `await`（`runAgentLoop` 里的
    /// `await emit({type:"agent_start"})`），因此 `prompt()`/`continue()` 返回 promise 时
    /// `activeRun` 已经存在、`isStreaming` 已经为 true、`agent_start` 的 listener 已经运行到
    /// 各自的第一个 `await`。这里在**调用时**（而不是 future 首次 poll 时）同步完成同样的
    /// 登记，并用一次无 waker 的轮询把 run 执行到同一个挂起点：事件 sink 在首个事件派发后
    /// 让出一次（[`YieldOnce`]），所以轮询恰好停在 `agent_start` 之后，随后才交给
    /// `tokio::spawn` 继续。返回的 future 只等待 run 结束；丢弃它不会中断 run（对应 TS 里
    /// 无人 `await` 的 promise 仍会跑完），`waitForIdle()` 也照常等到 `finishRun()`。
    ///
    /// `conflict` 是已存在 `activeRun` 时返回的错误：TS 在 `runWithLifecycle` 里的防御性
    /// 检查抛 `Agent is already processing.`，但正常路径上各入口的同步检查先抛各自逐字文本；
    /// Rust 的并发调用可能让两个入口同时通过前置检查，因此这里按入口返回对应文本。
    ///
    /// 不在 tokio 运行时上下文里调用时无法 spawn，登记与执行都退化为 future 首次 poll 时
    /// 进行（Rust async 的惰性）；该路径下丢弃 future 等价于从未调用。
    fn begin_run(
        &self,
        executor: RunExecutor,
        conflict: AgentError,
    ) -> BoxFuture<'static, Result<(), AgentError>> {
        // 分支一（有 tokio 运行时）：在调用线程上同步完成登记与同步前缀，再把剩余 future
        // spawn 出去。分支二（没有运行时）：登记与执行都推迟到 future 首次 poll。
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                // 先占住 active_run；失败即返回各入口对应的冲突错误。
                let (run_id, signal) = match self.claim_run(conflict) {
                    Ok(claim) => claim,
                    Err(error) => return ready_error(error),
                };
                let agent = self.clone();
                // `finally { finishRun() }` 的兜底：guard 随 run future 一起存活，既覆盖正常
                // 结束 / panic，也覆盖任务在首次被 poll 前就被运行时丢弃（此时 execute_run
                // 从未运行，函数内的 guard 也就从未创建）。
                let guard = RunGuard::new(agent.clone(), run_id);
                let mut run =
                    Box::pin(async move { agent.execute_run(signal, executor, guard).await });

                // TS 的同步前缀（见上面的文档）：停在首次 `await`（agent_start 派发之后）。
                // 防御性快速路径：若 run 在首次轮询内就完成则直接返回结果。正常路径的第一个
                // 事件是 `agent_start`，event_sink 在首次派发后让出一次（或停在尚未完成的
                // listener 上），不会在这次轮询内结束；但 `run_continuation` 的 executor 会
                // 先执行 `run_agent_loop_continue` 的前置校验，校验失败时循环在发出任何事件
                // 前就返回 Err，随后 `handle_run_failure` 直接调用 `process_events`（不经过
                // event_sink，因此没有 YieldOnce）；若所有 listener 都能同步完成，整个 run
                // 会在首次轮询内完成并走这条快速路径。
                if let Poll::Ready(result) = poll_once(run.as_mut()) {
                    return Box::pin(async move { result });
                }

                let handle = runtime.spawn(run);
                Box::pin(async move {
                    match handle.await {
                        Ok(result) => result,
                        // executor / 任务内部的 panic 原先会在调用方的 poll 中展开，保持该行为。
                        Err(error) if error.is_panic() => {
                            std::panic::resume_unwind(error.into_panic())
                        }
                        // 运行时关闭导致任务被丢弃（未执行或执行到一半）：任务 future 被 Drop，
                        // RunGuard 已执行 finishRun()。被丢弃的 run 没有产生错误，与 TS 里
                        // 进程退出时 promise 不再结算等价。
                        Err(_) => Ok(()),
                    }
                })
            }
            // 没有 tokio 运行时：无法 spawn，退化为 Rust async 的惰性执行——登记与 run 都
            // 推迟到 future 首次 poll（此时丢弃 future 不会留下残留状态）。
            Err(_) => {
                let agent = self.clone();
                Box::pin(async move {
                    let (run_id, signal) = agent.claim_run(conflict)?;
                    let guard = RunGuard::new(agent.clone(), run_id);
                    agent.execute_run(signal, executor, guard).await
                })
            }
        }
    }

    /// `runWithLifecycle` 的登记步骤：创建 `activeRun`（`promise`/`abortController` 等价物）
    /// 并置 `isStreaming = true`、清空 `streamingMessage`/`errorMessage`。
    ///
    /// 返回本次 run 的编号与取消令牌；已存在 run 时返回 `conflict`。
    ///
    /// 分两段取锁：先在 `active_run` 锁内检查并占位，再单独取 `state` 锁写流式标志，
    /// 不嵌套持锁。两段之间的极短窗口里，并发观察者可能看到 `active_run` 已存在而
    /// `is_streaming` 仍为 `false`（`signal()` 只读 `active_run`）。
    fn claim_run(&self, conflict: AgentError) -> Result<(u64, CancellationToken), AgentError> {
        // 第一段：检查 + 占位。run_id 与取消令牌在这里分配；watch 通道的初值是 false，
        // 等待者稍后由 wait_for_idle 订阅。
        let (run_id, signal) = {
            let mut active_run = lock(&self.inner.active_run);
            if active_run.is_some() {
                return Err(conflict);
            }
            let run_id = self.inner.next_run_id.fetch_add(1, Ordering::SeqCst);
            let signal = CancellationToken::new();
            let (done, _receiver) = tokio::sync::watch::channel(false);
            *active_run = Some(ActiveRun {
                id: run_id,
                signal: signal.clone(),
                done,
            });
            (run_id, signal)
        };
        // 第二段：单独取 state 锁。清空上一轮的流式/错误残留（error_message 在这里被清掉）。
        {
            let mut state = lock(&self.inner.state);
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }
        Ok((run_id, signal))
    }

    /// `runWithLifecycle(executor)` 的异步后半段（在独立任务中执行）。
    ///
    /// `try { await executor(signal) } catch (e) { await handleRunFailure(e, signal.aborted) }
    /// finally { finishRun() }` 的等价物：executor 失败时合成 assistant 失败消息并发出完整
    /// 生命周期事件；无论成功、失败还是 panic，`finishRun()` 都会执行（run future 被 Drop 时
    /// 由 [`RunGuard`] 兜底），因此即使调用方丢弃了返回的 future、或运行时在任务首次被 poll
    /// 前就关闭，Agent 也不会卡在 processing 状态。
    async fn execute_run(
        &self,
        signal: CancellationToken,
        executor: RunExecutor,
        mut guard: RunGuard,
    ) -> Result<(), AgentError> {
        // executor 的 Err 是低层循环的错误文本，也可能是 event sink 记录的 processEvents
        // 错误（循环成功时由 finish_executor 从槽里取出，见其文档）；这里把它合成为失败
        // 消息。收尾事件派发本身也可能返回 Err，但无论哪种结果，下面都会先完成 finish_run。
        let result = match executor(signal.clone()).await {
            Ok(()) => Ok(()),
            Err(message) => {
                let aborted = signal.is_cancelled();
                self.handle_run_failure(message, aborted).await
            }
        };
        // 手动清理后解除 guard，避免 Drop 时重复执行 finish_run。
        self.finish_run(guard.run_id);
        guard.armed = false;
        result
    }

    /// `handleRunFailure(error, aborted)`：合成 assistant 失败消息并发出完整生命周期事件。
    ///
    /// 事件序列固定为 `message_start` → `message_end` → `turn_end` → `agent_end`，不带
    /// `agent_start` / `turn_start`：失败可能发生在 `agent_start` 之前——`run_agent_loop_continue`
    /// 的前置校验失败时，executor 在发出任何事件前就返回（见 `agent_loop.rs` 的
    /// `validate_continue`），此时 run 内没有任何前序事件；其余路径循环已发过 `agent_start`。
    /// 这里只补 TS `handleRunFailure` 的收尾事件。
    ///
    /// 失败消息的字段：空的文本内容块、模型信息取自当前 `state.model`、零用量、
    /// `stopReason` 按 `aborted` 取 `"aborted"` 或 `"error"`、错误文本放在 `errorMessage`、
    /// `timestamp` 取当前时间；`agent_end` 只带这一条消息，而不是完整 transcript。
    ///
    /// 这些事件同样经过 `process_events`，因此失败消息会被追加进 `state.messages`，
    /// 并在 `turn_end` 时把 `errorMessage` 写进 `state.error_message`。
    async fn handle_run_failure(&self, error: String, aborted: bool) -> Result<(), AgentError> {
        let model = self.model();
        let failure_message = AgentMessage::Standard(Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: String::new(),
                text_signature: None,
            })],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: empty_usage(),
            stop_reason: if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            },
            error_message: Some(error),
            timestamp: now_ms(),
            ..Default::default()
        }));

        self.process_events(AgentEvent::MessageStart {
            message: failure_message.clone(),
        })
        .await?;
        self.process_events(AgentEvent::MessageEnd {
            message: failure_message.clone(),
        })
        .await?;
        self.process_events(AgentEvent::TurnEnd {
            message: failure_message.clone(),
            tool_results: Vec::new(),
        })
        .await?;
        self.process_events(AgentEvent::AgentEnd {
            messages: vec![failure_message],
        })
        .await?;
        Ok(())
    }

    /// `finishRun()`：清 runtime 状态、唤醒 `waitForIdle`、清 `activeRun`。
    ///
    /// 只清理编号为 `run_id` 的那次 run：正常路径上 `activeRun` 一定是它；万一它已被清理而
    /// 新 run 已经登记（防御性），不能误清新 run 的状态。
    fn finish_run(&self, run_id: u64) {
        let run = {
            let mut active_run = lock(&self.inner.active_run);
            match active_run.as_ref() {
                Some(run) if run.id == run_id => active_run.take(),
                _ => None,
            }
        };
        let Some(run) = run else {
            return;
        };
        // 注意 error_message 不在这里清除：它保留到下一次 run 的 claim_run 或 reset。
        // 这里也不清除 messages——transcript 是 run 的产物，不属于“运行态”。
        {
            let mut state = lock(&self.inner.state);
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
        }
        let _ = run.done.send(true);
    }

    /// `processEvents(event)`：先改状态，再按订阅顺序 `await` listener。
    ///
    /// 两个阶段刻意分开：状态更新在 `state` 锁内完成并立即释放，listener 在锁外逐个
    /// `await`，因此慢 listener 不会阻塞 `Agent::state` / `steer` 等调用。
    ///
    /// 状态更新发生在检查 `active_run` **之前**：若在 run 之外被调用，状态已被本次事件
    /// 修改，然后才返回 `ListenerOutsideActiveRun`。run 内部调用时 `active_run` 必然存在，
    /// 该错误只是防御。
    async fn process_events(&self, event: AgentEvent) -> Result<(), AgentError> {
        {
            let mut state = lock(&self.inner.state);
            match &event {
                AgentEvent::MessageStart { message } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageUpdate { message, .. } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    state.streaming_message = None;
                    state.messages.push(message.clone());
                    // `systemPrompt` 是 TS 的 getter（读取时现算）；这里不在每次 message_end
                    // 重算整个 transcript（长会话会退化成 O(n²)），由 [`Agent::state`] /
                    // [`Agent::system_prompt`] 读取时现算。
                }
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    state.pending_tool_calls.insert(tool_call_id.clone());
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    state.pending_tool_calls.remove(tool_call_id);
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if let Some(error_message) = assistant_error_message(message) {
                        state.error_message = Some(error_message.to_string());
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    state.streaming_message = None;
                }
                _ => {}
            }
        }

        let signal = lock(&self.inner.active_run)
            .as_ref()
            .map(|run| run.signal.clone());
        let Some(signal) = signal else {
            return Err(AgentError::ListenerOutsideActiveRun);
        };
        // TS: `for (const listener of this.listeners) await listener(event, signal)`。
        // JS 的 Set 迭代是**活**的：派发中途新 `add` 的 listener 会收到当前事件；在轮到自己
        // 之前被 `delete` 的会被跳过；`delete` 后重新 `add` 会拿到新序号并再次被访问。
        // 这里用“已访问的最大 seq + 每次重新取最小未访问 seq”复刻该语义。多线程下
        // “选中后、调用前”被退订的竞态无法完全排除（JS 单线程不存在），按仍会被调用处理。
        let mut last_seq: Option<u64> = None;
        loop {
            let next = {
                let listeners = lock(&self.inner.listeners);
                listeners
                    .iter()
                    .filter(|entry| last_seq.is_none_or(|seq| entry.seq > seq))
                    .min_by_key(|entry| entry.seq)
                    .map(|entry| (entry.seq, Arc::clone(&entry.listener)))
            };
            let Some((seq, listener)) = next else {
                break;
            };
            last_seq = Some(seq);
            // 每个 listener 拿到事件的独立克隆（listener 按值拥有事件）；取消令牌以借用传入，
            // 避免每个 listener 克隆一次句柄。
            listener(event.clone(), &signal).await;
        }
        Ok(())
    }
}

/// `runWithLifecycle` 的 executor：接收本次 run 的取消令牌，返回失败消息文本（`Err`）。
///
/// `Err` 里的 `String` 是低层循环的错误文本，或 `finish_executor` 从 event sink 槽里取出的
/// `processEvents` 错误文本；随后由 `handle_run_failure` 合成失败消息。`FnOnce` 保证一个
/// executor 只执行一次。
type RunExecutor =
    Box<dyn FnOnce(CancellationToken) -> BoxFuture<'static, Result<(), String>> + Send>;

/// `try { … } finally { finishRun() }` 的 Drop 形态。
///
/// 在 `begin_run` 里与 run future 一起创建（而不是在 `execute_run` 的函数体里），因此任务即使
/// 在首次被 poll 前就被运行时丢弃，future 被 Drop 时也会走到 `finishRun()`。
struct RunGuard {
    agent: Agent,
    run_id: u64,
    /// 正常路径在 `execute_run` 里手动调用 `finish_run` 后置 `false`，防止 `Drop` 重复清理。
    armed: bool,
}

impl RunGuard {
    fn new(agent: Agent, run_id: u64) -> Self {
        RunGuard {
            agent,
            run_id,
            armed: true,
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.armed {
            self.agent.finish_run(self.run_id);
        }
    }
}

/// 让出一次调度：首次 poll 注册唤醒并返回 `Pending`，再次 poll 返回 `Ready(())`。
///
/// 用来复刻 JS `await` 总把后续代码排进微任务的语义（见 [`Agent::event_sink`]）。
struct YieldOnce {
    yielded: bool,
}

impl YieldOnce {
    fn new() -> Self {
        YieldOnce { yielded: false }
    }
}

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// 用 no-op waker 轮询一次 future（同步前缀的执行器）。
///
/// 返回 `Pending` 时调用方必须继续用真实 waker 轮询（这里只用于“同步执行到第一个挂起点，
/// 然后把同一个 future 交给 tokio 任务继续”）。期间注册的 no-op waker 不会丢事件：任务恢复
/// 时的第一次 poll 会重新注册真实 waker 并重新检查内部状态。
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut context = Context::from_waker(Waker::noop());
    future.poll(&mut context)
}

/// 立即可用的错误 future（TS async 方法里的同步 `throw` 表现为 rejected promise）。
fn ready_error(error: AgentError) -> BoxFuture<'static, Result<(), AgentError>> {
    Box::pin(async move { Err(error) })
}

/// 合并 executor 的循环错误与事件 sink 记录的 `processEvents` 错误。
///
/// 优先级：循环错误优先——`Err` 直接原样返回，sink 槽里的错误被忽略；只有循环成功时才
/// 取出 sink 记录的第一个 `processEvents` 错误。这样 listener 侧的错误不会覆盖真正的
/// 循环失败原因。
fn finish_executor(
    outcome: Result<(), String>,
    error_slot: &Mutex<Option<AgentError>>,
) -> Result<(), String> {
    match outcome {
        Ok(()) => match lock(error_slot).take() {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        },
        Err(message) => Err(message),
    }
}

/// `normalizePromptInput(input: string, images?)` 的字符串分支。
///
/// `content` 总是块数组（即使只有文本），与 TS 的 `[text, ...images]` 形态一致；文本块在
/// 最前，图片按给定顺序追加；`timestamp` 取调用时刻的 Unix 毫秒。
fn normalize_prompt_input(input: String, images: Vec<ImageContent>) -> AgentMessage {
    let mut content = Vec::with_capacity(1 + images.len());
    content.push(UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: input,
        text_signature: None,
    }));
    content.extend(images.into_iter().map(UserContentBlock::Image));
    AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(content),
        timestamp: now_ms(),
    }))
}

/// `createMutableAgentState(initialState?)`。
///
/// 规则：
/// - `tools` 先投影成模型可见的声明（`agent_tool_declaration`），用于初始 system 消息的
///   `toolsAdded`；
/// - 只有当 `messages` 的首条不是 `role: "system"` 时，才把
///   `create_initial_system_message(systemPrompt, declarations)` 插到最前；该函数在
///   prompt 为空且工具声明为空时返回 `None`，此时什么都不插；
/// - `model` 缺省落成 `DEFAULT_MODEL` 的 `Some(...)`，`thinkingLevel` 缺省为 `"off"`；
/// - 返回的 `AgentState` 中运行态字段全部为零值，`system_prompt` 由最终 transcript 现算。
fn create_mutable_agent_state(initial_state: Option<AgentInitialState>) -> AgentState {
    let initial_state = initial_state.unwrap_or_default();
    let tools = initial_state.tools.unwrap_or_default();
    let mut messages = initial_state.messages.unwrap_or_default();
    let declarations: Vec<Tool> = tools.iter().map(agent_tool_declaration).collect();
    let initial_message =
        create_initial_system_message(initial_state.system_prompt.as_deref(), Some(&declarations));
    if messages.first().map(AgentMessage::role) != Some("system")
        && let Some(initial_message) = initial_message
    {
        messages.insert(0, AgentMessage::Standard(Message::System(initial_message)));
    }

    let system_prompt = current_system_prompt(&messages);
    AgentState {
        system_prompt,
        model: Some(initial_state.model.unwrap_or_else(default_model)),
        thinking_level: initial_state.thinking_level.unwrap_or_default(),
        tools,
        messages,
        is_streaming: false,
        streaming_message: None,
        pending_tool_calls: BTreeSet::new(),
        error_message: None,
    }
}

// ---------------------------------------------------------------------------
// Subscription
// ---------------------------------------------------------------------------

/// [`Agent::subscribe`] 返回的订阅句柄。
///
/// 对应 TS `subscribe` 返回的 `unsubscribe` 闭包：**只有**显式调用
/// [`Subscription::unsubscribe`] 才退订（幂等），drop 不会退订——TS 调用方丢弃该闭包时
/// listener 仍然有效（agent.test.ts 里大量 `agent.subscribe(...)` 语句依赖这一点）。
/// 内部持 `Weak`，不会延长 `Agent` 的生命周期。
pub struct Subscription {
    inner: Weak<AgentInner>,
    listener: AgentListener,
}

impl Subscription {
    /// 退订：从 listener 集合中删除该监听器。
    ///
    /// 同一个 listener `Arc` 只可能对应一个 Set 条目（订阅时按指针去重），因此退订按
    /// `Arc::ptr_eq` 删除；重复调用幂等。
    pub fn unsubscribe(&self) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner.listeners).retain(|entry| !Arc::ptr_eq(&entry.listener, &self.listener));
        }
    }
}

// ---------------------------------------------------------------------------
// WeakAgent
// ---------------------------------------------------------------------------

/// [`Agent::downgrade`] 返回的弱句柄：供 listener 内部回写队列/中止而不形成 `Arc` 环。
///
/// 只暴露 listener 内常用的控制面方法；需要完整能力时用 [`WeakAgent::upgrade`]。
///
/// 所有方法在 `Agent` 已被释放（`upgrade` 失败）时都不会 panic：写操作与清除操作不生效，
/// `has_queued_messages` 返回 `false`，`peek_queued_messages` 返回空数组，`signal` 返回
/// `None`，`upgrade` 返回 `None`。这样 listener 即使存活到 Agent 释放之后也不会出错。
#[derive(Clone)]
pub struct WeakAgent {
    inner: Weak<AgentInner>,
}

impl WeakAgent {
    /// 升级为 [`Agent`]；`Agent` 已被释放时返回 `None`。
    pub fn upgrade(&self) -> Option<Agent> {
        self.inner.upgrade().map(|inner| Agent { inner })
    }

    /// 见 [`Agent::steer`]。
    pub fn steer(&self, message: AgentMessage) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner.steering_queue).enqueue(message);
        }
    }

    /// 见 [`Agent::follow_up`]。
    pub fn follow_up(&self, message: AgentMessage) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner.follow_up_queue).enqueue(message);
        }
    }

    /// 见 [`Agent::clear_steering_queue`]。
    pub fn clear_steering_queue(&self) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner.steering_queue).clear();
        }
    }

    /// 见 [`Agent::clear_follow_up_queue`]。
    pub fn clear_follow_up_queue(&self) {
        if let Some(inner) = self.inner.upgrade() {
            lock(&inner.follow_up_queue).clear();
        }
    }

    /// 见 [`Agent::clear_all_queues`]。
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    /// 见 [`Agent::has_queued_messages`]。
    pub fn has_queued_messages(&self) -> bool {
        self.inner.upgrade().is_some_and(|inner| {
            lock(&inner.steering_queue).has_items() || lock(&inner.follow_up_queue).has_items()
        })
    }

    /// 见 [`Agent::peek_queued_messages`]。
    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let Some(inner) = self.inner.upgrade() else {
            return Vec::new();
        };
        let steering = lock(&inner.steering_queue).peek();
        if !steering.is_empty() {
            return steering;
        }
        lock(&inner.follow_up_queue).peek()
    }

    /// 见 [`Agent::abort`]。
    pub fn abort(&self) {
        if let Some(inner) = self.inner.upgrade()
            && let Some(run) = lock(&inner.active_run).as_ref()
        {
            run.signal.cancel();
        }
    }

    /// 见 [`Agent::signal`]。
    pub fn signal(&self) -> Option<CancellationToken> {
        self.inner.upgrade().and_then(|inner| {
            lock(&inner.active_run)
                .as_ref()
                .map(|run| run.signal.clone())
        })
    }
}
