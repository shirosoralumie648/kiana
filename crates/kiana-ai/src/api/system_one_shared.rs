//! api/system-one-shared.ts。
//!
//! TypeSafe 的 System One 协议在多个服务上的共享实现：请求封装、图片编码方式
//! （`assets` / `images` / `state-image-urls`）、bool ↔ 线上 `noul` 的映射与答案解析。
//!
//! TS: `packages/ai/src/api/system-one-shared.ts`。
//!
//! # 这个文件解决什么问题
//!
//! System One 是 TypeSafe 的「分类器」协议：把一个 state（任意 JSON）和若干问题发给模型，
//! 换回每个问题的答案（choice / score / bool）与 token 用量。不同服务（TypeSafe、
//! OpenRouter、Cloudflare Workers AI）共用同一套模型输出，但请求信封（`{model, ...request}`
//! 还是 `{model, input}`）、端点路径与图片承载方式不同。本文件把**协议本身**（线上字段、
//! 校验、答案解析）收在一处；各服务只实现 [`SystemOneTransport`] 描述的差异。
//!
//! # 在系统中的位置
//!
//! 调用方：`api::typesafe_system_one`（TypeSafe / OpenRouter）与
//! `api::cloudflare_workers_ai_system_one` 的 `classify` 各自构造一个 transport，再调用
//! [`classify_system_one`]；它们再被 `models` 的 `ProviderClassifier` 包装给上层。
//!
//! 被调用方：HTTP、重试、header 合并与计费都委托给 `api::classifier_shared`
//! （`post_classifier_request` 与 `parse_classifier_usage`）。本文件自身**不做任何 I/O**，
//! 因此可以脱离网络单独测试请求形状与答案解析。
//!
//! # 一次调用的执行流程
//!
//! 1. 校验 `model.api` 与 transport 声明的 API 名一致；不一致时请求不会发出。
//! 2. `wire_request`：把 [`ClassifierContext`] 转成线上请求，bool 题的 `type` 改写为
//!    `"noul"`。
//! 3. 若带图片：校验模型声明了图片输入，按 `compat.imageInput`（逐字段覆盖 transport
//!    的检测值）把图片编码进 `assets` / `images` / `state`，必要时加 opt-in 头。
//! 4. `post_classifier_request` 发送并拿回 JSON；`transport.output` 从各服务的响应信封里
//!    取出 `{answers, usage}`。
//! 5. **先**解析 usage（请求已经被计费），**再**解析 answers；answers 畸形时错误向上抛，
//!    但 usage 已写入结果。
//!
//! # 重要约束
//!
//! - 所有失败都收敛成 [`ClassifierResult`]：`stop_reason` 为 `error`（调用方取消时为
//!   `aborted`），`error_message` 走 TS 的 `formatProviderError(normalizeProviderError(...))`
//!   文案；[`classify_system_one`] 本身不返回 `Result`，也不会因业务错误 panic。
//! - 问题表与 `probabilities` 对象按 JS 对象键序（整数键升序在前、其余保持插入序）遍历，
//!   由 `classifier_shared::js_entries` 提供；返回的 `IndexMap<String, ClassifierAnswer>`
//!   按同一顺序插入。
//! - 图片格式校验按模式分三档：`assets` 只收 PNG/JPEG；`images` 额外收 WebP 且最多 4 张；
//!   `state-image-urls` 不做格式与数量校验。本模块不校验 base64 内容本身。
//!
//! # 注意事项
//!
//! - `state` 的静态类型只能是 `serde_json::Value`：无图片时是对象；`assets` /
//!   `state-image-urls` 模式下会被改写成「图片描述 + 原 state」的数组；`images` 模式下
//!   `state` 保持原对象，图片放在 `images` 字段。
//! - `compat.imageInput` 的未知字符串/非字符串值映射到 [`SystemOneImageInput::Other`]，
//!   与 TS 的 `else` 分支一样落到 `assets`。
//! - 答案解析复刻 JS 的 `value[id]` 原型链语义：缺失的 `"__proto__"` 会读到
//!   `Object.prototype`（一个空对象），因此报「答案类型不对」而不是「没有答案」。

use std::sync::LazyLock;

use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::models::ClassifierOptions;
use crate::types::{
    ClassifierAnswer, ClassifierContext, ClassifierQuestion, ClassifierResult,
    ClassifierStopReason, ImageContent, Model, ProviderHeaders,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error};
use crate::utils::json_parse::json_object_from_map;

use super::classifier_shared::{
    ClassifierHttpError, has_images, is_record, js_entries, json_object_value,
    model_accepts_images, now_unix_ms, parse_classifier_usage, post_classifier_request,
    required_number, url_with_base,
};

/// A System One image asset: base64 file bytes, referenced by its name from the state.
///
/// 线上形状是 `{ "type": "image", "format": "png"|"jpeg", "data_base64": "..." }`。序列化后的
/// 字段名是固定的线上契约，因此结构体**没有** `rename_all`，只有
/// `type` 键做了重命名。它只实现 `Serialize`：资产只出现在请求里，响应里的图片不会走这个类型。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemOneImageAsset {
    /// 固定判别值 `"image"`。
    ///
    /// 由 [`SystemOneAssetKind`] 的单一变体保证，调用方无需也无法写别的值。
    #[serde(rename = "type")]
    pub kind: SystemOneAssetKind,
    /// `"png" | "jpeg"`。
    ///
    /// 本模块只通过 `asset_format` 从 MIME 映射得到它；PNG/JPEG 之外的图片在编码前就被拒绝。
    pub format: SystemOneAssetFormat,
    /// base64 文件字节。
    ///
    /// 原样携带 [`ImageContent::data`]，本模块不解码、不校验大小，也不检查它与 MIME 是否匹配。
    pub data_base64: String,
}

/// `SystemOneImageAsset["type"]`。
///
/// 只有 `Image` 一个取值；用 unit 枚举是为了让序列化结果固定为 `"image"`，且该类型只用于
/// 写出请求（没有 `Deserialize`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SystemOneAssetKind {
    /// 唯一的资产类型，序列化为 `"image"`。
    #[serde(rename = "image")]
    Image,
}

/// `SystemOneImageAsset["format"]`。
///
/// 只有 PNG/JPEG 两种：TS 的 `ASSET_FORMATS` 没有 `image/jpg`，也没有 `image/webp`，
/// 因此 WebP 图片在 `assets` 模式下会被拒绝（`images` 模式才收 WebP）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SystemOneAssetFormat {
    /// 序列化为 `"png"`。
    #[serde(rename = "png")]
    Png,
    /// 序列化为 `"jpeg"`。
    #[serde(rename = "jpeg")]
    Jpeg,
}

/// TypeSafe System One request body without the transport-specific envelope.
///
/// 序列化后就是 TS 的 `SystemOneWireRequest`：字段名保持 snake_case（`data_base64` 等），
/// 外层信封（`{model, ...}` 或 `{model, input}`）由 transport 的 `payload` 添加。
/// `assets` 只在 `Assets`/`Other` 模式填写，`images` 只在 `Images` 模式填写；
/// `StateImageUrls` 两者都不填。未填写的字段会被 `skip_serializing_if` 整个省略，
/// 与 TS 的可选字段一致，不会写出 `null`。
///
/// `state` 只能用 `serde_json::Value`：没有图片时是 `context.state` 的对象；`assets` /
/// `state-image-urls` 模式下会被替换成数组（图片描述在前、原 state 在最后）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemOneWireRequest {
    /// The context state, or with images a list of the images or their asset names followed by the state.
    pub state: Value,
    /// 题目 id → 线上题目对象。
    ///
    /// bool 题在 `wire_request` 里已把判别字段 `type` 从 `"bool"` 改成 `"noul"`，其余字段
    /// （`instructions`、`criteria`）原样保留。
    pub questions: IndexMap<String, Value>,
    /// Images as System One assets, keyed by the names the state references.
    ///
    /// 键名固定为 `<image-1>`、`<image-2>`（从 1 开始），`state` 数组里按同样顺序列出。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<IndexMap<String, SystemOneImageAsset>>,
    /// Images as data URLs, Clef's extension to System One.
    ///
    /// 每项形如 `data:<mime>;base64,<data>`，最多 4 项（见 `MAX_DATA_URL_IMAGES`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

/// How a service takes images (`SystemOneCompat.imageInput`).
///
/// `Other` 覆盖运行期出现的未知值：TS 的 `addImages` 只有 `"images"` 与
/// `"state-image-urls"` 两个分支，其余（含未知字符串、非字符串）都落进 `assets` 分支。
///
/// 取值来源有两层：transport 的 `detect_compat` 给出默认值；`model.compat.imageInput`
/// 非 `null` 时覆盖它（`sendImageOptInHeader` 独立处理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOneImageInput {
    /// TypeSafe's `assets` map plus state references.
    Assets,
    /// Clef's `images` list of data URLs.
    Images,
    /// OpenRouter's `image_url` parts placed before the state.
    StateImageUrls,
    /// 未知的 `compat.imageInput`：按 TS 的 `else` 分支处理成 `assets`。
    Other,
}

impl SystemOneImageInput {
    /// 已知字符串取值。
    ///
    /// 只有三个已注册的字符串被识别；其它字符串、数字、布尔、对象、数组与 `null` 都得到
    /// [`SystemOneImageInput::Other`]。唯一的调用点 `compat_image_input` 已先把 `null`
    /// 当作未设置，因此实际不会走到这里的 `null` 分支。
    fn from_compat_value(value: &Value) -> Self {
        match value {
            Value::String(text) if text == "assets" => SystemOneImageInput::Assets,
            Value::String(text) if text == "images" => SystemOneImageInput::Images,
            Value::String(text) if text == "state-image-urls" => {
                SystemOneImageInput::StateImageUrls
            }
            _ => SystemOneImageInput::Other,
        }
    }
}

/// System One compatibility settings with every field resolved.
///
/// transport 的 `detect_compat` 返回它；`run` 再用 `model.compat` 里对应的两个字段
/// （`imageInput` / `sendImageOptInHeader`）逐项覆盖。两个字段都是 Copy 标量，检测结果
/// 可以直接复制、无需分配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedSystemOneCompat {
    /// 图片的请求形态（`assets` / `images` / `state-image-urls`）。
    pub image_input: SystemOneImageInput,
    /// 带图片时是否附加 `TypeSafe-Danger-Zone-Images: true`。
    pub send_image_opt_in_header: bool,
}

/// TypeSafe's opt-in header for its experimental image input.
///
/// 只在本次请求带图片且 `sendImageOptInHeader` 为真时克隆进 `extra_headers`；header 名
/// 与值 `"true"` 是 TypeSafe 的线上契约，必须保持不变。`classifier_shared` 之后才合并
/// `options.headers`，因此用户仍可覆盖或删除这个头。
static IMAGE_OPT_IN_HEADERS: LazyLock<ProviderHeaders> = LazyLock::new(|| {
    let mut headers = ProviderHeaders::new();
    headers.insert(
        "TypeSafe-Danger-Zone-Images".to_string(),
        Some("true".to_string()),
    );
    headers
});

/// Differences between services that serve System One models.
///
/// 实现方只负责「服务差异」：API 名、错误里的服务名、图片兼容性检测、URL、请求信封与
/// 响应信封；协议校验、图片编码、重试与计费由本文件及 `api::classifier_shared` 负责。
///
/// `Sync` 让 `&dyn SystemOneTransport` 可以跨 `.await` 持有，并使返回的 future 满足
/// `Send`（`&T: Send` 只要求 `T: Sync`）；`Send` 是额外的 trait 约束，当前只借用
/// `&dyn` 的路径不依赖它。
pub trait SystemOneTransport: Send + Sync {
    /// Classifier API implemented by this transport.
    ///
    /// 与 `Model.api` 比较；不一致时 `run` 在发请求前失败，错误为
    /// `Unsupported classifier API: <model.api>`。
    fn api(&self) -> &'static str;
    /// Service name used in error messages.
    ///
    /// 同时用于拼 `formatProviderError` 的 `${label} error` 前缀与各条校验错误文本。
    fn label(&self) -> &'static str;
    /// Compatibility settings for models that do not set them in `compat`.
    ///
    /// 只在本轮带图片时调用；返回的两个字段随后可被 `model.compat` 的对应字段
    /// （`imageInput` / `sendImageOptInHeader`）独立覆盖。
    fn detect_compat(&self, model: &Model) -> ResolvedSystemOneCompat;
    /// Absolute request URL.
    ///
    /// 解析失败返回普通 `ClassifierHttpError`（`Invalid URL`）；`base_url` 末尾有无 `/`
    /// 不影响结果。
    fn url(&self, model: &Model) -> Result<Url, ClassifierHttpError>;
    /// Wraps the System One request in the service's request envelope.
    ///
    /// 负责键序：`model` 必须排在请求字段之前（对应 TS 的 `{ model, ...request }` 或
    /// `{ model, input: request }`）。
    fn payload(&self, model: &Model, request: &SystemOneWireRequest) -> Value;
    /// Extracts the System One output (`{ answers, usage }`) from the service's response envelope.
    ///
    /// `run` 只按 key 读取 `answers`/`usage`，返回值的键序不影响结果；实现通常直接返回
    /// 服务端 map（保留其键序）。信封形状不符时返回普通 `ClassifierHttpError`；折叠后
    /// `stop_reason` 为 `error`（调用方令牌已取消时为 `aborted`）、`error_message` 记录该
    /// 错误，`answers` 保持空。
    fn output(&self, body: &Value) -> Result<IndexMap<String, Value>, ClassifierHttpError>;
}

/// `payload(model, request) = { model, ...request }`（`model` 在前）。
///
/// 手工建 `serde_json::Map` 而不是用 `json!`，是为了在 `preserve_order` 下稳定地把
/// `model` 放在第一个键；序列化后的键序与 TS 的 `{ model, ...request }` 一致。
///
/// `expect` 不会触发：`SystemOneWireRequest` 的字段只含 JSON 值、字符串键 map 与可空标量，
/// 序列化不可能失败。
pub(crate) fn payload_with_model(model: &Model, request: &SystemOneWireRequest) -> Value {
    let serialized = serde_json::to_value(request).expect("SystemOneWireRequest serializes");
    let mut map = serde_json::Map::new();
    map.insert("model".to_string(), Value::String(model.id.clone()));
    if let Value::Object(inner) = serialized {
        for (key, value) in inner {
            map.insert(key, value);
        }
    }
    Value::Object(map)
}

/// `payload(model, request) = { model, input: request }`。
///
/// Cloudflare Workers AI 的信封把整个 System One 请求放进 `input`；`model` 同样排在前面。
/// `expect` 的理由与 `payload_with_model` 相同。
pub(crate) fn payload_with_input(model: &Model, request: &SystemOneWireRequest) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("model".to_string(), Value::String(model.id.clone()));
    map.insert(
        "input".to_string(),
        serde_json::to_value(request).expect("SystemOneWireRequest serializes"),
    );
    Value::Object(map)
}

/// `probabilities(label, value, id)`：非 record 或非有限数字 → 错误；键序保留。
///
/// `value` 是答案里的 `probabilities` 对象；每个值都必须能通过 `required_number`
/// （有限 `f64`，负数与 `-0` 也算合法）。返回 [`IndexMap`]，键序按 JS 对象键序排列：
/// 整数样式的键升序在前、其余保持插入序（`js_entries`）。
///
/// 失败只产生普通 `ClassifierHttpError`（无 HTTP 字段），错误文本是外部契约：
/// `"<label> returned invalid probabilities for <id>"`，或
/// `"<label> returned an invalid probability for <id>.<key>"`。
fn probabilities(
    label: &str,
    value: &Value,
    id: &str,
) -> Result<IndexMap<String, f64>, ClassifierHttpError> {
    let Value::Object(map) = value else {
        return Err(ClassifierHttpError::new(format!(
            "{label} returned invalid probabilities for {id}"
        )));
    };
    let ordered = json_object_from_map(map.clone());
    let mut result = IndexMap::with_capacity(ordered.len());
    for (key, probability) in js_entries(&ordered) {
        let probability =
            required_number(label, probability, &format!("probability for {id}.{key}"))?;
        result.insert(key.clone(), probability);
    }
    Ok(result)
}

/// `answerType(answer)`：`answer.type` 的字符串形态（非字符串视为不匹配）。
///
/// 调用方已用 `is_record` 确认 `answer` 是对象；缺 `type` 或 `type` 不是字符串时返回
/// `None`，与 TS 的 `answer.type !== "<期望值>"` 得到同一个判断。
fn answer_type<'a>(answer: &'a Value, key: &str) -> Option<&'a str> {
    answer.get(key)?.as_str()
}

/// `Object.prototype` 的替身：解析 JSON 的 `value[id]` 在 id 为 `"__proto__"` 且没有自有
/// 属性时读到它（普通对象，能通过 `isRecord`，但缺少 `type`/`noul`）。
///
/// 只建模这一个属性：其它原型属性（`constructor`、`toString` 等）在 JS 里是函数，
/// `isRecord` 同样为假，与「完全缺失」走到同一条错误，因此不需要单独表示。
static OBJECT_PROTOTYPE: LazyLock<Value> = LazyLock::new(|| Value::Object(serde_json::Map::new()));

/// `value[id]` 的 JS 语义：自有属性优先，缺失的 `"__proto__"` 走原型链读到
/// `Object.prototype`（其余原型属性是函数，`isRecord` 同样为假，与缺失一致）。
///
/// 自有属性优先这一点是必要的：`JSON.parse('{"__proto__": ...}')` 会把 `"__proto__"`
/// 建成普通自有属性，serde 的 map 也能存放同名的键，此时必须返回答案本身而不是原型。
fn js_answer<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    value
        .get(id)
        .or_else(|| (id == "__proto__").then(|| &*OBJECT_PROTOTYPE))
}

/// `parseAnswers(label, value, context)`。
///
/// 把服务端返回的 `answers` 原始 JSON 按 [`ClassifierContext`] 的问题定义逐题解析。
/// 返回的 [`IndexMap`] 以**问题**顺序（不是答案对象的顺序）排列，键就是题目 id；答案里
/// 多出来的键被忽略。
///
/// 每个问题都必须给出与题型一致的 `type`：`choice` / `score` / `noul`（线上的 bool）。
/// 数字字段统一走 `required_number`，因此非数字或非有限值都会失败。
///
/// # 失败模式
///
/// 全部是普通 `ClassifierHttpError`（无 HTTP 字段），文本是外部契约：
/// - `answers` 不是对象 → `"<label> returned an unexpected response"`；
/// - 某题缺失或不是对象 → `"<label> did not return an answer for <id>"`；
/// - 类型不符 → `"<label> did not return a choice|score|bool answer for <id>"`；
/// - 概率/分数/置信度非法 → `"<label> returned an invalid ..."`。
///
/// 任一题失败都会让整次解析失败（不会返回部分答案）；调用方 `run` 因此只在成功时
/// 才把结果写进 `output.answers`。
fn parse_answers(
    label: &str,
    value: &Value,
    context: &ClassifierContext,
) -> Result<IndexMap<String, ClassifierAnswer>, ClassifierHttpError> {
    if !is_record(value) {
        return Err(ClassifierHttpError::new(format!(
            "{label} returned an unexpected response"
        )));
    }
    let mut answers: IndexMap<String, ClassifierAnswer> =
        IndexMap::with_capacity(context.questions.len());
    // 按问题遍历（JS 对象键序），而不是按答案对象的顺序：返回值的顺序必须与请求里的
    // 题目顺序一致，测试会直接比较键序列。
    for (id, question) in js_entries(&context.questions) {
        // 缺失的答案用 Null 表示；`__proto__` 例外，会读到原型对象，随后按类型不符报错。
        let answer = js_answer(value, id.as_str()).unwrap_or(&Value::Null);
        if !is_record(answer) {
            return Err(ClassifierHttpError::new(format!(
                "{label} did not return an answer for {id}"
            )));
        }
        match question {
            ClassifierQuestion::Choice { .. } => {
                if answer_type(answer, "type") != Some("choice") {
                    return Err(ClassifierHttpError::new(format!(
                        "{label} did not return a choice answer for {id}"
                    )));
                }
                let Some(choice) = answer.get("choice").and_then(Value::as_str) else {
                    return Err(ClassifierHttpError::new(format!(
                        "{label} did not return a choice answer for {id}"
                    )));
                };
                let probabilities = probabilities(
                    label,
                    answer.get("probabilities").unwrap_or(&Value::Null),
                    id,
                )?;
                let confidence = required_number(
                    label,
                    answer.get("confidence").unwrap_or(&Value::Null),
                    &format!("confidence for {id}"),
                )?;
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Choice {
                        choice: choice.to_string(),
                        probabilities,
                        confidence,
                    },
                );
            }
            ClassifierQuestion::Score { .. } => {
                if answer_type(answer, "type") != Some("score") {
                    return Err(ClassifierHttpError::new(format!(
                        "{label} did not return a score answer for {id}"
                    )));
                }
                let score = required_number(
                    label,
                    answer.get("score").unwrap_or(&Value::Null),
                    &format!("score for {id}"),
                )?;
                let confidence = required_number(
                    label,
                    answer.get("confidence").unwrap_or(&Value::Null),
                    &format!("confidence for {id}"),
                )?;
                answers.insert(id.clone(), ClassifierAnswer::Score { score, confidence });
            }
            ClassifierQuestion::Bool { .. } => {
                if answer_type(answer, "type") != Some("noul") {
                    return Err(ClassifierHttpError::new(format!(
                        "{label} did not return a bool answer for {id}"
                    )));
                }
                let probability = required_number(
                    label,
                    answer.get("noul").unwrap_or(&Value::Null),
                    &format!("probability for {id}"),
                )?;
                answers.insert(id.clone(), ClassifierAnswer::Bool { probability });
            }
        }
    }
    Ok(answers)
}

/// `ASSET_FORMATS`：只有 png/jpeg（没有 `image/jpg`、`image/webp`）。
///
/// `assets` 模式用它把 MIME 转成线上 `format`；映射不到就是不支持该图片格式，由调用方
/// 报错，而不是静默跳过。
fn asset_format(mime_type: &str) -> Option<SystemOneAssetFormat> {
    match mime_type {
        "image/png" => Some(SystemOneAssetFormat::Png),
        "image/jpeg" => Some(SystemOneAssetFormat::Jpeg),
        _ => None,
    }
}

/// `DATA_URL_TYPES`：png/jpeg/webp。
///
/// 该函数只判断 MIME 是否在白名单内（`images` 比 `assets` 多接受 WebP）；数量上限由
/// `add_images` 单独检查。
fn data_url_type(mime_type: &str) -> bool {
    matches!(mime_type, "image/png" | "image/jpeg" | "image/webp")
}

/// Clef accepts at most this many images per request.
///
/// 只约束 `images`（data URL）模式；`assets` 与 `state-image-urls` 模式没有数量上限。
const MAX_DATA_URL_IMAGES: usize = 4;

/// Adds the context images to `request` in the service's form.
///
/// 三种模式都会原地改写 `request`，且都假定 `images` 非空（调用方只在
/// `has_images(context)` 为真时调用）：
///
/// - `Images`：校验数量（≤4）与 MIME（PNG/JPEG/WebP），写入 `request.images` 的 data URL 列表；
/// - `StateImageUrls`：把每张图转成 `{type:"image_url", image_url:{url}}`，放到 `state`
///   数组前面，原 `state` 放最后；不做数量/格式校验；
/// - `Assets` / `Other`：MIME 只能是 PNG/JPEG，按出现顺序命名为 `<image-1>`…，
///   写入 `request.assets`，并把资产名列表放到 `state` 数组前面。
///
/// 失败返回普通 `ClassifierHttpError`，错误文本包含 `label` 与模型 id；MIME 校验失败的
/// 两条错误还包含原始 MIME。
/// 所有校验都在写入 `request` 之前完成，因此失败时 `request` 保持调用前的状态。
fn add_images(
    label: &str,
    model: &Model,
    input: SystemOneImageInput,
    images: &[ImageContent],
    request: &mut SystemOneWireRequest,
) -> Result<(), ClassifierHttpError> {
    match input {
        SystemOneImageInput::Images => {
            // 先查数量再逐张查 MIME：数量超限时一次请求都不发，错误文本与 TS 相同顺序。
            if images.len() > MAX_DATA_URL_IMAGES {
                return Err(ClassifierHttpError::new(format!(
                    "{label} accepts at most {MAX_DATA_URL_IMAGES} images for {}, got {}",
                    model.id,
                    images.len()
                )));
            }
            let mut data_urls = Vec::with_capacity(images.len());
            for image in images {
                if !data_url_type(&image.mime_type) {
                    return Err(ClassifierHttpError::new(format!(
                        "{label} accepts PNG, JPEG, and WebP images for {}, got {}",
                        model.id, image.mime_type
                    )));
                }
                data_urls.push(format!("data:{};base64,{}", image.mime_type, image.data));
            }
            request.images = Some(data_urls);
            Ok(())
        }
        SystemOneImageInput::StateImageUrls => {
            // OpenRouter 的形态：图片是 state 数组里的 `image_url` part，原 state 是最后一项。
            let parts: Vec<Value> = images
                .iter()
                .map(|image| {
                    let mut part = serde_json::Map::new();
                    part.insert("type".to_string(), Value::String("image_url".to_string()));
                    let mut image_url = serde_json::Map::new();
                    image_url.insert(
                        "url".to_string(),
                        Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                    );
                    part.insert("image_url".to_string(), Value::Object(image_url));
                    Value::Object(part)
                })
                .collect();
            let mut state = Vec::with_capacity(parts.len() + 1);
            state.extend(parts);
            state.push(request.state.clone());
            request.state = Value::Array(state);
            Ok(())
        }
        SystemOneImageInput::Assets | SystemOneImageInput::Other => {
            // TypeSafe 原生的形态：图片按 `<image-N>` 命名放进 assets，state 只引用名字。
            // 任一张图格式不支持就整单失败（与 TS 一样不做部分发送）。
            let mut assets: IndexMap<String, SystemOneImageAsset> =
                IndexMap::with_capacity(images.len());
            for (index, image) in images.iter().enumerate() {
                let Some(format) = asset_format(&image.mime_type) else {
                    return Err(ClassifierHttpError::new(format!(
                        "{label} accepts PNG and JPEG images for {}, got {}",
                        model.id, image.mime_type
                    )));
                };
                assets.insert(
                    format!("<image-{}>", index + 1),
                    SystemOneImageAsset {
                        kind: SystemOneAssetKind::Image,
                        format,
                        data_base64: image.data.clone(),
                    },
                );
            }
            // 顺序很重要：state 里的名字列表与 assets 的键顺序都来自同一次插入，
            // 服务端按名字回查资产。
            request.state = Value::Array(
                assets
                    .keys()
                    .map(|name| Value::String(name.clone()))
                    .chain(std::iter::once(request.state.clone()))
                    .collect(),
            );
            request.assets = Some(assets);
            Ok(())
        }
    }
}

/// Maps public `bool` questions to TypeSafe's wire-level `noul` type.
///
/// 逐题序列化后只改 bool 题的判别字段：`{"type":"bool",...}` → `{"type":"noul",...}`，
/// 其余字段（`instructions`、`criteria`）逐字保留。`choice`/`score` 的序列化结果已经是
/// 线上形状，不做改动。题目顺序按 `js_entries`（JS 对象键序）写入。
///
/// `expect` 不会触发：`ClassifierQuestion` 的字段只含字符串与字符串容器（map、数组、
/// 两个字符串字段的结构体），序列化不会失败。
fn wire_request(context: &ClassifierContext) -> SystemOneWireRequest {
    let mut questions: IndexMap<String, Value> = IndexMap::with_capacity(context.questions.len());
    for (id, question) in js_entries(&context.questions) {
        // 题目的其余字段（instructions/criteria）由 serde 按原形状写出，只替换 type。
        let mut value = serde_json::to_value(question).expect("ClassifierQuestion serializes");
        if matches!(question, ClassifierQuestion::Bool { .. })
            && let Value::Object(object) = &mut value
        {
            object.insert("type".to_string(), Value::String("noul".to_string()));
        }
        questions.insert(id.clone(), value);
    }
    SystemOneWireRequest {
        state: json_object_value(&context.state),
        questions,
        assets: None,
        images: None,
    }
}

/// `model.compat?.imageInput`（`null`/缺失 → `None`）。
///
/// 只有 `compat` 是对象且字段存在、非 `null` 时才覆盖 transport 的检测值；未知值同样返回
/// `Some(Other)`，从而「覆盖成一个会落到 assets 的值」，与 TS 的可选链 + `??` 行为一致。
fn compat_image_input(model: &Model) -> Option<SystemOneImageInput> {
    let compat = model.compat.as_ref()?.as_object()?;
    match compat.get("imageInput") {
        None | Some(Value::Null) => None,
        Some(value) => Some(SystemOneImageInput::from_compat_value(value)),
    }
}

/// `model.compat?.sendImageOptInHeader`（`null`/缺失 → `None`；其余按 JS truthiness）。
///
/// 「按 JS truthiness」意味着对象与数组恒为真（包括空对象与空数组），字符串 `"false"`
/// 也为真；假值只有 `false`、`0`/`-0`、`NaN`、空串与 `null`。这与 `??`（只跳过
/// `null`/缺失）是两层不同的判断，不能合并。
fn compat_send_image_opt_in(model: &Model) -> Option<bool> {
    let compat = model.compat.as_ref()?.as_object()?;
    match compat.get("sendImageOptInHeader") {
        None | Some(Value::Null) => None,
        Some(value) => Some(kiana_json::js_string::is_truthy(Some(value))),
    }
}

/// `classifySystemOne(transport, model, context, options)` 的错误分支写法：
/// `formatProviderError(normalizeProviderError(error), `${label} error`)`。
///
/// 把 [`ClassifierHttpError`] 的 message/status/body 还原成 TS 的规范化形状再格式化。
/// 注意 `formatProviderError` 的 `label` 前缀**只在有 HTTP 状态时**出现（变成
/// `"<label> error (<status>): ..."`）；普通校验错误保持原始 message，不额外加前缀。
fn error_message(label: &str, error: &ClassifierHttpError) -> String {
    format_provider_error(
        &normalize_provider_error(&error.to_provider_error_value()),
        Some(&format!("{label} error")),
    )
}

/// 实际执行一次分类请求，并把成功得到的结果写进 `output`。
///
/// 与 [`classify_system_one`] 的分工：本函数返回 `Result`，负责「请求 + 解析」；
/// 外层负责把错误折叠成 `ClassifierResult` 的字段。`output` 是唯一的输出通道，
/// 可能在中途被部分填充（例如 usage 已写入但 answers 解析失败）。
///
/// # 执行阶段
///
/// 1. API 名校验：任何 I/O 之前，错误为 `Unsupported classifier API: <model.api>`。
/// 2. 构造线上请求；带图片时依次做模型能力校验、compat 覆盖、图片编码与 opt-in 头。
/// 3. 发送：URL、payload、header 由 transport 提供，重试/超时/鉴权由
///    `classifier_shared` 负责；调用时 `no_retry_statuses` 为空，表示不额外禁用任何
///    状态码的重试。
/// 4. 从响应的 `{answers, usage}` 解析：**usage 先于 answers**，见下面的行内注释。
async fn run(
    transport: &dyn SystemOneTransport,
    model: &Model,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
    output: &mut ClassifierResult,
) -> Result<(), ClassifierHttpError> {
    // 阶段 1：API 名必须是本 transport 实现的协议；不匹配时连 URL 都不解析。
    if model.api != transport.api() {
        return Err(ClassifierHttpError::unsupported_api(&model.api));
    }
    // 阶段 2：先把 context 转成线上请求，再按需追加图片。
    let mut request = wire_request(context);
    let mut extra_headers: Option<ProviderHeaders> = None;
    if has_images(context) {
        // 模型目录没声明 image 输入时直接失败，不把图片发给不支持它的服务。
        if !model_accepts_images(model) {
            return Err(ClassifierHttpError::new(format!(
                "{} does not support image input for {}",
                transport.label(),
                model.id
            )));
        }
        let detected = transport.detect_compat(model);
        // `compat.imageInput` 与 `compat.sendImageOptInHeader` 按字段独立覆盖检测值。
        let image_input = compat_image_input(model).unwrap_or(detected.image_input);
        add_images(
            transport.label(),
            model,
            image_input,
            context.images.as_deref().unwrap_or(&[]),
            &mut request,
        )?;
        let send_opt_in =
            compat_send_image_opt_in(model).unwrap_or(detected.send_image_opt_in_header);
        if send_opt_in {
            extra_headers = Some(IMAGE_OPT_IN_HEADERS.clone());
        }
    }
    // 阶段 3：发送。transport.url 的错误在发请求前返回（`Invalid URL`）。
    let body = post_classifier_request(
        transport.label(),
        transport.url(model)?,
        model,
        transport.payload(model, &request),
        options,
        &[],
        extra_headers.as_ref(),
    )
    .await?;
    // 阶段 4：先拆信封，再解析 usage 与 answers。
    let result = transport.output(&body)?;
    // Set before parsing answers: a request with malformed answers was still billed.
    let usage = parse_classifier_usage(result.get("usage").unwrap_or(&Value::Null), model);
    if let Some(usage) = usage {
        output.usage = Some(usage);
    }
    // answers 解析失败会 `?` 返回；此时 usage 已写入，最终结果仍保留计费信息。
    output.answers = parse_answers(
        transport.label(),
        result.get("answers").unwrap_or(&Value::Null),
        context,
    )?;
    Ok(())
}

/// Runs one System One classification over the given transport.
///
/// 这是本模块对适配器暴露的唯一入口：给定 transport、目标模型、分类上下文与可选请求
/// 参数，返回**总是**一个 [`ClassifierResult`]（不返回 `Result`）。
///
/// # 参数与副作用
///
/// - `transport`：提供服务的 API 名、label、URL、payload/output 信封与图片默认形态。
/// - `model`：必须与 transport 的 `api()` 匹配，且带图时 `input` 必须声明 `image`。
/// - `context`：`state`、`questions` 与可选 `images`；不会被修改。
/// - `options`：API key、超时、重试、header、`onPayload`/`onResponse` 钩子与取消令牌，
///   全部透传给 `classifier_shared`。网络 I/O 与钩子回调都发生在 `.await` 期间。
///
/// # 返回与错误
///
/// 成功时 `stop_reason = stop`，`answers` 按问题顺序填充，`usage` 在服务端返回 token
/// 计数时可计费。失败时不 panic，而是把错误折叠进结果：
///
/// - 调用方取消令牌已触发 → `stop_reason = aborted`；
/// - 其余失败（API 不匹配、图片不支持、格式非法、HTTP/超时/解析错误）→ `error`；
/// - `error_message` 由 `formatProviderError(normalizeProviderError(...))` 生成；
/// - 失败时 `answers` 保持空；`usage` 可能已填（请求已被计费，例如 answers 畸形时）。
pub async fn classify_system_one(
    transport: &dyn SystemOneTransport,
    model: &Model,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
) -> ClassifierResult {
    // 先按 TS 的 `ClassifierResult` 初值构造（`answers: {}`、`stopReason: "stop"`、
    // `timestamp: Date.now()`）；失败分支只覆盖 stop_reason/error_message。
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: IndexMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: now_unix_ms(),
    };
    if let Err(error) = run(transport, model, context, options, &mut output).await {
        // `aborted` 只看调用方令牌是否已取消；超时算 `error`（这是当前稳定契约）。
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
        output.stop_reason = if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        };
        output.error_message = Some(error_message(transport.label(), &error));
    }
    output
}

/// `output(body)` 的通用前置：body 必须是 record，否则 `` `${label} returned an unexpected response` ``。
///
/// 返回的 [`IndexMap`] 保留服务端键序（`serde_json::Map` 在 `preserve_order` 下是插入序）；
/// 这是自然结果，解析方只按键读取 `answers`/`usage`/信封字段，不依赖顺序。非对象
/// （数组、字符串、`null`）都会失败；这是各 transport 共用的响应形状校验。
pub(crate) fn require_output_record(
    label: &str,
    body: &Value,
) -> Result<IndexMap<String, Value>, ClassifierHttpError> {
    match body {
        Value::Object(map) => Ok(json_object_from_map(map.clone())),
        _ => Err(ClassifierHttpError::new(format!(
            "{label} returned an unexpected response"
        ))),
    }
}

/// 便捷函数：把 `url_with_base` 的错误面固定成 TS `new URL` 的 `Invalid URL`。
///
/// 存在的意义是让各 transport 不必重复写错误映射：`systemone`（TypeSafe/OpenRouter）与
/// Cloudflare 的 `run` 相对路径都是在模型的 `base_url` 上拼接。
pub(crate) fn transport_url(base_url: &str, relative: &str) -> Result<Url, ClassifierHttpError> {
    url_with_base(base_url, relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoolQuestionCriteria, ModelInputModality};
    use serde_json::json;

    fn model() -> Model {
        Model {
            id: "jev-latest".into(),
            name: "Jev".into(),
            api: "typesafe-system-one".into(),
            provider: "typesafe".into(),
            base_url: "https://api.typesafe.ai/v1/".into(),
            input: vec![ModelInputModality::Text, ModelInputModality::Image],
            ..Default::default()
        }
    }

    fn context() -> ClassifierContext {
        let mut state = IndexMap::new();
        state.insert("text".to_string(), Value::String("hi".into()));
        let mut criteria = IndexMap::new();
        criteria.insert("success".to_string(), "Successful".to_string());
        criteria.insert("failure".to_string(), "Failed".to_string());
        let mut questions = IndexMap::new();
        questions.insert(
            "category".to_string(),
            ClassifierQuestion::Choice {
                instructions: "Classify the message".into(),
                criteria,
            },
        );
        questions.insert(
            "approved".to_string(),
            ClassifierQuestion::Bool {
                instructions: "Does the user approve?".into(),
                criteria: BoolQuestionCriteria {
                    r#true: "Approval".into(),
                    r#false: "No approval".into(),
                },
            },
        );
        ClassifierContext {
            state,
            images: None,
            questions,
        }
    }

    #[test]
    fn maps_bool_questions_to_noul() {
        let request = wire_request(&context());
        assert_eq!(request.questions["approved"]["type"], json!("noul"));
        assert_eq!(
            request.questions["approved"]["instructions"],
            json!("Does the user approve?")
        );
        assert_eq!(request.questions["category"]["type"], json!("choice"));
    }

    #[test]
    fn assets_state_lists_the_asset_names_before_the_state() {
        let mut request = wire_request(&context());
        let images = vec![
            ImageContent {
                data: "cG5n".into(),
                mime_type: "image/png".into(),
                ..Default::default()
            },
            ImageContent {
                data: "anBlZw==".into(),
                mime_type: "image/jpeg".into(),
                ..Default::default()
            },
        ];
        add_images(
            "System One API",
            &model(),
            SystemOneImageInput::Assets,
            &images,
            &mut request,
        )
        .expect("assets");
        assert_eq!(
            request.state,
            json!(["<image-1>", "<image-2>", {"text": "hi"}])
        );
        let assets = request.assets.expect("assets");
        assert_eq!(assets["<image-1>"].format, SystemOneAssetFormat::Png);
        assert_eq!(assets["<image-2>"].data_base64, "anBlZw==");
        let serialized = serde_json::to_value(&assets["<image-1>"]).expect("serializes");
        assert_eq!(
            serialized,
            json!({"type": "image", "format": "png", "data_base64": "cG5n"})
        );
    }

    #[test]
    fn rejects_unsupported_image_formats() {
        let mut request = wire_request(&context());
        let images = vec![ImageContent {
            data: "d2VicA==".into(),
            mime_type: "image/webp".into(),
            ..Default::default()
        }];
        let error = add_images(
            "System One API",
            &model(),
            SystemOneImageInput::Assets,
            &images,
            &mut request,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "System One API accepts PNG and JPEG images for jev-latest, got image/webp"
        );

        let error = add_images(
            "System One API",
            &model(),
            SystemOneImageInput::Images,
            &vec![images[0].clone(); 5],
            &mut request,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "System One API accepts at most 4 images for jev-latest, got 5"
        );
    }

    #[test]
    fn parses_answers_in_question_order() {
        let answers = json!({
            "approved": {"type": "noul", "noul": 0.95},
            "category": {
                "type": "choice",
                "choice": "success",
                "probabilities": {"success": 0.9, "failure": 0.1},
                "confidence": 0.8
            }
        });
        let parsed = parse_answers("System One API", &answers, &context()).expect("answers");
        let keys: Vec<&str> = parsed.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["category", "approved"]);
        assert_eq!(
            parsed["approved"],
            ClassifierAnswer::Bool { probability: 0.95 }
        );
    }

    #[test]
    fn reports_missing_and_mistyped_answers() {
        let missing = parse_answers("System One API", &json!({}), &context()).unwrap_err();
        assert_eq!(
            missing.message,
            "System One API did not return an answer for category"
        );
        let mistyped = parse_answers(
            "System One API",
            &json!({
                "category": {"type": "choice", "choice": "x", "probabilities": {}, "confidence": 1},
                "approved": {"type": "bool", "noul": 1}
            }),
            &context(),
        )
        .unwrap_err();
        assert_eq!(
            mistyped.message,
            "System One API did not return a bool answer for approved"
        );
    }
}
