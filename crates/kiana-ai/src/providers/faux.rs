//! ``providers/faux.ts``：可脚本化的测试 provider。
//!
//! TS: `packages/ai/src/providers/faux.ts`（逐条对应 `spec/ai-core.md` §2.3.13）。
//!
//! 语义要点：
//! - [`faux_provider`] 用 `createProvider` 注册一个 token 级流式输出的 provider；
//!   `create_faux_core` 暴露同样的核心，供 `compat.ts` 的 `registerFauxProvider` 复用。
//! - 提示词/输出 token 数按 UTF-16 码元长度 `ceil(len / 4)` 估算；`sessionId` +
//!   `cacheRetention !== "none"` 时模拟按公共前缀命中的 prompt cache。
//! - 流式事件序列固定：每个内容块先 push `*_start`，按 token 大小切块 push
//!   `*_delta`，最后 `*_end`。
//! - 延迟响应（`deferred`）的提交、轮询（`pendingFetches`）与取消语义见 §2.3.13。
//!
//! Rust 侧差异（事件是 owned 值，行为在增量消费下一致）：
//! - TS 的事件携带 `{ ...partial }` 浅拷贝，与后续 delta **共享** content 数组和块对象，
//!   因此事后回看已收集的事件时，`*_start`/`*_delta` 的 partial 显示的是该块的最终内容
//!   （`text_start.partial.content[0].text` 也是全量文本）。Rust 的每个事件是发送时刻的
//!   深拷贝快照；逐事件消费的行为相同，只有「收集后再检查旧事件」的结果不同。
//! - TS 的响应工厂收到 `(context, options, state, model)`；Rust 用一条 owned 的
//!   [`FauxResponseInput`] 传入同样的值（future 必须是 `'static`，无法借用参数）。
//!   `options` 里的 `deferred`/`signal`/`onResponse` 等载体见 [`FauxStreamOptions`]。
//! - TS 用 `Math.random()`/`Date.now()` 生成 id；Rust 用线程本地 PRNG，形态相同
//!   （`prefix:时间戳:base36`）。
//! - 文本分块按 UTF-16 码元切片；若切片恰好劈开代理对，JS 会产生孤立代理项，
//!   Rust `String` 无法表示，因此把这个字符整体并入当前 chunk（每次这类切分让后续起点
//!   比 TS 后移一个码元，偏移会累积；拼接结果与原文一致，不影响 BMP 文本）。
//!
//! # 为什么需要这个模块
//!
//! 真实 provider（Anthropic/OpenAI/…）受网络、凭据与限流影响，输出不可复现。测试和上层
//! 的会话/工具编排需要一个「输出完全由脚本决定」的 provider：调用方先排好每一步响应
//! （静态消息或异步工厂），faux 按排队顺序逐条消费，再用与真实 provider 相同的事件形状
//! （`start`/`delta`/`end`、usage、deferred）把它们流出来。因此本模块只做协议仿真：
//! 不发起任何网络请求，也不读取环境变量或凭据。
//!
//! # 一次请求的执行流程
//!
//! 1. `stream`/`stream_simple` 进入内部函数 `start_stream`：**同步**从脚本队列弹出一条并
//!    `callCount += 1`，然后用 `spawn_forward` 把流式任务交给后台，立即返回事件流。
//! 2. 后台任务先调用 `onResponse`（若提供），再按情况三选一：队列为空 → 错误消息；
//!    `options.deferred` 为真 → 登记延迟条目并流出 `deferred` 占位消息；否则调用响应工厂
//!    或取静态消息，补齐 usage 后流式输出。
//! 3. `stream_with_deltas` 逐内容块产出 `*_start` → 若干 `*_delta` → `*_end`，最后以
//!    `done` 或 `error` 结束并 `end` 事件流。
//! 4. 延迟条目由 `fetch_deferred` 兑换：前 `pendingFetches` 次继续返回占位消息，之后执行
//!    脚本并把最终消息写回条目，顺序轮询时后续请求直接重放缓存。兑换没有 single-flight：
//!    并发的首次兑换可能各执行一次脚本，条目以最后写入的 `final_message` 为准。
//!
//! # 并发与共享状态
//!
//! [`FauxCore`] 的 `Clone` 是**浅克隆**：脚本队列、[`FauxProviderState`]、prompt cache 与
//! 延迟条目表都经 `Arc` 共享，多个 clone 可以并发发起请求。脚本的弹出发生在
//! `start_stream` 的同步阶段，所以「谁拿到哪条脚本」由弹出时的加锁顺序决定（单线程内即
//! 调用顺序）；各请求的流式任务彼此独立，输出可以交错。所有互斥锁的临界区都很短，且**不跨 `.await` 持锁**：
//! `fetch_deferred_inner` 先在锁内算出计划（`FetchPlan`），释放锁后才等待工厂或写出事件。
//! `lock` 辅助函数在锁中毒时取回内部值继续执行，所以一次线程 panic 不会让后续请求永久失败。
//!
//! # 边界与失败模式
//!
//! - 队列为空：以 `error` 事件结束，消息文本 `No more faux responses queued`。
//! - 工厂返回 `Err`：转成 `stopReason = "error"` 的助手消息（错误文本按 `ThrownValue`
//!   的形态取值）。
//! - 消息的 `stopReason` 仍为 `pending`：视为脚本错误，最终以
//!   `Faux response ended without a stop reason` 结束。
//! - 取消（`CancellationToken`）：在每个内容块开始与每个 chunk 发送前检查；已取消则用
//!   `stopReason = "aborted"` 的 partial 结束流，不向上抛错。
//! - 延迟响应：未知/已取消的 handle 分别产生 `Unknown faux deferred response` 与
//!   `Faux deferred response was cancelled` 错误消息；重复兑换命中缓存。
//!
//! # 阅读顺序
//!
//! 先看 [`FauxResponseStep`] 与 [`FauxCore`] 理解脚本模型，再读 `stream_with_deltas` 的
//! 事件状态机，最后看 `fetch_deferred_inner` 的兑换流程与 `with_usage_estimate` 的
//! usage/token 估算和 prompt cache 模拟（cost 恒为 0，不模拟计费）。端到端行为见
//! `crates/kiana-ai/tests/faux_provider.rs`。

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::api::lazy::spawn_forward;
use crate::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use crate::error::AiError;
use crate::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use crate::models::{CreateProviderOptions, Provider, ProviderStreams, create_provider};
use crate::types::{
    AnyModel, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole,
    CacheRetention, ClientIdentity, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle,
    DeferredRequest, DoneReason, ErrorReason, Model, ModelCost, ModelInputLimits,
    ModelInputModality, OnPayloadFn, OnProviderStreamEventFn, OnResponseFn, ProviderEnv,
    ProviderHeaders, ProviderResponse, SamplingParams, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, TextKind, ThinkingBudgets, ThinkingContent, ThinkingKind,
    ThinkingLevel, ToolCall, ToolCallKind, ToolChoice, ToolReference, ToolResultMessage,
    TranscriptContext, Transport, Usage, UsageCost, UserContent, UserContentBlock,
};
use crate::utils::diagnostics::ThrownValue;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::get_system_message_text;

/// TS: `DEFAULT_API`。写进 [`faux_assistant_message`] 的 api 字段，并作为默认随机 api id
/// 的前缀；默认模型对象的 api 是随机 id（`faux:<毫秒>:<base36>`），不是该常量。
pub const DEFAULT_API: &str = "faux";
/// TS: `DEFAULT_PROVIDER`。写进默认消息与默认模型对象的 provider 字段。
pub const DEFAULT_PROVIDER: &str = "faux";
/// TS: `DEFAULT_MODEL_ID`。未提供 `models` 时生成的默认模型 id。
pub const DEFAULT_MODEL_ID: &str = "faux-1";
/// TS: `DEFAULT_MODEL_NAME`。默认模型的展示名。
pub const DEFAULT_MODEL_NAME: &str = "Faux Model";
/// TS: `DEFAULT_BASE_URL`。仅用于填充模型对象；faux 不发真实请求，因此不会被访问。
pub const DEFAULT_BASE_URL: &str = "http://localhost:0";
/// TS: `DEFAULT_MIN_TOKEN_SIZE`。每块最少「token」数；实际按 `token * 4` 个 UTF-16 码元切。
pub const DEFAULT_MIN_TOKEN_SIZE: usize = 3;
/// TS: `DEFAULT_MAX_TOKEN_SIZE`。每块最多「token」数。
pub const DEFAULT_MAX_TOKEN_SIZE: usize = 5;

/// `Math.random().toString(36).slice(2)` 的位数上限（JS 双精度大约 11-13 位）。
const RANDOM_ID_DIGITS: usize = 13;

/// TS `DEFAULT_USAGE`：全零 usage 与全零 cost（`totalFromProvider` 缺省为 `None`）。
///
/// 返回新值而不是共享常量，因为 `Usage` 会随消息被修改。
fn default_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
            total_from_provider: None,
        },
    }
}

/// `Date.now()`：Unix 毫秒。系统时钟早于 epoch 时退回 0；TS 的 `Date.now()` 在该情形下
/// 会返回负毫秒值，这里的 0 是有意近似。
fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 取互斥锁；锁中毒时取回内部值继续。
///
/// faux 的共享状态只有简单计数器、字符串表和条目结构，持锁代码不做用户回调，因此中毒后
/// 继续使用不会放大不一致；这样线程 panic 不会让后续请求永久失败。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// [`FauxModelDefinition`]：脚本里声明的模型定义。
///
/// 这里只描述「与默认值不同的部分」；`create_faux_core` 在构造 [`Model`] 时，除
/// `input_limits`（`None` 原样保留为不设置）外，会为其余 `None` 字段补默认值（见各字段
/// 文档）。派生 `Default` 会得到空 id，调用方应显式设置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FauxModelDefinition {
    /// 模型 id，原样写入 `Model::id`，不做校验。
    pub id: String,
    /// 展示名；`None` 时回退为 id。
    pub name: Option<String>,
    /// 是否支持推理；`None` 视为 `false`。
    pub reasoning: Option<bool>,
    /// 允许的输入模态；`None` 视为 `[Text, Image]`。
    pub input: Option<Vec<ModelInputModality>>,
    /// 输入长度限制；`None` 表示不设置。
    pub input_limits: Option<ModelInputLimits>,
    /// 计费参数；`None` 视为 `ModelCost::default()`（全 0）。
    pub cost: Option<ModelCost>,
    /// 上下文窗口；`None` 视为 128000。
    pub context_window: Option<u64>,
    /// 最大输出 token；`None` 视为 16384。
    pub max_tokens: Option<u64>,
}

impl FauxModelDefinition {
    /// `{ id }` 的最小定义。
    pub fn new(id: impl Into<String>) -> Self {
        FauxModelDefinition {
            id: id.into(),
            ..Default::default()
        }
    }
}

/// `fauxText(text)`。
pub fn faux_text(text: impl Into<String>) -> TextContent {
    TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    }
}

/// `fauxThinking(thinking)`。
pub fn faux_thinking(thinking: impl Into<String>) -> ThinkingContent {
    ThinkingContent {
        kind_thinking: ThinkingKind::Thinking,
        thinking: thinking.into(),
        thinking_signature: None,
        redacted: None,
    }
}

/// `fauxToolCall` 的 `options: { id? }`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FauxToolCallOptions {
    /// 工具调用 id；`None` 时生成 `tool:<毫秒>:<base36>` 形式的随机 id。
    pub id: Option<String>,
}

/// `fauxToolCall(name, arguments, options = {})`：构造一个完整的工具调用块。
///
/// `arguments` 直接成为 `ToolCall::arguments`；流式输出时它会被 `JSON.stringify`
/// 后切块，再由 `ToolCallArgumentsStream` 还原成 partial 参数。
pub fn faux_tool_call_with(
    name: impl Into<String>,
    arguments: crate::types::JsonObject,
    options: FauxToolCallOptions,
) -> ToolCall {
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: options.id.unwrap_or_else(|| random_id("tool")),
        name: name.into(),
        arguments,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

/// `fauxToolCall(name, arguments)`（id 随机）。
pub fn faux_tool_call(name: impl Into<String>, arguments: crate::types::JsonObject) -> ToolCall {
    faux_tool_call_with(name, arguments, FauxToolCallOptions::default())
}

/// `string | FauxContentBlock | FauxContentBlock[]`。
///
/// 供 [`faux_assistant_message`] 接受多种写法；无论哪种形态，最终都规范化为
/// `Vec<AssistantContentBlock>`（见 `normalize_faux_assistant_content`）。
#[derive(Debug, Clone, PartialEq)]
pub enum FauxContent {
    /// 纯文本，规范化为单个 text 块。
    Text(String),
    /// 单个内容块（text/thinking/toolCall）。
    Block(AssistantContentBlock),
    /// 多个内容块，顺序即流式输出顺序。
    Blocks(Vec<AssistantContentBlock>),
}

impl From<&str> for FauxContent {
    fn from(text: &str) -> Self {
        FauxContent::Text(text.to_string())
    }
}

impl From<String> for FauxContent {
    fn from(text: String) -> Self {
        FauxContent::Text(text)
    }
}

impl From<AssistantContentBlock> for FauxContent {
    fn from(block: AssistantContentBlock) -> Self {
        FauxContent::Block(block)
    }
}

impl From<TextContent> for FauxContent {
    fn from(block: TextContent) -> Self {
        FauxContent::Block(AssistantContentBlock::Text(block))
    }
}

impl From<ThinkingContent> for FauxContent {
    fn from(block: ThinkingContent) -> Self {
        FauxContent::Block(AssistantContentBlock::Thinking(block))
    }
}

impl From<ToolCall> for FauxContent {
    fn from(block: ToolCall) -> Self {
        FauxContent::Block(AssistantContentBlock::ToolCall(block))
    }
}

impl From<Vec<AssistantContentBlock>> for FauxContent {
    fn from(blocks: Vec<AssistantContentBlock>) -> Self {
        FauxContent::Blocks(blocks)
    }
}

fn normalize_faux_assistant_content(content: FauxContent) -> Vec<AssistantContentBlock> {
    match content {
        FauxContent::Text(text) => vec![AssistantContentBlock::Text(faux_text(text))],
        FauxContent::Block(block) => vec![block],
        FauxContent::Blocks(blocks) => blocks,
    }
}

/// `fauxAssistantMessage` 的 `options`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FauxAssistantMessageOptions {
    /// 终止原因；`None` 时用 `Stop`。显式设为 `Pending` 会让流在末尾报脚本错误。
    pub stop_reason: Option<StopReason>,
    /// 预置的延迟 handle，原样写入消息（用于构造「已是 deferred」的输入）。
    pub deferred: Option<DeferredHandle>,
    /// 错误文本，原样写入 `AssistantMessage::error_message`。
    pub error_message: Option<String>,
    /// 响应 id，原样写入。
    pub response_id: Option<String>,
    /// 消息时间戳（Unix 毫秒）；`None` 时取当前时间。
    pub timestamp: Option<i64>,
}

/// `fauxAssistantMessage(content, options = {})`。
///
/// 生成的消息固定带 `api = "faux"`、`provider = "faux"`、`model = "faux-1"` 与全零
/// usage；真正流出前 `resolve_response` 会用本次请求的 api/provider/model 覆盖前三个字段，
/// 并用 `with_usage_estimate` 覆盖 usage，所以这些占位值不会泄漏给调用方。
pub fn faux_assistant_message(
    content: impl Into<FauxContent>,
    options: FauxAssistantMessageOptions,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: normalize_faux_assistant_content(content.into()),
        api: DEFAULT_API.into(),
        provider: DEFAULT_PROVIDER.into(),
        model: DEFAULT_MODEL_ID.into(),
        response_model: None,
        response_id: options.response_id,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: options.stop_reason.unwrap_or(StopReason::Stop),
        deferred: options.deferred,
        error_message: options.error_message,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: options.timestamp.unwrap_or_else(now_unix_ms),
        duration_ms: None,
    }
}

/// `FauxProviderState`：跨请求共享的调用计数。
///
/// `Clone` 共享同一份内部状态（`Arc`），所以 handle、core 以及响应工厂收到的 state 观察
/// 到的是同一组计数器，可用于在测试里断言「请求发生过几次」「第几次轮询才出结果」。
/// `call_count`/`deferred_fetch_count` 用 `SeqCst` 原子自增；`cancelled_deferred` 是
/// **只追加**的历史（每次取消 push 一个 handle 副本，无论该 handle 是否存在）。
#[derive(Debug, Clone, Default)]
pub struct FauxProviderState {
    inner: Arc<FauxProviderStateInner>,
}

#[derive(Debug, Default)]
struct FauxProviderStateInner {
    call_count: AtomicU64,
    deferred_fetch_count: AtomicU64,
    cancelled_deferred: Mutex<Vec<DeferredHandle>>,
}

impl FauxProviderState {
    /// `state.callCount`。
    pub fn call_count(&self) -> u64 {
        self.inner.call_count.load(Ordering::SeqCst)
    }

    /// `state.deferredFetchCount`。
    pub fn deferred_fetch_count(&self) -> u64 {
        self.inner.deferred_fetch_count.load(Ordering::SeqCst)
    }

    /// `state.cancelledDeferred`（副本）。
    pub fn cancelled_deferred(&self) -> Vec<DeferredHandle> {
        lock(&self.inner.cancelled_deferred).clone()
    }

    fn increment_call_count(&self) {
        self.inner.call_count.fetch_add(1, Ordering::SeqCst);
    }

    fn increment_deferred_fetch_count(&self) {
        self.inner
            .deferred_fetch_count
            .fetch_add(1, Ordering::SeqCst);
    }

    fn push_cancelled(&self, handle: DeferredHandle) {
        lock(&self.inner.cancelled_deferred).push(handle);
    }
}

/// 响应工厂看到的 `SimpleStreamOptions` 视图（TS 把原始对象直接交给工厂）。
///
/// 数据字段按值拷贝；TS 里工厂还能看到 `deferred`/`signal`/`onResponse` 以及其余载体，
/// Rust 侧以 `CancellationToken` 和回调 `Arc` 的克隆承载。延迟响应的兑换路径上 TS 会先
/// 解构掉 `deferred`/`signal`/`onResponse`，因此那里的这三个字段为 `None`。
///
/// `PartialEq` 只用于测试；回调/客户端胶囊按「同为 `None` 或同一个 `Arc`」比较。
///
/// 与请求选项同名的普通数据字段保持同名同义（对应 [`StreamOptions`] /
/// [`SimpleStreamOptions`]），这里只解释载体层面的差异；
/// `signal`/`on_response`/`fetch` 等以 `Arc`/token 克隆，与请求共享同一个对象。
#[derive(Clone, Default)]
pub struct FauxStreamOptions {
    /// 调用方显式传入的 API key；faux 不使用，工厂可读取。
    pub api_key: Option<String>,
    /// provider 作用域的环境变量覆盖。
    pub env: Option<ProviderEnv>,
    /// 额外请求头（值为 `None` 表示抑制同名默认头）。
    pub headers: Option<ProviderHeaders>,
    /// 客户端身份信息。
    pub identity: Option<ClientIdentity>,
    /// 请求超时（毫秒）；faux 不发起请求，仅透传。
    pub timeout_ms: Option<u64>,
    /// 最大重试次数。
    pub max_retries: Option<u32>,
    /// 最大重试延迟（毫秒）。
    pub max_retry_delay_ms: Option<u64>,
    /// 采样温度。
    pub temperature: Option<f64>,
    /// API 专属采样参数集合。
    pub sampling_params: Option<SamplingParams>,
    /// 最大输出 token。
    pub max_tokens: Option<u64>,
    /// 传输方式（SSE/WebSocket/自动）。
    pub transport: Option<Transport>,
    /// prompt cache 保留策略；只有 `Some(CacheRetention::None)` 关闭缓存模拟，
    /// `None`（未设置）按允许缓存处理。
    pub cache_retention: Option<CacheRetention>,
    /// 会话 id；非空且未显式关闭缓存时启用 prompt cache 模拟。
    pub session_id: Option<String>,
    /// WebSocket 连接超时（毫秒）。
    pub websocket_connect_timeout_ms: Option<u64>,
    /// 请求元数据。
    pub metadata: Option<crate::types::JsonObject>,
    /// API 专属的额外选项（键为 TS 字段名，值原样透传）。
    pub extra: crate::types::JsonObject,
    /// 工具选择策略。
    pub tool_choice: Option<ToolChoice>,
    /// 推理强度。
    pub reasoning: Option<ThinkingLevel>,
    /// 各推理级别的 thinking token 预算。
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// `options.deferred`（TS 原样交给工厂；延迟响应的兑换路径上已被剥掉）。
    pub deferred: Option<DeferredRequest>,
    /// TS 的 `options.signal`（Rust 里由 [`CancellationToken`] 承载）。
    pub signal: Option<CancellationToken>,
    /// TS 的 `options.onResponse`：流本身在工厂之前已调用一次，工厂仍可再次调用。
    pub on_response: Option<OnResponseFn>,
    /// TS 的 `options.onPayload`。
    pub on_payload: Option<OnPayloadFn>,
    /// TS 的 `options.onProviderStreamEvent`。
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    /// TS 的 `options.fetch`（Rust 的 HTTP 客户端胶囊）。
    pub fetch: Option<Arc<dyn crate::http::HttpClient>>,
    /// TS 的 `options.telemetry`。
    pub telemetry: Option<Arc<dyn kiana_telemetry::TelemetrySpan>>,
}

/// 回调/客户端胶囊的 `PartialEq`：同为 `None` 或指向同一个 `Arc`。
fn option_arc_ptr_eq<T: ?Sized>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

impl std::fmt::Debug for FauxStreamOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 回调与客户端胶囊没有有意义的 Debug 输出，只显示是否存在。
        f.debug_struct("FauxStreamOptions")
            .field("api_key", &self.api_key)
            .field("env", &self.env)
            .field("headers", &self.headers)
            .field("identity", &self.identity)
            .field("timeout_ms", &self.timeout_ms)
            .field("max_retries", &self.max_retries)
            .field("max_retry_delay_ms", &self.max_retry_delay_ms)
            .field("temperature", &self.temperature)
            .field("sampling_params", &self.sampling_params)
            .field("max_tokens", &self.max_tokens)
            .field("transport", &self.transport)
            .field("cache_retention", &self.cache_retention)
            .field("session_id", &self.session_id)
            .field(
                "websocket_connect_timeout_ms",
                &self.websocket_connect_timeout_ms,
            )
            .field("metadata", &self.metadata)
            .field("extra", &self.extra)
            .field("tool_choice", &self.tool_choice)
            .field("reasoning", &self.reasoning)
            .field("thinking_budgets", &self.thinking_budgets)
            .field("deferred", &self.deferred)
            .field("signal", &self.signal)
            .field(
                "on_response",
                &self.on_response.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_payload",
                &self.on_payload.as_ref().map(|_| "<callback>"),
            )
            .field(
                "on_provider_stream_event",
                &self.on_provider_stream_event.as_ref().map(|_| "<callback>"),
            )
            .field("fetch", &self.fetch.as_ref().map(|_| "<client>"))
            .field("telemetry", &self.telemetry.as_ref().map(|_| "<span>"))
            .finish()
    }
}

impl PartialEq for FauxStreamOptions {
    fn eq(&self, other: &Self) -> bool {
        self.api_key == other.api_key
            && self.env == other.env
            && self.headers == other.headers
            && self.identity == other.identity
            && self.timeout_ms == other.timeout_ms
            && self.max_retries == other.max_retries
            && self.max_retry_delay_ms == other.max_retry_delay_ms
            && self.temperature == other.temperature
            && self.sampling_params == other.sampling_params
            && self.max_tokens == other.max_tokens
            && self.transport == other.transport
            && self.cache_retention == other.cache_retention
            && self.session_id == other.session_id
            && self.websocket_connect_timeout_ms == other.websocket_connect_timeout_ms
            && self.metadata == other.metadata
            && self.extra == other.extra
            && self.tool_choice == other.tool_choice
            && self.reasoning == other.reasoning
            && self.thinking_budgets == other.thinking_budgets
            && self.deferred == other.deferred
            && self.signal == other.signal
            && option_arc_ptr_eq(&self.on_response, &other.on_response)
            && option_arc_ptr_eq(&self.on_payload, &other.on_payload)
            && option_arc_ptr_eq(
                &self.on_provider_stream_event,
                &other.on_provider_stream_event,
            )
            && option_arc_ptr_eq(&self.fetch, &other.fetch)
            && option_arc_ptr_eq(&self.telemetry, &other.telemetry)
    }
}

impl FauxStreamOptions {
    /// 从 [`SimpleStreamOptions`] 取视图（数据字段按值、载体按 `Arc` 克隆）。
    pub fn from_simple(options: &SimpleStreamOptions) -> Self {
        let base = &options.base;
        FauxStreamOptions {
            api_key: base.api_key.clone(),
            env: base.env.clone(),
            headers: base.headers.clone(),
            identity: base.identity.clone(),
            timeout_ms: base.timeout_ms,
            max_retries: base.max_retries,
            max_retry_delay_ms: base.max_retry_delay_ms,
            temperature: base.temperature,
            sampling_params: base.sampling_params.clone(),
            max_tokens: base.max_tokens,
            transport: base.transport,
            cache_retention: base.cache_retention,
            session_id: base.session_id.clone(),
            websocket_connect_timeout_ms: base.websocket_connect_timeout_ms,
            metadata: base.metadata.clone(),
            extra: base.extra.clone(),
            tool_choice: options.tool_choice,
            reasoning: options.reasoning,
            thinking_budgets: options.thinking_budgets.clone(),
            deferred: options.deferred,
            signal: base.signal.clone(),
            on_response: base.on_response.clone(),
            on_payload: base.on_payload.clone(),
            on_provider_stream_event: base.on_provider_stream_event.clone(),
            fetch: base.fetch.clone(),
            telemetry: base.telemetry.clone(),
        }
    }
}

/// 响应工厂的输入（TS 的四个位置参数）。
#[derive(Debug, Clone)]
pub struct FauxResponseInput {
    /// 归一化后的会话记录（`systemPrompt`/`tools` 已折叠进首条 system 消息）。
    pub context: TranscriptContext,
    /// 请求选项视图；普通流路径携带调用方原始选项，延迟兑换路径已剥掉
    /// `deferred`/`signal`/`onResponse`。
    pub options: Option<FauxStreamOptions>,
    /// 与 provider 共享的计数器，工厂可读取调用次数。
    pub state: FauxProviderState,
    /// 本次请求使用的模型。
    pub model: Model,
}

/// 响应工厂返回的 future。
///
/// 必须是 `Send + 'static`：工厂在后台任务里被调用，不能借用 [`FauxResponseInput`]。
pub type FauxResponseFuture =
    Pin<Box<dyn Future<Output = Result<AssistantMessage, ThrownValue>> + Send + 'static>>;

/// `FauxResponseFactory`：可失败的异步响应工厂。
///
/// 输入按值给出（见 [`FauxResponseInput`]）；返回 `Err(ThrownValue)` 时流会以
/// `stopReason = "error"` 结束，而不是向外抛异常。
pub type FauxResponseFactory = Arc<dyn Fn(FauxResponseInput) -> FauxResponseFuture + Send + Sync>;

/// `FauxResponseStep = AssistantMessage | FauxResponseFactory`。
///
/// 脚本队列中的一步：要么是现成的助手消息，要么是一个在请求被消费时才执行的工厂。
/// 两种形态都会经过同一套 usage 补齐与流式切块流程。
#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum FauxResponseStep {
    /// 直接返回这条消息（仍会被重写 api/provider/model 并补齐 usage）。
    Message(AssistantMessage),
    /// 请求真正被消费时调用，可依据 context/options/state/model 动态生成响应。
    Factory(FauxResponseFactory),
}

impl FauxResponseStep {
    /// 可失败的异步工厂：把任意 `Fn(FauxResponseInput) -> Future` 装箱成
    /// [`FauxResponseFactory`]。
    ///
    /// 工厂会被 `spawn_forward` 派生的任务调用，因此闭包与 future 都必须 `Send`，
    /// 且 future 必须 `'static`（不能借用输入）。
    pub fn factory<F, Fut>(factory: F) -> Self
    where
        F: Fn(FauxResponseInput) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<AssistantMessage, ThrownValue>> + Send + 'static,
    {
        FauxResponseStep::Factory(Arc::new(move |input| {
            Box::pin(factory(input)) as FauxResponseFuture
        }))
    }

    /// 不可失败的异步工厂（TS 里返回 `AssistantMessage | Promise<AssistantMessage>`）。
    ///
    /// 内部包成 `Ok(message)`；需要模拟脚本失败时用 [`FauxResponseStep::factory`]。
    pub fn message_factory<F, Fut>(factory: F) -> Self
    where
        F: Fn(FauxResponseInput) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AssistantMessage> + Send + 'static,
    {
        FauxResponseStep::factory(move |input| {
            let future = factory(input);
            async move { Ok(future.await) }
        })
    }
}

impl From<AssistantMessage> for FauxResponseStep {
    fn from(message: AssistantMessage) -> Self {
        FauxResponseStep::Message(message)
    }
}

impl From<FauxResponseFactory> for FauxResponseStep {
    fn from(factory: FauxResponseFactory) -> Self {
        FauxResponseStep::Factory(factory)
    }
}

/// `options.deferred`：延迟响应模拟参数。
///
/// 提交延迟请求时不会执行脚本；第一次真正兑换时才解析。`pending_fetches` 决定前几次
/// 轮询继续返回占位 handle，`poll_after_ms` 只是原样写进 handle，faux 不会据此定时。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FauxDeferredOptions {
    /// 返回原始 handle 的轮询次数（`Math.max(0, Math.floor(...))`；`None`/NaN 视为 0）。
    pub pending_fetches: Option<f64>,
    /// 写进 handle 的 `pollAfterMs`；`None` 时 handle 不带该字段。
    pub poll_after_ms: Option<f64>,
}

/// `options.tokenSize`：每块「token」数的取值范围。
///
/// 注意它是**模拟 token**：切块时 `charSize = tokenSize * 4` 个 UTF-16 码元，与
/// `estimateTokens` 的 4 码元/token 估算对应。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FauxTokenSize {
    /// 每块最少 token 数；`None` → 3。
    pub min: Option<usize>,
    /// 每块最多 token 数；`None` → 5。
    pub max: Option<usize>,
}

/// `RegisterFauxProviderOptions`：构造 faux provider/core 的全部可调参数。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegisterFauxProviderOptions {
    /// api id；`None` 时生成随机 id（`faux:<毫秒>:<base36>`），**不是**常量 `"faux"`。
    pub api: Option<String>,
    /// provider id；`None` → `"faux"`。
    pub provider: Option<String>,
    /// 模型定义；`None` 或空数组时使用内置的 `faux-1`（text+image、128k/16k）。
    pub models: Option<Vec<FauxModelDefinition>>,
    /// 延迟响应的轮询参数。
    pub deferred: Option<FauxDeferredOptions>,
    /// 流式节流（模拟 token/秒）；`None` 或非正值表示不 sleep，只让出执行权。
    pub tokens_per_second: Option<f64>,
    /// 分块大小范围（模拟 token）。
    pub token_size: Option<FauxTokenSize>,
}

// 一条已提交、尚未（或已经）兑换的延迟响应。
//
// `step`/`context`/`options`/`model` 是提交时的快照，首次兑换用它执行脚本；`options`
// 已经剥掉 deferred/signal/onResponse（与 TS 的解构一致）。`pending_fetches` 是剩余
// 「继续返回占位消息」的次数；`final_message` 一旦写入（成功或失败消息）就不再重算，
// 后续轮询直接重放；`cancelled` 只置位不撤销。
struct FauxDeferredEntry {
    handle: DeferredHandle,
    step: FauxResponseStep,
    context: TranscriptContext,
    options: Option<Arc<SimpleStreamOptions>>,
    model: Model,
    pending_fetches: u64,
    cancelled: bool,
    final_message: Option<AssistantMessage>,
}

/// `createFauxCore(options)` 的返回值：流实现 + 脚本队列 + 状态。
///
/// 可直接作为 [`ProviderStreams`] 使用（`Arc<FauxCore>`），与 TS 的
/// `core.stream`/`core.streamSimple`/`core.fetchDeferred`/`core.cancelDeferred` 对应。
///
/// `Clone` 是共享句柄：脚本队列、[`FauxProviderState`]（内部持 `Arc`）、prompt cache 与
/// 延迟条目表等可变状态都经 `Arc` 共享，克隆出的 core 看到的是同一条队列、同一组计数器
/// 和同一张条目表；api/provider 字符串与 [`FauxDeferredOptions`] 等不可变配置按值复制。
#[derive(Clone)]
pub struct FauxCore {
    api: String,
    provider: String,
    models: Arc<Vec<Model>>,
    pending_responses: Arc<Mutex<VecDeque<FauxResponseStep>>>,
    state: FauxProviderState,
    prompt_cache: Arc<Mutex<HashMap<String, Vec<String>>>>,
    deferred_responses: Arc<Mutex<HashMap<String, FauxDeferredEntry>>>,
    min_token_size: usize,
    max_token_size: usize,
    tokens_per_second: Option<f64>,
    deferred: FauxDeferredOptions,
}

impl FauxCore {
    /// `core.api`。
    pub fn api(&self) -> &str {
        &self.api
    }

    /// `core.provider`。
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// `core.models`。
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// `core.getModel(id?)`：无 id（或空字符串）返回第一个模型，否则线性查找。
    ///
    /// 空字符串按「未提供」处理，复刻 TS 的假值判断；找不到时返回 `None`。
    pub fn get_model(&self, requested_model_id: Option<&str>) -> Option<&Model> {
        // `if (!requestedModelId) return models[0]`：空字符串在 JS 里为假。
        match requested_model_id.filter(|id| !id.is_empty()) {
            None => self.models.first(),
            Some(id) => self.models.iter().find(|candidate| candidate.id == id),
        }
    }

    /// `core.state`：与所有 clone、响应工厂共享的计数器。
    pub fn state(&self) -> &FauxProviderState {
        &self.state
    }

    /// `core.setResponses(responses)`：用新脚本**替换**整个待消费队列。
    ///
    /// 已经在流式输出中的请求不受影响；替换只作用于后续请求。
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        let mut pending = lock(&self.pending_responses);
        *pending = responses.into_iter().collect();
    }

    /// `core.appendResponses(responses)`：追加到队列尾部，不改变已有条目的顺序。
    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        lock(&self.pending_responses).extend(responses);
    }

    /// `core.getPendingResponseCount()`。
    pub fn get_pending_response_count(&self) -> usize {
        lock(&self.pending_responses).len()
    }

    // 同步阶段：取脚本、计数、派发后台任务，然后立刻把事件流交给调用方。
    //
    // 脚本在 spawn **之前**弹出，因此并发调用时「谁拿到哪条响应」由弹出时的加锁顺序而非
    // 任务调度决定（单线程内即调用顺序）；`spawn_forward` 在已有 Tokio runtime 时直接
    // spawn，否则新建专用线程跑 current-thread runtime（见 `api::lazy`），所以本函数
    // 不要求调用方处于异步上下文。
    fn start_stream(
        &self,
        request_model: &Model,
        context: &TranscriptContext,
        stream_options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        let step = lock(&self.pending_responses).pop_front();
        self.state.increment_call_count();
        let core = self.clone();
        let task_stream = outer.clone();
        let task_model = request_model.clone();
        let task_context = context.clone();
        spawn_forward(async move {
            core.stream_task(task_stream, task_model, task_context, stream_options, step)
                .await;
        });
        outer
    }

    // 后台任务入口：内部错误统一在这里转成终态 error 事件与终止消息。
    //
    // 这一步是必要的「错误边界」：spawn 出去的任务没有调用方可以接收 `Result`，若不在此
    // 收口，流会永远不结束（消费者一直等下一个事件）。
    async fn stream_task(
        self,
        outer: AssistantMessageEventStream,
        request_model: Model,
        context: TranscriptContext,
        stream_options: Option<SimpleStreamOptions>,
        step: Option<FauxResponseStep>,
    ) {
        if let Err(error) = self
            .stream_task_inner(&outer, &request_model, &context, stream_options, step)
            .await
        {
            let message =
                create_error_message(&error, &self.api, &self.provider, &request_model.id);
            let _ = outer.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: Box::new(message.clone()),
            });
            outer.end(message);
        }
    }

    #[allow(clippy::result_large_err)]
    async fn stream_task_inner(
        &self,
        outer: &AssistantMessageEventStream,
        request_model: &Model,
        context: &TranscriptContext,
        stream_options: Option<SimpleStreamOptions>,
        step: Option<FauxResponseStep>,
    ) -> Result<(), ThrownValue> {
        // 与真实 provider 一样，先回调 onResponse（faux 永远给伪 200 响应）；它发生在
        // 判断队列之前，所以「队列为空」的错误请求同样会触发回调。
        if let Some(on_response) = stream_options
            .as_ref()
            .and_then(|options| options.base.on_response.clone())
        {
            on_response(
                ProviderResponse {
                    status: 200,
                    headers: Default::default(),
                },
                request_model,
            );
        }

        // 队列为空：构造错误消息，但也要补 usage（错误请求同样消耗提示词 token）。
        let Some(step) = step else {
            let mut message = create_error_message(
                &ThrownValue::from_text("No more faux responses queued"),
                &self.api,
                &self.provider,
                &request_model.id,
            );
            message = with_usage_estimate(self, message, context, stream_options.as_ref());
            let _ = outer.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: Box::new(message.clone()),
            });
            outer.end(message);
            return Ok(());
        };

        let signal = stream_options
            .as_ref()
            .and_then(|options| options.base.signal.clone());

        // 延迟提交路径：脚本先存进条目表，本次只流出「占位」消息（stopReason = deferred）。
        // 注意 `deferred: false` 走普通路径，但工厂仍能在 options 里看到它
        // （由 `deferred_is_truthy` 表达 JS 的真值判断，Window 形态恒为真）。
        if stream_options
            .as_ref()
            .and_then(|options| options.deferred.as_ref())
            .is_some_and(deferred_is_truthy)
        {
            // handle 的 provider/model/api 取自请求模型；expiresAt/data 不设置。
            let handle = DeferredHandle {
                provider: request_model.provider.clone(),
                model_id: request_model.id.clone(),
                api: request_model.api.clone(),
                id: random_id("deferred"),
                expires_at: None,
                poll_after_ms: self.deferred.poll_after_ms,
                data: None,
            };
            // TS 在兑换前解构掉 deferred/signal/onResponse；这里预先剥离并保存其余选项，
            // 供首次兑换时交给工厂。onPayload 等载体保留。
            let submission_options = stream_options.map(|mut options| {
                options.deferred = None;
                options.base.signal = None;
                options.base.on_response = None;
                Arc::new(options)
            });
            let entry = FauxDeferredEntry {
                handle: handle.clone(),
                step,
                context: context.clone(),
                options: submission_options,
                model: request_model.clone(),
                pending_fetches: pending_fetch_count(self.deferred.pending_fetches),
                cancelled: false,
                final_message: None,
            };
            lock(&self.deferred_responses).insert(handle.id.clone(), entry);
            // 占位消息 content 为空、stopReason=deferred；仍走完整事件流，
            // 调用方会收到 start → done(deferred)。
            let message = create_deferred_message(request_model, handle);
            stream_with_deltas(
                outer,
                &message,
                self.min_token_size,
                self.max_token_size,
                self.tokens_per_second,
                signal.as_ref(),
            )
            .await?;
            return Ok(());
        }

        // 普通路径：执行脚本 → 补齐 usage → 切块流式输出。
        // 工厂返回的 Err 用 `?` 直接冒泡到 stream_task 的错误边界。
        let message = self
            .resolve_response(step, context, stream_options.as_ref(), request_model)
            .await?;
        stream_with_deltas(
            outer,
            &message,
            self.min_token_size,
            self.max_token_size,
            self.tokens_per_second,
            signal.as_ref(),
        )
        .await?;
        Ok(())
    }

    // 把一条脚本解析成最终的助手消息。
    //
    // 静态消息与工厂结果走同一条收尾路径：先重写 api/provider/model（防止脚本里写死的
    // 占位值泄漏），再无条件用估算值覆盖 usage（工厂不能自定义 usage）。工厂在这里被
    // 调用，因此它看到的 `state` 已经包含本次请求的 callCount。
    #[allow(clippy::result_large_err)]
    async fn resolve_response(
        &self,
        step: FauxResponseStep,
        context: &TranscriptContext,
        stream_options: Option<&SimpleStreamOptions>,
        request_model: &Model,
    ) -> Result<AssistantMessage, ThrownValue> {
        let resolved = match step {
            FauxResponseStep::Message(message) => message,
            FauxResponseStep::Factory(factory) => {
                factory(FauxResponseInput {
                    context: context.clone(),
                    options: stream_options.map(FauxStreamOptions::from_simple),
                    state: self.state.clone(),
                    model: request_model.clone(),
                })
                .await?
            }
        };
        let cloned = clone_message(resolved, &self.api, &self.provider, &request_model.id);
        Ok(with_usage_estimate(self, cloned, context, stream_options))
    }

    // 同步阶段：计数 + 派发后台任务，立刻返回事件流。
    //
    // `deferred_fetch_count` 在调用瞬间自增，与流是否已经产出结果无关，测试可以据此
    // 断言轮询次数；脚本条目本身只在任务真正执行时才被检查。
    fn start_fetch_deferred(
        &self,
        request_model: &Model,
        handle: &DeferredHandle,
        fetch_options: DeferredFetchOptions,
    ) -> AssistantMessageEventStream {
        let outer = create_assistant_message_event_stream();
        self.state.increment_deferred_fetch_count();
        let core = self.clone();
        let task_stream = outer.clone();
        let task_model = request_model.clone();
        let task_handle = handle.clone();
        spawn_forward(async move {
            core.fetch_deferred_task(task_stream, task_model, task_handle, fetch_options)
                .await;
        });
        outer
    }

    // 兑换任务的错误边界：任何 Err 都变成终态 error 事件，保证流一定结束。
    async fn fetch_deferred_task(
        self,
        outer: AssistantMessageEventStream,
        request_model: Model,
        handle: DeferredHandle,
        fetch_options: DeferredFetchOptions,
    ) {
        if let Err(error) = self
            .fetch_deferred_inner(&outer, &request_model, &handle, &fetch_options)
            .await
        {
            let message =
                create_error_message(&error, &self.api, &self.provider, &request_model.id);
            let _ = outer.push(AssistantMessageEvent::Error {
                reason: ErrorReason::Error,
                error: Box::new(message.clone()),
            });
            outer.end(message);
        }
    }

    #[allow(clippy::result_large_err)]
    async fn fetch_deferred_inner(
        &self,
        outer: &AssistantMessageEventStream,
        request_model: &Model,
        handle: &DeferredHandle,
        fetch_options: &DeferredFetchOptions,
    ) -> Result<(), ThrownValue> {
        if let Some(on_response) = fetch_options.base.on_response.clone() {
            on_response(
                ProviderResponse {
                    status: 200,
                    headers: Default::default(),
                },
                request_model,
            );
        }

        // 锁内只做状态判断并把后续动作提炼成 `FetchPlan`，锁在 match 前已经释放。
        // 这样工厂调用与流式输出（都可能耗时不定的 await）不会持锁，避免同一 provider 的
        // 其他轮询被阻塞。
        #[allow(clippy::large_enum_variant)]
        enum FetchPlan {
            /// 仍在 `pendingFetches` 次数内：继续返回占位 handle。
            Pending(DeferredHandle),
            /// 已有缓存的最终消息：直接重放，不重新执行脚本。
            Final(AssistantMessage),
            /// 首次兑换：执行保存的脚本。
            Resolve {
                step: FauxResponseStep,
                context: TranscriptContext,
                options: Option<Arc<SimpleStreamOptions>>,
                model: Model,
            },
        }

        let plan = {
            let mut responses = lock(&self.deferred_responses);
            // 先按 id 找条目；再校验 provider/model/api 三元组，防止把发给其他
            // provider 或模型的 handle 拿来兑换（TS 同样比较这三个字段）。
            let Some(entry) = responses.get_mut(&handle.id) else {
                return Err(unknown_deferred(handle));
            };
            if entry.handle.provider != handle.provider
                || entry.handle.model_id != handle.model_id
                || entry.handle.api != handle.api
            {
                return Err(unknown_deferred(handle));
            }
            // 取消优先于 pending/缓存：取消后即使已有最终消息也不再返回。
            if entry.cancelled {
                return Err(ThrownValue::from_text(format!(
                    "Faux deferred response was cancelled: {}",
                    handle.id
                )));
            }
            if entry.pending_fetches > 0 {
                entry.pending_fetches -= 1;
                FetchPlan::Pending(entry.handle.clone())
            } else if let Some(final_message) = entry.final_message.clone() {
                FetchPlan::Final(final_message)
            } else {
                FetchPlan::Resolve {
                    step: entry.step.clone(),
                    context: entry.context.clone(),
                    options: entry.options.clone(),
                    model: entry.model.clone(),
                }
            }
        };

        match plan {
            FetchPlan::Pending(pending_handle) => {
                let message = create_deferred_message(request_model, pending_handle);
                stream_with_deltas(
                    outer,
                    &message,
                    self.min_token_size,
                    self.max_token_size,
                    self.tokens_per_second,
                    fetch_options.base.signal.as_ref(),
                )
                .await?;
            }
            FetchPlan::Final(message) => {
                stream_with_deltas(
                    outer,
                    &message,
                    self.min_token_size,
                    self.max_token_size,
                    self.tokens_per_second,
                    fetch_options.base.signal.as_ref(),
                )
                .await?;
            }
            FetchPlan::Resolve {
                step,
                context,
                options,
                model,
            } => {
                // 首次兑换：工厂错误也会被固化成错误消息并写回条目，因此顺序轮询时后续请求
                // 重放同一结果（不会在「先失败后成功」间摇摆）。但锁不跨 `.await`，并发的
                // 首次兑换可能都进入本分支、各执行一次脚本；条目以最后写入的
                // `final_message` 为准，各请求的流仍按自己算出的消息输出。
                let final_message = match self
                    .resolve_response(step, &context, options.as_deref(), &model)
                    .await
                {
                    Ok(message) => message,
                    Err(error) => {
                        create_error_message(&error, &self.api, &self.provider, &model.id)
                    }
                };
                // 写缓存前重新加锁；若条目在此期间被删除则静默跳过，本条流仍按已算出的
                // 消息输出。
                if let Some(entry) = lock(&self.deferred_responses).get_mut(&handle.id) {
                    entry.final_message = Some(final_message.clone());
                }
                stream_with_deltas(
                    outer,
                    &final_message,
                    self.min_token_size,
                    self.max_token_size,
                    self.tokens_per_second,
                    fetch_options.base.signal.as_ref(),
                )
                .await?;
            }
        }
        Ok(())
    }

    // 取消延迟响应：先记录历史，再置位条目，最后回调 onResponse。
    //
    // 顺序固定：即使 handle 不存在也照样记入 `cancelledDeferred`（只追加），
    // 因而本方法永远不会失败；未知 handle 只在之后兑换时报错。
    async fn cancel_deferred_impl(
        &self,
        request_model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) {
        self.state.push_cancelled(handle.clone());
        if let Some(entry) = lock(&self.deferred_responses).get_mut(&handle.id) {
            entry.cancelled = true;
        }
        if let Some(on_response) = options.on_response.clone() {
            on_response(
                ProviderResponse {
                    status: 200,
                    headers: Default::default(),
                },
                request_model,
            );
        }
    }
}

/// 未知 handle 的统一错误（id 不存在或 provider/model/api 不匹配）。
fn unknown_deferred(handle: &DeferredHandle) -> ThrownValue {
    ThrownValue::from_text(format!("Unknown faux deferred response: {}", handle.id))
}

/// TS `if (streamOptions?.deferred)` 的真值判断。
///
/// `false` 走普通流；`true` 与任意 `{ window }` 对象都走延迟提交（`window` 的取值不影响
/// faux 的模拟行为）。
fn deferred_is_truthy(deferred: &DeferredRequest) -> bool {
    match deferred {
        DeferredRequest::Enabled(enabled) => *enabled,
        DeferredRequest::Window { .. } => true,
    }
}

/// `Math.max(0, Math.floor(pendingFetches ?? 0))`，NaN 也归 0。
fn pending_fetch_count(pending_fetches: Option<f64>) -> u64 {
    let value = pending_fetches.unwrap_or(0.0);
    if value.is_nan() {
        return 0;
    }
    value.max(0.0).floor() as u64
}

// faux 总是声明支持延迟接口：`has_*` 全为 true，因而 `create_provider` 会把 provider 级
// 能力标记为「支持」；具体调用仍由上方的方法实现。
#[async_trait]
impl ProviderStreams for FauxCore {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // TS 的 `stream` 与 `streamSimple` 是同一个函数；`StreamOptions` 只是
        // `SimpleStreamOptions` 的子集（没有 `deferred`）。
        self.start_stream(
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
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.start_stream(model, context, Some(options))
    }

    fn has_fetch_deferred(&self) -> bool {
        true
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        Some(self.start_fetch_deferred(model, handle, options))
    }

    fn has_cancel_deferred(&self) -> bool {
        true
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Option<Result<(), AiError>> {
        self.cancel_deferred_impl(model, handle, options).await;
        Some(Ok(()))
    }
}

/// `createFauxCore(options)`：构造脚本化 provider 核心，不注册任何全局状态。
///
/// - `api` 缺省是**随机 id**（`faux:<毫秒>:<base36>`）；`provider` 缺省才是
///   `"faux"`。因此依赖固定 api 名的代码应显式传入 `api`。
/// - 未给 `models`（或给空数组）时生成默认模型 `faux-1`：text+image、128k 上下文、
///   16k 输出、零成本。
/// - `token_size` 会被夹到合法区间：`min = max(1, min(min, max))`，
///   `max = max(min, max)`；min 大于 max 时两者都退化为同一个值。
///
/// 返回值可直接 `Arc` 包装成 [`ProviderStreams`]，也可用同一组
/// [`RegisterFauxProviderOptions`] 调用 `compat` 的 `register_faux_provider` 注册；该函数
/// 会自行构造 core，不会复用本函数的返回值。
pub fn create_faux_core(options: RegisterFauxProviderOptions) -> FauxCore {
    // api 缺省不是常量：TS 用 `randomId("faux")`，因此同进程里多个 faux core 会得到
    // 不同的 api 名；需要固定名字的调用方必须显式传入。
    let api = options
        .api
        .clone()
        .unwrap_or_else(|| random_id(DEFAULT_API));
    let provider = options
        .provider
        .clone()
        .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());

    let requested_min = options
        .token_size
        .as_ref()
        .and_then(|size| size.min)
        .unwrap_or(DEFAULT_MIN_TOKEN_SIZE);
    let requested_max = options
        .token_size
        .as_ref()
        .and_then(|size| size.max)
        .unwrap_or(DEFAULT_MAX_TOKEN_SIZE);
    // 夹取顺序保证 `1 <= min <= max`：先让 min 不超过 max，再至少为 1；
    // 最后让 max 不小于 min（max 为 0 等异常输入也会被抬到 1）。
    let min_token_size = requested_min.min(requested_max).max(1);
    let max_token_size = requested_max.max(min_token_size);

    // 没有声明模型时给一个可用的默认模型，避免调用方必须自己构造 Model。
    let model_definitions = match options.models {
        Some(models) if !models.is_empty() => models,
        _ => vec![FauxModelDefinition {
            id: DEFAULT_MODEL_ID.into(),
            name: Some(DEFAULT_MODEL_NAME.into()),
            reasoning: Some(false),
            input: Some(vec![ModelInputModality::Text, ModelInputModality::Image]),
            input_limits: None,
            cost: Some(ModelCost::default()),
            context_window: Some(128_000),
            max_tokens: Some(16_384),
        }],
    };

    let models: Vec<Model> = model_definitions
        .into_iter()
        .map(|definition| Model {
            id: definition.id.clone(),
            name: definition.name.unwrap_or_else(|| definition.id.clone()),
            api: api.clone(),
            provider: provider.clone(),
            base_url: DEFAULT_BASE_URL.into(),
            input: definition
                .input
                .unwrap_or_else(|| vec![ModelInputModality::Text, ModelInputModality::Image]),
            input_limits: definition.input_limits,
            cost: definition.cost.unwrap_or_default(),
            model_type: None,
            reasoning: Some(definition.reasoning.unwrap_or(false)),
            context_window: Some(definition.context_window.unwrap_or(128_000)),
            max_tokens: Some(definition.max_tokens.unwrap_or(16_384)),
            ..Default::default()
        })
        .collect();

    FauxCore {
        api,
        provider,
        models: Arc::new(models),
        pending_responses: Arc::new(Mutex::new(VecDeque::new())),
        state: FauxProviderState::default(),
        prompt_cache: Arc::new(Mutex::new(HashMap::new())),
        deferred_responses: Arc::new(Mutex::new(HashMap::new())),
        min_token_size,
        max_token_size,
        tokens_per_second: options.tokens_per_second,
        deferred: options.deferred.unwrap_or_default(),
    }
}

// faux 的鉴权实现：永远成功且不产生凭据。
//
// `faux_provider_with` 把它放进 `ProviderAuth` 的 api_key 槽位（名字为 `Faux`）；
// `resolve` 恒返回只含空 `ModelAuth` 的 `AuthResult`，与 TS 的 `createProvider` 配置一致。
// faux 本身不读环境变量、不发请求，因此没有可解析的凭据。
struct FauxApiKeyAuth;

#[async_trait]
impl ApiKeyAuth for FauxApiKeyAuth {
    fn name(&self) -> &str {
        "Faux"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}

/// `FauxProviderHandle`：`fauxProvider()` 的返回值。
///
/// `provider` 是包装后的 [`Provider`]（可交给模型注册表/会话层使用）；`core` 保留脚本
/// 控制面（排队、计数、延迟响应）。两者共享同一份状态，因此通过 handle 改脚本会立即
/// 影响已注册的 provider。
#[derive(Clone)]
pub struct FauxProviderHandle {
    /// `provider`（已用 `createProvider` 包装）。
    pub provider: Arc<dyn Provider>,
    core: FauxCore,
}

impl FauxProviderHandle {
    /// `handle.api`。
    pub fn api(&self) -> &str {
        self.core.api()
    }

    /// `handle.models`。
    pub fn models(&self) -> &[Model] {
        self.core.models()
    }

    /// `handle.getModel(id?)`。
    pub fn get_model(&self, requested_model_id: Option<&str>) -> Option<&Model> {
        self.core.get_model(requested_model_id)
    }

    /// `handle.state`。
    pub fn state(&self) -> &FauxProviderState {
        self.core.state()
    }

    /// `handle.setResponses(responses)`。
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.set_responses(responses);
    }

    /// `handle.appendResponses(responses)`。
    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.append_responses(responses);
    }

    /// `handle.getPendingResponseCount()`。
    pub fn get_pending_response_count(&self) -> usize {
        self.core.get_pending_response_count()
    }

    /// 供 `compat.ts` 的 `registerFauxProvider` 复用核心。
    pub fn core(&self) -> &FauxCore {
        &self.core
    }
}

/// `fauxProvider(options = {})`：用全默认选项构造一个 faux provider。
///
/// 默认 api 是随机 id（不是 `"faux"`），与 `create_faux_core` 的规则一致；需要固定 api
/// 名时用 [`faux_provider_with`]。
pub fn faux_provider() -> FauxProviderHandle {
    faux_provider_with(RegisterFauxProviderOptions::default())
}

/// `fauxProvider(options)`：按选项构造核心并经 `create_provider` 包装成 [`Provider`]。
///
/// `create_provider` 要求至少提供一种实现（stream/images/classifiers 之一）；这里固定
/// 传入 `api`，所以错误分支不可能触发，`expect` 仅作为不变量断言。
pub fn faux_provider_with(options: RegisterFauxProviderOptions) -> FauxProviderHandle {
    let core = create_faux_core(options);
    let streams: Arc<dyn ProviderStreams> = Arc::new(core.clone());
    let provider = create_provider(CreateProviderOptions {
        id: core.provider().to_string(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(FauxApiKeyAuth)),
            oauth: None,
        },
        models: core.models().iter().cloned().map(AnyModel::Chat).collect(),
        api: Some(streams.into()),
        ..Default::default()
    })
    .expect("faux provider declares an api implementation");
    FauxProviderHandle { provider, core }
}

// ---------------------------------------------------------------------------
// 内部工具（逐条对应 TS 私有函数）
// ---------------------------------------------------------------------------

/// `Math.ceil(text.length / 4)`：按 UTF-16 码元数估算 token。
///
/// 用 UTF-16 而不是字节/字符数，是为了与 JS 的 `String.length` 对齐（emoji 等非 BMP 字符
/// 计 2）。这只是估算，不是真实分词器。
fn estimate_tokens(text: &str) -> u64 {
    (utf16_len(text) as u64).div_ceil(4)
}

/// JS `String.length`：UTF-16 码元个数。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `Math.random()` 的替代实现：线程本地 xorshift64 + 53 位尾数，返回 `[0, 1)`。
///
/// 每个线程独立一条序列，不保证跨线程/跨进程可重复；这不是密码学随机数，只用于生成 id
/// 与切块大小，测试不应依赖具体取值。种子见 `initial_random_seed`。
fn random_f64() -> f64 {
    thread_local! {
        static STATE: std::cell::Cell<u64> = std::cell::Cell::new(initial_random_seed());
    }
    STATE.with(|state| {
        let mut value = state.get();
        // xorshift64
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        state.set(value);
        let mixed = value.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((mixed >> 11) as f64) / ((1u64 << 53) as f64)
    })
}

/// 混入时间、线程 id 与栈地址作为 xorshift 种子；`| 1` 保证状态非零
/// （xorshift64 的 0 是吸收态，会一直输出 0）。
fn initial_random_seed() -> u64 {
    use std::hash::{Hash, Hasher};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    let thread_hash = hasher.finish();
    let stack_address = &nanos as *const u64 as u64;
    (nanos ^ thread_hash.rotate_left(17) ^ stack_address.rotate_left(31)) | 1
}

/// `` `${prefix}:${Date.now()}:${Math.random().toString(36).slice(2)}` `` 的等价物。
///
/// 逐位取 base36 数字，最多 `RANDOM_ID_DIGITS` 位；随机值提前归零时字符串会更短，
/// 与 JS 一致。id 由毫秒时间戳与伪随机数拼成，不保证绝对唯一，不能当作持久标识。
fn random_id(prefix: &str) -> String {
    let mut value = random_f64();
    let mut digits = String::with_capacity(RANDOM_ID_DIGITS);
    for _ in 0..RANDOM_ID_DIGITS {
        value *= 36.0;
        let digit = value.floor() as u32;
        value -= f64::from(digit);
        digits.push(std::char::from_digit(digit, 36).unwrap_or('0'));
        if value == 0.0 {
            break;
        }
    }
    format!("{prefix}:{}:{digits}", now_unix_ms())
}

/// `contentToText`：文本原样；块数组逐块转换后用 `\n` 连接。
///
/// 该函数只服务于 token 估算，不参与请求构造，因此格式只需保证估算口径一致。
fn content_to_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(user_block_to_text)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// 用户消息内的一个块。
///
/// 图片转成 `[image:<mime>:<长度>]`；长度用 UTF-16 码元数，复刻 JS 的 `data.length`
/// （data 通常是 base64 ASCII，因此与字节数相同，但这里不假设编码）。
fn user_block_to_text(block: &UserContentBlock) -> String {
    match block {
        UserContentBlock::Text(text) => text.text.clone(),
        UserContentBlock::Image(image) => {
            format!("[image:{}:{}]", image.mime_type, utf16_len(&image.data))
        }
    }
}

/// `assistantContentToText`：text 取正文、thinking 取思考内容、
/// toolCall 取 `name:JSON.stringify(arguments)`，块之间用 `\n` 连接。
///
/// 序列化走 `kiana_json` 的 JS `JSON.stringify` 语义，保证 token 估算与 TS 同形。
fn assistant_content_to_text(content: &[AssistantContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(text) => text.text.clone(),
            AssistantContentBlock::Thinking(thinking) => thinking.thinking.clone(),
            AssistantContentBlock::ToolCall(tool_call) => format!(
                "{}:{}",
                tool_call.name,
                js_object_stringify(
                    &serde_json::to_value(&tool_call.arguments)
                        .expect("tool arguments serialize to JSON")
                )
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `toolResultToText`：工具名 + 各内容块的文本，`\n` 连接。
///
/// 复用 `content_to_text` 的单块形态，让图片也得到与用户消息一致的 `[image:...]` 占位。
fn tool_result_to_text(message: &ToolResultMessage) -> String {
    let mut parts = vec![message.tool_name.clone()];
    for block in &message.content {
        parts.push(content_to_text(&UserContent::Blocks(vec![block.clone()])));
    }
    parts.join("\n")
}

/// `messageToText`：把任意一条 transcript 消息渲染成用于 token 估算的文本。
///
/// - system：`get_system_message_text`，再为 `toolsRemoved` 追加 `tool-:<JSON>`、为
///   `toolsAdded` 追加 `tool+:<JSON>`，过滤空串后 `\n` 连接。
/// - user：`contentToText`。
/// - assistant：`assistantContentToText`。
/// - toolResult：`toolResultToText`（首行是工具名）。
fn message_to_text(message: &crate::types::Message) -> String {
    use crate::types::Message;
    match message {
        Message::System(message) => {
            let mut parts = vec![get_system_message_text(message)];
            if let Some(tools_removed) = &message.tools_removed {
                for tool in tools_removed {
                    parts.push(format!("tool-:{}", tool_reference_json(tool)));
                }
            }
            if let Some(tools_added) = &message.tools_added {
                for tool in tools_added {
                    parts.push(format!("tool+:{}", tool_json(tool)));
                }
            }
            parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        }
        Message::User(message) => content_to_text(&message.content),
        Message::Assistant(message) => assistant_content_to_text(&message.content),
        Message::ToolResult(message) => tool_result_to_text(message),
    }
}

/// 工具引用（被移除的工具）的 `JSON.stringify` 文本。
fn tool_reference_json(tool: &ToolReference) -> String {
    js_object_stringify(
        &serde_json::to_value(tool).expect("ToolReference serializes to a JSON value"),
    )
}

/// 工具声明的 `JSON.stringify` 文本。
fn tool_json(tool: &crate::types::Tool) -> String {
    js_object_stringify(&serde_json::to_value(tool).expect("Tool serializes to a JSON value"))
}

/// JS `JSON.stringify` 的等价实现：键顺序、转义与数字格式由 `kiana_json` 保证与 V8 一致。
fn js_object_stringify(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}

/// `joinedLength(messages, count)`：前 `count` 条用 `"\n\n"` 连接后的 UTF-16 长度。
///
/// 即各条长度之和加 `(count - 1) * 2` 个分隔符；`count > messages.len()` 时仍按 `count`
/// 计入分隔符（调用方只传合法值）。
fn joined_length(messages: &[String], count: usize) -> usize {
    let mut length = if count > 0 { (count - 1) * 2 } else { 0 };
    for message in messages.iter().take(count) {
        length += utf16_len(message);
    }
    length
}

/// 两个 prompt（每条消息一段文本）按 `"\n\n"` 连接后的公共前缀长度（UTF-16 码元），
/// 用于模拟 prompt cache 的命中量。
///
/// 先整条跳过完全相同的消息，再比较剩余部分：`rest` 重新拼出后缀在完整字符串里的样子，
/// 并在 `index > 0` 时补上被跳过消息与剩余部分之间的 `"\n\n"` 分隔符。因此结果与直接对
/// `join("\n\n")` 的两个字符串求公共前缀等价，但不必重复比较相同的前缀消息。
///
/// 比较单位是 Rust `char`（Unicode 标量值）：逐字符比较，并用 `len_utf16` 折算码元；对
/// 合法字符串这与按 UTF-16 码元比较等价（spec/ai-core.md §2.3.13）。Rust 字符串不存在
/// 孤立代理项，因此代理对不会被从中间切开。
fn common_prompt_prefix_length(previous: &[String], current: &[String]) -> usize {
    let mut index = 0;
    while index < previous.len() && index < current.len() && previous[index] == current[index] {
        index += 1;
    }
    let rest = |messages: &[String]| -> String {
        if index == messages.len() {
            String::new()
        } else {
            let mut text = String::new();
            if index > 0 {
                text.push_str("\n\n");
            }
            text.push_str(&messages[index..].join("\n\n"));
            text
        }
    };
    joined_length(previous, index) + common_prefix_length(&rest(previous), &rest(current))
}

/// 两段文本的公共前缀长度，按 UTF-16 码元计（emoji 等字符计 2）。
fn common_prefix_length(a: &str, b: &str) -> usize {
    let mut units = 0;
    for (left, right) in a.chars().zip(b.chars()) {
        if left != right {
            break;
        }
        units += left.len_utf16();
    }
    units
}

/// `withUsageEstimate`：用估算值覆盖消息的 usage，并可选地模拟 prompt cache。
///
/// 估算口径：
/// - prompt 每条消息渲染成 `"<role>:<text>"`，整体用 `"\n\n"` 连接；
///   `systemPrompt`/`tools` 已在上下文归一化时折叠进首条 system 消息。
/// - `promptTokens = ceil(UTF-16 长度 / 4)`；输出 token 同样按
///   `assistantContentToText`（text/thinking/toolCall 的 JSON 串）估算。
///
/// cache 模拟只在 `session_id` 非空且 `cacheRetention` 不为 `CacheRetention::None` 时启用
/// （未设置也算启用）：
/// - 该 session 首次出现：`cacheWrite = promptTokens`；
/// - 之后与上一次 prompt 求公共前缀，`cacheRead = ceil(前缀长度 / 4)`，
///   `cacheWrite = promptTokens - cacheRead`；
/// - 启用时 `input` 固定为 0（缓存读 + 写已覆盖全部 prompt token）。
///
/// 无论是否命中缓存，`totalTokens = promptTokens + outputTokens` 保持不变；cost 全为 0，
/// 因为 faux 不模拟计费。消息按值传入、按值返回，调用方原先持有的那条已被移动。
fn with_usage_estimate(
    core: &FauxCore,
    mut message: AssistantMessage,
    context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
) -> AssistantMessage {
    // 每条消息一段文本；整个 prompt 用空行连接。
    let prompt: Vec<String> = context
        .as_context()
        .messages
        .iter()
        .map(|message| format!("{}:{}", message.role(), message_to_text(message)))
        .collect();
    let prompt_length = joined_length(&prompt, prompt.len());
    let prompt_tokens = (prompt_length as u64).div_ceil(4);
    let output_tokens = estimate_tokens(&assistant_content_to_text(&message.content));
    let mut input = prompt_tokens;
    let mut cache_read = 0;
    let mut cache_write = 0;

    // 空 sessionId 在 JS 里是假值；只有显式 `cacheRetention: "none"` 才关闭缓存模拟，
    // 未设置（None）视为允许缓存。
    let session_id = options
        .and_then(|options| options.base.session_id.as_deref())
        .filter(|session_id| !session_id.is_empty());
    let cache_enabled =
        options.and_then(|options| options.base.cache_retention) != Some(CacheRetention::None);

    if let Some(session_id) = session_id
        && cache_enabled
    {
        // 缓存表按 sessionId 保存「上一次的 prompt 文本」；本次覆盖旧值，因此只能命中
        // 与紧邻上一次请求的公共前缀，而不是任意历史版本。
        let mut prompt_cache = lock(&core.prompt_cache);
        if let Some(previous_prompt) = prompt_cache.get(session_id) {
            let cached_chars = common_prompt_prefix_length(previous_prompt, &prompt);
            cache_read = (cached_chars as u64).div_ceil(4);
            // 余下 token 都算写入；避免前缀估算因取整出现 cacheRead > promptTokens。
            cache_write = prompt_tokens.saturating_sub(cache_read);
        } else {
            cache_write = prompt_tokens;
        }
        // 每个 prompt token 要么从模拟缓存读取，要么写入缓存。
        input = 0;
        prompt_cache.insert(session_id.to_string(), prompt);
    }

    // 覆盖而不是累加：脚本消息里携带的 usage 是占位值，工厂也无法自定义。
    message.usage = Usage {
        input,
        output: output_tokens,
        cache_read,
        cache_write,
        cache_write1h: None,
        reasoning: None,
        total_tokens: input + output_tokens + cache_read + cache_write,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
            total_from_provider: None,
        },
    };
    message
}

/// TS `splitStringByTokenSize`：按 UTF-16 码元长度切块。
///
/// TS 的 `text.slice(index, index + charSize)` 可能从代理对中间切开，孤立的半个代理项在下一个
/// chunk 里重新拼成原字符；Rust 的 `String` 不能保存孤立代理项，所以切点落在代理对中间时把该
/// 字符整体并入当前 chunk。与 TS 相比，每次这类切分让下一个 chunk 的起点后移一个码元（偏移会
/// 累积，chunk 数也可能不同）；拼接结果与原文一致，不会出现 `from_utf16_lossy` 的 U+FFFD。
fn split_string_by_token_size(text: &str, min: usize, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        // tokenSize ∈ [min, max]（span 至少为 1；clamp 防止随机数落在边界外）。
        let span = max.saturating_sub(min).saturating_add(1);
        let token_size = min.saturating_add(
            (random_f64() * span as f64)
                .floor()
                .max(0.0)
                .min(span.saturating_sub(1) as f64) as usize,
        );
        // 一个「token」按 4 个 UTF-16 码元折算，和 estimate_tokens 的口径一致。
        // 饱和乘法：`token_size` 来自可配置的 `FauxTokenSize`，接近 `usize::MAX` 时普通
        // `* 4` 在 debug 构建 panic、release 构建回绕成小值（退化成逐字符 delta）。
        let char_size = token_size.saturating_mul(4).max(1);
        // 按字符边界收集，直到累计码元数达到 char_size。检查在累加之前，因此跨过阈值
        // 的那个字符（可能是代理对）会整体留在当前 chunk，长度最多超出 1 个码元。
        let mut units = 0;
        let mut end = 0;
        for (offset, ch) in rest.char_indices() {
            if units >= char_size {
                break;
            }
            units += ch.len_utf16();
            end = offset + ch.len_utf8();
        }
        // char_size >= 1 且非空字符串至少有一个字符，所以 end > 0，循环必然前进。
        chunks.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    // TS 对空字符串返回 `[""]`，调用方因此总能拿到至少一个 delta。
    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

/// `cloneMessage`：保留脚本消息的内容与时间戳，只把 api/provider/model 改成本次请求的值。
///
/// TS 用 `structuredClone` 后覆盖字段；Rust 里直接按值移动，等价且不做深拷贝。
fn clone_message(
    message: AssistantMessage,
    api: &str,
    provider: &str,
    model_id: &str,
) -> AssistantMessage {
    AssistantMessage {
        api: api.to_string(),
        provider: provider.to_string(),
        model: model_id.to_string(),
        ..message
    }
}

/// `createDeferredMessage`：延迟提交/轮询时返回的占位消息（content 为空、stopReason=deferred）。
fn create_deferred_message(model: &Model, handle: DeferredHandle) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: default_usage(),
        stop_reason: StopReason::Deferred,
        deferred: Some(handle),
        timestamp: now_unix_ms(),
        ..Default::default()
    }
}

/// `error instanceof Error ? error.message : String(error)`。
fn thrown_message(error: &ThrownValue) -> String {
    match error {
        ThrownValue::Error(info) => info.message.clone(),
        ThrownValue::Text(text) => text.clone(),
        ThrownValue::Json(value) => kiana_json::js_string::js_string(Some(value)),
    }
}

/// `createErrorMessage`：把内部错误转成 `stopReason = error` 的助手消息。
///
/// usage 先留全零；只有「队列为空」路径会再用 `with_usage_estimate` 覆盖，其余错误路径
/// 直接以全零 usage 结束。
fn create_error_message(
    error: &ThrownValue,
    api: &str,
    provider: &str,
    model_id: &str,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: api.to_string(),
        provider: provider.to_string(),
        model: model_id.to_string(),
        usage: default_usage(),
        stop_reason: StopReason::Error,
        error_message: Some(thrown_message(error)),
        timestamp: now_unix_ms(),
        ..Default::default()
    }
}

/// `createAbortedMessage`：复制已产出的 partial，改写终止原因、错误文本与时间戳。
///
/// 内容与 usage 都保留，因此调用方能看到「取消前已经流出多少」。
fn create_aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    AssistantMessage {
        stop_reason: StopReason::Aborted,
        error_message: Some("Request was aborted".into()),
        timestamp: now_unix_ms(),
        ..partial.clone()
    }
}

/// `signal?.aborted ?? false`。
fn signal_aborted(signal: Option<&CancellationToken>) -> bool {
    signal.is_some_and(CancellationToken::is_cancelled)
}

/// `scheduleChunk`：按 tokens/秒 的速率睡眠，或在不限速时仅让出执行权。
///
/// `tokens_per_second` 为 `None`、0、负数或 NaN 时都不睡眠（`> 0.0` 为假），改用
/// `yield_now`，保证同一 runtime 上的其他任务有机会推进。限速时每块的延迟按该块自身的
/// 估算 token 数计算，因此总时长约等于 `总token / tps` 秒。
async fn schedule_chunk(chunk: &str, tokens_per_second: Option<f64>) {
    match tokens_per_second {
        Some(tokens_per_second) if tokens_per_second > 0.0 => {
            let delay_ms = (estimate_tokens(chunk) as f64 / tokens_per_second) * 1000.0;
            // 极小的 tps（如 1e-300）会让除法溢出成 `inf`，`Duration::from_secs_f64`
            // 会 panic；超长但有限的延迟也可能在 `Instant::now() + duration` 时溢出。
            // 这类退化配置统一按一小时封顶：既不会 panic，也能在暂停时钟的测试里推进。
            const MAX_DELAY: Duration = Duration::from_secs(60 * 60);
            let delay = Duration::try_from_secs_f64(delay_ms / 1000.0)
                .unwrap_or(MAX_DELAY)
                .min(MAX_DELAY);
            tokio::time::sleep(delay).await;
        }
        _ => tokio::task::yield_now().await,
    }
}

/// `partial: {...partial}`：事件快照。
fn partial_event(partial: &AssistantMessage) -> Box<AssistantMessage> {
    Box::new(partial.clone())
}

/// 取消路径的统一收尾：发一个 `error(aborted)` 事件，再以同样的 aborted 消息 `end` 流。
///
/// 取消在内部不返回 `Err`；事件层仍以 `error(reason = aborted)` 通知调用方，
/// 事件流的 `end` 结果也是 aborted 消息。
fn push_aborted(stream: &AssistantMessageEventStream, partial: &AssistantMessage) {
    let aborted = create_aborted_message(partial);
    let _ = stream.push(AssistantMessageEvent::Error {
        reason: ErrorReason::Aborted,
        error: Box::new(aborted.clone()),
    });
    stream.end(aborted);
}

/// `streamWithDeltas(...)`：把一条完整的助手消息拆成与真实 provider 相同的事件序列。
///
/// 状态机：
/// 1. 进入时检查取消；已取消则只发 `error(aborted)` 并结束。
/// 2. 发 `start`（partial 内容为空、stopReason=pending）。
/// 3. 按 `message.content` 顺序逐块输出：先向 partial 追加空壳块并发出 `*_start`，再按
///    `split_string_by_token_size` 的切块逐段追加、发出 `*_delta`，最后发 `*_end`。
///    toolCall 的 partial arguments 额外经过 `ToolCallArgumentsStream` 解析。
/// 4. 终止：
///    - stopReason=pending → 返回 `Err("Faux response ended without a stop reason")`，
///      由调用方的错误边界转成 `error` 事件；
///    - error/aborted → 发 `error` 事件并 `end`；
///    - stop/length/tool_use/deferred → 发 `done` 事件并 `end`。
///
/// 取消检查点：进入函数时、每个内容块开始时、每个 chunk 的 `schedule_chunk` 之后。
/// 取消时 partial 保留已追加的内容，但不包含当时正在等待的那一块。
///
/// 每个事件携带的 `partial` 都是发送时刻的深拷贝；TS 的浅拷贝会让事后回看的旧事件显示
/// 最终内容（见模块文档的差异说明）。
#[allow(clippy::result_large_err)]
async fn stream_with_deltas(
    stream: &AssistantMessageEventStream,
    message: &AssistantMessage,
    min_token_size: usize,
    max_token_size: usize,
    tokens_per_second: Option<f64>,
    signal: Option<&CancellationToken>,
) -> Result<(), ThrownValue> {
    // partial 是「到目前为止已经发出」的累积消息；复制元数据但清空内容、置为 pending。
    let mut partial = AssistantMessage {
        content: Vec::new(),
        stop_reason: StopReason::Pending,
        ..message.clone()
    };
    if signal_aborted(signal) {
        push_aborted(stream, &partial);
        return Ok(());
    }

    let _ = stream.push(AssistantMessageEvent::Start {
        partial: partial_event(&partial),
    });

    // content_index 直接使用 message.content 下标，与事件契约一致。
    for (index, block) in message.content.iter().enumerate() {
        if signal_aborted(signal) {
            push_aborted(stream, &partial);
            return Ok(());
        }

        match block {
            AssistantContentBlock::Thinking(thinking) => {
                // 先追加空壳块，之后的 delta 只改字段；partial 的块顺序与 message 一致。
                partial
                    .content
                    .push(AssistantContentBlock::Thinking(ThinkingContent {
                        kind_thinking: ThinkingKind::Thinking,
                        thinking: String::new(),
                        thinking_signature: None,
                        redacted: None,
                    }));
                let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                    content_index: index,
                    partial: partial_event(&partial),
                });
                // 每个 chunk 先按 tps 等待，再检查取消：等待期间被取消时不再发出这一块。
                for chunk in
                    split_string_by_token_size(&thinking.thinking, min_token_size, max_token_size)
                {
                    schedule_chunk(&chunk, tokens_per_second).await;
                    if signal_aborted(signal) {
                        push_aborted(stream, &partial);
                        return Ok(());
                    }
                    if let Some(AssistantContentBlock::Thinking(current)) =
                        partial.content.get_mut(index)
                    {
                        current.thinking.push_str(&chunk);
                    }
                    let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial_event(&partial),
                    });
                }
                let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking.thinking.clone(),
                    partial: partial_event(&partial),
                });
            }
            AssistantContentBlock::Text(text) => {
                // 与 thinking 相同：空壳块 + start/delta/end；end 事件带完整文本。
                partial
                    .content
                    .push(AssistantContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: String::new(),
                        text_signature: None,
                    }));
                let _ = stream.push(AssistantMessageEvent::TextStart {
                    content_index: index,
                    partial: partial_event(&partial),
                });
                for chunk in split_string_by_token_size(&text.text, min_token_size, max_token_size)
                {
                    schedule_chunk(&chunk, tokens_per_second).await;
                    if signal_aborted(signal) {
                        push_aborted(stream, &partial);
                        return Ok(());
                    }
                    if let Some(AssistantContentBlock::Text(current)) =
                        partial.content.get_mut(index)
                    {
                        current.text.push_str(&chunk);
                    }
                    let _ = stream.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial_event(&partial),
                    });
                }
                let _ = stream.push(AssistantMessageEvent::TextEnd {
                    content_index: index,
                    content: text.text.clone(),
                    partial: partial_event(&partial),
                });
            }
            AssistantContentBlock::ToolCall(tool_call) => {
                // arguments 先以空对象占位，随 delta 逐步替换为解析出的 partial 参数。
                partial
                    .content
                    .push(AssistantContentBlock::ToolCall(ToolCall {
                        kind_tool_call: ToolCallKind::ToolCall,
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: Default::default(),
                        thought_signature: None,
                        namespace: None,
                        arguments_error: None,
                    }));
                let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                    content_index: index,
                    partial: partial_event(&partial),
                });
                // partial arguments 由真实 provider 使用的同一个解析器产出：
                // 先把完整 arguments 序列化成 JS JSON 文本，再按 token 切块喂给解析器。
                let mut tool_arguments = ToolCallArgumentsStream::new();
                let argument_text = js_object_stringify(
                    &serde_json::to_value(&tool_call.arguments)
                        .expect("tool arguments serialize to JSON"),
                );
                for chunk in
                    split_string_by_token_size(&argument_text, min_token_size, max_token_size)
                {
                    schedule_chunk(&chunk, tokens_per_second).await;
                    if signal_aborted(signal) {
                        push_aborted(stream, &partial);
                        return Ok(());
                    }
                    let arguments = tool_arguments.append(&chunk);
                    if let Some(AssistantContentBlock::ToolCall(current)) =
                        partial.content.get_mut(index)
                    {
                        current.arguments = arguments;
                    }
                    let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial_event(&partial),
                    });
                }
                // 结束前换回原始 arguments 对象，不依赖解析器对完整文本的
                // 重建结果。
                if let Some(AssistantContentBlock::ToolCall(current)) =
                    partial.content.get_mut(index)
                {
                    current.arguments = tool_call.arguments.clone();
                }
                let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                    content_index: index,
                    tool_call: tool_call.clone(),
                    partial: partial_event(&partial),
                });
            }
        }
    }

    // 内容已全部发出后才校验终止原因：pending 表示脚本忘记设置 stopReason，属于脚本错误
    // （此时 start 与各 delta 已经发出，错误只能以 error 事件收尾）。
    if message.stop_reason == StopReason::Pending {
        return Err(ThrownValue::from_text(
            "Faux response ended without a stop reason",
        ));
    }
    // 显式 error/aborted 消息：内容照常流出，最后再发终态 error 事件。
    if message.stop_reason == StopReason::Error || message.stop_reason == StopReason::Aborted {
        let reason = if message.stop_reason == StopReason::Aborted {
            ErrorReason::Aborted
        } else {
            ErrorReason::Error
        };
        let _ = stream.push(AssistantMessageEvent::Error {
            reason,
            error: Box::new(message.clone()),
        });
        stream.end(message.clone());
        return Ok(());
    }

    // 其余终止原因映射为 done 事件允许的 reason 子集；DoneReason 没有 Error/Aborted，
    // 所以上面必须先处理掉这两种。
    let reason = match message.stop_reason {
        StopReason::Stop => DoneReason::Stop,
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        // 本地预算 cap 不会由 provider 产生；若调用方手工构造了带 cap 的消息，`done`
        // 事件仍只能表达 `DoneReason` 的四个值，按“被上限截断”映射为 `Length`。
        StopReason::TurnCapped
        | StopReason::TokenCapped
        | StopReason::CostCapped
        | StopReason::TimeCapped => DoneReason::Length,
        StopReason::Pending | StopReason::Error | StopReason::Aborted => {
            // 上面已处理；保持穷尽匹配。
            unreachable!("terminal stop reason handled above")
        }
    };
    let _ = stream.push(AssistantMessageEvent::Done {
        reason,
        message: Box::new(message.clone()),
    });
    stream.end(message.clone());
    Ok(())
}

// 单元测试只锁定本文件的纯函数（token 估算、前缀、切块、id 形状）；完整事件序列、
// usage 与延迟语义由集成测试 `crates/kiana-ai/tests/faux_provider.rs` 覆盖。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_tokens_by_utf16_length() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        // 非 BMP 字符占两个 UTF-16 码元。
        assert_eq!(estimate_tokens("😀😀"), 1);
    }

    #[test]
    fn joins_prompt_text_with_blank_lines() {
        let messages = vec!["a".to_string(), "bcd".to_string()];
        assert_eq!(joined_length(&messages, 2), 2 + 1 + 3);
        assert_eq!(joined_length(&messages, 1), 1);
        assert_eq!(joined_length(&messages, 0), 0);
    }

    #[test]
    fn finds_common_prompt_prefix_across_messages() {
        let previous = vec!["user:hello".to_string(), "user:next".to_string()];
        let current = vec!["user:hello".to_string(), "user:next".to_string()];
        assert_eq!(
            common_prompt_prefix_length(&previous, &current),
            joined_length(&previous, previous.len())
        );
        let edited = vec!["user:hello wide".to_string(), "user:next".to_string()];
        assert_eq!(
            common_prompt_prefix_length(&previous, &edited),
            "user:hello".len()
        );
    }

    #[test]
    fn split_is_never_empty() {
        let chunks = split_string_by_token_size("", 1, 1);
        assert_eq!(chunks, vec![String::new()]);
    }

    #[test]
    fn split_chunks_never_corrupt_surrogate_pairs() {
        // min == max == 1 → charSize == 4 UTF-16 码元。TS 的 slice 会从代理对中间切开，
        // 孤立代理项在下一个 chunk 里重新拼成原字符；Rust 侧切点必须落在字符边界上。
        let chunks = split_string_by_token_size("abc😀", 1, 1);
        assert_eq!(chunks, vec!["abc😀".to_string()]);

        let chunks = split_string_by_token_size("a😀bc😀😀", 1, 1);
        assert_eq!(
            chunks,
            vec!["a😀b".to_string(), "c😀😀".to_string()],
            "chunk 边界落在代理对中间时整对并入当前 chunk"
        );
        assert_eq!(chunks.concat(), "a😀bc😀😀");
        assert!(chunks.iter().all(|chunk| !chunk.contains('\u{FFFD}')));
    }

    #[test]
    fn split_handles_huge_token_sizes_without_overflow() {
        // 回归：`token_size * 4` 曾在 `FauxTokenSize` 接近 `usize::MAX` 时溢出
        // （debug panic / release 回绕成 1，退化为逐字符输出）。
        let chunks = split_string_by_token_size("abcdef", usize::MAX, usize::MAX);
        assert_eq!(chunks, vec!["abcdef".to_string()]);
    }

    #[test]
    fn random_id_keeps_ts_shape() {
        let id = random_id("deferred");
        let parts: Vec<&str> = id.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "deferred");
        assert!(parts[1].parse::<i64>().is_ok());
        assert!(!parts[2].is_empty());
    }
}
