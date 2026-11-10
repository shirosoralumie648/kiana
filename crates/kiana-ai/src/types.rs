//! types.ts：内容块、消息、模型、usage、请求上下文与流事件。
//!
//! 跨 crate 契约（`kiana-agent-core`、`kiana-session-core`、`kiana-config`、`kiana-durable` 都直接使用）。
//! 字段名按 TS 的 JSON 形状序列化（camelCase），字段顺序按 `types.ts` 的接口声明顺序，
//! 这样 `serde_json` + `preserve_order` 的输出字节顺序与 `JSON.stringify` 一致。
//!
//! 数字约定：
//! - 时间戳是 Unix 毫秒整数（JS 的 `Date.now()`），用 `i64`，JSON 形状与 JS 完全相同。
//! - token 计数是整数（provider 报告与累加都为整数），用 `u64`。
//! - 价格是浮点，用 `f64`；写出到 JSON 时必须走 `kiana_json::js_object::stringify` / `kiana_json::js_num::format_js_number`
//!   （见 `ARCHITECTURE.md` §4.3）。
//! - 任意 JSON 用 `JsonValue`（`serde_json::Value`，工作区开启 `preserve_order`）。
//!
//! # 这个文件为什么存在
//!
//! 它是本 crate 的**数据契约层**：只定义形状（消息、模型、用量、流事件、请求选项），
//! 不发起网络请求、不做 IO、不持有全局状态。请求构造在 `api/*`，目录与计费在 `models` /
//! `model_catalog`，流的中转在 `event_stream`。集中定义的好处是上下游共用同一份类型：
//! `lib.rs` 里 `pub use types::*` 把它们提升到 crate 根，因此 `kiana_ai::types::Message`
//! 与 `kiana_ai::Message` 指向同一个类型。
//!
//! # 阅读顺序
//!
//! 1. thinking level / modality / cache retention 等基础标量（本文件开头）。
//! 2. 计费（`ModelCost*`）与输入限制（`Model*Limits`）。
//! 3. 内容块（`TextContent` 等）及其联合（`AssistantContentBlock` / `UserContentBlock`）。
//! 4. 用量与停止原因（`Usage` / `StopReason` / `DoneReason` / `ErrorReason`）。
//! 5. 模型（[`crate::types::Model`] 超集结构 + [`crate::types::AnyModel`] 分型视图）。
//! 6. 消息（四个 `*Message`）与 [`crate::types::Message`] 的 untagged 联合。
//! 7. 工具与受约束采样（含两处手写 `Deserialize`）。
//! 8. 流事件（[`crate::types::AssistantMessageEvent`]）与请求选项
//!    （[`crate::types::StreamOptions`] 等）。
//! 9. 图像生成与分类器结果。
//!
//! # 一次典型调用中这些类型如何流动
//!
//! 1. 调用方构造 [`crate::types::Context`]（或已归一化的
//!    [`crate::types::TranscriptContext`]）与 [`crate::types::SimpleStreamOptions`]。
//! 2. `api::simple_options::build_base_options` 把 [`crate::types::SimpleStreamOptions`]
//!    冻结成 [`crate::types::StreamOptions`]，并按「模型默认 < thinking 档位 < 请求」
//!    合并采样参数。
//! 3. provider 适配器把这些结构序列化成线格式、解析响应，产出
//!    [`crate::types::AssistantMessageEvent`] 事件序列，最终得到
//!    [`crate::types::AssistantMessage`]。
//! 4. 上层把 [`crate::types::AssistantMessage`] 的 `content` / `usage` / `stop_reason`
//!    写回会话，并把工具结果构造成 [`crate::types::ToolResultMessage`] 继续下一轮。
//!
//! # 序列化契约（改字段前必读）
//!
//! - **字段声明顺序就是 JSON 键序**：工作区 `serde_json` 开启 `preserve_order`，结构体按
//!   声明序写出，与 TS 接口声明序一致（`ARCHITECTURE.md` §4.3 第 3 条）。重排字段会改变
//!   会话文件/快照的字节。
//! - 可选字段统一 `default` + `skip_serializing_if = "Option::is_none"`：省略表示 TS 的
//!   `undefined`。显式 `null` 与省略不同，个别字段用自定义 `deserialize_with` 保留区别
//!   （见 [`crate::types::ToolResultMessage::details`] 与
//!   [`crate::types::ClientIdentity::attribution`]）。
//! - 大多数类型 `derive(Serialize, Deserialize)`；手写实现只有
//!   [`crate::types::AnyModel`] 的 `Serialize`、[`crate::types::ConstrainedSampling`] /
//!   [`crate::types::ConstrainedSamplingConfig`] 的 `Deserialize`
//!   和 [`crate::types::UserContent`] 的 `Default`，不要当成冗余代码删掉。
//! - 价格、耗时等用 `f64`（TS 的 number），因此这些结构只实现 `PartialEq`，不实现 `Eq`。
//!
//! # 失败模式与注意事项
//!
//! - 本文件没有 IO 错误；失败集中在**反序列化边界**：未知枚举值报 `unknown variant`。
//!   [`crate::types::Message`] 是 `untagged` 联合，失败时 serde 只给
//!   `data did not match any variant`，不指出缺哪个字段；排查时先按 `serde_json::Value`
//!   读出原始 JSON 再逐变体尝试。
//! - [`crate::types::ConstrainedSampling`] 刻意**从不**因形状怪异而反序列化失败：
//!   非规范形状降级为 `Raw(JsonValue)` 原样保留，交由 resolver 按 TS 运行期语义解释。
//! - [`crate::types::AssistantMessageEvent`] 的 `partial` 是**逐事件克隆的完整快照**
//!   （适配器在每个事件处克隆当前消息），不是共享引用；不要从中推断增量或原地修改语义。
//! - [`crate::types::StreamOptions`] 含回调、`CancellationToken`、HTTP 客户端与遥测 span，
//!   因此不实现 `Serialize`；需要落盘的只有其中可序列化的字段。
//! - 修改字段名/顺序前先看 `kiana-durable` 的持久化与 `kiana-agent-core::wire_keys` 的
//!   重排规则：那里按 TS 字面量键序逐字节对齐（例如 `SystemMessage` 的
//!   `toolsAdded`/`toolsRemoved`）。

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize};
use tokio_util::sync::CancellationToken;

/// `JsonValue` / `JsonObject`：与 TS 的 `JsonValue` / `JsonObject` 同义。
pub type JsonValue = serde_json::Value;
/// `JsonObject`：有序 JSON 对象。用 `IndexMap` 而非 `HashMap` 保留插入/解析顺序，是复现
/// JS 属性枚举序的基础；但 JS 还会把整数索引键按数值升序前置，这一步由
/// `kiana_json::js_object`（`ordered_keys`）负责，不能只靠 `IndexMap`。
/// 需要与 `JSON.stringify` 逐字节一致时不能替换成无序容器。
pub type JsonObject = IndexMap<String, JsonValue>;

// ---------------------------------------------------------------------------
// Thinking levels, modalities, cache retention
// ---------------------------------------------------------------------------

/// `THINKING_LEVELS`。TS 侧不含 "off"。
///
/// 一次请求的推理强度，序列化为 `minimal`/`low`/`medium`/`high`/`xhigh`/`max`。
/// 适配器通常先查 [`ThinkingLevelMap`] 做模型级覆盖，再回退到各家的默认折算
/// （例如 Anthropic 把 minimal/low 折算成 effort 的 `low`）；Google 只认四档，
/// `xhigh`/`max` 必须靠映射先折到四档之一。
///
/// 该枚举不含关闭档：不请求推理在 [`SimpleStreamOptions::reasoning`] 里用 `None` 表达。
/// 档位由弱到强依次为 `minimal` < `low` < `medium` < `high` < `xhigh` < `max`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    Xhigh,
    Max,
}

/// `MODEL_THINKING_LEVELS`：`["off", ...THINKING_LEVELS]`。
///
/// 模型级 thinking 档位：在 [`ThinkingLevel`] 之上增加 `off`。它至少出现在三处：
/// 模型目录的 [`ThinkingLevelMap`] 键（映射到 provider 的具体取值）、
/// [`SamplingParamsByThinkingLevel`] 的键，以及 [`AssistantMessage::thinking_level`]
/// （记录本次请求实际使用的档位）。此外 `models::thinking` 的 `clamp_thinking_level` /
/// `EXTENDED_THINKING_LEVELS` 等查询与钳制 API 也以它为输入输出。
/// `off` 是 `Default`，含义是「关闭推理」；其余档位由弱到强与 [`ThinkingLevel`] 同序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    Xhigh,
    Max,
}

/// `MODEL_INPUT_MODALITIES`。
///
/// 模型支持的输入模态；[`Model::input`] 的数组顺序就是模型目录里的声明顺序。
/// 适配器据此决定是否发送图片：不含 [`ModelInputModality::Image`] 时，
/// `api::transform_messages::downgrade_unsupported_images` 会把图片替换成占位文本，
/// 而不是报错。`Text` 与 `Image` 两种取值可同时出现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelInputModality {
    Text,
    Image,
}

/// `CACHE_RETENTIONS`。
///
/// prompt cache 的保留偏好，序列化为 `none`/`short`/`long`。它只是请求侧偏好：
/// 适配器会结合模型 compat（如 `supportsLongCacheRetention`）与环境变量
/// `KIANA_CACHE_RETENTION` 解析出实际档位（见 `api::anthropic_messages::resolve_cache_retention`）。
/// 显式 [`CacheRetention::None`] 表示关闭本次请求的缓存标记，与 `Option::None`（未指定，
/// 落到默认 `short`）语义不同；`long` 是否真正生效取决于 provider 与模型 compat
/// （例如 Anthropic 需要 `supportsLongCacheRetention`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

/// `MODEL_PROMPT_CACHE_RETENTIONS`（prompt cache 生命周期的键）。
///
/// [`ModelPromptCache`] 用它在模型目录的 `promptCache` 表里查该档位的缓存秒数；
/// `kiana-config` 的 cache warmer 再乘 1000 得到毫秒 TTL，用于决定何时续期缓存。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptCacheTier {
    Short,
    Long,
}

/// `ThinkingLevelMap`：每个 kiana thinking level 到 provider 取值的映射；`None`（JSON null）表示不支持。
///
/// 「不支持」是 TS 类型层的表述；运行期实现并不统一：缺键与显式 `null` 在多数适配器里都
/// 回退到默认折算（TS 的 `??` 语义，见 `map_thinking_level_to_effort`），只有 Codex 的
/// `"none"` 分支把显式 `null` 当作「省略 reasoning」，与缺键不同。新增适配器时必须先确认
/// 这两种情况。
pub type ThinkingLevelMap = IndexMap<ModelThinkingLevel, Option<String>>;
/// `ModelPromptCache`：每个 retention 档位的缓存秒数。
/// 值是秒（`f64`，与 TS number 一致）；缺键表示该档位没有配置 TTL。
pub type ModelPromptCache = IndexMap<PromptCacheTier, f64>;
/// `SamplingParams`：任意采样参数（透传给 OpenAI 兼容端点）。
/// 键是请求体字段名，值原样合并；合并优先级见 `api::simple_options::resolve_sampling_params`。
pub type SamplingParams = IndexMap<String, JsonValue>;
/// `SamplingParamsByThinkingLevel`。
/// 键是 `models::thinking::clamp_thinking_level` 钳制后的有效档位；未命中的档位不贡献参数。
pub type SamplingParamsByThinkingLevel = IndexMap<ModelThinkingLevel, SamplingParams>;

// ---------------------------------------------------------------------------
// Model pricing / limits
// ---------------------------------------------------------------------------

/// `ModelCostRates`。
///
/// 单位是 **USD / 百万 token**（与 TS 的字段注释一致）。四个字段分别是输入（未命中缓存）、
/// 输出、缓存读取、缓存写入；1h cache write 按 2x input 计的费用规则在
/// `models::cost::calculate_cost` 里实现，不在本结构体内。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    /// Input cost in USD per million tokens.
    pub input: f64,
    /// 输出价格，USD / 百万 token。
    pub output: f64,
    /// 缓存读取（命中）价格，USD / 百万 token。
    pub cache_read: f64,
    /// 缓存写入价格，USD / 百万 token。
    pub cache_write: f64,
}

/// `ModelCostTier`。
///
/// 请求级价格分档：当 `input + cacheRead + cacheWrite` 严格大于
/// [`ModelCostTier::input_tokens_above`] 时启用本档，多个档位同时命中时取阈值最高的那个
/// （见 `models::cost::calculate_cost`）。其余四个费率字段与 [`ModelCostRates`] 同义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    /// Use this tier when total request input exceeds this token count.
    pub input_tokens_above: f64,
    /// 本档的输入价格，USD / 百万 token。
    pub input: f64,
    /// 本档的输出价格，USD / 百万 token。
    pub output: f64,
    /// 本档的缓存读取价格，USD / 百万 token。
    pub cache_read: f64,
    /// 本档的缓存写入价格，USD / 百万 token。
    pub cache_write: f64,
}

/// `ModelCost`。
///
/// 顶层四个费率是基础价；一旦某个 [`ModelCostTier`] 命中，该档的四个费率**整组替换**基础价
/// （不是叠加）。`tiers` 为 `None` 表示不分档。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    /// 基础输入价格，USD / 百万 token。
    pub input: f64,
    /// 基础输出价格，USD / 百万 token。
    pub output: f64,
    /// 基础缓存读取价格，USD / 百万 token。
    pub cache_read: f64,
    /// 基础缓存写入价格，USD / 百万 token。
    pub cache_write: f64,
    /// Request-wide pricing tiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// `ModelImageResizeOptions`。
///
/// 图片预处理目标。实际缩放/重编码由上层在发送前完成（`kiana-session` 的工具结果图片处理、
/// `kiana-tools` / `kiana-durable` 的 read 工具会读取这里的配置）；本 crate 只承载数据。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageResizeOptions {
    /// 缩放后的最大宽度（像素）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u64>,
    /// 缩放后的最大高度（像素）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u64>,
    /// Maximum base64-encoded payload size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// JPEG 重编码质量；具体取值范围由图像处理实现解释，本 crate 不做校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jpeg_quality: Option<u8>,
}

/// `ModelImageInputLimits`。
///
/// 图片相关的 provider 限额，数值来自模型目录（例如 Anthropic 系列的
/// `maxPerRequest` 为 100/600，Bedrock 系列的 `maxPerMessage` 为 20）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageInputLimits {
    /// 发送前的缩放/重编码配置；缺省表示没有该配置，由调用方决定默认行为。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<ModelImageResizeOptions>,
    /// 单条消息允许的图片数量上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_message: Option<u64>,
    /// 单次请求允许的图片数量上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_request: Option<u64>,
}

/// `ModelInputLimits`。
///
/// 模型级输入上限的聚合，随模型对象在目录/配置中往返；是否执行这些限制由上层决定，
/// 本 crate 的适配器不会据此裁剪内容。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInputLimits {
    /// 单次请求体（含图片等负载）的字节上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_request_bytes: Option<u64>,
    /// 图片相关限额。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<ModelImageInputLimits>,
}

// ---------------------------------------------------------------------------
// Content blocks
// ---------------------------------------------------------------------------

/// `TextContent`。
///
/// 文本块，`type` 固定为 `"text"`。同一个结构既用于 assistant 内容
/// （[`AssistantContentBlock`]），也用于 user / toolResult 内容（[`UserContentBlock`]）。
/// `text_signature` 是 provider 私有的回放数据：OpenAI Responses 存 [`TextSignatureV1`]
/// （旧版是 id 字符串），Google 存 thought signature；一般只出现在 assistant 内容里。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    /// 固定判别值 `"text"`，serde 写出为 `type` 键。
    #[serde(rename = "type")]
    pub kind_text: TextKind,
    /// 文本内容本身。
    pub text: String,
    /// e.g. OpenAI responses message metadata (legacy id string or TextSignatureV1 JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

/// 固定判别值 `"text"`；用 unit 枚举是为了让 serde 精确输出字面量且无法被调用方写错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextKind {
    #[default]
    #[serde(rename = "text")]
    Text,
}

/// `TextSignatureV1`：OpenAI Responses 文本签名的 JSON 负载
/// （`TextContent::text_signature` 里的 `{"v":1,"id":...}` 形式）。
///
/// TS: `packages/ai/src/types.ts#TextSignatureV1`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    /// 版本号；`parseTextSignature` 只认 `v === 1`。
    pub v: u32,
    /// Responses API 的 item id。
    pub id: String,
    /// 可选通道标记；缺省表示旧版签名或未区分通道。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextSignaturePhase>,
}

/// `TextSignatureV1["phase"]`：`"commentary" | "final_answer"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSignaturePhase {
    /// 模型的中间解说通道。
    Commentary,
    /// 最终答案通道。
    FinalAnswer,
}

/// `ThinkingContent`。
///
/// 推理/思考块，`type` 固定为 `"thinking"`。`thinking` 是明文推理文本；
/// `thinking_signature` 是 provider 私有的回放数据（加密负载或序列化后的 reasoning）；
/// `redacted == Some(true)` 表示内容被安全过滤，真正的载荷在 `thinking_signature` 中。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    /// 固定判别值 `"thinking"`，serde 写出为 `type` 键。
    #[serde(rename = "type")]
    pub kind_thinking: ThinkingKind,
    /// 模型输出的明文思考文本。
    pub thinking: String,
    /// Provider-specific opaque or serialized reasoning replay data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// Redacted by safety filters; the encrypted payload lives in `thinkingSignature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

/// 固定判别值 `"thinking"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThinkingKind {
    #[default]
    #[serde(rename = "thinking")]
    Thinking,
}

/// `ImageContent`：`data` 是 base64。
///
/// `type` 固定为 `"image"`。本 crate 不校验 base64 与 MIME 是否匹配，也不做尺寸检查；
/// 缩放与「模型不支持图片时降级为占位文本」分别在调用方和
/// `api::transform_messages::downgrade_unsupported_images` 里完成。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// 固定判别值 `"image"`，serde 写出为 `type` 键。
    #[serde(rename = "type")]
    pub kind_image: ImageKind,
    /// base64 encoded image data
    pub data: String,
    /// e.g. "image/jpeg", "image/png"
    pub mime_type: String,
}

/// 固定判别值 `"image"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImageKind {
    #[default]
    #[serde(rename = "image")]
    Image,
}

/// `ToolCall`。
///
/// 模型请求调用工具，`type` 固定为 `"toolCall"`。`id` 通常来自 provider；Google 在
/// provider 未提供 id、提供空 id 或 id 与已有调用重复时，由适配器生成
/// （`api::google_shared::generate_tool_call_id`）。工具结果通过
/// [`ToolResultMessage::tool_call_id`] 与它配对；`arguments` 是已解析的 JSON 对象。
/// 原始参数文本不是合法 JSON 对象时，适配器把 `arguments` 置为 `{}` 并填写
/// `arguments_error`，消息仍然可用（不会因此丢弃整条回复）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// 固定判别值 `"toolCall"`，serde 写出为 `type` 键。
    #[serde(rename = "type")]
    pub kind_tool_call: ToolCallKind,
    /// provider 侧的调用 id，用于和工具结果配对。
    pub id: String,
    /// 工具名；未知名字不属于本 crate 的错误，由 `kiana-tools` 在分发时以
    /// `Unknown tool name: {name}` 报告。
    pub name: String,
    /// 解析后的参数对象；参数文本非法时为空对象。
    pub arguments: JsonObject,
    /// Google-specific: opaque signature for reusing thought context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// OpenAI Responses namespace for calls to dynamically loaded or namespaced tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Set when the model's complete argument text was not a valid JSON object; `arguments` is then `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_error: Option<String>,
}

/// 固定判别值 `"toolCall"`（camelCase，与文本/图片/思考块的 lowercase 值不同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolCallKind {
    #[default]
    #[serde(rename = "toolCall")]
    ToolCall,
}

/// assistant 消息的内容块联合：`TextContent | ThinkingContent | ToolCall`。
///
/// 使用 `untagged`：三个变体靠各自的固定 `type` 判别值区分，JSON 与 TS 的普通联合一致。
/// 反序列化按声明顺序尝试，未知 `type` 会令整个联合失败（没有兜底变体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

/// user 消息与 toolResult 的内容块联合：`TextContent | ImageContent`。
///
/// `untagged`，靠固定 `type`（`"text"`/`"image"`）区分。toolResult 也使用该类型，
/// 因此工具结果可以携带图片（模型不支持图片时会被替换成占位文本）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

/// `string | TextContent[]`（system 消息）。
///
/// `untagged`：字符串形态是整段 prompt；块形态只允许文本块（system 消息不支持图片）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<TextContent>),
}

/// `string | (TextContent | ImageContent)[]`（user / custom 消息）。
///
/// `untagged`：字符串形态是单段文本（简单 prompt 的常见形态）；块形态可混合文本与图片。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

// 默认值是字符串形态的空内容（序列化为 `content: ""`），而不是空块数组；两者在 TS 里
// 形状不同，后者会写出 `[]`。
impl Default for UserContent {
    fn default() -> Self {
        UserContent::Text(String::new())
    }
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

/// `Usage.cost`。
///
/// 一次请求的费用明细，单位是 USD（整次请求的金额，不是每百万 token 单价）。
/// 由 `models::cost::calculate_cost` 填充；provider 上报账单总额时（如 OpenRouter）
/// 会覆盖 `total` 并把 `total_from_provider` 置真。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    /// 输入费用（USD）。
    pub input: f64,
    /// 输出费用（USD）。
    pub output: f64,
    /// 缓存读取费用（USD）。
    pub cache_read: f64,
    /// 缓存写入费用（USD）；1h 缓存写入按 2x input 计价，已包含在这里。
    pub cache_write: f64,
    /// 总费用：正常是四项之和，provider 上报时是账单总额。
    pub total: f64,
    /// True when `total` is the billed cost reported by the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_from_provider: Option<bool>,
}

/// `Usage`。
///
/// 一次模型调用的 token 用量与费用。字段口径由各适配器统一：`input` 通常是**已扣除**
/// 缓存命中与缓存写入后的净输入，缓存部分单列在 `cache_read`/`cache_write`。
/// `total_tokens` 有的直接采用 provider 上报值、有的由适配器重算；下游
/// `utils::estimate::calculate_context_tokens` 只在它为 0 时才回退到四项之和。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// 净输入 token（不含缓存命中/写入）。
    pub input: u64,
    /// 输出 token（含 reasoning）。
    pub output: u64,
    /// 命中 prompt cache 的输入 token。
    pub cache_read: u64,
    /// 写入 prompt cache 的输入 token。
    pub cache_write: u64,
    /// Subset of `cacheWrite` written with 1h retention. Only Anthropic reports this split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write1h: Option<u64>,
    /// Reasoning tokens; a subset of `output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// 总 token 数；口径由适配器保证（provider 上报值或四项之和）。为 0 时下游会回退到
    /// `input + output + cache_read + cache_write`。
    pub total_tokens: u64,
    /// 本调用费用明细。
    pub cost: UsageCost,
}

/// `StopReason`。
///
/// 一条 assistant 消息的结束原因，是适配器收尾校验的核心状态：
///
/// - [`StopReason::Pending`]：构造中的初始值。流结束时仍是 `Pending` 会被判为失败
///   （适配器报 "stream ended without a stop reason"）。
/// - [`StopReason::Stop`] / [`StopReason::Length`] / [`StopReason::ToolUse`]：正常结束的
///   三种情形（自然结束 / 触顶 / 模型请求调用工具）。
/// - [`StopReason::Error`] / [`StopReason::Aborted`]：失败结束；`Aborted` 表示调用方取消
///   （取消令牌已触发），`Error` 表示解析/传输/provider 错误。
/// - [`StopReason::Deferred`]：provider 已受理但结果需稍后轮询，消息的
///   [`AssistantMessage::deferred`] 携带句柄。
/// - [`StopReason::TurnCapped`] / [`StopReason::TokenCapped`] / [`StopReason::CostCapped`] /
///   [`StopReason::TimeCapped`]：本地预算控制器（`kiana-agent-core::budget`）在
///   turn / token / cost / wall-clock 预算耗尽时写入的精确原因。它们不是 provider
///   返回值；适配器的收尾校验不会产生这些变体。把 cap 表达在本枚举而不是折到
///   `Length`，是为了让会话、UI 与 RPC 消费方无需再读 `raw_stop_reason` 才能区分
///   “输出被模型截断”和“被本地预算停下”。
///
/// [`DoneReason`] 与 [`ErrorReason`] 是它投射到流事件时的子集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// 初始/未结束状态；正常结果不应保留该值。
    #[default]
    Pending,
    /// 模型自然结束本轮。
    Stop,
    /// 达到输出 token 上限。
    Length,
    /// 模型请求调用工具。
    ToolUse,
    /// 适配器或 provider 报错。
    Error,
    /// 调用方取消。
    Aborted,
    /// 结果需稍后轮询。
    Deferred,
    /// 本地 turn 预算耗尽。
    TurnCapped,
    /// 本地 token 预算耗尽。
    TokenCapped,
    /// 本地 cost 预算耗尽。
    CostCapped,
    /// 本地 wall-clock 预算耗尽。
    TimeCapped,
}

/// `done` 事件允许的 reason 子集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneReason {
    /// 自然结束。
    Stop,
    /// 达到输出上限。
    Length,
    /// 请求调用工具。
    ToolUse,
    /// 结果需稍后轮询。
    Deferred,
}

/// `error` 事件允许的 reason 子集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    /// 调用方取消（取消令牌已触发）。
    Aborted,
    /// 解析、传输或 provider 错误。
    Error,
}

/// `DeferredHandle`。
///
/// 延迟响应的句柄：provider 不立即返回完整回复，而是给出一个之后可轮询/取消的引用。
/// 当 [`AssistantMessage::stop_reason`] 为 [`StopReason::Deferred`] 时，消息的
/// [`AssistantMessage::deferred`] 携带它。`provider` / `model_id` / `api` 是重建
/// provider 调用所需的路由信息（决定用哪个适配器去兑换结果）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    /// 发起请求的 provider id。
    pub provider: String,
    /// 发起请求时的模型 id。
    pub model_id: String,
    /// API 实现名，决定兑换结果时使用哪个适配器。
    pub api: String,
    /// Provider token, such as a response id or batch id plus row id.
    pub id: String,
    /// 过期时间（Unix 毫秒时间戳，见 `spec/ai-core.md`）；faux provider 不填。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
    /// 建议的下次轮询延迟（毫秒，TS 的 `pollAfterMs`）；durable harness 用它计算
    /// `pollAt = floor(max(now + (pollAfterMs ?? 默认值), 上次 pollAt + 1))`（结果向下取整
    /// 到整毫秒；有上一次 pollAt 时靠 `+1` 保证严格前进），本 crate 不强制执行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<f64>,
    /// Provider conversion data required to reconstruct the final assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// `KnownProvider` 共有 42 个已知值；Rust 侧保留开放字符串语义（TS 是 `KnownProvider | string`）。
///
/// 不用枚举：模型目录与用户自定义 provider 可以出现任意 id，枚举化会让新增 provider
/// 变成破坏性变更。需要内置清单时以 `model_catalog` / `providers` 的数据为准。
pub type ProviderId = String;

/// `KnownApi` 的已知值；`Api` 是开放字符串。
///
/// 这里只是清单，当前仓库没有运行期调用方（实现选择由 `api::lazy` 的工厂完成）；
/// 新增适配器时要人工同步本常量与对应模块。
pub const KNOWN_APIS: &[&str] = &[
    "openai-completions",
    "mistral-conversations",
    "openai-responses",
    "azure-openai-responses",
    "openai-codex-responses",
    "anthropic-messages",
    "bedrock-converse-stream",
    "google-generative-ai",
    "google-vertex",
    "kiana-messages",
];
/// 开放字符串语义。
///
/// 已知值见 [`KNOWN_APIS`]；模型目录和自定义模型可以携带任意值，因此不能收窄成枚举。
pub type Api = String;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// `ModelType`。
///
/// 模型的类型判别，序列化为 `chat`/`image`/`classifier`。内置模型目录会显式写出
/// `"type":"chat"`（反序列化后 [`Model::model_type`] 为 `Some(ModelType::Chat)`）；
/// 只有用户自定义或稀疏的模型可以省略 `type`，缺省按 `chat` 解析，`image`/`classifier`
/// 必须显式写出。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    /// 对话模型（缺省）。
    #[default]
    Chat,
    /// 图像生成模型。
    Image,
    /// 分类器模型。
    Classifier,
}

/// `BaseModel` + `Model`（chat）。TS 通过结构化类型让 image/classifier 模型只多一个 `type`
/// 并去掉/加上少量字段；Rust 侧用「全部字段的超集结构 + `type` 判别」复现同一 JSON 形状，
/// 再用 [`AnyModel`] 提供分型视图。
///
/// 字段按用途分组：所有模型共有、chat 专属、image 专属（另有少量 chat+classifier 共用）。
/// 注释里的 "Chat only"/"Image generation only" 标出专属字段；构造非对应类型的模型时
/// 这些专属字段应保持 `None`（例如 chat 模型不该填写 `output`），否则会写出 TS 形状里
/// 不该出现的键。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    /// 模型 id（provider 内唯一）。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// API 实现名，见 [`KNOWN_APIS`]。
    pub api: Api,
    /// provider id。
    pub provider: ProviderId,
    /// 端点根地址；适配器在此基础上拼接具体路径。
    pub base_url: String,
    /// 支持的输入模态，数组顺序为模型目录的声明顺序。
    pub input: Vec<ModelInputModality>,
    /// Provider input limits and cache-safe preprocessing metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    /// 价格与请求级分档。
    pub cost: ModelCost,
    /// 模型级附加请求头（键为 header 名）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,

    /// Chat is the default model type.
    ///
    /// `None` 等价于 chat（TS 的 `type?: "chat" | ...` 省略），因此写出时不带 `type` 键。
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<ModelType>,
    /// Chat only.
    ///
    /// `Some(true)` 表示模型支持推理；适配器据此决定是否发送 thinking 相关参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// 模型级 thinking 档位覆盖表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// prompt cache 各档位的 TTL（秒），供 cache warmer 使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    /// Chat + classifier.
    ///
    /// 上下文窗口 token 数；`clamp_max_tokens_to_context` 用它给输出上限封顶。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Chat only.
    ///
    /// 模型默认的最大输出 token 数；请求未显式指定时使用，并再按上下文窗口钳制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// 默认采样参数（合并优先级最低）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    /// 按 thinking 档位覆盖的采样参数（优先级高于 `sampling_params`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    /// Compatibility overrides; the concrete schema depends on `api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<JsonValue>,
    /// Image generation only: output modalities (`"text"`/`"image"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<String>>,
}

/// `AnyModel`：provider 可以列出的任意模型类型。
///
/// 四个变体都围绕同一个 [`Model`]：`Chat`/`Image`/`Classifier` 是已知类型的视图，
/// `Remote` 额外保留远端目录的原始 JSON。`Remote` 的用途是**不丢未知字段**：
/// 请求时用投影出的 `model`，持久化/展示时用 `json` 逐字还原（见
/// `models::store::parse_any_model` 与 [`REMOTE_MODEL_JSON_KEY`]）。
#[derive(Debug, Clone, PartialEq)]
pub enum AnyModel {
    Chat(Model),
    Image(Model),
    Classifier(Model),
    /// A remote-catalog model with its original sparse JSON object. `model` is
    /// the best-effort typed projection used for requests; `json` is the public
    /// shape and preserves unknown fields and insertion order.
    Remote {
        model: Model,
        json: JsonValue,
    },
}

impl AnyModel {
    /// 返回内部 [`Model`] 的共享视图；`Remote` 返回其尽力投影（可能缺失未知字段）。
    pub fn model(&self) -> &Model {
        match self {
            AnyModel::Chat(m)
            | AnyModel::Image(m)
            | AnyModel::Classifier(m)
            | AnyModel::Remote { model: m, .. } => m,
        }
    }
    /// 返回模型的类型视图：前三个变体直接对应；`Remote` 读投影里的 `type`，
    /// 缺失按 chat 处理（与 TS 的 `type ?? "chat"` 一致）。
    pub fn model_type(&self) -> ModelType {
        match self {
            AnyModel::Chat(_) => ModelType::Chat,
            AnyModel::Image(_) => ModelType::Image,
            AnyModel::Classifier(_) => ModelType::Classifier,
            AnyModel::Remote { model, .. } => model.model_type.unwrap_or(ModelType::Chat),
        }
    }
}

/// Internal wrapper key consumed by the shared wire writer. This is never
/// persisted: it lets the writer distinguish original remote JSON from typed
/// custom-model objects without guessing from sparse fields.
///
/// 该键只作为 `kiana-agent-core::wire_keys::to_wire_value` 生成的**中间包装对象**出现：
/// `reorder_wire_keys` 识别并解包后立即丢弃，最终 JSON 与持久化文件里不会出现它。
/// 它是两个 crate 之间的内部约定（TS 侧没有对应字段），不是公共 wire 格式。
pub const REMOTE_MODEL_JSON_KEY: &str = "__kianaRemoteCatalogModelJson";

/// `AnyModel` 的序列化：`Remote` 直接写出原始 JSON（保留未知字段与键序），其余变体写出
/// 内部 [`Model`]。因此 `serde_json::to_value(&AnyModel)` 的输出是稳定的逐字节序列化；
/// [`REMOTE_MODEL_JSON_KEY`] 不会出现在这里，它只由 wire writer 在内部包装时使用。
impl Serialize for AnyModel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            AnyModel::Remote { json, .. } => json.serialize(serializer),
            _ => self.model().serialize(serializer),
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// `SystemMessage`。
///
/// 字段声明顺序决定 serde 的 JSON 键序，按 `ARCHITECTURE.md` §4.3.3 与 TS
/// `interface SystemMessage` 的声明顺序一致（`toolsAdded` 在 `toolsRemoved` 之前）。
/// durable 的 `harness/prompt.ts#systemEntry` 构造序是 `toolsRemoved` 在 `toolsAdded`
/// 之前（`kiana.system` 条目会逐字节持久化），该差异由
/// `kiana-durable::types::persisted_message_json()` 在写出时显式重排，不依赖本结构体的字段序。
///
/// `sections` 是增量补丁：值为 `Some(text)` 表示设置/替换该命名段（新名字追加到末尾），
/// 值为 `None`（JSON null）表示删除前面 system 消息定义的同名段。回放逻辑在
/// `utils::transcript::get_current_system_message`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    /// 固定判别值 `"system"`；`Message` 的 untagged 联合靠它识别本变体。
    pub role: SystemRole,
    /// Instruction text. On the leading message this is the base prompt.
    pub content: SystemContent,
    /// Named, ordered prompt sections rendered verbatim after `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<IndexMap<String, Option<String>>>,
    /// Complete definitions of tools that become available at this point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<Tool>>,
    /// Tools that stop being available at this point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_removed: Option<Vec<ToolReference>>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// 固定判别值 `"system"`；让 `Message` 的 untagged 联合能稳定识别 system 消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SystemRole {
    #[default]
    #[serde(rename = "system")]
    System,
}

/// `UserMessage`。
///
/// 用户输入；`content` 是字符串或 `TextContent | ImageContent` 块数组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    /// 固定判别值 `"user"`。
    pub role: UserRole,
    /// 用户输入：纯字符串，或文本/图片块数组。
    pub content: UserContent,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// 固定判别值 `"user"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UserRole {
    #[default]
    #[serde(rename = "user")]
    User,
}

/// `AssistantMessageDiagnostic`。
///
/// 诊断条目里内嵌的错误快照（TS 中即 `AssistantMessageDiagnostic["error"]`）。
/// `name`/`message`/`stack`/`code` 从 JS 的 Error 对象尽力提取（见 `utils::diagnostics`）；
/// `code` 用任意 JSON，因为 Node 的错误码可能是字符串或数字。`message` 必有，其余可选。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticErrorInfo {
    /// 错误名（如 `"TypeError"`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 错误消息（必有）。
    pub message: String,
    /// 错误堆栈（若 provider/运行时提供）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    /// 错误码，形态不限（字符串、数字等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<JsonValue>,
}

/// `AssistantMessageDiagnostic`。
///
/// assistant 消息附带的诊断条目，只用于会话记录/调试，不参与模型请求。`kind` 是自由字符串
/// （例如 `"anthropic_input_transformations"`），`timestamp` 是 Unix 毫秒。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessageDiagnostic {
    /// 诊断类型标签（TS 的 `type`）；不是封闭枚举，取值由写入方定义。
    #[serde(rename = "type")]
    pub kind: String,
    /// 诊断产生的 Unix 毫秒时间戳。
    pub timestamp: i64,
    /// 错误快照（若该诊断源自异常）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DiagnosticErrorInfo>,
    /// 诊断附带的任意结构化细节。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonObject>,
}

/// `AssistantMessage`。
///
/// 模型一次完整回复。`content` 是块序列（文本/思考/工具调用可混合）；`usage` 与
/// `stop_reason` 驱动 agent 循环的下一步；`deferred` 存在时表示结果需稍后轮询。
/// 时间戳记录请求开始时刻，`duration_ms` 记录到响应结束的耗时。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    /// 固定判别值 `"assistant"`。
    pub role: AssistantRole,
    /// 回复的内容块序列。
    pub content: Vec<AssistantContentBlock>,
    /// 请求使用的 API 实现名。
    pub api: Api,
    /// 请求使用的 provider id。
    pub provider: ProviderId,
    /// 请求时指定的模型 id。
    pub model: String,
    /// Concrete model reported by the provider, when reported.
    ///
    /// 填写规则因适配器而异：Anthropic 只在响应模型与请求模型不同时设置（用于 fallback
    /// 计价），OpenAI completions 用 `||=` 记录 provider 实际路由到的模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Provider-specific response/message identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Exact provider-native effort level used for this response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
    /// Kiana thinking level the agent loop requested for this response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 适配器附加的诊断条目（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    /// 本次调用的 token 用量与费用。
    pub usage: Usage,
    /// 结束原因，见 [`StopReason`]；正常结果不应是 `Pending`。
    pub stop_reason: StopReason,
    /// 延迟响应句柄；仅 `stop_reason == Deferred` 时存在。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    /// 失败时的用户可见错误文本；成功回复为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// provider 原始的 stop reason 字符串，用于诊断；原样保存，不做归一化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// Provider indication of whether the model explicitly ended its turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    /// Unix timestamp in milliseconds when the request started.
    /// 即请求开始的 Unix 毫秒时间戳，也是 `duration_ms` 的计时起点。
    pub timestamp: i64,
    /// Milliseconds from `timestamp` until the response ended (monotonic clock).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

/// 固定判别值 `"assistant"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AssistantRole {
    #[default]
    #[serde(rename = "assistant")]
    Assistant,
}

/// `NestedToolCallRecord` / `NestedToolCalls`。
///
/// 工具执行期间嵌套调用其他工具的记录，只保存在会话记录里，不发送给模型。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCalls {
    /// 原始记录数组；单条形状由产生它的工具定义，因此用 [`JsonValue`] 原样保留。
    pub calls: Vec<JsonValue>,
    /// False when calls were dropped, arguments omitted, or calls had not finished.
    pub complete: bool,
}

/// `ToolResultMessage`。
///
/// 工具执行结果，作为 `toolResult` 消息回灌给模型。`content` 与 user 消息同形（可含图片）；
/// `details` 是工具自定义的 JSON（供 UI/会话记录使用），不属于模型输入内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    /// 固定判别值 `"toolResult"`。
    pub role: ToolResultRole,
    /// 对应的 [`ToolCall::id`]。
    pub tool_call_id: String,
    /// 被调用工具的名字。
    pub tool_name: String,
    /// 工具输出内容块（文本 / 图片）。
    pub content: Vec<UserContentBlock>,
    /// `details?: JsonValue`；显式 `null` 会保留成 `Some(Value::Null)` 并原样写出
    /// （TS 的 `details: null` 与省略不同，见 `message-types.test.ts`）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub details: Option<JsonValue>,
    /// Usage from the tool execution itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Calls this tool made to other tools; kept for the session record, not sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_calls: Option<NestedToolCalls>,
    /// 工具是否失败；失败结果仍会发给模型，由模型决定如何补救。
    pub is_error: bool,
    /// Unix timestamp in milliseconds when the result was created.
    pub timestamp: i64,
    /// Milliseconds the tool's execution took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

/// 固定判别值 `"toolResult"`（camelCase）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolResultRole {
    #[default]
    #[serde(rename = "toolResult")]
    ToolResult,
}

/// `Message`。
///
/// 变体大小差异来自 TS 的消息形状本身（`AssistantMessage` 字段最多）。装箱会改变所有下游 match，
/// 因此这里保留直接嵌套并关闭该 lint。
///
/// ## 为什么是 `untagged` 而不是 `#[serde(tag = "role")]`
///
/// 四个变体都是**内嵌 `role` 字段的结构体**（`SystemMessage.role: SystemRole` 等），JSON 是
/// 普通的角色判别联合体。serde 的 internally tagged 形式（`tag = "role"`）对“newtype 变体 + 自带同名
/// 字段的结构体”是**不可用**的：序列化会输出两份 `role` 键，反序列化则在剥离 tag 后报
/// `missing field \`role\``（已实测）。`untagged` + 每个变体各不相同的 `role` 枚举是等价且可往返的
/// 表达，也符合 `ARCHITECTURE.md` §4.3 第 6 条（判别顺序与 TS 类型守卫一致时用 `untagged`）。
/// 公开类型与字段没有任何变化。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult(ToolResultMessage),
}

impl Message {
    /// 返回 TS 联合的 `role` 判别值：`"system"` / `"user"` / `"assistant"` / `"toolResult"`。
    pub fn role(&self) -> &'static str {
        match self {
            Message::System(_) => "system",
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        }
    }
}

// ---------------------------------------------------------------------------
// Context / tools
// ---------------------------------------------------------------------------

/// `ToolReference`。
///
/// 只按名字引用工具；用于 [`SystemMessage::tools_removed`]（移除工具不需要完整定义）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolReference {
    /// 被引用工具的名字。
    pub name: String,
}

/// `GrammarFormat` / `GrammarVariants` / `ConstrainedSamplingConfig`。
///
/// 受约束采样的 grammar 变体：两种 provider 语法文本（lark / regex），由 provider 选择
/// 自己支持的字段。
/// 两者都为空/全空白时，resolver 会报 "no supported grammar variant was provided"；
/// 同时提供时 lark 优先（见 `api::constrained_sampling::resolve_grammar_constrained_sampling`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrammarVariants {
    /// OpenAI lark 语法；与 `openai_regex` 同时存在时优先使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_lark: Option<String>,
    /// OpenAI regex 语法。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_regex: Option<String>,
}

/// `ConstrainedSamplingConfig`（判别键 `type`）。
///
/// `Deserialize` 是手写的：TS 的类型只是编译期约束，运行期对宽松 JSON 很宽容（缺 `strict`、
/// `strict: null` 都按 `"prefer"`；`GrammarVariants` 里非字符串的值按未设置处理）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstrainedSamplingConfig {
    JsonSchema {
        /// `"prefer" | "require"`.
        strict: StrictMode,
    },
    Grammar {
        /// grammar 变体；两者都为空时 resolver 报错，lark 优先于 regex。
        variants: GrammarVariants,
    },
}

impl<'de> Deserialize<'de> for ConstrainedSamplingConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = JsonValue::deserialize(deserializer)?;
        if !value.is_object() {
            return Err(serde::de::Error::custom(
                "constrained sampling config must be an object",
            ));
        }
        config_from_json(&value)
            .ok_or_else(|| serde::de::Error::custom("unknown constrained sampling type"))
    }
}

/// 按 TS 运行期的 `type` 分派解析配置对象（宽松：缺字段取默认、非字符串变体按未设置）。
fn config_from_json(value: &JsonValue) -> Option<ConstrainedSamplingConfig> {
    let object = value.as_object()?;
    match object.get("type").and_then(JsonValue::as_str) {
        Some("json_schema") => Some(ConstrainedSamplingConfig::JsonSchema {
            strict: strict_mode_from_json(object.get("strict")),
        }),
        Some("grammar") => Some(ConstrainedSamplingConfig::Grammar {
            variants: grammar_variants_from_json(object.get("variants")),
        }),
        _ => None,
    }
}

/// TS 的真值判断 `strict === "require"`：缺失 / `null` / 其它值都按 `"prefer"`。
fn strict_mode_from_json(value: Option<&JsonValue>) -> StrictMode {
    match value.and_then(JsonValue::as_str) {
        Some("require") => StrictMode::Require,
        _ => StrictMode::Prefer,
    }
}

/// `GrammarVariants` 的宽松解析：非字符串值按「未设置」处理（TS 用 `typeof === "string"`）。
fn grammar_variants_from_json(value: Option<&JsonValue>) -> GrammarVariants {
    let object = value.and_then(JsonValue::as_object);
    let variant = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    };
    GrammarVariants {
        openai_lark: variant("openai_lark"),
        openai_regex: variant("openai_regex"),
    }
}

/// `"prefer" | "require"`：strict JSON schema 的失败策略。
///
/// `Prefer` 表示 schema 无法收紧成 provider 的 strict 子集时回退为非 strict；
/// `Require` 表示直接报错。判定与错误文本见 `api::constrained_sampling`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrictMode {
    /// 尽量 strict，做不到则回退。
    Prefer,
    /// 必须 strict，做不到则报错。
    Require,
}

/// `Tool<TParameters>`；`parameters` 是 JSON Schema（TypeBox 的序列化结果）。
///
/// 工具定义会被原样存进 session / `SystemMessage::tools_added`，因此字段顺序与
/// [`ConstrainedSampling`] 的原始形状都影响回放字节，不能随手归一化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// 工具名（模型可见，用于生成 tool call）。
    pub name: String,
    /// 工具说明（模型可见）。
    pub description: String,
    /// 参数 JSON Schema。
    pub parameters: JsonValue,
    /// 可选的受约束采样配置；缺省表示不启用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

/// `false | ConstrainedSamplingConfig`。
///
/// `Deserialize` 是手写的：TS 的类型只是编译期约束，运行期按属性访问（`config.type`、
/// `config.strict`、`config.variants?.openai_lark`）解释任意 JSON，并且从不改写工具定义
/// （它会原样存进 session / `toolsAdded`）。因此：
/// - `false` / `true` → [`ConstrainedSampling::Disabled`]；
/// - 恰好等于 [`ConstrainedSamplingConfig`] 的规范形状（往返序列化不变）→
///   [`ConstrainedSampling::Config`]；
/// - 其余（缺 `strict`、`variants: null`、未知 `type`、额外键、非对象值……）→
///   [`ConstrainedSampling::Raw`]：保留原始 JSON 供回放逐字写回，resolver 按 TS 的运行期
///   语义解释它（例如缺 `variants` 时的 TypeError 文本）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ConstrainedSampling {
    Disabled(bool),
    Config(ConstrainedSamplingConfig),
    Raw(JsonValue),
}

impl<'de> Deserialize<'de> for ConstrainedSampling {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = JsonValue::deserialize(deserializer)?;
        if let JsonValue::Bool(disabled) = value {
            return Ok(ConstrainedSampling::Disabled(disabled));
        }
        // 只有规范形状才归一成 `Config`；其余保留原始 JSON，序列化时不改写。
        if let Some(config) = config_from_json(&value)
            && serde_json::to_value(&config).ok().as_ref() == Some(&value)
        {
            return Ok(ConstrainedSampling::Config(config));
        }
        Ok(ConstrainedSampling::Raw(value))
    }
}

/// `Context`。
///
/// 一次请求的原始上下文。它允许把 system prompt 与工具放在顶层字段里；送给 provider 前
/// 通常先经 `utils::transcript::normalize_context` 归一化成 [`TranscriptContext`]
/// （此时 `system_prompt` 与 `tools` 都会被清空，内容并入首条 system 消息）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    /// 顶层 system prompt；归一化后为 `None`（已并入首条 system 消息）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// 按时间顺序排列的完整消息序列。
    pub messages: Vec<Message>,
    /// 顶层工具声明；归一化后为 `None`（已并入首条 system 消息的 `toolsAdded`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// `TranscriptContext`：已归一化的上下文（system prompt 与工具声明都在首条 system 消息里）。
///
/// TS 用 branded 类型防止直接构造；Rust 用 newtype 达到同样效果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TranscriptContext(pub Context);

impl TranscriptContext {
    /// 仅 `kiana-ai::utils::transcript::normalize_context` 应调用。
    pub fn from_normalized(context: Context) -> Self {
        TranscriptContext(context)
    }
    /// 借出内部 [`Context`] 的只读引用。要修改请先克隆、改完再重新归一化，
    /// 否则会破坏「system prompt 与工具都在首条 system 消息里」这一不变量。
    pub fn as_context(&self) -> &Context {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Assistant message events
// ---------------------------------------------------------------------------

/// `AssistantMessageEvent`（TS 的判别键是 `type`，其中 toolcall_* 是 snake_case）。
///
/// 流式响应的事件协议：`start` → 若干内容块的 `*_start` / `*_delta` / `*_end` →
/// `done` 或 `error`。字段名经 `rename_all_fields = "camelCase"` 序列化为
/// `contentIndex`/`toolCall`。
///
/// `content_index` 是 `partial.content` 里的下标（不是 provider 的 block index）；
/// `partial` 是**每个事件各自克隆的完整快照**，消费方应从它读取累积状态，而不是自己拼增量。
/// `done`/`error` 没有 `partial`，改为在 `message`/`error` 里携带最终消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AssistantMessageEvent {
    /// 流开始；`partial` 是尚未包含内容块的初始消息（已带路由与时间戳元数据）。
    #[serde(rename = "start")]
    Start { partial: Box<AssistantMessage> },
    /// `content_index` 处的文本块开始；`partial` 已追加该块。
    #[serde(rename = "text_start")]
    TextStart {
        content_index: usize,
        partial: Box<AssistantMessage>,
    },
    /// 文本增量；`partial` 已把 `delta` 合并进对应块。
    #[serde(rename = "text_delta")]
    TextDelta {
        content_index: usize,
        delta: String,
        partial: Box<AssistantMessage>,
    },
    /// 文本块结束；`content` 与 `partial` 中该块的完整文本一致。
    #[serde(rename = "text_end")]
    TextEnd {
        content_index: usize,
        content: String,
        partial: Box<AssistantMessage>,
    },
    /// `content_index` 处的思考块开始；`partial` 已追加该块。
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        content_index: usize,
        partial: Box<AssistantMessage>,
    },
    /// 思考增量；`partial` 已把 `delta` 合并进对应块。
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        content_index: usize,
        delta: String,
        partial: Box<AssistantMessage>,
    },
    /// 思考块结束；`content` 与 `partial` 中该块的完整思考文本一致。
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        content_index: usize,
        content: String,
        partial: Box<AssistantMessage>,
    },
    /// 工具调用块开始；`partial` 已追加该块（参数可能尚未解析）。
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: usize,
        partial: Box<AssistantMessage>,
    },
    /// 工具参数的原始 JSON 文本增量；最终参数以 `toolcall_end` 的 `tool_call` 为准。
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        content_index: usize,
        delta: String,
        partial: Box<AssistantMessage>,
    },
    /// 工具调用块结束；`tool_call` 是解析后的最终结构（参数非法时 `arguments` 为 `{}`
    /// 且带 `arguments_error`），与 `partial` 中的对应块一致。
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: usize,
        tool_call: ToolCall,
        partial: Box<AssistantMessage>,
    },
    /// 正常结束；`message` 是最终完整消息，`reason` 是 [`DoneReason`]。
    #[serde(rename = "done")]
    Done {
        reason: DoneReason,
        message: Box<AssistantMessage>,
    },
    /// 失败结束（含调用方取消）；`error` 是带 `error_message` 的最终消息。
    #[serde(rename = "error")]
    Error {
        reason: ErrorReason,
        error: Box<AssistantMessage>,
    },
}

impl AssistantMessageEvent {
    /// `type` 字段的字面量，用于日志与快照。
    pub fn kind(&self) -> &'static str {
        match self {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
            AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
            AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
        }
    }
    /// 事件附带的 partial 快照（`done` / `error` 没有）。
    pub fn partial(&self) -> Option<&AssistantMessage> {
        match self {
            AssistantMessageEvent::Start { partial }
            | AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => Some(partial),
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Request options
// ---------------------------------------------------------------------------

/// `ToolChoice`。
///
/// 是否允许模型调用工具。`Auto` 由模型决定，`None` 禁止调用；适配器把它写成 provider 的
/// `tool_choice`/`toolChoice` 字段（对象形如 `{"type":"auto"}`，Google 用字符串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    /// 由模型决定是否调用工具。
    Auto,
    /// 禁止调用工具。
    None,
}

/// `ThinkingBudgets`。
///
/// 按思考档位自定义的 token 预算。适配器只在支持显式预算的 API 使用它；
/// `xhigh`/`max` 没有独立字段，经 `clamp_reasoning` 归一到 `high` 后取 `high` 的值，
/// 未设置的档位使用内置默认预算（见 `api::simple_options::thinking_budget_for_level`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    /// `minimal` 档的预算（token）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u64>,
    /// `low` 档的预算（token）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<u64>,
    /// `medium` 档的预算（token）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<u64>,
    /// `high`（以及归一后的 `xhigh`/`max`）档的预算（token）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<u64>,
}

/// `Transport`。
///
/// 传输方式，目前只有 `openai-codex-responses` 适配器解释该字段，其它 API 忽略它。
/// 在 Codex 下凡不是 `Sse` 的取值都会先尝试 WebSocket，失败后按会话粘滞策略回退到 SSE。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// 只走 HTTP SSE。
    Sse,
    /// WebSocket 传输。
    Websocket,
    /// WebSocket 传输并复用缓存会话（序列化为 `"websocket-cached"`）。
    #[serde(rename = "websocket-cached")]
    WebsocketCached,
    /// 自动选择（Codex 缺省）。
    Auto,
}

/// `ProviderHeaders`：值为 `null` 表示抑制同名默认头。
///
/// 注意这与「删除请求头」不同：`Some(value)` 是覆盖，`None`（JSON null）是告诉适配器
/// 不要发送它本会添加的同名默认头。
pub type ProviderHeaders = IndexMap<String, Option<String>>;
/// `ProviderEnv`。
///
/// provider 级环境变量覆盖：查询时先看这里的值，再回落到进程环境（见 `getProviderEnvValue`
/// 的等价实现）。
pub type ProviderEnv = IndexMap<String, String>;

/// `ClientAttribution`。
///
/// 客户端归因信息，作为请求头的一部分告诉 provider 是哪个客户端在调用
/// （例如 OpenRouter 的 `HTTP-Referer`/`X-OpenRouter-Title`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientAttribution {
    /// 客户端名称（如 `"kiana"`）。
    pub name: String,
    /// 客户端主页 URL。
    pub url: String,
    /// 自由分类标签，用于 provider 侧统计。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
}

/// `UserAgentMode = "short" | "detailed" | { custom: string }`。
///
/// TS: `utils/identity.ts#UserAgentMode`。缺省是 `"detailed"`。`Short(String)` 保留开放字符串
/// 形状（TS 的类型层只允许两个字面量，运行期反序列化仍接受任意字符串）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserAgentMode {
    /// `"detailed"`（缺省值）。
    Detailed(UserAgentModeDetailed),
    /// `"short"` 或任意字符串（TS 运行期不校验）。
    Short(String),
    /// `{ custom: string }`。
    Custom { custom: String },
}

/// 固定判别值 `"detailed"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UserAgentModeDetailed {
    #[default]
    #[serde(rename = "detailed")]
    Detailed,
}

impl UserAgentMode {
    /// TS 的 `mode === "short"` 分支。
    pub fn is_short(&self) -> bool {
        matches!(self, UserAgentMode::Short(value) if value == "short")
    }
    /// TS 的 `typeof mode === "object"` 分支。
    pub fn custom(&self) -> Option<&str> {
        match self {
            UserAgentMode::Custom { custom } => Some(custom),
            _ => None,
        }
    }
}

/// `ClientIdentity`。
///
/// `attribution` 的三态（TS: `attribution?: ClientAttribution | null`）：
/// 缺省（`None`）= 不改动；`Some(None)` = JSON `null`（关闭从合并来的 attribution）；
/// `Some(Some(_))` = 设置 attribution。serde 默认会把 `null` 与缺省都映射成 `None`，
/// 因此这里用自定义反序列化保留 `null`（`mergeClientIdentity` 依赖这一区别）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
    /// UA 生成模式；缺省按 `"detailed"` 处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<UserAgentMode>,
    /// 三态：缺省（`None`）= 不改动；`Some(None)` = JSON `null`（关闭已有 attribution）；
    /// `Some(Some(v))` = 设置 attribution。详见结构体说明。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_option"
    )]
    pub attribution: Option<Option<ClientAttribution>>,
    /// 客户端版本号，用于 UA / attribution 请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 把“键存在”与“键为 null”分开：缺省由 `#[serde(default)]` 处理，存在时调用本函数，
/// `null` 反序列化成 `Some(None)`，其余值 `Some(Some(v))`。
fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 保留显式 `null`：`details?: JsonValue` 里省略与 `null` 是两种不同的 JSON 形状
/// （`message-types.test.ts` 断言 `details toBeNull()`）。serde 的 `Option` 默认把 `null`
/// 映射成 `None`，这里把存在（含 null）一律收成 `Some`。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<JsonValue>, D::Error>
where
    D: Deserializer<'de>,
{
    JsonValue::deserialize(deserializer).map(Some)
}

/// `ProviderResponse`。
///
/// 传给 `on_response` 回调的响应头快照（不含响应体）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头，键为 header 名；由调用方按需查找。
    pub headers: IndexMap<String, String>,
}

/// `StreamOptions`：TS 的 `StreamOptions`。回调与取消令牌不参与序列化。
///
/// 由 `api::simple_options::build_base_options` 从 [`SimpleStreamOptions`] 构造，是适配器
/// 读取请求参数的统一入口。字段分两组：上半部是可序列化的数据（适配器写进请求），
/// 下半部是不可序列化的载体（取消、回调、HTTP 客户端、遥测）。每个字段都是 `Option`，
/// `None` 表示「不设置」，由适配器决定默认行为。
#[derive(Default)]
pub struct StreamOptions {
    // --- serializable data -------------------------------------------------
    /// 显式 API key；`None` 时由鉴权层解析（OAuth / 环境变量）。
    pub api_key: Option<String>,
    /// Provider-scoped environment values (override process env).
    pub env: Option<ProviderEnv>,
    /// 请求级附加头；与模型级 headers 合并后发送。
    pub headers: Option<ProviderHeaders>,
    /// 客户端身份（UA / attribution）。
    pub identity: Option<ClientIdentity>,
    /// 单次 HTTP 请求超时（毫秒）。
    pub timeout_ms: Option<u64>,
    /// 重试次数上限（不含首次请求）；`None` 的默认值由适配器决定：SDK 风格默认 2，
    /// 内部 retry helper 默认 0（见 `spec/ai-core.md` 与 `api::classifier_shared`）。
    pub max_retries: Option<u32>,
    /// 服务端要求延迟（Retry-After）的上限（毫秒）；`Some(0)` 表示关闭上限。
    pub max_retry_delay_ms: Option<u64>,
    /// 采样温度；是否真的发送由适配器与模型 compat 决定。
    pub temperature: Option<f64>,
    /// Merged into the request body as-is after named fields.
    pub sampling_params: Option<SamplingParams>,
    /// 输出 token 上限；构造时通常已按上下文窗口钳制。
    pub max_tokens: Option<u64>,
    /// 传输方式；仅 Codex 适配器解释。
    pub transport: Option<Transport>,
    /// 缓存保留偏好，见 [`CacheRetention`]。
    pub cache_retention: Option<CacheRetention>,
    /// 会话 id；用作 prompt cache key 与会话亲和性头。
    pub session_id: Option<String>,
    /// WebSocket 连接超时（毫秒，仅 Codex）。
    pub websocket_connect_timeout_ms: Option<u64>,
    /// provider 元数据；Anthropic 适配器只取其中的 `user_id` 放进请求 `metadata`。
    pub metadata: Option<JsonObject>,
    /// TS 的 `ProviderStreamOptions = StreamOptions & Record<string, unknown>` 尾部，以及各
    /// `ApiStreamOptions<TApi>`（`AnthropicOptions.thinkingEnabled`/`effort`、
    /// `OpenAIResponsesOptions.reasoningEffort`/`reasoningSummary`/`serviceTier` …）的 API 专属
    /// 选项：键为 TS 字段名，值按原样传给适配器（`Models` 只负责透传，不解释内容）。
    /// `Models` 的 `ModelsStreamOptions.reasoning` 也会以 `"reasoning"` 键合并进来。
    pub extra: JsonObject,

    // --- non-serializable carriers (skip) ----------------------------------
    /// Replaces `AbortSignal`.
    ///
    /// 适配器在阶段边界检查 `is_cancelled()` 并返回 aborted 错误；令牌本身不携带取消原因
    /// （原因由更上层的 `signal_reason` 保存），因此不能从它区分「用户取消」与「超时」。
    pub signal: Option<CancellationToken>,
    /// `onPayload(payload, model) -> payload | undefined`.
    pub on_payload: Option<OnPayloadFn>,
    /// 异步版 `onPayload`。
    ///
    /// 扩展桥需要 await 非 `Send` 的 JS handler，不能走同步的 [`OnPayloadFn`]；适配器
    /// 优先使用本字段（见 [`StreamOptions::apply_on_payload`]）。保留同步字段是为了兼容
    /// 内建适配器与既有测试。
    pub on_payload_async: Option<OnPayloadAsyncFn>,
    /// `onResponse(response, model)`.
    pub on_response: Option<OnResponseFn>,
    /// `onProviderStreamEvent(data, model)`.
    pub on_provider_stream_event: Option<OnProviderStreamEventFn>,
    /// Custom `fetch` implementation; the Rust port replaces it with an HTTP client handle.
    ///
    /// `None` 时适配器使用内置的 reqwest 实现（各模块的 `default_http_client()`）。
    pub fetch: Option<Arc<dyn crate::http::HttpClient>>,
    /// Parent telemetry span for this logical request.
    ///
    /// 由 `models` 层读取并据此创建请求观察器（`observe_model_request`）；适配器本身不直接
    /// 使用该字段。`None` 表示不埋点。
    pub telemetry: Option<Arc<dyn kiana_telemetry::TelemetrySpan>>,
}

/// `onPayload(payload, model)`：返回替换后的 payload，或 `None` 表示不修改。
pub type OnPayloadFn = Arc<dyn Fn(JsonValue, &Model) -> Option<JsonValue> + Send + Sync>;

/// 异步版 `onPayload`（扩展桥使用；适配器 `await` 它）。
pub type OnPayloadAsyncFn = Arc<
    dyn for<'a> Fn(JsonValue, &'a Model) -> futures::future::BoxFuture<'a, Option<JsonValue>>
        + Send
        + Sync,
>;

impl StreamOptions {
    /// 应用 `onPayload`：优先异步回调，其次同步回调，都没有则返回 `None`。
    pub async fn apply_on_payload(&self, payload: JsonValue, model: &Model) -> Option<JsonValue> {
        if let Some(callback) = &self.on_payload_async {
            return callback(payload, model).await;
        }
        self.on_payload
            .as_ref()
            .and_then(|callback| callback(payload, model))
    }
}

/// `onResponse(response, model)`。
pub type OnResponseFn = Arc<dyn Fn(ProviderResponse, &Model) + Send + Sync>;

/// `onProviderStreamEvent(data, model)`。
pub type OnProviderStreamEventFn = Arc<dyn Fn(JsonValue, &Model) + Send + Sync>;

/// `SimpleStreamOptions`。
///
/// `streamSimple` 的高层选项：`base` 是通用请求选项，其余是便捷开关。
/// `build_base_options` 只用 `reasoning` 选择 thinking 档位的采样参数；
/// `thinking_budgets`/`tool_choice`/`deferred` 以及 `reasoning` 的其余解释都由适配器
/// 从原始 [`SimpleStreamOptions`] 读取（见 `api/simple_options.rs` 模块文档）。
#[derive(Default)]
pub struct SimpleStreamOptions {
    /// 通用请求选项（鉴权、超时、回调等）。
    pub base: StreamOptions,
    /// 工具调用策略；`None` 表示不写该字段，由 provider 默认行为决定。
    pub tool_choice: Option<ToolChoice>,
    /// 思考档位；`None` 表示不指定档位，支持 thinking 的适配器通常据此关闭推理
    /// （Anthropic 会显式设 `thinking_enabled = false`）。
    pub reasoning: Option<ThinkingLevel>,
    /// `boolean | { window }`；Rust 用枚举表达。
    pub deferred: Option<DeferredRequest>,
    /// 按思考档位覆盖的 token 预算；`None` 使用内置默认预算。
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl StreamOptions {
    /// 逐字段复制（本类型是冻结契约、没有实现 `Clone`）。
    ///
    /// 用列全字段的结构体字面量而不是 `..Default::default()`：上游新增字段时这里会
    /// 编译失败，强制每个拷贝点看到新字段。回调/客户端字段都是 `Arc`/`Option<Arc>`，
    /// clone 只共享句柄，不做深拷贝。
    ///
    /// 所有需要复制请求选项的地方（会话 SDK、预热器、压缩、bug report、agent loop）
    /// 都应调用本方法，避免各自维护一份字段清单而漂移。
    pub fn duplicate(&self) -> Self {
        StreamOptions {
            api_key: self.api_key.clone(),
            env: self.env.clone(),
            headers: self.headers.clone(),
            identity: self.identity.clone(),
            timeout_ms: self.timeout_ms,
            max_retries: self.max_retries,
            max_retry_delay_ms: self.max_retry_delay_ms,
            temperature: self.temperature,
            sampling_params: self.sampling_params.clone(),
            max_tokens: self.max_tokens,
            transport: self.transport,
            cache_retention: self.cache_retention,
            session_id: self.session_id.clone(),
            websocket_connect_timeout_ms: self.websocket_connect_timeout_ms,
            metadata: self.metadata.clone(),
            extra: self.extra.clone(),
            signal: self.signal.clone(),
            on_payload: self.on_payload.clone(),
            on_payload_async: self.on_payload_async.clone(),
            on_response: self.on_response.clone(),
            on_provider_stream_event: self.on_provider_stream_event.clone(),
            fetch: self.fetch.clone(),
            telemetry: self.telemetry.clone(),
        }
    }
}

impl SimpleStreamOptions {
    /// 逐字段复制：`base` 走 [`StreamOptions::duplicate`]。
    pub fn duplicate(&self) -> Self {
        SimpleStreamOptions {
            base: self.base.duplicate(),
            tool_choice: self.tool_choice,
            reasoning: self.reasoning,
            deferred: self.deferred,
            thinking_budgets: self.thinking_budgets.clone(),
        }
    }
}

/// `deferred?: boolean | { window?: "15m" | "1h" | "24h" }`。
///
/// 是否请求延迟响应。当前仓库只有 faux provider 实现该能力（用于测试）：
/// `Enabled(false)` 关闭；对象形态视为启用，`window` 原样交给 provider 解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeferredRequest {
    /// 显式开关。
    Enabled(bool),
    /// 启用并给出可选的等待窗口。
    Window { window: Option<DeferredWindow> },
}

/// `window?: "15m" | "1h" | "24h"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredWindow {
    /// 序列化为 `"15m"`。
    #[serde(rename = "15m")]
    M15,
    /// 序列化为 `"1h"`。
    #[serde(rename = "1h")]
    H1,
    /// 序列化为 `"24h"`。
    #[serde(rename = "24h")]
    H24,
}

/// `DeferredFetchOptions` / `DeferredCancelOptions`。
///
/// 轮询或取消延迟响应时传给 provider 的选项。轮询失败是否重试由 provider 决定，
/// `Models` 层不再重试。
#[derive(Default)]
pub struct DeferredFetchOptions {
    /// 基础请求选项。
    pub base: StreamOptions,
    /// Maximum provider long-poll duration in milliseconds (default 0: one status check).
    pub wait: Option<u64>,
}
/// 取消延迟响应沿用普通 [`StreamOptions`]（API key、headers、取消令牌等）。
pub type DeferredCancelOptions = StreamOptions;

// ---------------------------------------------------------------------------
// Images / classifier
// ---------------------------------------------------------------------------

/// `ImagesContext`：图像生成的输入。
///
/// TS: `types.ts#ImagesContext` —— `{ input: ImagesInputContent[] }`，唯一必填字段。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImagesContext {
    /// 按顺序排列的输入块（文本 / 图片）。
    pub input: Vec<ImagesInputContent>,
}

/// `ImagesInputContent = TextContent | ImageContent`（与 [`UserContentBlock`] 同形）。
pub type ImagesInputContent = UserContentBlock;

/// `ImagesOutputContent = TextContent | ImageContent`。
pub type ImagesOutputContent = UserContentBlock;

/// `ImagesStopReason = "stop" | "error" | "aborted"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesStopReason {
    /// 正常产出结果。
    #[default]
    Stop,
    /// 生成失败。
    Error,
    /// 调用方取消。
    Aborted,
}

/// `AssistantImages`：一次图像生成的结果。
///
/// TS: `types.ts#AssistantImages` —— `{api, provider, model, output, responseId?, usage?,
/// stopReason, errorMessage?, timestamp}`。注意字段是 `output`（不是 `images`），`stopReason` 是
/// [`ImagesStopReason`] 而不是完整的 chat [`StopReason`]。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantImages {
    /// 请求使用的 API 实现名。
    pub api: Api,
    /// provider id。
    pub provider: ProviderId,
    /// 模型 id。
    pub model: String,
    /// 生成的内容块（文本 / 图片）。
    pub output: Vec<ImagesOutputContent>,
    /// provider 返回的响应 id（若提供）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// 图像生成的用量（若 provider 报告）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 生成结束原因（图像生成专用，不是 chat 的 [`StopReason`]）。
    pub stop_reason: ImagesStopReason,
    /// 生成失败时的错误文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Unix 毫秒时间戳。
    pub timestamp: i64,
}

/// `ClassifierQuestion` 的三种形态。
///
/// TS: `types.ts#ClassifierQuestion` —— 每个形态都带 `instructions` 与 `criteria`：
/// `choice` 的 criteria 是 `Record<string, string>`，`score` 的是 `string[]`，
/// `bool` 的是 `{ true, false }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClassifierQuestion {
    /// 从命名候选中单选，返回被选项与各候选概率。
    Choice {
        /// 给分类器的任务说明。
        instructions: String,
        /// 候选标签 → 该标签的说明。
        criteria: IndexMap<String, String>,
    },
    /// 给出期望档位（离散等级索引；`0..criteria.len()-1`）。
    Score {
        /// 给分类器的任务说明。
        instructions: String,
        /// 评分档位标签，按从低到高排列（spec/ai-core.md 的 `ClassifierScoreQuestion`）。
        criteria: Vec<String>,
    },
    /// 是/否判定，返回为真的概率。
    Bool {
        /// 给分类器的任务说明。
        instructions: String,
        /// 两种结果各自的说明。
        criteria: BoolQuestionCriteria,
    },
}

/// `ClassifierBoolQuestion.criteria`：`{ true: string; false: string }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoolQuestionCriteria {
    /// 结果为 true 时的判定说明。
    #[serde(rename = "true")]
    pub r#true: String,
    /// 结果为 false 时的判定说明。
    #[serde(rename = "false")]
    pub r#false: String,
}

/// `ClassifierContext`。
///
/// 分类请求的输入：一段任意 JSON `state`、可选图片，以及按名字编号的问题表。
/// 答案按同一套问题名返回（见 [`ClassifierResult::answers`]）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierContext {
    /// 任意 JSON 状态，原样透传给分类器。
    pub state: JsonObject,
    /// 可选图片输入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageContent>>,
    /// 问题名 → 问题定义；顺序即声明顺序。
    pub questions: IndexMap<String, ClassifierQuestion>,
}

/// `ClassifierAnswer`。
///
/// 分类器的回答，与 [`ClassifierQuestion`] 的三种形态一一对应。数值字段只校验「是数字」，
/// 本 crate 不校验概率/置信度的取值范围（由 provider 与调用方约定）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClassifierAnswer {
    /// 单选回答。
    Choice {
        /// 被选中的候选标签。
        choice: String,
        /// 候选标签 → 概率。
        probabilities: IndexMap<String, f64>,
        /// 整体置信度。
        confidence: f64,
    },
    /// 分值回答。
    Score {
        /// 期望档位下标（0..`criteria.len()-1`，见 `spec/ca-builtin-extensions.md`）。
        score: f64,
        /// 置信度。
        confidence: f64,
    },
    /// 布尔回答。
    Bool {
        /// 结果为真的概率。
        probability: f64,
    },
}

/// `ClassifierStopReason`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStopReason {
    /// 正常完成。
    #[default]
    Stop,
    /// 分类失败。
    Error,
    /// 调用方取消。
    Aborted,
}

/// `ClassifierResult`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
    /// 请求使用的 API 实现名。
    pub api: Api,
    /// provider id。
    pub provider: ProviderId,
    /// 模型 id。
    pub model: String,
    /// 问题名 → 回答，键与 [`ClassifierContext::questions`] 对应。
    pub answers: IndexMap<String, ClassifierAnswer>,
    /// 可选用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 结束原因。
    pub stop_reason: ClassifierStopReason,
    /// 失败时的错误文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Unix 毫秒时间戳。
    pub timestamp: i64,
}

/// `KIANA_ATTRIBUTION`。
///
/// 返回 Kiana 自身的归因信息（name=`kiana`、url=`https://kiana.dev`、
/// categories=`cli-agent`）。每次调用返回新副本，调用方可自由修改。
pub fn kiana_attribution() -> ClientAttribution {
    ClientAttribution {
        name: "kiana".into(),
        url: "https://kiana.dev".into(),
        categories: Some(vec!["cli-agent".into()]),
    }
}

/// 把 provider 返回的 `Model` 强制视为 chat 模型（供 `Provider::stream` 的调用方使用）。
///
/// TS: `utils/model-operations.ts#assertChatModel` —— 失败时抛
/// `ModelsError("provider", `Model ${model.provider}/${model.id} is not a chat model`)`。
/// Rust 侧把同一段文本放进 [`ModelTypeError`] 的 `Display`（== TS 的 `error.message`），
/// 并通过 [`ModelTypeError::code`] / [`ModelTypeError::name`] 暴露 `ModelsError` 的字段。
pub fn require_chat(model: &Model) -> Result<&Model, ModelTypeError> {
    match model.model_type.unwrap_or(ModelType::Chat) {
        ModelType::Chat => Ok(model),
        actual => Err(ModelTypeError {
            provider: model.provider.clone(),
            id: model.id.clone(),
            actual,
        }),
    }
}

/// 模型类型不匹配（TS 的 `assertChatModel` 抛出的 `ModelsError`）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Model {provider}/{id} is not a chat model")]
pub struct ModelTypeError {
    /// 模型所属 provider。
    pub provider: String,
    /// 模型 id。
    pub id: String,
    /// 模型实际类型（不是 chat）。
    pub actual: ModelType,
}

impl ModelTypeError {
    /// TS `error.name`。
    pub fn name(&self) -> &'static str {
        "ModelsError"
    }
    /// TS `error.code`。
    pub fn code(&self) -> crate::error::ModelsErrorCode {
        crate::error::ModelsErrorCode::Provider
    }
    /// 转成 crate 的统一错误（`Display` 是稳定的错误文本契约）。
    pub fn into_models_error(self) -> crate::error::ModelsError {
        crate::error::ModelsError::new(crate::error::ModelsErrorCode::Provider, self.to_string())
    }
}
