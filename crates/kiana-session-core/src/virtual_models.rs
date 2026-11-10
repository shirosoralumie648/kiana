//! 虚拟模型：目录条目把每个请求路由到物理模型。
//!
//! TS: `packages/coding-agent/src/core/virtual-models.ts`。
//!
//! 选择层（`model_change`、`agent.state.model`、`ctx.model`）可以命名虚拟模型；路由之后
//! 的一切只看到物理模型。虚拟模型永不到达 provider。
//!
//! Rust 侧的 `route` 是 `Arc<dyn Fn(ModelRouteRequest) -> RouteFuture>`（返回 `'static`
//! future，便于 `ModelRuntime` 持有 `Arc<Self>`）；路由状态用 [`kiana_ai::types::JsonValue`]
//! 承载（TS 是泛型 `TState`）。
//!
//! # 这个文件解决什么问题
//!
//! 目录里可以出现一种「不存在的模型」：它的 `api` 是
//! [`crate::virtual_models::VIRTUAL_MODEL_API`]，没有真实端点。它本身不能发请求——
//! 两个 provider 包装器的 `stream` 遇到它都会返回 [`crate::virtual_models::unrouted_stream`]
//! 的错误流。要让请求真正完成，调用方必须在发请求之前经 `ModelRuntime::resolve_model`
//! 调用注册的 `route` 回调，把虚拟条目换成物理模型（`AgentSession` 在 `prepare_request`
//! 里这样做，`ModelRuntime::stream_simple` 的虚拟分支也这样做）。这样「当前选哪个模型」
//! 就可以依赖本次对话内容，而不是在写 `model_change` 时固定死。
//!
//! # 在系统中的位置
//!
//! - `kiana-config::models::runtime`：生产/消费本模块的主要调用方。`register_virtual_model`
//!   用 [`crate::virtual_models::create_virtual_model`] 生成目录条目；`resolve_model`
//!   组装 [`crate::virtual_models::ModelRouteRequest`]、调用 `route` 并校验返回值；
//!   `recompose_provider` 用 [`crate::virtual_models::with_virtual_models`] 把虚拟条目
//!   并进 provider 目录。
//! - `kiana-session` 的 `AgentSession`：`prepare_request` 用
//!   [`crate::virtual_models::get_virtual_model_state`] 读取本分支保存的路由状态，把
//!   返回的新状态写成 [`crate::virtual_models::VIRTUAL_MODEL_STATE_ENTRY`] 自定义条目；
//!   每次 run 开始时 `record_selection` 用 [`crate::virtual_models::get_branch_selection`]
//!   读取分支隐含的选择，必要时把当前选择 append 成 `model_change`；`create_agent_session`
//!   在恢复已有会话且未显式指定模型时也用它得到要还原的模型。
//! - `kiana-extensions` 的 loader：把 JS `registerVirtualModel(definition)` 转成
//!   [`crate::virtual_models::VirtualModelDefinition`]，并把 JS 回调包装成
//!   [`crate::virtual_models::RouteFuture`]。
//! - `kiana-config::models::virtual_models` 与 `kiana-coding-agent` 的 `sdk_exports`：
//!   只做再导出，没有第二份实现。
//!
//! # 一次请求的路由流程
//!
//! 1. `ModelRuntime::resolve_model` 从注册表取出虚拟条目与 `route` 回调。
//! 2. 它用 [`crate::virtual_models::find_latest_response`] 找到最近的成功 assistant
//!    消息，再用目录把消息里的 provider/model 投影成 `previous`；`retry` 时同样投影
//!    失败消息得到 `failed`。
//! 3. 回调返回 [`crate::virtual_models::ModelRoute`]：目标物理模型、thinking level 与
//!    可选的新状态。
//! 4. 调用方校验目标必须是已注册的**物理**模型且其 provider 有凭证，然后把 thinking
//!    level 钳制到目标模型支持的档位。
//! 5. 若 `route.state` 为 `Some` 且与请求里的旧 `state` 不同，`AgentSession` 追加一条
//!    `kiana.virtual-model-state` 条目；回调返回 `None` 表示保持现状，即使旧状态是
//!    `Some` 也不会追加。下一次请求会通过 `get_virtual_model_state` 读回来。
//!
//! # 注意事项
//!
//! - [`crate::virtual_models::is_virtual_model`] 只比较 `Model::api`，与 `provider`
//!   字段无关；任何 `api` 恰为 `"kiana-virtual"` 的条目都会被当成虚拟模型。
//! - 本文件只提供纯函数和两个 provider 包装器：不校验 provider/id 冲突、不校验路由
//!   结果是否为物理模型、不持久化状态。这些工作分别在 `ModelRuntime` 与 `AgentSession`
//!   里完成。
//! - [`crate::virtual_models::VirtualModelDefinition`] 的 `route` 是 `Option`：
//!   `create_virtual_model` 不需要它；注册后真正路由时缺失会由 `ModelRuntime` 报错
//!   （`has no route.`）。
//! - 路由状态必须是 JSON 值；TS 的泛型 `TState` 在这里统一成 `Option<JsonValue>`。

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use kiana_agent_core::AgentMessage;
use kiana_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ProviderAuth};
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{ClassifierOptions, ImagesOptions, Provider, RefreshModelsContext};
use kiana_ai::types::{
    AnyModel, AssistantImages, AssistantMessage, ClassifierContext, ClassifierResult,
    DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImagesContext, JsonValue, Message,
    Model, ModelInputModality, ModelThinkingLevel, SimpleStreamOptions, StopReason, StreamOptions,
    ThinkingLevelMap, TranscriptContext,
};
use kiana_ai::utils::abort::AbortScope;

use crate::messages::assistant_message_from_object;
use crate::session_manager::{ModelRef, SessionEntry};

/// 虚拟目录条目的 API id。未路由前对它的请求会失败。
///
/// 该值会写进虚拟条目的 `Model::api`，是 [`is_virtual_model`] 的唯一判据。物理模型的
/// `api` 是具体适配器名（如 `"anthropic-messages"`、`"openai-completions"`），因此这个
/// 字面量同时充当「未路由」哨兵：两个包装 provider 的 `stream`/`stream_simple` 遇到它
/// 都返回 [`unrouted_stream`]，而不是转发给真实 provider。
///
/// TS: `virtual-models.ts#VIRTUAL_MODEL_API`
pub const VIRTUAL_MODEL_API: &str = "kiana-virtual";

/// 在会话分支上保存路由器状态的自定义条目类型。
///
/// 条目由 `SessionManager::append_custom_entry` 写入，`CustomEntry::data` 是
/// [`VirtualModelStateData`] 的 JSON（camelCase）。读取方是
/// [`get_virtual_model_state`]：它从分支末尾向前扫描，只认 `custom_type` 逐字等于本
/// 常量的条目，并且要求条目里的 provider/model id 与查询目标一致。
///
/// TS: `virtual-models.ts#VIRTUAL_MODEL_STATE_ENTRY`
pub const VIRTUAL_MODEL_STATE_ENTRY: &str = "kiana.virtual-model-state";

/// `THINKING_LEVELS`（含 `"off"`，顺序固定）。
///
/// 该数组同时决定 [`create_virtual_model`] 生成的 `thinking_level_map` 的键顺序：七个
/// 档位**全部**出现在 map 里，不支持的档位映射为 `None`（序列化成 `null`），而不是缺键。
/// 下游 `clamp_thinking_level` 通过这张表得出模型实际支持的档位；写满七个键可以避免
/// 触发「缺键默认可用」的回退规则（只有 `xhigh`/`max` 缺键会被视为禁用）。
///
/// TS: `virtual-models.ts#THINKING_LEVELS`
pub const THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// `kiana.virtual-model-state` 条目的数据。
///
/// 序列化为 camelCase：`provider`、`modelId`、`state`。只有 `state` 带
/// `#[serde(default)]`，缺键按 `None` 解析；`provider` / `model_id` 没有默认值，缺失或
/// 类型不符会让整条数据解析失败，[`get_virtual_model_state`] 会静默跳过该条目。
/// `state` 没有 `skip_serializing_if`，`None` 会写成 `"state":null`；读取时 `null` 与
/// 缺键都得到 `None`。
///
/// TS: `virtual-models.ts#VirtualModelStateData`
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualModelStateData {
    /// provider id。
    pub provider: String,
    /// model id。
    pub model_id: String,
    /// 路由器状态（缺失为 `None`）。
    ///
    /// 内容对 kiana-session-core 不透明：生产者与消费者都是路由回调和
    /// `AgentSession`，这里只负责 JSON 往返。
    #[serde(default)]
    pub state: Option<JsonValue>,
}

/// 请求被路由的原因。
///
/// 回调用它区分请求来源。`AgentSession::prepare_request` 的判定顺序是：有失败消息 →
/// `Retry`；否则最近一条 assistant 消息之后出现过 user 消息 → `User`；否则
/// `Continuation`。`Models::stream_simple` 与压缩摘要的模型解析固定用 `Direct`。
/// 序列化为小写字符串（`#[serde(rename_all = "lowercase")]`），与 TS 的字符串字面量一致。
///
/// TS: `virtual-models.ts#ModelRouteReason`
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelRouteReason {
    /// 用户写下消息后的首个请求。
    User,
    /// agent 循环中的其它请求（工具结果/扩展消息之后）。
    Continuation,
    /// 失败请求后的自动重试。
    Retry,
    /// agent 循环之外的请求（压缩摘要、扩展调用）。
    Direct,
}

/// 最新的成功响应（物理模型 + thinking level）。
///
/// 由 `ModelRuntime::resolve_model` 用 [`find_latest_response`] 加目录查询构造：消息里的
/// provider/model 必须能解析成**物理**模型，否则 `previous` 为 `None`（解析失败按「没有
/// 上一次响应」处理，而不是报错）。
///
/// TS: `ModelRouteRequest["previous"]`
#[derive(Debug, Clone, PartialEq)]
pub struct RouteModelRef {
    /// 物理模型。
    pub model: Model,
    /// 该响应使用的 thinking level。
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// `retry` 的失败请求。
///
/// 失败消息由调用方传入，`ModelRuntime` 同样用目录投影：消息里的 provider/model 若是
/// 虚拟模型或已注销，投影失败，`failed` 为 `None`。
///
/// TS: `ModelRouteRequest["failed"]`
#[derive(Debug, Clone, PartialEq)]
pub struct RouteFailure {
    /// 失败请求的物理模型。
    pub model: Model,
    /// 失败请求的 thinking level。
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 失败消息（带 `stopReason` 与 `errorMessage`）。
    pub message: AssistantMessage,
}

/// 一次路由请求。
///
/// 一次 `route` 调用的全部输入，按值传给回调（回调可以自由持有或克隆）。`messages` 是
/// 面向 LLM 的 [`Message`] 投影，不是会话条目（`SessionEntry`）。
///
/// TS: `virtual-models.ts#ModelRouteRequest`
#[derive(Debug, Clone)]
pub struct ModelRouteRequest {
    /// 选中的虚拟模型。
    ///
    /// 是目录条目本身（`api == VIRTUAL_MODEL_API`），不是路由结果。
    pub model: Model,
    /// 选中的 thinking level。
    pub thinking_level: ModelThinkingLevel,
    /// 路由原因。
    pub reason: ModelRouteReason,
    /// `messages` 中最新成功响应的物理模型与 thinking level。
    pub previous: Option<RouteModelRef>,
    /// `retry` 时的失败请求。
    pub failed: Option<RouteFailure>,
    /// 本会话分支上最近返回的路由状态。
    ///
    /// 值来自 [`get_virtual_model_state`]；`Direct` 调用（`Models::stream_simple` 与压缩
    /// 摘要）通常传 `None`，也不回写结果。
    pub state: Option<JsonValue>,
    /// 本次请求的对话（含 system 消息）。
    pub messages: Vec<Message>,
    /// 取消信号。
    ///
    /// 调用方把 `CancellationToken` 转成 `AbortScope` 后传入；`None` 表示没有取消信号。
    pub signal: Option<AbortScope>,
}

/// 一次路由结果。
///
/// 返回后 `ModelRuntime::resolve_model` 会验证并规整它：`model` 必须解析为已注册的
/// **物理**模型（虚拟模型不算），其 provider 必须有凭证，否则整个解析返回错误；
/// `thinking_level` 会被 `clamp_thinking_level` 调整到目标模型实际支持的档位。因此回调
/// 给出的档位只是「期望值」。
///
/// TS: `virtual-models.ts#ModelRoute`
#[derive(Debug, Clone)]
pub struct ModelRoute {
    /// 物理模型。
    pub model: Model,
    /// thinking level。
    pub thinking_level: ModelThinkingLevel,
    /// 新路由状态（`None`/与 `request.state` 相同表示保持现状）。
    ///
    /// 只有 `Some` 且与请求里的 `state` 不同时，`AgentSession` 才会追加一条新的状态
    /// 条目；`Direct` 调用不读也不写状态。
    pub state: Option<JsonValue>,
}

/// 路由失败（TS 里 `route` 抛出的任意错误）。
///
/// `name` 目前只是信息字段：`ModelRuntime` 只把 `message` 转成自己的错误类型，`name`
/// 不会出现在上层错误里。
#[derive(Debug, Clone, PartialEq, Eq)]
/// TS: virtual-models.ts#VirtualModelRouteError
pub struct VirtualModelRouteError {
    /// `error.name`。
    pub name: String,
    /// `error.message`。
    pub message: String,
}

impl VirtualModelRouteError {
    /// 用消息构造（name 为 `"Error"`）。
    /// TS: virtual-models.ts#new
    pub fn new(message: impl Into<String>) -> Self {
        VirtualModelRouteError {
            name: "Error".to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for VirtualModelRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VirtualModelRouteError {}

/// 路由 future。
///
/// 用 `Box<dyn Future + Send>` 而不是泛型或 `impl Future`：`VirtualModelDefinition::route`
/// 要存进 `Arc<dyn Fn ...>` 并跨 crate 传递，必须是对象安全的。只要求 `Send`（不要求
/// `Sync`），因为 future 只会被单个任务轮询；`Box<dyn ...>` 的默认生命周期是 `'static`，
/// 所以回调不能借用调用栈上的临时数据。错误类型固定为 [`VirtualModelRouteError`]。
///
/// TS: virtual-models.ts#RouteFuture
pub type RouteFuture =
    Pin<Box<dyn Future<Output = Result<ModelRoute, VirtualModelRouteError>> + Send>>;

/// 虚拟模型定义。
///
/// 这是**注册输入**，不是目录条目：目录条目由 [`create_virtual_model`] 投影得到。
/// `route` 为 `None` 在类型上合法，`create_virtual_model` 也照常成功；只有真正发起路由
/// 时 `ModelRuntime::resolve_model` 才会报 `has no route.`。`id` 与同 provider 的物理
/// 模型重名的检查同样在 `ModelRuntime::register_virtual_model` 里（`conflicts with a
/// physical model.`），本文件不做校验。
///
/// TS: `virtual-models.ts#VirtualModelDefinition`（`route` 在 Rust 里是 `Option`，便于
/// [`create_virtual_model`] 接受 TS 的 `Omit<VirtualModelDefinition, "route">` 形状）。
#[derive(Clone)]
pub struct VirtualModelDefinition {
    /// 列在该 provider 下（可以是已有物理模型的 provider）。
    pub provider: String,
    /// model id；不得与该 provider 的物理模型 id 相同。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 可供选择的 thinking level；默认 `["off"]`。
    pub thinking_levels: Option<Vec<ModelThinkingLevel>>,
    /// 首个响应前展示的上下文上限；未设为 0（未知）。
    pub context_window: Option<u64>,
    /// 首个响应前展示的输出上限；未设为 0。
    pub max_tokens: Option<u64>,
    /// 可接受的输入类型；默认 text+image。
    pub input: Option<Vec<ModelInputModality>>,
    /// 选择物理模型与 thinking level。
    ///
    /// 类型是 `Fn`（不是 `FnMut`）且要求 `Send + Sync`：`ModelRuntime` 会把 `Arc` 克隆
    /// 出来在锁外 `await`，所以实现不能假定自己只被顺序调用；需要可变状态时要自行同步。
    /// `None` 表示该定义只用于目录展示，真正路由时会失败。
    pub route: Option<Arc<dyn Fn(ModelRouteRequest) -> RouteFuture + Send + Sync>>,
}

impl std::fmt::Debug for VirtualModelDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualModelDefinition")
            .field("provider", &self.provider)
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// `isVirtualModel(model)`。
///
/// 判据只有 `model.api == "kiana-virtual"`，不看 provider 或 id；因此虚拟模型可以挂在
/// 已有物理 provider 名下。分支选择、provider 包装器的流分派和 `AgentSession` 的
/// 「当前选择是否虚拟」判断都依赖这个谓词。
///
/// TS: `virtual-models.ts#isVirtualModel`
pub fn is_virtual_model(model: &Model) -> bool {
    model.api == VIRTUAL_MODEL_API
}

/// 最新成功响应（跳过 error/aborted）；其模型是物理模型。
///
/// 最新的成功响应。
///
/// TS 只按 `role === "assistant"` 与 `stopReason` 过滤，并原样返回消息对象；Rust 的返回类型
/// 是 typed [`AssistantMessage`]，因此宽松解析成 `AgentMessage::Custom` 的 assistant 消息也
/// 会用 `assistant_message_from_object` 投影后返回（`stopReason` 缺省视为 `Pending`，即
/// 不是 error/aborted）。
///
/// 扫描从末尾向前进行，命中第一条就返回它的**克隆**（调用方拿不到原始对象）。函数本身
/// 不检查 `api`：所谓「其模型是物理模型」是调用方的推论——虚拟模型的流必然以 error
/// 结束，因此不会成为成功响应；若真出现 `stopReason` 非 error/aborted 的虚拟消息，
/// 本函数同样会返回它。
///
/// TS: `virtual-models.ts#findLatestResponse`
pub fn find_latest_response(messages: &[AgentMessage]) -> Option<AssistantMessage> {
    for message in messages.iter().rev() {
        if message.role() != "assistant" {
            continue;
        }
        let assistant = match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => assistant.clone(),
            AgentMessage::Custom(object) => assistant_message_from_object(object),
            _ => continue,
        };
        if !matches!(
            assistant.stop_reason,
            StopReason::Error | StopReason::Aborted
        ) {
            return Some(assistant);
        }
    }
    None
}

/// 会话分支记录的模型选择。
///
/// 虚拟 `model_change` 保持到下一个 `model_change`；否则最新物理响应胜出。已注销的虚拟模型
/// 不再保持。整个函数**最多一次**目录查询。
///
/// `branch` 是 root→leaf 顺序（`SessionManager::get_branch` 保证），函数从末尾向前扫描：
///
/// 1. 遇到 `model_change`：立即返回它，不论目标是否还能在目录里解析（那是调用方的事）。
/// 2. 遇到物理 assistant 消息：先把它记为最新响应候选，再向前找最近的 `model_change`；
///    只有该 change 能解析且解析结果是**虚拟模型**时才返回 change，否则返回该响应。
/// 3. 其它条目跳过。
///
/// 因此 `get_model` 只在第 2 步、且候选之前确实存在 `model_change` 时被调用一次；
/// tests/virtual_models.rs 的 `looks_up_only_the_last_model_change` 锁定了「最多一次」。
///
/// TS: `virtual-models.ts#getBranchSelection`
pub fn get_branch_selection(
    branch: &[SessionEntry],
    get_model: &dyn Fn(&str, &str) -> Option<Model>,
) -> Option<ModelRef> {
    // 从末尾（最新）向 root 扫描；分支顺序由 SessionManager::get_branch 保证。
    for index in (0..branch.len()).rev() {
        let entry = &branch[index];
        match entry {
            SessionEntry::ModelChange(model_change) => {
                // 最新的 model_change 优先于任何响应：即便它指向的虚拟模型已注销，
                // 也原样返回，由调用方决定能否解析。
                return Some(ModelRef {
                    provider: model_change.provider.clone(),
                    model_id: model_change.model_id.clone(),
                });
            }
            SessionEntry::Message(message_entry) => {
                // TS 判断 `entry.message.role === "assistant"` 并直接读取 `api` /
                // `provider` / `model`；宽松解析成 `AgentMessage::Custom` 的消息从原始对象读取。
                let Some((api, provider, model_id)) = assistant_identity(&message_entry.message)
                else {
                    continue;
                };
                if api == VIRTUAL_MODEL_API {
                    // 虚拟模型产生的消息不代表一次物理响应，直接跳过（与 TS 的
                    // `!isVirtualModel(entry.message)` 对应）。
                    continue;
                }
                let response = ModelRef { provider, model_id };
                // 只可能查询一次：只有这里会调用目录回调。失败/注销（None）同样回退。
                let change = find_last_model_change(branch, index);
                let model = change
                    .as_ref()
                    .and_then(|change| get_model(&change.provider, &change.model_id));
                return match (change, model) {
                    (Some(change), Some(model)) if is_virtual_model(&model) => Some(change),
                    _ => Some(response),
                };
            }
            _ => {}
        }
    }
    None
}

/// assistant 消息的 `{api, provider, model}`（Custom 视图从原始对象读取）。
///
/// 非 assistant 消息返回 `None`。Custom 视图用 `json_field_string` 读取：缺失的
/// `api`/`provider`/`model` 得到空串而不是 `None`，本函数不据此过滤消息。
fn assistant_identity(message: &AgentMessage) -> Option<(String, String, String)> {
    if message.role() != "assistant" {
        return None;
    }
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => Some((
            assistant.api.clone(),
            assistant.provider.clone(),
            assistant.model.clone(),
        )),
        AgentMessage::Custom(object) => Some((
            crate::session_manager::json_field_string(object, "api"),
            crate::session_manager::json_field_string(object, "provider"),
            crate::session_manager::json_field_string(object, "model"),
        )),
        _ => None,
    }
}

/// 在 `before` 之前反向找最近的 `model_change`。
///
/// 只扫描下标小于 `before` 的条目（不含 `before` 自身），不访问目录；注册状态由调用方
/// 用 `get_model` 单独判断。
fn find_last_model_change(branch: &[SessionEntry], before: usize) -> Option<ModelRef> {
    for index in (0..before).rev() {
        if let SessionEntry::ModelChange(model_change) = &branch[index] {
            return Some(ModelRef {
                provider: model_change.provider.clone(),
                model_id: model_change.model_id.clone(),
            });
        }
    }
    None
}

/// 会话分支为某虚拟模型保存的最新路由器状态。
///
/// 从分支末尾向前找匹配的 `custom` 条目：`custom_type` 必须逐字等于
/// [`VIRTUAL_MODEL_STATE_ENTRY`]，`data` 必须能解析成 [`VirtualModelStateData`]，且其中的
/// provider/model id 与参数逐字相等。`custom_type` 不符、`data` 缺失或解析失败（例如缺
/// `provider` 键、`modelId` 不是字符串）都会被**静默跳过**，不产生错误。
///
/// 返回的是 `data.state` 的克隆：条目存在但状态为 `null` 时同样得到 `None`，调用方无法
/// 区分「没有条目」与「条目里没有状态」。
///
/// TS: `virtual-models.ts#getVirtualModelState`
pub fn get_virtual_model_state(
    branch: &[SessionEntry],
    provider: &str,
    model_id: &str,
) -> Option<JsonValue> {
    for entry in branch.iter().rev() {
        let SessionEntry::Custom(custom) = entry else {
            continue;
        };
        if custom.custom_type != VIRTUAL_MODEL_STATE_ENTRY {
            continue;
        }
        let Some(data) = &custom.data else {
            continue;
        };
        // 结构不符的条目直接跳过，不让单条坏数据阻断整个分支的状态读取。
        let Ok(data) = serde_json::from_value::<VirtualModelStateData>(data.clone()) else {
            continue;
        };
        if data.provider == provider && data.model_id == model_id {
            return data.state;
        }
    }
    None
}

/// 构建虚拟模型的目录条目。
///
/// 纯投影函数：读取定义里的展示字段，**忽略** `route`（回调由 `ModelRuntime` 单独保存），
/// 不访问目录、不校验 id 冲突。产物是普通 [`Model`]，只靠 `api == VIRTUAL_MODEL_API` 与
/// 物理模型区分。
///
/// 默认值是当前稳定契约（tests/virtual_models.rs 锁定）：`thinking_levels` 缺省为 `["off"]`；
/// `context_window`/`max_tokens` 缺省为 `0`（表示未知，不是「无限制」）；`input` 缺省为
/// text+image；`cost` 全 0；`base_url` 为空串；`model_type` 为 `None`（即 chat）。
///
/// TS: `virtual-models.ts#createVirtualModel`
pub fn create_virtual_model(definition: &VirtualModelDefinition) -> Model {
    // 未声明时只暴露 "off"（TS 的 `?? ["off"]`）。
    let levels = definition
        .thinking_levels
        .clone()
        .unwrap_or_else(|| vec![ModelThinkingLevel::Off]);
    // 七个档位全部写入；不支持的档位是显式 None（序列化为 null），键顺序固定为
    // THINKING_LEVELS 顺序。下游 clamp_thinking_level 读这张表判断可用档位。
    let mut thinking_level_map: ThinkingLevelMap = ThinkingLevelMap::new();
    for level in THINKING_LEVELS {
        thinking_level_map.insert(
            level,
            levels
                .contains(&level)
                .then(|| thinking_level_string(level)),
        );
    }
    Model {
        id: definition.id.clone(),
        name: definition.name.clone(),
        api: VIRTUAL_MODEL_API.to_string(),
        provider: definition.provider.clone(),
        // 虚拟模型没有端点：它不该被用来发请求，流函数由包装 provider 拦截。
        base_url: String::new(),
        // 存在任一非 off 档位才算推理模型；适配器据此决定是否发送 thinking 参数。
        reasoning: Some(levels.iter().any(|level| *level != ModelThinkingLevel::Off)),
        thinking_level_map: Some(thinking_level_map),
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![ModelInputModality::Text, ModelInputModality::Image]),
        cost: kiana_ai::types::ModelCost::default(),
        // 0 表示未知（TS 的 `?? 0`），不是「无限制」；调用方（如 AgentSession 的压缩
        // 阈值判断）见到 0 会跳过依赖上下文窗口的逻辑。
        context_window: Some(definition.context_window.unwrap_or(0)),
        max_tokens: Some(definition.max_tokens.unwrap_or(0)),
        // None 即 chat（TS 省略 type 字段）；不写 headers/compat。
        model_type: None,
        ..Default::default()
    }
}

/// `ModelThinkingLevel` 的字符串字面量（与 serde 的 `lowercase` 一致）。
fn thinking_level_string(level: ModelThinkingLevel) -> String {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
    .to_string()
}

/// 未路由虚拟模型的流：`error` 事件带逐字消息。
///
/// 返回的流不通过 `Result` 报错：失败以流上的 assistant 消息呈现（`stopReason = Error`，
/// 并经 `result()` 取得）。错误消息逐字为
/// `Virtual model {provider}/{id} must be routed before streaming`；
/// tests/virtual_models.rs 断言了该文案与 `StopReason::Error`。
///
/// TS: `virtual-models.ts#unroutedStream`
pub fn unrouted_stream(model: &Model) -> AssistantMessageEventStream {
    let message = format!(
        "Virtual model {}/{} must be routed before streaming",
        model.provider, model.id
    );
    kiana_ai::api::lazy::lazy_stream_error(model, AiError::new(AiErrorKind::Other, message))
}

// ---------------------------------------------------------------------------
// 纯虚拟 provider
// ---------------------------------------------------------------------------

/// 纯虚拟 provider 的 api-key auth（`name: "Virtual model"`，`source: "virtual"`）。
///
/// `resolve` 不读取输入、也不访问环境：总是返回 `Some`，其中 `auth` 是空的
/// `ModelAuth`、`env` 为 `None`、`source` 为 `"virtual"`。因此 auth 解析对纯虚拟
/// provider 永远不会返回「没有凭证」，但它并不提供任何真实凭据——虚拟模型永远不会发
/// 请求。
#[derive(Debug, Default)]
/// TS: virtual-models.ts#VirtualModelAuth
pub struct VirtualModelAuth;

impl ApiKeyAuth for VirtualModelAuth {
    fn name(&self) -> &str {
        "Virtual model"
    }

    fn resolve<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _input: ApiKeyAuthInput<'life1>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<AuthResult>, AuthError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            // 忽略 _input：虚拟 provider 没有真实凭据，空 auth 与 "virtual" 来源只用于
            // 状态展示与「始终可解析」的语义。
            Ok(Some(AuthResult {
                auth: kiana_ai::auth::types::ModelAuth::default(),
                env: None,
                source: Some("virtual".to_string()),
            }))
        })
    }
}

/// 只列出虚拟模型的无凭据 provider。
///
/// 用于「该 provider 只有虚拟模型、没有物理内层」的情况。不覆盖 `get_all_models`，
/// 因此沿用 trait 默认实现（把 chat 目录包装成 `AnyModel::Chat`），`has_get_all_models`
/// 保持默认 `false`——对应 TS 纯虚拟 provider 没有 `getAllModels` 属性。没有
/// `base_url`/`headers`，`stream`/`stream_simple` 只返回未路由错误。
struct VirtualOnlyProvider {
    id: String,
    name: String,
    models: Vec<Model>,
    auth: ProviderAuth,
}

impl std::fmt::Debug for VirtualOnlyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualOnlyProvider")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Provider for VirtualOnlyProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        unrouted_stream(model)
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        unrouted_stream(model)
    }
}

// ---------------------------------------------------------------------------
// withVirtualModels
// ---------------------------------------------------------------------------

/// 把虚拟模型加到 provider 的目录上。
///
/// 无 provider 时返回只列虚拟模型的无凭据 provider。虚拟模型会隐藏同 id 的物理 chat 模型
/// （目录刷新可能在注册后加入）。可用性跟随 provider 的 auth。
///
/// 两种形态的细节：
///
/// - `provider == None`：返回 `VirtualOnlyProvider`，id/name 都等于 `provider_id`，
///   `base_url`/`headers` 为 `None`，auth 是 [`VirtualModelAuth`]，目录只有传入的
///   `virtual_models`。
/// - `provider == Some`：返回包装 provider。id/name/base_url/headers/auth 在**构造时**
///   从内层复制成 wrapper 自己的字段；内层之后再改变它们不会反映到 wrapper。刷新、
///   过滤、延迟、图像、分类等操作逐项委托给内层。
///
/// 目录合并的关键不变量：物理 chat 模型中与本包装注册的虚拟模型**同 id** 的条目被隐藏
/// （目录刷新可能在注册后加入），虚拟模型按传入顺序追加在物理条目之后。
/// `filter_models`/`filter_all_models` 只追加**输入里**的虚拟模型，且是在内层过滤之后
/// 追加，因此内层的凭证过滤无法删除它们。每次读取目录都会重新调用内层，但虚拟模型列表
/// 与 id 集合是 `with_virtual_models` 调用时的快照。
///
/// TS: `virtual-models.ts#withVirtualModels`
pub fn with_virtual_models(
    provider_id: &str,
    provider: Option<Arc<dyn Provider>>,
    virtual_models: Vec<Model>,
) -> Arc<dyn Provider> {
    let Some(provider) = provider else {
        // 没有物理内层：构造一个只列虚拟模型的 provider，用 VirtualModelAuth 让 auth
        // 解析始终成功（纯虚拟 provider 不需要真实凭据）。
        let auth = ProviderAuth {
            api_key: Some(Arc::new(VirtualModelAuth)),
            oauth: None,
        };
        return Arc::new(VirtualOnlyProvider {
            id: provider_id.to_string(),
            name: provider_id.to_string(),
            models: virtual_models,
            auth,
        });
    };
    // 同 id 集合用于隐藏目录刷新后可能出现的同名物理 chat 模型。
    let ids: HashSet<String> = virtual_models
        .iter()
        .map(|model| model.id.clone())
        .collect();
    Arc::new(WithVirtualModelsProvider {
        id: provider.id().to_string(),
        name: provider.name().to_string(),
        base_url: provider.base_url().map(str::to_string),
        headers: provider.headers().cloned(),
        auth: provider.auth().clone(),
        inner: provider,
        virtual_models,
        ids,
    })
}

/// 包装物理 provider 并在目录末尾追加虚拟模型。
///
/// 构造时缓存元信息与虚拟模型 id 集合。目录读取以 `inner` 的目录为基础：`get_models` /
/// `get_all_models` 先丢弃 `inner` 目录里的虚拟条目与同 id 物理 chat 模型，再按
/// `virtual_models` 的顺序追加本层的虚拟条目；`filter_models` / `filter_all_models` 先从
/// 输入里摘出虚拟条目，把物理条目交给 `inner` 过滤后再把虚拟条目原样追加；刷新、延迟、
/// 图像、分类等操作原样转发 `inner`；虚拟模型的 `stream` / `stream_simple` 由本层拦截，
/// 返回 [`unrouted_stream`] 错误流，不进入 `inner`。
/// `virtual_models` 的顺序就是目录里追加的顺序，`ids` 用于隐藏同 id 的物理 chat 模型。
struct WithVirtualModelsProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<kiana_ai::types::ProviderHeaders>,
    auth: ProviderAuth,
    inner: Arc<dyn Provider>,
    virtual_models: Vec<Model>,
    ids: HashSet<String>,
}

impl std::fmt::Debug for WithVirtualModelsProvider {
    // 手写 Debug：`Arc<dyn Provider>` 没有实现 Debug；这里只打印 id。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WithVirtualModelsProvider")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl WithVirtualModelsProvider {
    /// `physical(models)`：去掉虚拟模型与同 id 的物理 chat 模型。
    ///
    /// 输入是 chat 模型列表：`get_models` 传入内层目录，`filter_models` 传入调用方给的
    /// 列表（标准 `Models` 路径下传的是包装层自己的 `get_models()`，因此可能已经含有虚拟
    /// 条目）。先丢弃所有 `api == kiana-virtual` 的条目，再丢弃与本 wrapper 虚拟模型同 id
    /// 的条目；其余保持原顺序。
    fn physical_chat(&self, models: Vec<Model>) -> Vec<Model> {
        models
            .into_iter()
            .filter(|model| !is_virtual_model(model) && !self.ids.contains(&model.id))
            .collect()
    }

    /// `physical(models)`（任意模型类型）。
    ///
    /// 与 `physical_chat` 相同，但只有 `AnyModel::Chat` 变体按 id 隐藏；同 id 的
    /// Image/Classifier 条目保留（对应 TS 的 `isModelType(m, "chat") && ids.has(...)`）。
    fn physical_all(&self, models: Vec<AnyModel>) -> Vec<AnyModel> {
        models
            .into_iter()
            .filter(|entry| {
                let model = entry.model();
                let is_chat = matches!(entry, AnyModel::Chat(_));
                !is_virtual_model(model) && !(is_chat && self.ids.contains(&model.id))
            })
            .collect()
    }

    /// `virtual(models)`（任意模型类型）。
    ///
    /// 挑出输入中所有 `model().api == kiana-virtual` 的条目，**不只是**本 wrapper 注册的
    /// 那些；用于在过滤时把调用方传入的虚拟模型原样保留。
    fn virtual_all(models: Vec<AnyModel>) -> Vec<AnyModel> {
        models
            .into_iter()
            .filter(|model| is_virtual_model(model.model()))
            .collect()
    }
}

impl Provider for WithVirtualModelsProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn headers(&self) -> Option<&kiana_ai::types::ProviderHeaders> {
        self.headers.as_ref()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        // 每次都重新读内层目录（内层可能已刷新），再拼接注册顺序的虚拟模型。
        let mut models = self.physical_chat(self.inner.get_models());
        models.extend(self.virtual_models.iter().cloned());
        models
    }

    fn get_all_models(&self) -> Vec<AnyModel> {
        // 虚拟模型只可能是 chat，统一包成 AnyModel::Chat 追加在末尾。
        let mut models = self.physical_all(self.inner.get_all_models());
        models.extend(self.virtual_models.iter().cloned().map(AnyModel::Chat));
        models
    }

    fn has_get_all_models(&self) -> bool {
        // 转发内层的意图：`Models` 不读这个谓词，但外层包装器用它判断能否把
        // `get_all_models` 当成内层真正的实现。
        self.inner.has_get_all_models()
    }

    fn has_refresh_models(&self) -> bool {
        self.inner.has_refresh_models()
    }

    fn refresh_models<'life0, 'async_trait>(
        &'life0 self,
        context: RefreshModelsContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), AiError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        // TS 的对象展开会原样复制 `refreshModels`，所以这里委托；`has_refresh_models`
        // 也同步转发，避免包装层谎报刷新能力。
        Box::pin(async move { self.inner.refresh_models(context).await })
    }

    fn has_filter_models(&self) -> bool {
        self.inner.has_filter_models()
    }

    fn filter_models(
        &self,
        models: Vec<Model>,
        credential: Option<&kiana_ai::auth::types::Credential>,
    ) -> Vec<Model> {
        // 阶段 1：先把输入里的虚拟模型摘出来，稍后原样追加。
        let virtual_models: Vec<Model> = models
            .iter()
            .filter(|model| is_virtual_model(model))
            .cloned()
            .collect();
        // 阶段 2：去掉虚拟条目与同 id chat，交给内层按凭证过滤。
        let real = self.physical_chat(models);
        let mut filtered = self.inner.filter_models(real, credential);
        // 阶段 3：追加虚拟模型——它们不参与凭证过滤。
        filtered.extend(virtual_models);
        filtered
    }

    fn has_filter_all_models(&self) -> bool {
        self.inner.has_filter_all_models()
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&kiana_ai::auth::types::Credential>,
    ) -> Vec<AnyModel> {
        // 与 filter_models 同构：先摘虚拟条目，物理条目（含 Image/Classifier）交给内层，
        // 最后把虚拟条目原样追加。
        let virtual_models = Self::virtual_all(models.clone());
        let real = self.physical_all(models);
        let mut filtered = self.inner.filter_all_models(real, credential);
        filtered.extend(virtual_models);
        filtered
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // 走到这里仍是虚拟模型，说明路由没有发生：不委托内层，返回逐字错误流。
        if is_virtual_model(model) {
            unrouted_stream(model)
        } else {
            self.inner.stream(model, context, options)
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // 同 stream：虚拟模型一律返回未路由错误，物理模型委托内层。
        if is_virtual_model(model) {
            unrouted_stream(model)
        } else {
            self.inner.stream_simple(model, context, options)
        }
    }

    fn has_fetch_deferred(&self) -> bool {
        self.inner.has_fetch_deferred()
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        // 原样委托，不按模型类型分派：虚拟模型的流永远失败，不会产出 deferred 句柄。
        self.inner.fetch_deferred(model, handle, options)
    }

    fn has_cancel_deferred(&self) -> bool {
        self.inner.has_cancel_deferred()
    }

    fn cancel_deferred<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        model: &'life1 Model,
        handle: &'life2 DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Pin<Box<dyn Future<Output = Result<(), AiError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        // 原样委托（与 fetch_deferred 同理，TS 展开复制了 cancelDeferred）。
        Box::pin(async move { self.inner.cancel_deferred(model, handle, options).await })
    }

    // TS 用 `{...provider}` 展开，`generateImages` / `classify`（以及它们的
    // `has*` 谓词）会被原样复制；这里逐项委托给内层 provider。
    // `has_*` 也必须转发：内层没有实现时包装层不能报 `true`，否则调用方会先通过能力
    // 检查、再收到内层默认的「不支持」错误
    // （tests/virtual_models.rs 的 `with_virtual_models_delegates_image_and_classifier_support`
    // 锁定了两个方向）。
    fn has_generate_images(&self) -> bool {
        self.inner.has_generate_images()
    }

    fn generate_images<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        model: &'life1 Model,
        context: &'life2 ImagesContext,
        options: Option<&'life3 ImagesOptions>,
    ) -> Pin<Box<dyn Future<Output = AssistantImages> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.generate_images(model, context, options).await })
    }

    fn has_classify(&self) -> bool {
        self.inner.has_classify()
    }

    fn classify<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        model: &'life1 Model,
        context: &'life2 ClassifierContext,
        options: Option<&'life3 ClassifierOptions>,
    ) -> Pin<Box<dyn Future<Output = ClassifierResult> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.classify(model, context, options).await })
    }
}
