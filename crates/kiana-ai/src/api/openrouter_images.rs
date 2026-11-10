//! api/openrouter-images.ts。
//!
//! 通过 OpenRouter 的 chat completions 端点生成图片：请求 `POST {baseUrl}/chat/completions`，
//! body 里带 `modalities`；响应在 `choices[0].message` 的 `content`（文本）与 `images`
//! （data URL）里。
//!
//! TS 用 `openai` SDK；Rust 侧直接走 [`HttpClient`]，请求/响应形状与 SDK 一致（包括 SDK
//! 自己的 10 分钟默认超时、content-type 门控的响应解析，以及 `OPENAI_ORG_ID` /
//! `OPENAI_PROJECT_ID` / `OPENAI_CUSTOM_HEADERS` 环境头）。有意省略的只有 SDK 内部遥测头
//! （`x-stainless-*`），不影响端点的行为。
//!
//! TS: `packages/ai/src/api/openrouter-images.ts`。
//!
//! # 为什么需要这个文件
//!
//! TS 的图像生成只有 OpenRouter 一个实现：它不发 `/images/generations`，而是借用 chat
//! completions 端点的 `modalities` 字段要求模型返回图片。因此 Rust 侧也没有独立的图像
//! 客户端，只有一个薄适配层；模型目录里 `api == "openrouter-images"` 的模型
//! （由 `crate::image_models` / `crate::providers` 注册）全部落到本文件。
//!
//! # 执行流程
//!
//! 1. [`generate_images`] 先建结果骨架（`api`/`provider`/`model` 来自 `model` 入参，
//!    `timestamp` 取当前 Unix 毫秒、进入函数时只取一次；失败时也保留），然后调用 `run`。
//! 2. `run` 取 API key（缺失即失败），按 SDK 构造顺序校验头名，`build_params` 组请求体，
//!    再给 `on_payload` 钩子一次改写机会。
//! 3. `chat_completions_url` + `request_headers_with_env` 完成 `buildRequest`，随后
//!    `post_json(PostErrorStyle::OpenAi, ...)` 负责发送、重试与错误文案；成功后调用
//!    `on_response`。
//! 4. 响应体按 `id` → `usage` → `choices[0].message` 的顺序读取。任一步失败时，已经写入
//!    结果对象的字段**保留**（与 TS 的 try/catch 一致），[`generate_images`] 补上
//!    `stop_reason`（取消令牌已触发 → `aborted`，否则 `error`）和 `error_message`。
//!
//! # 重要约束
//!
//! - 本文件逐行为对齐 TS：错误文案、属性读取顺序、JS 强制转换（`||`、`ToNumber`、数组
//!   `toString`）都由 `tests/openrouter_images.rs` 与文件内单测锁定。改动前先确认 TS 的
//!   行为，不要“顺手修正”。
//! - 请求体序列化走 `post_json` 内部的 `kiana_json::js_object::stringify`，与 JS 的数字
//!   格式、键序一致；不要在本文件另用 `serde_json::to_string`。
//! - 结果契约是冻结的：`response_id` 只能是字符串、usage 的 token 字段是 `u64`，因此 TS
//!   能保留的任意值在这里会被丢弃或截断（见 `run_with_env` / `token_value` 的说明）。
//! - 本文件没有可变全局状态：`DATA_URL` 由 `LazyLock` 只编译一次、之后只读共享；其余状态
//!   都在单次调用的局部变量或 `output` 里，因此并发调用之间不共享可变数据。
//! - 本文件自身的代码不 panic，也不把错误交给调用方（[`ProviderImages`] 的
//!   `generate_images` 不返回 `Result`，错误只能折进返回对象）。例外是调用方提供的
//!   `on_payload` / `on_response` 回调与 `options.fetch` 指定的 `HttpClient` 实现：本文件
//!   直接调用它们、没有 `catch_unwind`，其中的 panic 会原样穿透。
//!
//! [`HttpClient`]: crate::http::HttpClient

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::Value;

use crate::models::{ImagesOptions, ProviderImages};
use crate::types::{
    AssistantImages, ImageContent, ImageKind, ImagesContext, ImagesStopReason, Model,
    ProviderHeaders, TextContent, TextKind, Usage, UsageCost, UserContentBlock,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error};
use crate::utils::headers::headers_to_record;
use crate::utils::sanitize_unicode::sanitize_surrogates;

use super::classifier_shared::{
    ClassifierHttpError, ParsedBody, PostErrorStyle, identity_headers, merge_header_sources,
    model_header_sources, now_unix_ms, post_json,
};

/// `data:([^;]+);base64,(.+)$`（无 `s` 标志）。
///
/// JS 的 `.` 不匹配任何行终止符（`\n`、`\r`、U+2028、U+2029）；Rust 的 `.` 只排除 `\n`，
/// 所以第二个分组显式写成排除这四者的字符类。
static DATA_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^data:([^;]+);base64,([^\n\r\u{2028}\u{2029}]+)$")
        .expect("the data URL pattern compiles")
});

/// `buildParams(model, context)`。
///
/// 把 [`ImagesContext`] 的输入块翻译成 chat completions 的 content parts：
/// `Text` → `{"type":"text","text":...}`（为对齐 TS 先调用 `sanitize_surrogates`；在
/// Rust 的合法 `&str` 上它是恒等变换，孤立代理项在 UTF-8 解码阶段已按 U+FFFD 处理），
/// `Image` → `{"type":"image_url","image_url":{"url":"data:<mime>;base64,<data>"}}`；所有
/// 输入合并进同一条 `role: "user"` 消息。
///
/// `modalities` 由模型能力决定：`model.output` 含 `"text"` 时请求 `["image","text"]`，
/// 否则只请求 `["image"]`（TS 的 `model.output.includes("text")`）。向纯图像模型请求文本
/// 输出没有意义；测试 `image_only_models_must_not_request_text_output` 锁定了 `modalities`
/// 字段，完整请求体（`model`/`messages`/`stream`/`modalities`）的断言则在
/// `returns_text_plus_images_in_final_output` 里。
///
/// 函数无副作用、不失败；返回的 [`Value`] 先交给 `on_payload`，再原样序列化为请求体。
/// TS 不传 `temperature`/`max_tokens`，这里同样不传；需要额外参数时由调用方在
/// `on_payload` 里补充。
fn build_params(model: &Model, context: &ImagesContext) -> Value {
    let content: Vec<Value> = context
        .input
        .iter()
        .map(|item| match item {
            UserContentBlock::Text(text) => {
                let mut part = serde_json::Map::new();
                part.insert("type".to_string(), Value::String("text".to_string()));
                part.insert(
                    "text".to_string(),
                    Value::String(sanitize_surrogates(&text.text)),
                );
                Value::Object(part)
            }
            UserContentBlock::Image(image) => {
                let mut image_url = serde_json::Map::new();
                image_url.insert(
                    "url".to_string(),
                    Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
                );
                let mut part = serde_json::Map::new();
                part.insert("type".to_string(), Value::String("image_url".to_string()));
                part.insert("image_url".to_string(), Value::Object(image_url));
                Value::Object(part)
            }
        })
        .collect();
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), Value::String("user".to_string()));
    message.insert("content".to_string(), Value::Array(content));
    let modalities = if model
        .output
        .as_deref()
        .is_some_and(|output| output.iter().any(|modality| modality == "text"))
    {
        vec![
            Value::String("image".to_string()),
            Value::String("text".to_string()),
        ]
    } else {
        vec![Value::String("image".to_string())]
    };
    let mut params = serde_json::Map::new();
    params.insert("model".to_string(), Value::String(model.id.clone()));
    params.insert(
        "messages".to_string(),
        Value::Array(vec![Value::Object(message)]),
    );
    params.insert("stream".to_string(), Value::Bool(false));
    params.insert("modalities".to_string(), Value::Array(modalities));
    Value::Object(params)
}

/// SDK 的 `buildURL(baseURL, '/chat/completions')`：`new URL(baseURL + path)`。
///
/// 先做字符串拼接再解析，而不是用 `Url::join`：`join("/chat/completions")` 会把
/// `baseURL` 的路径整段替换掉（`https://openrouter.ai/api/v1` 会变成
/// `https://openrouter.ai/chat/completions`），与 TS 的 `baseURL + path` 语义不同。末尾
/// 斜杠的重复只去掉一个，对应 SDK 自己的 `endsWith('/') && startsWith('/')` 分支。
///
/// 解析失败（`base_url` 不是合法的绝对 URL）返回 SDK 的 `Invalid URL` 文案。该检查发生在
/// `request_headers_with_env` 之前，因此“URL 非法 + 头名也非法”时报 `Invalid URL`；
/// `tests/openrouter_images.rs` 的 `reports_invalid_base_urls_before_header_validation`
/// 锁定了这一顺序。
fn chat_completions_url(base_url: &str) -> Result<String, ClassifierHttpError> {
    let path = "/chat/completions";
    let combined = if base_url.ends_with('/') && path.starts_with('/') {
        format!("{base_url}{}", &path[1..])
    } else {
        format!("{base_url}{path}")
    };
    url::Url::parse(&combined)
        .map(|url| url.to_string())
        .map_err(|_| ClassifierHttpError::new("Invalid URL"))
}

/// SDK 的 `readEnv`：整体 trim，空值视为未设置。
fn read_env(name: &str) -> Option<String> {
    normalize_env(std::env::var(name).ok())
}

/// 统一的“未设置”判定：先 trim，空串与纯空白都视为 `None`。
fn normalize_env(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `OPENAI_CUSTOM_HEADERS`：按行解析 `name: value`（第一个冒号分隔，两侧 trim）。
///
/// SDK 在构造函数里把解析结果并入 `options.defaultHeaders`，因此它先于 kiana 的
/// `defaultHeaders`（身份头、`model.headers`、`options.headers`）。
///
/// 与 JS 对象一致：同名头（此处大小写敏感）后值覆盖前值，但位置保留首次出现的顺序；
/// 本函数不校验头名，校验留给 `validate_constructed_headers_with_env` /
/// `request_headers_with_env`，以便让“构造期”和“请求期”两个报错时机与 SDK 对齐。
fn custom_headers_from_env(raw: Option<String>) -> Vec<(String, String)> {
    let Some(raw) = normalize_env(raw) else {
        return Vec::new();
    };
    let mut parsed: IndexMap<String, String> = IndexMap::new();
    for line in raw.split('\n') {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let name = line[..colon].trim();
        let value = line[colon + 1..].trim();
        // `parsed[name] = value` 是普通赋值：`__proto__` 会命中原型 setter（字符串值被忽略），
        // 不产生自有属性，因此这行头在 SDK 里根本不存在。
        if name == "__proto__" {
            continue;
        }
        parsed.insert(name.to_string(), value.to_string());
    }
    parsed.into_iter().collect()
}

/// `openai/internal/headers.mjs` 的 `httpTokenHeaderName`：
/// `/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/`。
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

/// SDK `buildHeaders` 的头名校验：非法名字 → `TypeError`，文案逐字取自 openai SDK。
fn validate_header_name(name: &str) -> Result<(), ClassifierHttpError> {
    if is_valid_header_name(name) {
        Ok(())
    } else {
        Err(ClassifierHttpError::new(format!(
            "Header name must be a valid HTTP token [\"{name}\"]"
        )))
    }
}

/// `createClient(...)` 传给 SDK 的 `defaultHeaders`：
/// `providerHeadersToRecord(getIdentityHeaders(model, identity), model.headers, options.headers)`。
///
/// 合并顺序即参数顺序，同名头（大小写不敏感）后者覆盖前者；值为 `null` 的条目会删除同名
/// 头。这里只生成字典、不校验头名：SDK 要到构造期（有 `OPENAI_CUSTOM_HEADERS` 时）或
/// 请求构建期才检查，两个时机对应本文件的两个校验函数。
fn default_headers(model: &Model, options: Option<&ImagesOptions>) -> IndexMap<String, String> {
    let identity = identity_headers(model, options.and_then(|options| options.identity.as_ref()));
    let model_headers = model_header_sources(model);
    let empty = ProviderHeaders::new();
    merge_header_sources(&[
        &identity,
        &model_headers,
        options
            .and_then(|options| options.headers.as_ref())
            .unwrap_or(&empty),
    ])
}

/// `new OpenAI({...defaultHeaders})` 的构造阶段校验。
///
/// 设置了 `OPENAI_CUSTOM_HEADERS` 时，SDK 在构造函数里立即执行
/// `buildHeaders([parsed, options.defaultHeaders])`，非法头名抛 `TypeError`——早于
/// `buildParams` 与 `onPayload`（`createClient` 在两者之前调用）。
///
/// 返回 `Err` 就是该 `TypeError` 的等价物：没有请求发出，`on_payload` 也不会执行
/// （单测 `validates_env_header_names_before_payload_and_dispatch` 用会 panic 的
/// `HttpClient` 与计数器锁定了时序）。未设置该环境变量时直接 `Ok`，把校验留给请求构建
/// 阶段，因此“设置/不设置 env”会造成 `onPayload` 是否执行的差异——这是 SDK 的真实行为。
fn validate_constructed_headers_with_env(
    model: &Model,
    options: Option<&ImagesOptions>,
    read_env: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<(), ClassifierHttpError> {
    let Some(raw) = normalize_env(read_env("OPENAI_CUSTOM_HEADERS")) else {
        return Ok(());
    };
    for (name, _) in custom_headers_from_env(Some(raw)) {
        validate_header_name(&name)?;
    }
    for name in default_headers(model, options).keys() {
        validate_header_name(name)?;
    }
    Ok(())
}

/// 大小写不敏感地设置一个头（`Headers.append` 的「小写名 + 删除后重插」语义）。
///
/// `shift_remove` + `insert` 会让被覆盖的头移动到 `IndexMap` 末尾，和 SDK 用 `Map`
/// 删除后重插的顺序一致；HTTP 头顺序本身不影响协议语义，但测试可能按顺序比较。
fn set_header(headers: &mut IndexMap<String, String>, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    if let Some(existing) = headers
        .keys()
        .find(|key| key.eq_ignore_ascii_case(&name))
        .cloned()
    {
        headers.shift_remove(&existing);
    }
    headers.insert(name, value.to_string());
}

/// `createClient(...)` 的请求头。
///
/// SDK 的 `buildHeaders` 合并顺序：`Accept` / `OpenAI-Organization` / `OpenAI-Project` →
/// `Authorization` → `defaultHeaders`（`OPENAI_CUSTOM_HEADERS`、身份头、`model.headers`、
/// `options.headers`）→ body 头（`content-type`）。`providerHeadersToRecord` 会丢弃 `null`，
/// 所以 defaultHeaders 里的 `null` 只能删除同一列表里较早的头，删不掉 SDK 自己加的
/// `Authorization`。
///
/// 每个源在合并前先做头名校验（`buildHeaders` 的 `httpTokenHeaderName`）；错误文案里保留
/// 名字的原始大小写。
///
/// 返回值全部是小写头名；`OPENAI_ORG_ID` / `OPENAI_PROJECT_ID` 未设置时不会产生对应头。
/// 两个校验循环先 env 自定义头、后 `defaultHeaders`，所以两者同时非法时报 env 里的名字
/// （单测 `validates_header_names_like_the_openai_sdk` 锁定）。
fn request_headers_with_env(
    model: &Model,
    api_key: &str,
    options: Option<&ImagesOptions>,
    read_env: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<IndexMap<String, String>, ClassifierHttpError> {
    let default_headers = default_headers(model, options);
    let custom_headers = custom_headers_from_env(read_env("OPENAI_CUSTOM_HEADERS"));
    for (name, _) in &custom_headers {
        validate_header_name(name)?;
    }
    for name in default_headers.keys() {
        validate_header_name(name)?;
    }
    let mut headers: IndexMap<String, String> = IndexMap::new();
    set_header(&mut headers, "accept", "application/json");
    if let Some(organization) = normalize_env(read_env("OPENAI_ORG_ID")) {
        set_header(&mut headers, "openai-organization", &organization);
    }
    if let Some(project) = normalize_env(read_env("OPENAI_PROJECT_ID")) {
        set_header(&mut headers, "openai-project", &project);
    }
    set_header(&mut headers, "authorization", &format!("Bearer {api_key}"));
    for (name, value) in &custom_headers {
        set_header(&mut headers, name, value);
    }
    for (name, value) in &default_headers {
        set_header(&mut headers, name, value);
    }
    // body 头最后设置：用户无法覆盖 content-type（SDK 的 bodyHeaders 在 defaultHeaders 之后）。
    set_header(&mut headers, "content-type", "application/json");
    Ok(headers)
}

/// JS 真值判定（`||` 左侧）。
fn js_truthy(value: &Value) -> bool {
    kiana_json::js_string::is_truthy(Some(value))
}

/// `String(number)`（`NaN`/`±Infinity` 用 JS 文案，不是 JSON 的 `null`）。
fn js_number_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        kiana_json::js_num::format_js_number(value)
    }
}

/// `String(value)`（`Array.prototype.join` 对元素的 ToString；`null` 当空串）。
fn js_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => js_number_string(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(js_to_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// `Number(value)`（ToNumber）：数组先走 `Array.prototype.toString`（`[5]` → 5）。
fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::js_num::parse_js_number(text),
        Value::Array(items) => kiana_json::js_num::parse_js_number(
            &items.iter().map(js_to_string).collect::<Vec<_>>().join(","),
        ),
        Value::Object(_) => f64::NAN,
    }
}

/// `value || 0` 之后的 `ToNumber`。
fn number_or_zero(value: Option<&Value>) -> f64 {
    match value {
        Some(value) if js_truthy(value) => js_to_number(value),
        _ => 0.0,
    }
}

/// `Math.max(0, value)`：`NaN` 保持 `NaN`（Rust 的 `f64::max` 会返回另一个操作数）。
fn non_negative(value: f64) -> f64 {
    if value.is_nan() {
        f64::NAN
    } else if value > 0.0 {
        value
    } else {
        0.0
    }
}

/// JS `Number` → Rust `u64`（[`crate::types::Usage`] 的冻结字段类型，ARCHITECTURE §4.9）。
///
/// TS 保留任意 Number（小数、负数、1e300、乃至 `"NaNabc0"` 这种字符串拼接结果），冻结的
/// `u64` 只能截断/饱和：负数与 NaN → 0，小数截断，超出范围饱和到 `u64::MAX`。费用字段是
/// `f64`，仍按 JS 的原始数值计算（含 NaN 传播）。
fn token_value(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

/// `parseUsage(rawUsage, model)`：按 `model.cost` 直接计价（不走 `calculateCost` 的分档）。
///
/// 计算分三段，顺序与 TS 相同：
/// 1. 拆 cache：provider 报告的 `cached_tokens` 在存在正数 `cache_write_tokens` 时被当成
///    “读 + 写”之和，`cache_read = max(0, cached - write)`；否则原样作为读。
/// 2. `input = max(0, prompt_tokens - cache_read - cache_write)`；
///    `output = completion_tokens`（**不**夹到非负，负值在 TS 里也保留并产生负费用）。
/// 3. 费用按模型目录的每百万 token 单价乘原始浮点值计算，`total` 是四项之和；
///    `total_from_provider = Some(false)` 表示这是本地计算，不是 provider 报价。
///
/// 与 TS 的唯一差异来自冻结类型：TS 的 token 字段可以是任意 Number，而 [`Usage`] 的 token
/// 字段是 `u64`，由 `token_value` 截断/饱和；费用字段仍是 `f64`，所以 NaN 会继续传播
/// （不会静默变成 0）。畸形输入的具体期望见单测 `coerces_usage_numbers_like_js`。
fn parse_usage(raw_usage: &Value, model: &Model) -> Usage {
    let prompt_tokens = number_or_zero(raw_usage.get("prompt_tokens"));
    let details = raw_usage.get("prompt_tokens_details");
    let reported_cached_tokens =
        number_or_zero(details.and_then(|details| details.get("cached_tokens")));
    let cache_write_tokens =
        number_or_zero(details.and_then(|details| details.get("cache_write_tokens")));
    let cache_read_tokens = if cache_write_tokens > 0.0 {
        non_negative(reported_cached_tokens - cache_write_tokens)
    } else {
        reported_cached_tokens
    };
    let input = non_negative(prompt_tokens - cache_read_tokens - cache_write_tokens);
    let output = number_or_zero(raw_usage.get("completion_tokens"));
    let cost_input = model.cost.input / 1_000_000.0 * input;
    let cost_output = model.cost.output / 1_000_000.0 * output;
    let cost_cache_read = model.cost.cache_read / 1_000_000.0 * cache_read_tokens;
    let cost_cache_write = model.cost.cache_write / 1_000_000.0 * cache_write_tokens;
    Usage {
        input: token_value(input),
        output: token_value(output),
        cache_read: token_value(cache_read_tokens),
        cache_write: token_value(cache_write_tokens),
        cache_write1h: None,
        reasoning: None,
        total_tokens: token_value(input + output + cache_read_tokens + cache_write_tokens),
        cost: UsageCost {
            input: cost_input,
            output: cost_output,
            cache_read: cost_cache_read,
            cache_write: cost_cache_write,
            total: cost_input + cost_output + cost_cache_read + cost_cache_write,
            total_from_provider: Some(false),
        },
    }
}

/// 从 `choices[0]` 里收集文本与图片（并复刻 TS 在畸形响应上的取值顺序与错误文本）。
///
/// TS 的取值顺序是 `output.responseId` → `usage` → `choices[0].message.content` →
/// `choices[0].message.images`，因此前一步已写入 `output` 的字段在后续抛错时**保留**。
///
/// 返回值语义：`Ok(())` 表示按 TS 逻辑走完，可能没有任何输出——`choices` 是空数组、空串、
/// 数字/布尔，或对象缺少 `"0"` 键、`"0"` 值为假值，以及首元素为假值的数组等；`images`
/// 为 `null` 或字符串、`message` 是原始值等更靠后的分支同样可能不产出。`Err` 表示 TS 会抛
/// `TypeError` 的形状，消息逐字对齐 V8：`choices` 缺失或为 `null` 时返回 `Err`，非空字符串
/// `choices` 也报错，而带真值 `"0"` 键的对象会继续解析（结果可能是输出、`Ok` 无输出或
/// `Err`）。错误发生前已经 push 的文本/图片留在 `output` 里
/// ——TS 的 catch 不清空 `output.output`。
fn apply_choice_output(
    output: &mut Vec<UserContentBlock>,
    body: &serde_json::Map<String, Value>,
) -> Result<(), ClassifierHttpError> {
    // `const choice = imageResponse.choices[0];`
    let choices = match body.get("choices") {
        None => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of undefined (reading '0')",
            ));
        }
        Some(Value::Null) => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of null (reading '0')",
            ));
        }
        Some(choices) => choices,
    };
    let choice: Option<&Value> = match choices {
        Value::Array(items) => items.first(),
        Value::Object(map) => map.get("0"),
        // 字符串可索引：非空串的首字符为真值，随后 `choice.message.content` 报 undefined；
        // 空串的 `[0]` 是 undefined（假值）。
        Value::String(text) if text.is_empty() => None,
        Value::String(_) => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of undefined (reading 'content')",
            ));
        }
        // 数字/布尔的属性访问得到 undefined → `[0]` 假值（null 已在上面处理）。
        Value::Number(_) | Value::Bool(_) | Value::Null => None,
    };
    let Some(choice) = choice else {
        return Ok(());
    };
    // `if (choice)`：假值（null/false/0/""）直接跳过。
    if !js_truthy(choice) {
        return Ok(());
    }
    // `choice.message.content`：`choice.message` 是 undefined 或 null 时抛错；原始值/数组的
    // 属性访问得到 undefined（`.content`/`.images` 都不存在，整段跳过）。
    let message = match choice {
        Value::Object(object) => match object.get("message") {
            None => {
                return Err(ClassifierHttpError::new(
                    "Cannot read properties of undefined (reading 'content')",
                ));
            }
            Some(Value::Null) => {
                return Err(ClassifierHttpError::new(
                    "Cannot read properties of null (reading 'content')",
                ));
            }
            Some(message) => message,
        },
        _ => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of undefined (reading 'content')",
            ));
        }
    };
    let Some(message) = message.as_object() else {
        // `message` 是原始值或数组：`.content` / `.images` 都是 undefined。
        return Ok(());
    };
    if let Some(Value::String(text)) = message.get("content")
        && !text.is_empty()
    {
        output.push(UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.clone(),
            text_signature: None,
        }));
    }
    // `for (const image of choice.message.images ?? [])`
    let images = match message.get("images") {
        None | Some(Value::Null) => return Ok(()),
        Some(images) => images,
    };
    let entries: Vec<&Value> = match images {
        Value::Array(items) => items.iter().collect(),
        // 字符串可迭代：逐字符的 `.image_url` 都是 undefined，全部跳过。
        Value::String(_) => return Ok(()),
        Value::Number(number) => {
            let number = number.as_f64().unwrap_or(f64::NAN);
            return Err(ClassifierHttpError::new(format!(
                "number {} is not iterable (cannot read property Symbol(Symbol.iterator))",
                kiana_json::js_num::format_js_number(number)
            )));
        }
        Value::Bool(flag) => {
            return Err(ClassifierHttpError::new(format!(
                "boolean {flag} is not iterable (cannot read property Symbol(Symbol.iterator))"
            )));
        }
        Value::Object(_) => {
            return Err(ClassifierHttpError::new(
                "object is not iterable (cannot read property Symbol(Symbol.iterator))",
            ));
        }
        Value::Null => return Ok(()),
    };
    for image in entries {
        // `image.image_url`：null 上取值抛错；原始值的属性访问得到 undefined。
        let image_url = match image {
            Value::Null => {
                return Err(ClassifierHttpError::new(
                    "Cannot read properties of null (reading 'image_url')",
                ));
            }
            Value::Object(object) => object.get("image_url"),
            _ => None,
        };
        // `typeof image.image_url === "string" ? image.image_url : image.image_url?.url`
        let image_url = match image_url {
            Some(Value::String(url)) => Some(Value::String(url.clone())),
            Some(Value::Object(object)) => object.get("url").cloned(),
            Some(Value::Null) | None => None,
            // 原始值的 `.url` 是 undefined。
            Some(Value::Number(_)) | Some(Value::Bool(_)) | Some(Value::Array(_)) => None,
        };
        let image_url = match image_url {
            None | Some(Value::Null) => continue,
            Some(Value::String(url)) => url,
            // `imageUrl?.startsWith is not a function`（数字/布尔/对象/数组）。
            Some(_) => {
                return Err(ClassifierHttpError::new(
                    "imageUrl?.startsWith is not a function",
                ));
            }
        };
        // 非 `data:` 前缀或不符合 `DATA_URL` 的条目静默跳过（TS 也是 `continue`，不视为
        // 错误，因此畸形图片不会让整次生成失败）。
        if !image_url.starts_with("data:") {
            continue;
        }
        let Some(captures) = DATA_URL.captures(&image_url) else {
            continue;
        };
        let (Some(mime_type), Some(data)) = (captures.get(1), captures.get(2)) else {
            continue;
        };
        output.push(UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: data.as_str().to_string(),
            mime_type: mime_type.as_str().to_string(),
        }));
    }
    Ok(())
}

/// [`generate_images`] 的实际执行体：用真实进程环境读取 `OPENAI_*` 变量。
///
/// 拆出 [`run_with_env`] 让单测注入假的 env 回调（头名校验时机依赖 env），生产路径只经过
/// 这里。`output` 会在执行过程中被逐步写入；返回 `Err` 时其中的部分结果按 TS 语义保留。
/// 错误类型是 [`ClassifierHttpError`]，由 [`generate_images`] 转成结果对象。
async fn run(
    model: &Model,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    output: &mut AssistantImages,
) -> Result<(), ClassifierHttpError> {
    run_with_env(model, context, options, output, &read_env).await
}

/// [`run`] 的可注入环境版本（测试用假 env 复刻 SDK 的构造阶段校验）。
///
/// 执行顺序与失败点（任一步返回 `Err` 都会立即结束，后续步骤不执行）：
/// 1. 取 `options.api_key`：缺失或空串 → `No API key for provider: <provider>`（TS 的
///    `if (!apiKey)` 把空串也当缺失）。
/// 2. 构造期头校验：只在设置了 `OPENAI_CUSTOM_HEADERS` 时发生，早于 `build_params` 与
///    `on_payload`。
/// 3. `build_params` → `on_payload`：钩子返回 `Some` 才替换 payload；`None` 或无钩子保持
///    原值。
/// 4. `chat_completions_url`（失败 → `Invalid URL`）→ `request_headers_with_env`（失败 →
///    头名错误），顺序对应 SDK 的 `buildURL` 先于 `buildHeaders`。
/// 5. `post_json(PostErrorStyle::OpenAi, ...)` 处理 HTTP 失败、超时与重试；成功后才调用
///    `on_response`（非 2xx 时 `post_json` 先返回 `Err`，钩子不会触发）。
/// 6. 解析成功响应：写 `response_id`（非字符串丢弃）→ truthy 的 `usage` 才计价 →
///    `choices[0]` 交给 [`apply_choice_output`]。
///
/// 注意 `output` 是 `&mut`：`id`/`usage` 可能在解析 `choices` 失败之前就已写入，这与 TS
/// 在 catch 中保留已赋值字段的行为一致，`tests/openrouter_images.rs` 的多条用例依赖它。
async fn run_with_env(
    model: &Model,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
    output: &mut AssistantImages,
    read_env: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<(), ClassifierHttpError> {
    let Some(api_key) = options
        .and_then(|options| options.api_key.as_deref())
        .filter(|api_key| !api_key.is_empty())
    else {
        return Err(ClassifierHttpError::new(format!(
            "No API key for provider: {}",
            model.provider
        )));
    };
    // `createClient(...)` 在 `buildParams` / `onPayload` 之前执行。
    validate_constructed_headers_with_env(model, options, read_env)?;
    let mut params = build_params(model, context);
    if let Some(options) = options
        && let Some(transformed) = options.apply_on_payload(params.clone(), model).await
    {
        params = transformed;
    }
    // SDK 的 `buildRequest` 先 `buildURL(...)` 再 `buildHeaders(...)`：baseUrl 非法时
    // 报 `Invalid URL`，即使头名也非法。
    let url = chat_completions_url(&model.base_url)?;
    let headers = request_headers_with_env(model, api_key, options, read_env)?;
    let (response, body) =
        post_json(PostErrorStyle::OpenAi, url, headers, &params, options, &[]).await?;
    if let Some(on_response) = options.and_then(|options| options.on_response.as_ref()) {
        on_response(
            crate::types::ProviderResponse {
                status: response.status,
                headers: headers_to_record(&response.headers),
            },
            model,
        );
    }
    let body = match body {
        ParsedBody::Json(Value::Object(body)) => body,
        // `imageResponse.id`：`undefined`/`null` 上取属性抛错；字符串、数字、布尔、数组的
        // `.id` 是 undefined，随后 `.choices[0]` 在 `undefined` 上取下标才报错。
        ParsedBody::Undefined => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of undefined (reading 'id')",
            ));
        }
        ParsedBody::Json(Value::Null) => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of null (reading 'id')",
            ));
        }
        ParsedBody::Json(_) | ParsedBody::Text(_) => {
            return Err(ClassifierHttpError::new(
                "Cannot read properties of undefined (reading '0')",
            ));
        }
    };
    // `output.responseId = imageResponse.id`（undefined 表示不写该字段）。TS 会把任意值写进
    // `responseId`（数字/null/对象都原样保留），冻结契约是 `Option<String>`（§4.9），因此
    // 非字符串 id 只能丢弃。
    if let Some(id) = body.get("id").and_then(Value::as_str) {
        output.response_id = Some(id.to_string());
    }
    if let Some(usage) = body.get("usage")
        && kiana_json::js_string::is_truthy(Some(usage))
    {
        output.usage = Some(parse_usage(usage, model));
    }
    apply_choice_output(&mut output.output, &body)
}

/// Image generation over OpenRouter's chat completions endpoint.
///
/// 本模块唯一的对外生成入口，对齐 TS 的 `ImagesFunction<ImagesOptions>`。返回值的每个字段：
///
/// - `api` / `provider` / `model` 原样取自 `model`，失败时也保留；
/// - `output` 按响应顺序累积文本与图片，失败前已解析的块保留；
/// - `response_id` / `usage` 只在成功读到对应字段时设置；
/// - `stop_reason` 初始为 `Stop`；`run` 失败时若 `options.signal` 已被取消则改为
///   `Aborted`，否则为 `Error`；
/// - `error_message` 由 `format_provider_error(normalize_provider_error(...))` 生成，前缀
///   参数是 `None`，因此错误文本没有 `openrouter-images` 之类的前缀（TS 原样如此）；
/// - `timestamp` 在进入函数时取一次 `Date.now()` 等价值，失败时也保留。
///
/// 本函数**不返回 `Result`**：所有内部错误都折进返回对象（[`ProviderImages`] 的契约）。
/// 本函数自身的代码不会 panic，但它直接调用调用方提供的 `on_payload` / `on_response`
/// 回调，并通过 `options.fetch` 调用调用方提供的 `HttpClient` 实现——这些代码里的 panic
/// 会原样穿透，本函数没有捕获（也没有 `catch_unwind`）。调用方判断成败只能看
/// `stop_reason` 与 `error_message`；网络失败时会重试与否取决于 `options.max_retries`，
/// 而默认是 0 次（SDK 的 `maxRetries: 0`）。
pub async fn generate_images(
    model: &Model,
    context: &ImagesContext,
    options: Option<&ImagesOptions>,
) -> AssistantImages {
    let mut output = AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp: now_unix_ms(),
    };
    if let Err(error) = run(model, context, options, &mut output).await {
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
        output.stop_reason = if aborted {
            ImagesStopReason::Aborted
        } else {
            ImagesStopReason::Error
        };
        // 注意：openrouter-images 的错误没有前缀（TS 的 `formatProviderError(norm)`）。
        output.error_message = Some(format_provider_error(
            &normalize_provider_error(&error.to_provider_error_value()),
            None,
        ));
    }
    output
}

/// `openrouterImagesApi()`。
///
/// 返回 `Arc<dyn ProviderImages>` 的无状态适配器，每次调用新建一个零大小对象。TS 侧这里
/// 是惰性 `import()` 的包装、加载失败会退化成错误结果；Rust 是静态链接，函数不会失败，
/// 用户可见的错误只来自 [`generate_images`] 内部。
pub fn openrouter_images_api() -> Arc<dyn ProviderImages> {
    Arc::new(OpenRouterImagesApi)
}

/// [`ProviderImages`] 的零大小适配器；唯一职责是转发到 [`generate_images`]。
struct OpenRouterImagesApi;

#[async_trait]
impl ProviderImages for OpenRouterImagesApi {
    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        generate_images(model, context, options).await
    }
}

#[cfg(test)]
mod tests {
    //! 只放需要访问私有辅助函数的单测：头合并/校验、JS 强制转换、data URL 正则，以及
    //! `run_with_env` 的时序。端到端的请求体、错误文案与响应解析在
    //! `tests/openrouter_images.rs`（黄金值来自 TS 参考实现）。

    use super::*;
    use crate::types::{ModelCost, ModelInputModality, StreamOptions};
    use serde_json::json;

    fn model() -> Model {
        Model {
            id: "m".into(),
            name: "m".into(),
            api: "openrouter-images".into(),
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            input: vec![ModelInputModality::Text],
            cost: ModelCost {
                input: 0.015,
                output: 0.03,
                cache_read: 0.001,
                cache_write: 0.002,
                tiers: None,
            },
            ..Model::default()
        }
    }

    fn headers(pairs: &[(&str, Option<&str>)]) -> ProviderHeaders {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.map(str::to_string)))
            .collect()
    }

    fn model_headers(pairs: &[(&str, &str)]) -> indexmap::IndexMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn merges_openai_env_headers_like_the_sdk() {
        let mut model = model();
        model.headers = Some(model_headers(&[("HTTP-Referer", "https://example.com")]));
        let options = StreamOptions {
            headers: Some(headers(&[
                ("openai-project", Some("opt-project")),
                ("Authorization", None),
                ("x-custom", None),
            ])),
            ..StreamOptions::default()
        };
        let env = |name: &str| -> Option<String> {
            match name {
                "OPENAI_ORG_ID" => Some("  org-1  ".into()),
                "OPENAI_PROJECT_ID" => Some("proj-1".into()),
                "OPENAI_CUSTOM_HEADERS" => Some(
                    " X-Custom : a \nX-Other:b\ncontent-type: text/csv\nUser-Agent: custom-agent\ninvalid"
                        .into(),
                ),
                _ => None,
            }
        };
        let merged = request_headers_with_env(&model, "test", Some(&options), &env)
            .expect("valid header names");

        assert_eq!(
            merged.get("openai-organization").map(String::as_str),
            Some("org-1")
        );
        // options.headers 覆盖环境变量。
        assert_eq!(
            merged.get("openai-project").map(String::as_str),
            Some("opt-project")
        );
        // OPENAI_CUSTOM_HEADERS 的任意头（kiana 的 defaultHeaders 没有同名键时保留）。
        assert_eq!(merged.get("x-custom").map(String::as_str), Some("a"));
        assert_eq!(merged.get("x-other").map(String::as_str), Some("b"));
        // body 头最后设置，覆盖自定义 env 的 content-type。
        assert_eq!(
            merged.get("content-type").map(String::as_str),
            Some("application/json")
        );
        // 身份头覆盖自定义 env 的 User-Agent。
        assert!(
            merged
                .get("user-agent")
                .is_some_and(|value| value.starts_with("kiana/"))
        );
        // `providerHeadersToRecord` 丢弃 null：删不掉 SDK 自己加的 Authorization。
        assert_eq!(
            merged.get("authorization").map(String::as_str),
            Some("Bearer test")
        );
        assert_eq!(
            merged.get("http-referer").map(String::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn omits_organization_and_project_when_the_env_is_unset() {
        let merged = request_headers_with_env(&model(), "test", None, &|_| None)
            .expect("valid header names");
        assert!(!merged.contains_key("openai-organization"));
        assert!(!merged.contains_key("openai-project"));
        assert_eq!(
            merged.get("authorization").map(String::as_str),
            Some("Bearer test")
        );
    }

    #[test]
    fn coerces_usage_numbers_like_js() {
        // `[5]` → ToNumber → 5（数组走 Array.prototype.toString）。
        let usage = parse_usage(
            &json!({"prompt_tokens": [5], "completion_tokens": 0}),
            &model(),
        );
        assert_eq!(usage.input, 5);
        assert_eq!(usage.cost.input, 0.015_f64 / 1_000_000.0 * 5.0);

        // `"many"` → NaN；`Math.max(0, NaN)` 仍是 NaN（费用保留 NaN）。
        let usage = parse_usage(
            &json!({"prompt_tokens": "many", "completion_tokens": 0}),
            &model(),
        );
        assert_eq!(usage.input, 0); // u64 字段按冻结契约截断
        assert!(usage.cost.input.is_nan());
        assert!(usage.cost.total.is_nan());

        // `[1,2]` → "1,2" → NaN；对象 → "[object Object]" → NaN。
        for prompt in [json!([1, 2]), json!({})] {
            let usage = parse_usage(
                &json!({"prompt_tokens": prompt, "completion_tokens": 0}),
                &model(),
            );
            assert!(usage.cost.input.is_nan(), "prompt: {prompt}");
        }

        // 布尔按 ToNumber：true → 1，false → 0。
        let usage = parse_usage(
            &json!({"prompt_tokens": true, "completion_tokens": false}),
            &model(),
        );
        assert_eq!((usage.input, usage.output), (1, 0));
        assert_eq!(usage.cost.input, 0.015_f64 / 1_000_000.0);

        // 小数：token 字段截断（冻结 u64），但费用按原值计算，totalTokens 按 JS 求和截断。
        let usage = parse_usage(
            &json!({"prompt_tokens": 10.5, "completion_tokens": 2.5}),
            &model(),
        );
        assert_eq!((usage.input, usage.output), (10, 2));
        assert_eq!(usage.total_tokens, 13);
        assert_eq!(usage.cost.input, 0.015_f64 / 1_000_000.0 * 10.5);
        assert_eq!(
            usage.cost.total,
            0.015_f64 / 1_000_000.0 * 10.5 + 0.03_f64 / 1_000_000.0 * 2.5
        );

        // 负值：input 被 Math.max 夹到 0，output 保持负值（费用为负；u64 字段截断为 0）。
        let usage = parse_usage(
            &json!({"prompt_tokens": -2, "completion_tokens": -3}),
            &model(),
        );
        assert_eq!((usage.input, usage.output), (0, 0));
        assert_eq!(usage.cost.output, 0.03_f64 / 1_000_000.0 * -3.0);
        assert_eq!(usage.cost.total, 0.03_f64 / 1_000_000.0 * -3.0);

        // 超出 u64 的数按 Rust 的饱和转换落位（JS 里仍是 1e300）。
        let usage = parse_usage(
            &json!({"prompt_tokens": 1e300, "completion_tokens": 0}),
            &model(),
        );
        assert_eq!(usage.input, u64::MAX);
        assert_eq!(usage.cost.input, 0.015_f64 / 1_000_000.0 * 1e300);

        // 非数字的 cached_tokens：cacheRead 截断为 0，费用 NaN（TS 里是 "abc" 与 "NaNabc0"）。
        let usage = parse_usage(
            &json!({
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "prompt_tokens_details": {"cached_tokens": "abc"}
            }),
            &model(),
        );
        assert_eq!(usage.cache_read, 0);
        assert!(usage.cost.cache_read.is_nan());
        assert!(usage.cost.total.is_nan());
    }

    #[test]
    fn validates_header_names_like_the_openai_sdk() {
        // `options.headers` 上的非法头名（request build 阶段）。
        let options = StreamOptions {
            headers: Some(headers(&[("bad name", Some("1"))])),
            ..StreamOptions::default()
        };
        let error = request_headers_with_env(&model(), "test", Some(&options), &|_| None)
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"bad name\"]"
        );

        // 错误文案保留原始大小写（校验发生在 `Headers` 的小写化之前）。
        let options = StreamOptions {
            headers: Some(headers(&[("Bad\tName", Some("1"))])),
            ..StreamOptions::default()
        };
        let error = request_headers_with_env(&model(), "test", Some(&options), &|_| None)
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"Bad\tName\"]"
        );

        // 空名字也不是合法 token。
        let options = StreamOptions {
            headers: Some(headers(&[("", Some("1"))])),
            ..StreamOptions::default()
        };
        let error = request_headers_with_env(&model(), "test", Some(&options), &|_| None)
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"\"]"
        );

        // `model.headers` 同样校验。
        let mut with_model_headers = model();
        with_model_headers.headers = Some(model_headers(&[("no spaces allowed", "1")]));
        let error = request_headers_with_env(&with_model_headers, "test", None, &|_| None)
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"no spaces allowed\"]"
        );

        // 环境变量里的头名先校验（SDK 构造函数里的 `buildHeaders([parsed, defaultHeaders])`）。
        let env = |name: &str| -> Option<String> {
            match name {
                "OPENAI_CUSTOM_HEADERS" => Some("even worse: 1".into()),
                _ => None,
            }
        };
        let options = StreamOptions {
            headers: Some(headers(&[("bad name", Some("1"))])),
            ..StreamOptions::default()
        };
        let error = request_headers_with_env(&model(), "test", Some(&options), &env)
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"even worse\"]"
        );

        // RFC 7230 token 允许的标点全部合法。
        let options = StreamOptions {
            headers: Some(headers(&[("x-._~!#$%&'*+^`|", Some("1"))])),
            ..StreamOptions::default()
        };
        request_headers_with_env(&model(), "test", Some(&options), &|_| None)
            .expect("token characters are valid");
    }

    #[test]
    fn env_custom_headers_cannot_create_a_proto_header() {
        // `parsed[name] = value` 是普通赋值：`__proto__` 命中原型 setter，不产生自有属性。
        let parsed = custom_headers_from_env(Some("__proto__: x\nX-A: 1".into()));
        assert_eq!(
            parsed,
            vec![("X-A".to_string(), "1".to_string())],
            "__proto__ must be dropped"
        );
    }

    #[tokio::test]
    async fn validates_env_header_names_before_payload_and_dispatch() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// 校验失败时不应有任何请求发出；真的发出就 panic。
        struct PanicClient;

        impl crate::http::HttpClient for PanicClient {
            fn execute<'a>(
                &'a self,
                _request: crate::http::HttpRequest,
                _signal: Option<&'a tokio_util::sync::CancellationToken>,
            ) -> crate::http::BoxFuture<'a, Result<crate::http::HttpResponse, crate::http::HttpError>>
            {
                panic!("no request should be dispatched");
            }
        }

        fn output() -> AssistantImages {
            AssistantImages {
                api: "openrouter-images".into(),
                provider: "openrouter".into(),
                model: "m".into(),
                output: Vec::new(),
                response_id: None,
                usage: None,
                stop_reason: ImagesStopReason::Stop,
                error_message: None,
                timestamp: 0,
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let on_payload = {
            let calls = Arc::clone(&calls);
            Arc::new(move |params: Value, _model: &Model| -> Option<Value> {
                calls.fetch_add(1, Ordering::SeqCst);
                Some(params)
            })
        };
        let options = StreamOptions {
            api_key: Some("test".into()),
            headers: Some(headers(&[("bad name", Some("1"))])),
            on_payload: Some(on_payload),
            fetch: Some(Arc::new(PanicClient)),
            ..StreamOptions::default()
        };
        let context = ImagesContext {
            input: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "x".into(),
                text_signature: None,
            })],
        };

        // 设置了 `OPENAI_CUSTOM_HEADERS`：SDK 在构造函数里就抛错，`onPayload` 不会执行。
        let env = |name: &str| -> Option<String> {
            match name {
                "OPENAI_CUSTOM_HEADERS" => Some("even worse: 1".into()),
                _ => None,
            }
        };
        let error = run_with_env(&model(), &context, Some(&options), &mut output(), &env)
            .await
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"even worse\"]"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "onPayload ran too early");

        // 未设置 `OPENAI_CUSTOM_HEADERS`：校验发生在 request build（`buildParams` /
        // `onPayload` 之后），与真实 SDK 的时序一致（参考探针 K：payload 被执行）。
        let calls = Arc::new(AtomicUsize::new(0));
        let on_payload = {
            let calls = Arc::clone(&calls);
            Arc::new(move |params: Value, _model: &Model| -> Option<Value> {
                calls.fetch_add(1, Ordering::SeqCst);
                Some(params)
            })
        };
        let options = StreamOptions {
            api_key: Some("test".into()),
            headers: Some(headers(&[("bad name", Some("1"))])),
            on_payload: Some(on_payload),
            fetch: Some(Arc::new(PanicClient)),
            ..StreamOptions::default()
        };
        let error = run_with_env(&model(), &context, Some(&options), &mut output(), &|_| None)
            .await
            .expect_err("invalid header name");
        assert_eq!(
            error.message,
            "Header name must be a valid HTTP token [\"bad name\"]"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "request-build validation runs after onPayload"
        );
    }

    #[test]
    fn skips_data_urls_containing_line_terminators() {
        // JS 的 `.`（无 `s` 标志）拒绝全部行终止符；U+2028/U+2029 尤其容易在 Rust 里漏掉。
        let body = json!({
            "id": "x",
            "choices": [{
                "message": {
                    "images": [
                        {"image_url": "data:image/png;base64,AA\rBB"},
                        {"image_url": "data:image/png;base64,AA\u{2028}BB"},
                        {"image_url": "data:image/png;base64,AA\u{2029}BB"},
                        {"image_url": "data:image/png;base64,AA\nBB"},
                        {"image_url": "data:image/png;base64,AA\n"},
                        {"image_url": "data:image/png;base64,QUJD"}
                    ]
                }
            }]
        });
        let mut output = Vec::new();
        apply_choice_output(&mut output, body.as_object().expect("object")).expect("no error");
        assert_eq!(output.len(), 1, "only the clean data URL survives");
        match &output[0] {
            UserContentBlock::Image(image) => {
                assert_eq!(image.mime_type, "image/png");
                assert_eq!(image.data, "QUJD");
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }
}
