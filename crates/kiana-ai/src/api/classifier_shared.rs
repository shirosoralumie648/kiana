//! api/classifier-shared.ts。
//!
//! 分类器适配器的共享层：带 Bearer 鉴权的 JSON POST、`onPayload`/`onResponse` 钩子、
//! 每次尝试独立的超时、provider 重试与错误归一化，以及 usage 计费。
//!
//! TS: `packages/ai/src/api/classifier-shared.ts`。
//!
//! Rust 侧的映射：
//! - 失败路径统一用 [`ClassifierHttpError`] 表达（带 HTTP 字段；非 HTTP 错误只是各字段为空）。
//! - TS 的 `fetch` 换成 [`HttpClient`]；`AbortSignal`/`AbortSignal.timeout` 换成
//!   `CancellationToken` + [`HttpRequest::timeout_ms`]，由客户端负责整条请求的 deadline
//!   （见 `http/reqwest_client.rs` 的模块文档）。
//! - `JSON.stringify(payload)` 走 `kiana_json::js_object::stringify`（JS 数字格式与键序）。
//! - `response.json()` 走 `kiana_json::js_parse::parse`（严格 `JSON.parse`，错误文本对齐 V8）。
//! - 读响应体也在 attempt 的 try/catch 内：deadline 到期导致的读体失败同样改写成超时错误并
//!   参与重试（`AbortSignal.timeout` 的 `aborted` 判定）。
//! - `PostErrorStyle::OpenAi` 额外复刻 OpenAI SDK 的默认 10 分钟超时与
//!   `defaultParseResponse`（204 / `content-length: 0` / 非 JSON content-type）的解析门控。
//!
//! # 这个模块解决什么问题
//!
//! 三个分类器适配器（System One 的两种 transport、OpenAI Decisions）与 llama.cpp 的
//! 分类实现共用同一条 HTTP 流水线：拼 header → 序列化 payload → 发送 → 读 body →
//! 解析 JSON → 归一化错误。把它集中在这里，是为了让「每次尝试的超时、重试与错误形状」
//! 只有一份实现；各适配器只负责 URL、payload 与答案的解析。
//!
//! 主要调用方：`api/system_one_shared.rs`（System One 的两种 transport）、
//! `api/openai_decisions.rs`、`api/llama_cpp_classify.rs`（只调用 `post_json`，不使用
//! `post_classifier_request`；其余辅助符号也来自本模块）与
//! `api/openrouter_images.rs`（唯一使用 `PostErrorStyle::OpenAi` 的调用方）。
//!
//! # 一次典型调用（post_classifier_request）
//!
//! 1. 校验 `options.apiKey` 非空，否则在发请求前返回
//!    `No API key for provider: <provider>`。
//! 2. 若提供 `onPayload` 且返回 `Some(value)`，用返回值替换整个 payload。
//! 3. 合并 header：identity 默认头 → `authorization`/`content-type` → `model.headers`
//!    → `extra_headers` → `options.headers`（后者覆盖前者，`None` 删除）。
//! 4. 进入 `post_json`：payload 只序列化一次，按 `max_retries` 最多重发该次数；每次尝试
//!    都带同一个 `timeout_ms`，由 [`HttpClient`] 为这一条请求重新计算 deadline。
//! 5. 成功（2xx 且能解析）后调用一次 `onResponse`，再返回 JSON。
//!
//! # 设计原因与约束
//!
//! - 错误必须在字段层面与 TS 的 `ClassifierHttpError` 同形：`status`/`body` 决定
//!   [`crate::utils::error_body::normalize_provider_error`] 能提取出什么；`headers` 不参与
//!   归一化，只经 `ProviderError`/`provider_error_from_http` 供重试策略读取。因此超时、
//!   解析失败这类「普通 Error」也要保留 `is_timeout` 与空 `body`，不能只留下一个字符串。
//! - [`retry_provider_request`] 只接收 [`ProviderError`]（message + status + 头查找），
//!   会丢掉 `body`/`is_timeout`。本模块把每次失败的完整错误暂存到
//!   `Arc<Mutex<Option<ClassifierHttpError>>>`，重试结束后按 message 匹配取回（见
//!   `post_json` 尾段）。
//! - header 与 payload 都在进入重试前准备好：TS 每次尝试重建 `Headers` 对象，但输入都是
//!   不可变快照，合并结果相同（`spec/ai-api-other.md` §A.1.2）。
//!
//! # 注意事项
//!
//! - 只有 HTTP 状态错误参与「按状态码重试」；Label 风格下网络/解析/读体错误按 TS 的
//!   `isProviderError` 判定不可重试，超时是例外（改写后以空 status 重试，直至
//!   `max_retries` 用尽）。OpenAi 风格把连接失败/超时映射为可重试的
//!   `APIConnectionError` 语义。
//! - `on_response` 只在成功路径触发一次；非 2xx、读体失败、解析失败都不会触发。
//! - `post_json` 返回的 `ParsedBody` 在 OpenAI SDK 风格下可能是 `Undefined`/`Text`；
//!   分类器路径用 `ParsedBody::into_json` 断言只可能是 JSON。

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::http::reqwest_client::TIMEOUT_MESSAGE;
use crate::http::{ByteStream, HttpClient, HttpError, HttpRequest};
use crate::models::{ClassifierOptions, calculate_cost};
use crate::types::{
    ClientIdentity, JsonObject, Model, ModelInputModality, OnResponseFn, ProviderHeaders,
    ProviderResponse, Usage,
};
use crate::utils::error_body::{ProviderErrorFields, ProviderErrorValue};
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::provider_retry::{
    HeaderLookup, ProviderError, ProviderRetryOptions, retry_provider_request,
};

/// `ProviderRetryOptions` 的 `maxRetries` 缺省值（分类器的 `options.maxRetries ?? 2`）。
const DEFAULT_MAX_RETRIES: u64 = 2;

/// TS 的 `ClassifierHttpError`：一次请求失败的完整可观察面。
///
/// 普通 `Error`（参数校验、畸形响应、超时文案）只是 `status`/`headers` 为 `None`、
/// `body` 为空的实例。
///
/// 这个类型同时是分类器适配器对外的错误类型：适配器在失败分支里调用
/// [`ClassifierHttpError::to_provider_error_value`]，再交给 `normalize_provider_error`
/// 拼成 `ClassifierResult::error_message`。因此字段必须保持完整，不能只保留 message。
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierHttpError {
    /// `error.message`。
    pub message: String,
    /// HTTP 状态；非 HTTP 失败为 `None`（TS 的 `undefined`）。
    pub status: Option<u16>,
    /// 响应头；非 HTTP 失败为 `None`。
    ///
    /// 装箱只为让 `Result<_, ClassifierHttpError>` 不超过 clippy 的 `result_large_err`
    /// 阈值（错误只在失败路径构造）。
    pub headers: Option<Box<IndexMap<String, String>>>,
    /// 原始响应体（未 trim）；非 HTTP 失败为空串。
    pub body: String,
    /// TS 的 `timeoutError`：请求超时（可重试，且 `stopReason` 仍是 `error`）。
    pub is_timeout: bool,
}

impl ClassifierHttpError {
    /// 普通 `Error`。
    ///
    /// 用于参数校验、响应形状不符等非 HTTP 失败：`status`/`headers` 为 `None`、`body`
    /// 为空、`is_timeout` 为 `false`，因此 `normalize_provider_error` 只会得到 message。
    pub fn new(message: impl Into<String>) -> Self {
        ClassifierHttpError {
            message: message.into(),
            status: None,
            headers: None,
            body: String::new(),
            is_timeout: false,
        }
    }

    /// `httpError(label, response, body)`：`` `${label} returned ${status}` ``。
    ///
    /// `headers` 原样保存响应头（不统一大小写），`body` 保留未 trim 的原始文本；
    /// trim/截断由 `normalize_provider_error` 的 `extractBody` 负责。
    pub fn http(label: &str, status: u16, headers: IndexMap<String, String>, body: String) -> Self {
        ClassifierHttpError {
            message: format!("{label} returned {status}"),
            status: Some(status),
            headers: Some(Box::new(headers)),
            body,
            is_timeout: false,
        }
    }

    /// `timeoutError(timeoutMs)`：`` `Request timed out after ${timeoutMs}ms` ``。
    ///
    /// `is_timeout = true` 取代 TS 的 `name = "TimeoutError"`；`status`/`headers` 为
    /// `None`、`body` 为空。重试器本身不读这个字段，是否重试取决于调用方为它构造的
    /// `ProviderError`（见 `post_json` 的两个超时分支）。
    pub fn timeout(timeout_ms: u64) -> Self {
        ClassifierHttpError {
            message: format!("Request timed out after {timeout_ms}ms"),
            status: None,
            headers: None,
            body: String::new(),
            is_timeout: true,
        }
    }

    /// `retryProviderRequest` 在调用方 signal 中止时抛出的 `createAbortError()`。
    ///
    /// `post_json` 在重试器失败后先检查 `CancellationToken::is_cancelled`，命中时统一返回
    /// 这个错误（而不是最后一次尝试的错误）。适配器随后也按 signal 是否取消把
    /// `stop_reason` 判成 `Aborted`，不依赖错误文本。
    pub fn abort() -> Self {
        ClassifierHttpError::new("Request aborted")
    }

    /// `Unsupported classifier API: <api>`。
    ///
    /// `api` 是 `Model.api` 的原文；分类器适配器在模型的 `api` 与自身不匹配时（请求发出前）
    /// 返回这个错误。
    pub fn unsupported_api(api: &str) -> Self {
        ClassifierHttpError::new(format!("Unsupported classifier API: {api}"))
    }

    /// 转成 `normalizeProviderError` 认识的值（`status` 用 `Number` 承载）。
    ///
    /// `body` 总是作为字符串字段存在（空串会被 `extractBody` trim 掉），这样普通 `Error`
    /// 与 `httpError` 走同一条归一化路径，结果与 TS 的字段探测一致。
    ///
    /// `headers` 不复制：`normalize_provider_error` 只探测 status/message/body 几类字段，
    /// 响应头只用于重试决策（走 `ProviderError`，见 `provider_error_from_http`）。
    pub fn to_provider_error_value(&self) -> ProviderErrorValue {
        ProviderErrorValue::Error(ProviderErrorFields {
            message: self.message.clone(),
            status: self.status.map(f64::from),
            body: Some(self.body.clone()),
            ..Default::default()
        })
    }
}

/// `isRecord(value)`：`typeof value === "object" && value !== null && !Array.isArray(value)`。
///
/// Rust 的 JSON `Value` 无法表示类实例、`Date` 等 JS 对象，因此这里把 `Value::Object`
/// 全部当作 record；`Value::Array`、`Value::Null` 与其它标量都返回 `false`。
pub fn is_record(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// `requiredNumber(label, value, field)`：非有限数字 → `` `${label} returned an invalid ${field}` ``。
///
/// 参数：`label` 是错误前缀（适配器名），`field` 是出错的字段名（如 `score`）。
/// 返回 `f64`：负数与 `-0` 都原样保留。错误：值不是 `Value::Number`，或数值不是有限数时，
/// 返回上面那条消息的普通 `ClassifierHttpError`（`status`/`headers` 为空）。
pub fn required_number(
    label: &str,
    value: &Value,
    field: &str,
) -> Result<f64, ClassifierHttpError> {
    match value {
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            if number.is_finite() {
                Ok(number)
            } else {
                Err(ClassifierHttpError::new(format!(
                    "{label} returned an invalid {field}"
                )))
            }
        }
        _ => Err(ClassifierHttpError::new(format!(
            "{label} returned an invalid {field}"
        ))),
    }
}

/// `JSON.stringify(object)` 的 `Value` 形态（`ClassifierContext.state` → 可序列化值）。
///
/// `JsonObject` 是 `IndexMap<String, Value>`，`collect` 后保留插入序；克隆一份是为了把
/// payload 的所有权交给调用方（原 `JsonObject` 仍归调用方所有）。
pub(crate) fn json_object_value(object: &JsonObject) -> Value {
    Value::Object(object.clone().into_iter().collect())
}

/// `JS 的对象自有键顺序`：整数索引键升序在前，其余键保持插入序。
///
/// `context.questions` / choice 题的 `criteria` 都按这个顺序参与序列化与遍历。
///
/// 算法分两趟：先一次遍历把键分成「数组索引键」与「其它键」；索引键按 `u32` 值升序排在
/// 前面，其它键按 `IndexMap` 的插入序接在后面（JS 对非索引字符串键也用插入序）。返回的是
/// 键/值引用，避免克隆。索引键互不相同，因此排序结果确定。时间复杂度 O(n + k log k)，
/// 其中 k 是索引键个数；k 通常远小于 n。
pub(crate) fn js_entries<V>(map: &IndexMap<String, V>) -> Vec<(&String, &V)> {
    let mut indices: Vec<(u32, &String, &V)> = Vec::new();
    let mut others: Vec<(&String, &V)> = Vec::new();
    for (key, value) in map {
        match array_index(key) {
            Some(index) => indices.push((index, key, value)),
            None => others.push((key, value)),
        }
    }
    indices.sort_by_key(|(index, _, _)| *index);
    indices
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(others)
        .collect()
}

/// [`js_entries`] 的键序版本。
pub(crate) fn js_keys<V>(map: &IndexMap<String, V>) -> Vec<&String> {
    js_entries(map).into_iter().map(|(key, _)| key).collect()
}

/// JS 的「数组索引键」判定（`ToString(ToUint32(k)) === k && k !== "4294967295"`）。
///
/// 逐条对应规范：空串、超过 10 个字符、以 `0` 开头的多位串、含非 ASCII 数字字符的串都
/// **不是**索引键；通过后还必须能解析成 `u32` 且不等于 `u32::MAX`。因此 `"01"`、`"-1"`、
/// `"1.0"`、`"4294967295"` 都按普通字符串键处理（保持插入序），`"0"`/`"10"` 参与升序排序。
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    if value == u32::MAX {
        return None;
    }
    Some(value)
}

/// TS 的 `Date.now()`（Unix 毫秒）。分类器结果的 `timestamp`。
///
/// 系统时钟早于 Unix epoch 时返回 0（`map_or(0, ...)`，不会 panic）；亚毫秒部分被
/// `as_millis()` 舍弃，结果再从 `u128` 转成 `i64`。
pub(crate) fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `new URL(relative, `${baseUrl.replace(/\/+$/u, "")}/`)`。
///
/// 只看相对路径的 URL 解析语义（`Url::join` 与 WHATWG 对普通 http(s) base 一致）。
/// 无法解析时给出 TS `new URL` 的 `Invalid URL` 文案。
///
/// 先把 base 末尾所有 `/` 去掉再补一个，因此 base 带不带尾斜杠结果相同；`relative` 以
/// `/` 开头时会替换 base 的路径。base 或相对串非法时返回普通 `ClassifierHttpError`
/// （`Invalid URL`），调用方据此在发请求前失败。
pub(crate) fn url_with_base(
    base_url: &str,
    relative: &str,
) -> Result<url::Url, ClassifierHttpError> {
    let base = format!("{}/", base_url.trim_end_matches('/'));
    let base = url::Url::parse(&base).map_err(|_| ClassifierHttpError::new("Invalid URL"))?;
    base.join(relative)
        .map_err(|_| ClassifierHttpError::new("Invalid URL"))
}

/// 身份默认头（`User-Agent` + attribution）。
///
/// `User-Agent` 总是存在：identity 缺省时 `get_identity_headers` 生成默认的
/// `kiana/<version> (<platform>; <runtime>; <arch>)` 形式；运行时探测失败（wasm 目标或
/// 找不到 node/bun）时退化为 `kiana/<version> (browser)`。attribution 相关头只在 identity
/// 显式提供 attribution 时出现；target 由 provider 名与 `base_url` 共同决定是否需要
/// OpenRouter/NVIDIA/OpenCode 的专用头。
pub(crate) fn identity_headers(
    model: &Model,
    identity: Option<&ClientIdentity>,
) -> ProviderHeaders {
    get_identity_headers(
        &ClientIdentityTarget::new(model.provider.clone(), model.base_url.clone()),
        identity,
    )
}

/// `model.headers` → [`ProviderHeaders`]（值全部为设置，没有 `null`）。
///
/// `model.headers` 内部 map 的类型是 `IndexMap<String, String>`（字段整体为
/// `Option<...>`），因此每个值都是 `Some`，没有 TS `ProviderHeaders` 那种「`null` 删除」
/// 语义；模型未配置头（`None`）与空 map 都得到空的 `ProviderHeaders`，参与合并时不会
/// 产生任何头。
pub(crate) fn model_header_sources(model: &Model) -> ProviderHeaders {
    model
        .headers
        .as_ref()
        .map(|headers| {
            headers
                .iter()
                .map(|(name, value)| (name.clone(), Some(value.clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// 依次合并 header 源：后者覆盖前者（大小写不敏感），`null` 删除该头。
///
/// 实际合并由 [`crate::utils::headers::provider_headers_to_record`] 完成：键统一按小写比较，
/// 值为 `None` 时删除该头，删除后再插入会移到末尾（因此最终顺序取决于最后一次设置）。
/// 所有源都为空或只含删除时返回空 map（`unwrap_or_default`），与 TS 的 `?? {}` 形状一致。
pub(crate) fn merge_header_sources(sources: &[&ProviderHeaders]) -> IndexMap<String, String> {
    let sources: Vec<Option<&ProviderHeaders>> =
        sources.iter().map(|source| Some(*source)).collect();
    provider_headers_to_record(&sources).unwrap_or_default()
}

/// `postClassifierRequest` 的 header：`getIdentityHeaders` → `{authorization, content-type}`
/// → `model.headers` → `extraHeaders` → `options.headers`。
///
/// 合并大小写不敏感，后面的源可以覆盖、甚至用 `None` 删除前面的 `authorization`/
/// `content-type`。`api_key` 由调用方保证非空；本函数只做字符串拼接。
fn request_headers(
    model: &Model,
    api_key: &str,
    extra_headers: Option<&ProviderHeaders>,
    options_headers: Option<&ProviderHeaders>,
    identity: Option<&ClientIdentity>,
) -> IndexMap<String, String> {
    let identity = identity_headers(model, identity);
    let mut base = ProviderHeaders::new();
    base.insert(
        "authorization".to_string(),
        Some(format!("Bearer {api_key}")),
    );
    base.insert(
        "content-type".to_string(),
        Some("application/json".to_string()),
    );
    let model_headers = model_header_sources(model);
    let empty = ProviderHeaders::new();
    let mut sources: Vec<&ProviderHeaders> = vec![&identity, &base, &model_headers];
    sources.push(extra_headers.unwrap_or(&empty));
    sources.push(options_headers.unwrap_or(&empty));
    merge_header_sources(&sources)
}

/// 默认 `fetch`：`http/reqwest_client.rs` 的 [`HttpClient`] 实现（TS 的 `globalThis.fetch`）。
///
/// `LazyLock` 保证进程内只构造一次：成功时所有请求共享同一个 `Arc`（同一个 reqwest 连接池）；
/// 构造失败时错误字符串同样被缓存，之后每次调用都返回同一个错误，不会反复重试初始化。
static DEFAULT_HTTP_CLIENT: LazyLock<Result<Arc<dyn HttpClient>, String>> = LazyLock::new(|| {
    crate::http::reqwest_client::ReqwestHttpClient::new()
        .map(|client| client.shared())
        .map_err(|error| error.message)
});

/// 取共享的默认客户端；初始化失败时把缓存的消息包成普通 [`ClassifierHttpError`]。
fn default_http_client() -> Result<Arc<dyn HttpClient>, ClassifierHttpError> {
    match &*DEFAULT_HTTP_CLIENT {
        Ok(client) => Ok(Arc::clone(client)),
        Err(message) => Err(ClassifierHttpError::new(message.clone())),
    }
}

fn signal_is_cancelled(signal: &Option<CancellationToken>) -> bool {
    signal.as_ref().is_some_and(CancellationToken::is_cancelled)
}

/// 读完整的响应体并解码成文本（`response.text()`：UTF-8 + 去掉 BOM）。
///
/// 与 `response.text()` 一致：按 UTF-8 解码（非法字节序列替换为 U+FFFD），并只去掉开头
/// 一个 BOM。整段 body 先缓冲到内存，内存占用与响应体大小成正比。读流期间的任何错误
/// （包括 deadline 到期）以 [`HttpError`] 原样返回，由调用方决定是否改写成超时。
async fn read_body_text(mut body: ByteStream) -> Result<String, HttpError> {
    use futures::StreamExt;

    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// 一次成功尝试的结果。
struct AttemptSuccess {
    response: ProviderResponse,
    body: ParsedBody,
}

/// 成功响应的解析结果。
///
/// 分类器/llama.cpp（[`PostErrorStyle::Label`]）总是 [`ParsedBody::Json`]；OpenAI SDK 风格
/// 还要复刻 `defaultParseResponse` 的三种返回形态，因为它们决定后续属性访问的报错文案
/// （`undefined` 与 JSON `null` 的错误文本不同，非 JSON content-type 返回原始文本）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ParsedBody {
    /// `JSON.parse(body)`。
    Json(Value),
    /// `undefined`：JSON content-type 但 body 为空（或 `content-length: 0`）。
    Undefined,
    /// 非 JSON content-type：`response.text()` 的原始文本。
    Text(String),
}

impl ParsedBody {
    /// 分类器路径的期望形态（`PostErrorStyle::Label` 只产出 [`ParsedBody::Json`]）。
    pub(crate) fn into_json(self) -> Result<Value, ClassifierHttpError> {
        match self {
            ParsedBody::Json(value) => Ok(value),
            // 不可达：`PostErrorStyle::Label` 的 `post_json` 只解析 JSON。
            ParsedBody::Undefined | ParsedBody::Text(_) => {
                Err(ClassifierHttpError::new("Unexpected response body"))
            }
        }
    }
}

/// 非 2xx 与传输错误的文案风格。
///
/// 由调用方按适配器选择：分类器/llama.cpp 走原生 `fetch` 语义（[`PostErrorStyle::Label`]），
/// openrouter-images 复刻 OpenAI SDK（[`PostErrorStyle::OpenAi`]）。风格同时影响超时文案、
/// 重试默认值、错误构造与成功 body 的解析门控，不能只当作显示格式。
#[derive(Debug, Clone, Copy)]
pub(crate) enum PostErrorStyle<'a> {
    /// 原生 `fetch`（分类器/llama.cpp）：`` `${label} returned ${status}` ``；网络失败保留
    /// `fetch` 文案（`fetch failed`），只有请求超时改写成 `Request timed out after <ms>ms`。
    Label(&'a str),
    /// OpenAI SDK（`openrouter-images`）：`APIError.generate` / `APIConnectionError` 的文案。
    OpenAi,
}

/// OpenAI SDK 的 `DEFAULT_TIMEOUT`：`new OpenAI({...})` 未传 `timeout` 时整条请求的 deadline。
const OPENAI_DEFAULT_TIMEOUT_MS: u64 = 600_000;

/// `Headers.get(name)`（大小写不敏感）。
///
/// 线性扫描并返回第一个匹配值；默认客户端已把响应头键小写，自定义 `HttpClient` 可能保留
/// 原始大小写，因此不能直接按精确键查找。
fn response_header<'a>(headers: &'a IndexMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// OpenAI SDK `defaultParseResponse` 的 content-type / 空 body 门控。
///
/// 只在 2xx 响应的路径上调用（非 2xx 由调用方先构造 status 错误）。阶段：
/// 1. 204 → JSON `null`（SDK 在读 content-type 之前就短路）；
/// 2. 取 `content-type` 分号前的媒体类型，trim 后转小写；
/// 3. 非 JSON 媒体类型 → `Text(原始文本)`，不是错误；
/// 4. JSON 但 `content-length: 0` 或文本为空 → `Undefined`（对应 JS 的 `undefined`）；
/// 5. 其余按严格 `JSON.parse` 解析，失败返回普通 `ClassifierHttpError`（因此不可重试）。
fn parse_openai_body(
    status: u16,
    headers: &IndexMap<String, String>,
    text: String,
) -> Result<ParsedBody, ClassifierHttpError> {
    // `if (response.status === 204) return null;`
    if status == 204 {
        return Ok(ParsedBody::Json(Value::Null));
    }
    // 媒体类型只看分号前的 MIME 部分；大小写不敏感。
    let media_type = response_header(headers, "content-type").map(|content_type| {
        content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase()
    });
    let is_json = media_type.as_deref().is_some_and(|media_type| {
        media_type.contains("application/json") || media_type.ends_with("+json")
    });
    if !is_json {
        return Ok(ParsedBody::Text(text));
    }
    // 声明 JSON 但实际没有 body：Content-Length 显式写 0，或分块传输读到空文本。
    if response_header(headers, "content-length").map(str::trim) == Some("0") || text.is_empty() {
        return Ok(ParsedBody::Undefined);
    }
    // 严格 JSON.parse；错误文本来自 kiana_json（与 V8 对齐），作为普通错误返回。
    match kiana_json::js_parse::parse(&text) {
        Ok(value) => Ok(ParsedBody::Json(value)),
        Err(error) => Err(ClassifierHttpError::new(error.message)),
    }
}

/// TS 的 `timeoutSignal?.aborted && !options.signal?.aborted`。
///
/// 三个条件缺一不可：本次调用配置了超时、`HttpError.message` 等于 [`TIMEOUT_MESSAGE`]、
/// 调用方 signal 尚未取消（用户主动取消优先于超时）。读体超时也会命中这条文案（此时
/// `HttpError.status` 可能带状态码，但这里不看 status）。
fn is_timeout_failure(
    error: &HttpError,
    timeout_ms: Option<u64>,
    signal: &Option<CancellationToken>,
) -> bool {
    timeout_ms.is_some() && error.message == TIMEOUT_MESSAGE && !signal_is_cancelled(signal)
}

/// 超时的用户可见错误：原生 `fetch` 改写成 `Request timed out after <ms>ms`；SDK 用
/// `APIConnectionTimeoutError` 的 `Request timed out.`。
///
/// OpenAi 分支构造的是普通错误（`is_timeout = false`、不带毫秒数），只对齐 SDK
/// `APIConnectionTimeoutError` 的文案；它是否可重试由调用方构造的 `ProviderError` 决定，
/// 而不是这个字段。
fn timeout_error(style: PostErrorStyle<'_>, timeout_ms: u64) -> ClassifierHttpError {
    match style {
        PostErrorStyle::Label(_) => ClassifierHttpError::timeout(timeout_ms),
        PostErrorStyle::OpenAi => ClassifierHttpError::new("Request timed out."),
    }
}

/// OpenAI SDK 对非 2xx 响应构造的错误（`APIError.generate` + `makeStatusError`）。
///
/// - message = `` `${status} ${msg}` ``（`msg` 取 `error.message`，否则 `JSON.stringify(error)`，
///   否则 `status code (no body)`）；
/// - `error` 字段是 SDK 的 `APIError.error`，`normalizeProviderError` 会把它当作 body。
///
/// 阶段与 SDK 的调用链一一对应：
/// 1. 宽松解析 body；解析失败或解析成 JS 假值时，原始文本作为 `errMessage`；
/// 2. `makeStatusError`：解析结果是数组/对象且 `error` 缺失或为 `null` 时，包成
///    `{error: <parsed>}`（JS 的 `typeof [] === "object"`，数组也会被包）；
/// 3. 取出 `errorResponse.error` 作为 `error` 字段；
/// 4. `APIError.makeMessage`：优先非空字符串的 `error.message`，否则对真值做
///    `JSON.stringify`，再否则序列化整个 `error`；都没有时用 `status code (no body)`；
/// 5. `body` 槽位只在 `error` 是普通非空对象时填 `JSON.stringify(error)`，供
///    `normalizeProviderError` 的 `extractBody` 提取；其余形态留空串。
pub(crate) fn openai_status_error(
    status: u16,
    headers: IndexMap<String, String>,
    body: String,
) -> ClassifierHttpError {
    // 阶段 1：解析失败不报错，SDK 会把原始文本当作 errMessage。
    let parsed = kiana_json::js_parse::parse(&body).ok();
    // `const errMessage = errJSON ? undefined : errText;`（JS truthiness）
    let err_message: Option<String> = match &parsed {
        Some(value) if kiana_json::js_string::is_truthy(Some(value)) => None,
        _ => Some(body.clone()),
    };
    // `makeStatusError`：`error && typeof error === 'object' && error.error == null ? { error } : error`
    // （JS 的 `typeof [] === "object"`，数组同样会被包一层。）
    let normalized: Option<Value> = match &parsed {
        Some(value)
            if (value.is_object() || value.is_array())
                && value.get("error").is_none_or(Value::is_null) =>
        {
            let mut wrapper = serde_json::Map::new();
            wrapper.insert("error".to_string(), parsed.clone().unwrap_or(Value::Null));
            Some(Value::Object(wrapper))
        }
        other => other.clone(),
    };
    // `const error = errorResponse?.['error']`
    let error_field: Option<Value> = normalized
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|map| map.get("error"))
        .cloned();
    // `APIError.makeMessage`
    let msg: Option<String> = match &error_field {
        Some(field) if kiana_json::js_string::is_truthy(Some(field)) => {
            match field.as_object().and_then(|map| map.get("message")) {
                Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
                Some(other) if kiana_json::js_string::is_truthy(Some(other)) => {
                    Some(kiana_json::js_object::stringify(other))
                }
                _ => Some(kiana_json::js_object::stringify(field)),
            }
        }
        _ => err_message,
    };
    let message = match msg.filter(|msg| !msg.is_empty()) {
        Some(msg) => format!("{status} {msg}"),
        None => format!("{status} status code (no body)"),
    };
    // `normalizeProviderError` 的 body 槽位：`isPlainNonEmptyObject(error.error)`。
    let body = match &error_field {
        Some(Value::Object(map)) if !map.is_empty() => {
            kiana_json::js_object::stringify(error_field.as_ref().expect("checked"))
        }
        _ => String::new(),
    };
    ClassifierHttpError {
        message,
        status: Some(status),
        headers: Some(Box::new(headers)),
        body,
        is_timeout: false,
    }
}

/// 读响应体失败时的归类。
///
/// 判定顺序：先看是否超时（2xx 与非 2xx 的读体都可能超时），再看 OpenAI 风格的非 2xx
/// （SDK 把读体错误的 message 当 body 构造 status 错误），最后归到其它。三类对应的重试
/// 行为见 `post_json` 里的 `match`：`Timeout` 可重试，`Status` 由状态码决定，`Other`
/// 立即失败。
#[derive(Debug, Clone, Copy)]
enum BodyReadFailure {
    /// 超时（TS 的 `timeoutSignal?.aborted` 改写）：可重试。
    Timeout,
    /// OpenAI SDK 的非 2xx：SDK 把读体错误的 message 当作 body 构造 status 错误。
    Status,
    /// 其他读体/网络失败：不是 `ProviderError`，不重试。
    Other,
}

/// 把 HTTP 失败映射成重试器认识的 `ProviderError`（状态 + 头查找）。
///
/// 响应头被克隆进 `HeaderLookup` 闭包：`ProviderError` 需要拥有数据（`'static`），而重试
/// 发生在本次 attempt 结束之后。查找大小写不敏感，因为自定义 `HttpClient` 未必把键小写。
/// `status` 原样保留，供 `is_retryable_provider_error` 与 `no_retry_statuses` 使用。
fn provider_error_from_http(
    error: &ClassifierHttpError,
    headers: &IndexMap<String, String>,
) -> ProviderError {
    let headers = headers.clone();
    // `Headers.get` 大小写不敏感（默认客户端已把键小写，自定义 fetch 未必）。
    let lookup: HeaderLookup = Some(Arc::new(move |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }));
    ProviderError {
        message: error.message.clone(),
        status: error.status,
        header: lookup,
    }
}

/// 非 HTTP 失败（网络/解析/读体）在 TS 里不是 `ProviderError`（没有 `status`/`headers`
/// 属性），因此**不重试**。Rust 的重试器只看 `ProviderError`，这里用一个必然不可重试的
/// 状态值表达「立即失败」；该状态不会出现在最终错误文本里（最终错误取自 stash 的完整错误）。
fn provider_error_not_retryable(error: &ClassifierHttpError) -> ProviderError {
    ProviderError {
        message: error.message.clone(),
        status: Some(400),
        header: None,
    }
}

/// 暂存一次尝试的完整错误。重试器只携带 `ProviderError`，`body`/`is_timeout` 必须靠这里
/// 保住；各次尝试串行执行，锁本身不会并发争用。锁中毒时仍取回内部值：这里只存诊断数据，
/// 恢复到半写状态不会影响后续的克隆。
fn stash(slot: &Mutex<Option<ClassifierHttpError>>, error: &ClassifierHttpError) {
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.clone());
}

/// 带重试的 JSON POST（内部实现）。返回最终成功的响应与解析后的 body。
///
/// `payload` 必须是 `onPayload` 处理后的最终值；`onResponse` 由调用方在成功后调用
/// （llama.cpp 的 `/tokenize`、`/apply-template` 不触发钩子）。
///
/// # 执行流程
///
/// 1. 解析本次调用使用的 `HttpClient`（`options.fetch` 或进程共享的默认实现）、取消令牌、
///    超时与重试参数。
/// 2. 把 `payload` 序列化成字符串一次，构造可重复调用的 attempt 闭包。
/// 3. attempt 内：发请求 →（OpenAi 风格）204 快速返回 → 读完整 body → 非 2xx 转错误
///    → 解析成功 body。
/// 4. [`retry_provider_request`] 依据 `ProviderError` 的 status/响应头决定是否重试与退避。
/// 5. 失败后优先返回 stash 的完整错误；若错误来自重试器自身（例如服务器要求的退避超过
///    上限），保留重试器给出的文案，构造普通错误。
///
/// # 错误与重试
///
/// - HTTP 非 2xx：保留 status/headers/body，重试与否由
///   `is_retryable_provider_error` 与 `no_retry_statuses` 共同决定。
/// - 超时：改写成各自的超时文案并以空 status 重试；调用方 signal 已取消时不算超时。
/// - Label 风格的网络/解析/读体错误不是 TS 的 `ProviderError`，只尝试一次；OpenAi 风格把
///   连接失败/超时映射为可重试的 `APIConnectionError` 语义。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn post_json(
    style: PostErrorStyle<'_>,
    url: String,
    headers: IndexMap<String, String>,
    payload: &Value,
    options: Option<&ClassifierOptions>,
    no_retry_statuses: &[u16],
) -> Result<(ProviderResponse, ParsedBody), ClassifierHttpError> {
    // 阶段 1：选定客户端与重试参数。`options` 缺省时用共享默认客户端、无取消令牌、
    // 无调用方超时（OpenAi 风格随后会补上 SDK 的默认 10 分钟）；重试次数与超时的
    // 风格差异在下面两段分别处理。
    let client = match options.and_then(|options| options.fetch.clone()) {
        Some(client) => client,
        None => default_http_client()?,
    };
    let signal = options.and_then(|options| options.signal.clone());
    let timeout_ms = options.and_then(|options| options.timeout_ms);
    // OpenAI SDK 的 `this.timeout` 缺省是 `DEFAULT_TIMEOUT`（requestOptions 不传 `timeout`），
    // 因此 openrouter-images 在未设置 `timeoutMs` 时也有 10 分钟的整条请求 deadline。
    let effective_timeout_ms = match style {
        PostErrorStyle::OpenAi => timeout_ms.or(Some(OPENAI_DEFAULT_TIMEOUT_MS)),
        PostErrorStyle::Label(_) => timeout_ms,
    };
    // 分类器/llama.cpp：`options.maxRetries ?? 2`；openrouter-images 直接把 `maxRetries`
    // 透传给 `retryProviderRequest`（缺省 → 0 次重试）。
    let max_retries = options
        .and_then(|options| options.max_retries)
        .map(u64::from)
        .unwrap_or(match style {
            PostErrorStyle::Label(_) => DEFAULT_MAX_RETRIES,
            PostErrorStyle::OpenAi => 0,
        });
    let retry_options = ProviderRetryOptions {
        max_retries: Some(max_retries),
        max_retry_delay_ms: options.and_then(|options| options.max_retry_delay_ms),
        signal: signal.clone().map(crate::utils::abort::AbortScope::from),
        no_retry_statuses: no_retry_statuses.to_vec(),
    };
    // 重试器只把 `ProviderError`（message/status/头）传回来；完整错误暂存在这里，
    // 成功时随函数返回被丢弃，失败时在尾段取回。
    let last_error: Arc<Mutex<Option<ClassifierHttpError>>> = Arc::new(Mutex::new(None));
    // 阶段 2：body 只序列化一次；所有尝试发送同一份 UTF-8 字节。
    let body_text = kiana_json::js_object::stringify(payload);

    // attempt 必须是 `FnMut`，会被重试器反复调用；闭包按值移动进
    // `retry_provider_request`，所以外层先克隆一份交给闭包，内层每次调用再克隆一份交给
    // `async move`（每次尝试各自拥有数据）。
    let attempt = {
        let client = Arc::clone(&client);
        let last_error = Arc::clone(&last_error);
        let headers = headers.clone();
        let body_text = body_text.clone();
        let url = url.clone();
        let signal = signal.clone();
        move || {
            let client = Arc::clone(&client);
            let last_error = Arc::clone(&last_error);
            let headers = headers.clone();
            let body_text = body_text.clone();
            let url = url.clone();
            let signal = signal.clone();
            async move {
                // 每次尝试都新建 `HttpRequest`：`timeout_ms` 交给客户端后，由客户端为这
                // 一条请求计算新的 deadline（等价 TS 每次尝试新建 `AbortSignal.timeout`）。
                let request = HttpRequest {
                    method: "POST".to_string(),
                    url: url.clone(),
                    headers: headers
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect(),
                    body: Some(body_text.into_bytes()),
                    timeout_ms: effective_timeout_ms,
                };
                match client.execute(request, signal.as_ref()).await {
                    Ok(response) => {
                        let status = response.response.status;
                        let response_headers = response.response.headers.clone();
                        // SDK：204 不读 body（`fetch` 也拒绝读），直接得到 `null`。
                        if matches!(style, PostErrorStyle::OpenAi) && status == 204 {
                            return Ok(AttemptSuccess {
                                response: ProviderResponse {
                                    status,
                                    headers: response_headers,
                                },
                                body: ParsedBody::Json(Value::Null),
                            });
                        }
                        let body = match read_body_text(response.body).await {
                            Ok(body) => body,
                            Err(error) => {
                                let failure =
                                    if is_timeout_failure(&error, effective_timeout_ms, &signal) {
                                        BodyReadFailure::Timeout
                                    } else if matches!(style, PostErrorStyle::OpenAi)
                                        && !(200..=299).contains(&status)
                                    {
                                        BodyReadFailure::Status
                                    } else {
                                        BodyReadFailure::Other
                                    };
                                let error = match failure {
                                    // 读 body 时超时：TS 的 try/catch 同样会改写（2xx 与
                                    // 非 2xx 都算），且超时错误按策略可重试。
                                    BodyReadFailure::Timeout => {
                                        timeout_error(style, effective_timeout_ms.expect("checked"))
                                    }
                                    // SDK：`await response.text().catch(err => err.message)`
                                    // 之后照常构造 status 错误（重试性由状态码决定）。
                                    BodyReadFailure::Status => {
                                        openai_status_error(status, response_headers, error.message)
                                    }
                                    BodyReadFailure::Other => {
                                        ClassifierHttpError::new(error.message)
                                    }
                                };
                                stash(&last_error, &error);
                                return Err(match failure {
                                    BodyReadFailure::Timeout => ProviderError {
                                        message: error.message.clone(),
                                        status: None,
                                        header: None,
                                    },
                                    BodyReadFailure::Status => {
                                        let headers =
                                            error.headers.as_deref().cloned().unwrap_or_default();
                                        provider_error_from_http(&error, &headers)
                                    }
                                    BodyReadFailure::Other => provider_error_not_retryable(&error),
                                });
                            }
                        };
                        // 非 2xx：body 已完整读出（TS 也是先 text() 再抛），status/headers/
                        // body 全部保留在错误里，既供上层展示，也供重试策略读取响应头。
                        if !(200..=299).contains(&status) {
                            let error = match style {
                                PostErrorStyle::Label(label) => {
                                    ClassifierHttpError::http(label, status, response_headers, body)
                                }
                                PostErrorStyle::OpenAi => {
                                    openai_status_error(status, response_headers, body)
                                }
                            };
                            stash(&last_error, &error);
                            let headers = error.headers.as_deref().cloned().unwrap_or_default();
                            return Err(provider_error_from_http(&error, &headers));
                        }
                        // 2xx 的 body 解析：Label 直接严格 JSON.parse；OpenAi 还要复刻 SDK
                        // 的 content-type/空 body 门控，因此可能得到 Undefined/Text。
                        // 解析失败是普通错误：不重试，但仍 stash 以保留原始错误文本。
                        let parsed = match style {
                            PostErrorStyle::Label(_) => kiana_json::js_parse::parse(&body)
                                .map(ParsedBody::Json)
                                .map_err(|parse_error| {
                                    ClassifierHttpError::new(parse_error.message)
                                }),
                            PostErrorStyle::OpenAi => {
                                parse_openai_body(status, &response_headers, body)
                            }
                        };
                        match parsed {
                            Ok(body) => Ok(AttemptSuccess {
                                response: ProviderResponse {
                                    status,
                                    headers: response_headers,
                                },
                                body,
                            }),
                            Err(error) => {
                                stash(&last_error, &error);
                                Err(provider_error_not_retryable(&error))
                            }
                        }
                    }
                    // 传输层失败（连接/超时/取消）：Label 风格保留 fetch 原文案，只有超时
                    // 改写；OpenAi 风格统一成 APIConnectionError 的两种文案，两者都可重试。
                    Err(error) => {
                        let timed_out = is_timeout_failure(&error, effective_timeout_ms, &signal);
                        let (error, retryable) = match style {
                            // `AbortSignal.timeout` 的文案在调用方 signal 未中止时改写成
                            // `Request timed out after <ms>ms`（可重试；status/headers 为空）。
                            PostErrorStyle::Label(_) => {
                                if timed_out {
                                    (
                                        timeout_error(
                                            style,
                                            effective_timeout_ms.expect("checked"),
                                        ),
                                        true,
                                    )
                                } else {
                                    (ClassifierHttpError::new(error.message), false)
                                }
                            }
                            // OpenAI SDK：连接失败/超时都是 `APIConnectionError`
                            // （`APIError` 的 `status`/`headers` 属性存在但为 undefined，可重试）。
                            PostErrorStyle::OpenAi => {
                                let message = if timed_out {
                                    "Request timed out."
                                } else {
                                    "Connection error."
                                };
                                (ClassifierHttpError::new(message), true)
                            }
                        };
                        stash(&last_error, &error);
                        if retryable {
                            Err(ProviderError {
                                message: error.message.clone(),
                                status: None,
                                header: None,
                            })
                        } else {
                            Err(provider_error_not_retryable(&error))
                        }
                    }
                }
            }
        }
    };

    match retry_provider_request(attempt, retry_options).await {
        Ok(success) => Ok((success.response, success.body)),
        Err(error) => {
            // TS 的 `retryProviderRequest` 在调用方 signal 中止时抛 `AbortError`。
            // 取消优先于最后一次尝试的错误：即使错误是超时，也统一返回 abort。
            if signal_is_cancelled(&signal) {
                return Err(ClassifierHttpError::abort());
            }
            let stashed = last_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            match stashed {
                // 重试器把 ProviderError 折叠成 `AiError`（只剩 message）；message 相同说明
                // 就是最后一次尝试的错误，用 stash 的完整错误（status/headers/body）。
                // 不同则来自重试器自身（如 `Server requested ... retry delay`），保留其文本。
                Some(stashed) if stashed.message == error.message => Err(stashed),
                _ => Err(ClassifierHttpError::new(error.message)),
            }
        }
    }
}

/// `postClassifierRequest`：带 Bearer 鉴权的 JSON POST。
///
/// `extra_headers` 是 Rust 对 TS「用展开语法造一个新 options 对象」的等价物：它插在
/// `model.headers` 之后、`options.headers` 之前（System One 的
/// `TypeSafe-Danger-Zone-Images` 头）。
///
/// # 参数与副作用
///
/// - `body` 在调用 `onPayload` 前克隆一份（回调接收所有权）；回调返回 `None` 表示不修改。
/// - `onPayload` 在进入重试前只调用一次，`onResponse` 在成功后只调用一次；两者都不会在
///   每次重试时重复触发。
/// - `options.headers` 与 `extra_headers` 都只在函数开头合并一次，之后逐次克隆给每个
///   attempt；合并顺序见 `request_headers`。
///
/// # 返回与错误
///
/// 返回解析后的 JSON body。缺少或空串 `apiKey` 时在发请求前返回
/// `No API key for provider: <provider>`；HTTP/传输/解析/超时失败返回
/// [`ClassifierHttpError`]；重试以失败告终且调用方 signal 已取消时返回
/// [`ClassifierHttpError::abort`]（成功的响应优先）。
pub async fn post_classifier_request(
    label: &str,
    url: url::Url,
    model: &Model,
    body: Value,
    options: Option<&ClassifierOptions>,
    no_retry_statuses: &[u16],
    extra_headers: Option<&ProviderHeaders>,
) -> Result<Value, ClassifierHttpError> {
    // `if (!options?.apiKey)`：缺省或空串都算没有 key。
    let Some(api_key) = options
        .and_then(|options| options.api_key.as_deref())
        .filter(|api_key| !api_key.is_empty())
    else {
        return Err(ClassifierHttpError::new(format!(
            "No API key for provider: {}",
            model.provider
        )));
    };
    // onPayload 只在进入重试前调用一次；`payload.clone()` 是因为回调按值接收 payload。
    let mut payload = body;
    if let Some(options) = options
        && let Some(transformed) = options.apply_on_payload(payload.clone(), model).await
    {
        payload = transformed;
    }
    // header 只合并这一次；post_json 每次尝试克隆这份记录构造新的 HttpRequest。
    let headers = request_headers(
        model,
        api_key,
        extra_headers,
        options.and_then(|options| options.headers.as_ref()),
        options.and_then(|options| options.identity.as_ref()),
    );
    // 提前借出回调（`Option<&OnResponseFn>`），成功返回后再调用；失败路径因 `?` 提前
    // 返回，onResponse 不会触发。
    let on_response: Option<&OnResponseFn> =
        options.and_then(|options| options.on_response.as_ref());
    let (response, body) = post_json(
        PostErrorStyle::Label(label),
        url.to_string(),
        headers,
        &payload,
        options,
        no_retry_statuses,
    )
    .await?;
    if let Some(on_response) = on_response {
        on_response(
            ProviderResponse {
                status: response.status,
                headers: headers_to_record(&response.headers),
            },
            model,
        );
    }
    body.into_json()
}

/// `parseClassifierUsage(value, model)`：`{input_tokens, output_tokens}` → 计费后的 [`Usage`]。
///
/// 两个字段都缺失（或值不是 record）时返回 `None`；缺一个时按 0 计。
///
/// TS 的 `Usage` token 计数是任意 `Number`（可带小数、负数或非有限值），Rust 的 [`Usage`]
/// 字段是 `u64`（`types.ts`，ARCHITECTURE §4.9 冻结），因此这里截断/饱和到整数，费用
/// （`calculate_cost` 只接受 u64）也按截断后的计数计算。真实 API 返回的都是整数；小数
/// usage 的差异（tokens 与 price）由冻结契约决定，无法在本单元修复。
///
/// 其余字段固定：`cache_read = 0`、`cache_write = 0`、`total_tokens = input + output`
/// （饱和加法）。`calculate_cost` 会按模型目录价格（含分档）写入 `usage.cost`，并把
/// `total_from_provider` 设为 `Some(false)`。
pub fn parse_classifier_usage(value: &Value, model: &Model) -> Option<Usage> {
    let Value::Object(map) = value else {
        return None;
    };
    if !map.contains_key("input_tokens") && !map.contains_key("output_tokens") {
        return None;
    }
    let input = token_count(map.get("input_tokens"));
    let output = token_count(map.get("output_tokens"));
    let mut usage = Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        total_tokens: input.saturating_add(output),
        cost: Default::default(),
        ..Default::default()
    };
    calculate_cost(model, &mut usage);
    Some(usage)
}

/// `tokenCount(value)`：有限且 `> 0` 的数字，否则 0。
///
/// 负数、0、NaN/Infinity 与非数字一律记 0（对应 TS 的
/// `typeof v === "number" && Number.isFinite(v) && v > 0`）；正小数向零截断，超过
/// `u64::MAX` 的值按 Rust 的浮点转整数规则饱和。
fn token_count(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            if number.is_finite() && number > 0.0 {
                number as u64
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// 模型是否声明了图片输入（`model.input.includes("image")`）。
///
/// 只看模型目录里声明的输入模态，不看本次 `context` 是否真的带图；后者由
/// [`has_images`] 判断。
pub(crate) fn model_accepts_images(model: &Model) -> bool {
    model.input.contains(&ModelInputModality::Image)
}

/// `context.images?.length` 的真值（`Some([])` 与 `None` 都是假）。
///
/// 对应 JS 的 `length` 真值判断：空数组长度为 0，因此与「没有 images」等价；调用方据此
/// 决定是否走图片分支（例如发送 opt-in 头、校验模型是否支持图片）。
pub(crate) fn has_images(context: &crate::types::ClassifierContext) -> bool {
    context
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error_body::{format_provider_error, normalize_provider_error};
    use serde_json::json;

    #[test]
    fn is_record_matches_the_js_type_guard() {
        assert!(is_record(&json!({})));
        assert!(is_record(&json!({"a": 1})));
        assert!(!is_record(&json!([])));
        assert!(!is_record(&Value::Null));
        assert!(!is_record(&json!("x")));
        assert!(!is_record(&json!(1)));
    }

    #[test]
    fn required_number_rejects_non_numbers_and_non_finite_values() {
        assert_eq!(required_number("L", &json!(1.5), "score").unwrap(), 1.5);
        assert_eq!(required_number("L", &json!(-0.0), "score").unwrap(), 0.0);
        for value in [json!("1"), json!(null), json!(true), json!([]), json!({})] {
            let error = required_number("L", &value, "score").unwrap_err();
            assert_eq!(error.message, "L returned an invalid score");
        }
    }

    #[test]
    fn js_entries_orders_index_keys_first() {
        let map: IndexMap<String, i32> = [
            ("b".to_string(), 2),
            ("10".to_string(), 10),
            ("a".to_string(), 1),
            ("2".to_string(), 2),
            ("01".to_string(), 1),
        ]
        .into_iter()
        .collect();
        let keys: Vec<&str> = js_entries(&map)
            .into_iter()
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(keys, vec!["2", "10", "b", "a", "01"]);
    }

    #[test]
    fn parses_usage_and_prices_it_from_the_catalog() {
        let model = Model {
            cost: crate::types::ModelCost {
                input: 0.042,
                ..Default::default()
            },
            ..Default::default()
        };
        let usage =
            parse_classifier_usage(&json!({"input_tokens": 308, "output_tokens": 23}), &model)
                .expect("usage");
        assert_eq!(usage.input, 308);
        assert_eq!(usage.output, 23);
        assert_eq!(usage.total_tokens, 331);
        assert!((usage.cost.total - 0.000012936).abs() < 1e-15);
        assert_eq!(usage.cost.total_from_provider, Some(false));
    }

    #[test]
    fn usage_requires_at_least_one_token_field() {
        let model = Model::default();
        assert!(parse_classifier_usage(&json!({}), &model).is_none());
        assert!(parse_classifier_usage(&json!({"cost": 0.1}), &model).is_none());
        assert!(parse_classifier_usage(&json!(null), &model).is_none());
        assert!(parse_classifier_usage(&json!([]), &model).is_none());
        let usage = parse_classifier_usage(&json!({"output_tokens": 3}), &model).expect("usage");
        assert_eq!(usage.input, 0);
        assert_eq!(usage.output, 3);
        let usage =
            parse_classifier_usage(&json!({"input_tokens": "many", "output_tokens": 3}), &model)
                .expect("usage");
        assert_eq!(usage.input, 0);
    }

    #[test]
    fn openai_status_errors_match_the_sdk_messages() {
        // 黄金值来自 TS 参考实现（真实 `openai` SDK 7.19.0 的 APIError 构造）。
        let cases = [
            ("[]", "400 []"),
            ("\"boom\"", "400 status code (no body)"),
            ("null", "400 null"),
            ("0", "400 0"),
            ("false", "400 false"),
            ("{}", "400 {}"),
            ("{\"error\":null}", "400 {\"error\":null}"),
            ("{\"error\":[]}", "400 []"),
            ("{\"error\":0}", "400 status code (no body)"),
            ("{\"error\":\"str\"}", "400 \"str\""),
            (
                "{\"error\":{\"message\":{\"nested\":1}}}",
                "400: {\"message\":{\"nested\":1}}",
            ),
        ];
        for (body, expected) in cases {
            let error = openai_status_error(400, IndexMap::new(), body.to_string());
            // `generateImages` 的 catch：`formatProviderError(normalizeProviderError(error))`。
            let formatted = format_provider_error(
                &normalize_provider_error(&error.to_provider_error_value()),
                None,
            );
            assert_eq!(formatted, expected, "body: {body}");
        }
    }

    #[test]
    fn timeout_errors_have_the_ts_shape() {
        let error = ClassifierHttpError::timeout(5);
        assert_eq!(error.message, "Request timed out after 5ms");
        assert!(error.is_timeout);
        assert_eq!(error.status, None);
        assert_eq!(error.body, "");
    }
}
