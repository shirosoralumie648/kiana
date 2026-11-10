#![allow(clippy::result_large_err)]
//! api/google-shared.ts——Gemini（Google Generative AI）与 Vertex 的共享逻辑。
//!
//! Rust 侧没有 `@google/genai` SDK：本模块把 SDK 的可观察表面拆成
//! [`GoogleClientConfig`]（构造参数）与 REST 请求（`streamGenerateContent?alt=sse`），
//! 事件归一化逻辑逐字对照 TS。
//!
//! # 为什么这个文件比 TS 大一个数量级
//!
//! TS 侧 `google-shared.ts` 只有 574 行、18 个导出，因为「客户端构造、URL 拼接、请求体
//! 序列化、ADC 鉴权、SSE 解码」都由 npm 包 `@google/genai` 完成。Rust 没有官方 SDK，
//! 这些行为必须在本文件里逐条复刻，于是它同时承担三类职责：
//!
//! 1. **TS 共享函数的移植**：思考等级、消息/工具/停止原因转换，函数名与 TS 一一对应；
//! 2. **SDK 可观察表面的复刻**：`GoogleClientConfig`（构造参数）、`build_wire_body`
//!    （请求体转换）、`request_stream`（URL/请求头/重试）、Application Default
//!    Credentials 链；
//! 3. **两个适配器共用的事件循环**：`read_google_events` + `handle_google_chunk`，
//!    对应 `google-generative-ai.ts` / `google-vertex.ts` 里的 chunk 循环。
//!
//! # 被谁调用
//!
//! - [`crate::api::google_generative_ai`] 与 [`crate::api::google_vertex`]：两个适配器的
//!   `stream`/`streamSimple` 都从这里取 [`build_params`]、[`request_stream`]、
//!   [`read_google_events`]、[`handle_google_chunk`]、[`finish_google_block`]。
//! - [`crate::api::bedrock_converse_stream`]：复用 [`object`] 按固定键序构造 JSON。
//! - [`crate::api::kiana_messages`]：复用 `js_string_of_value`（`pub(crate)`）做 JS 字符串化。
//! - 集成测试：`tests/google_shared_*.rs`、`tests/google_thinking_*.rs` 直接调用公开函数。
//!
//! # 一次典型调用的执行顺序
//!
//! 1. 适配器把 `Model`、`TranscriptContext`、选项交给 [`build_params`]，得到
//!    `{"model", "contents", "config"}` 形状的 `serde_json::Value`（对应 SDK 的
//!    `GenerateContentParameters`）；
//! 2. [`request_stream`] 用 [`GoogleClientConfig`] 拼出
//!    `.../models/{model}:streamGenerateContent?alt=sse`，按模式选择 API key 或 ADC 的
//!    `authorization: Bearer`，把请求交给可注入的 `HttpClient`；[`retry_google_request`] 的
//!    重试覆盖「发出请求直到拿到响应头」（非 2xx 时还会读完错误体），成功响应的流式 body
//!    读取不在重试范围内；
//! 3. [`read_google_events`] 把响应字节流按 SSE 分隔符切开，逐个 `data:` 事件解析成 JSON；
//! 4. [`handle_google_chunk`] 把每个 chunk 转成 Kiana 的 `AssistantMessageEvent`
//!    （文本/思考增量、toolCall 的 start/delta/end、usage 与 stopReason），并用
//!    [`GoogleStreamState`] 记录当前打开的块；
//! 5. 流结束后适配器再调用一次 [`finish_google_block`] 关闭仍未结束的块。
//!
//! # 数据表示与关键约束
//!
//! - 线上形状统一用 `serde_json::Value`，而不是强类型 struct：Google 字段极多，
//!   SDK 转换器还会按模式丢弃或改名，只有逐键操作才能复现「未设置就不发」与键序。
//!   workspace 的 `serde_json` 开启了 `preserve_order`，所以 [`object`] 的插入顺序就是
//!   线上 JSON 的键序；`tests` 里对请求体的断言是逐字节字符串比较，改动插入顺序会失败。
//! - [`convert_messages`] 依赖 `transform_messages_with` 先完成跨 provider 的归一化；
//!   本模块只做「内部消息 → Gemini `Content[]`」的最后一步。
//! - thought signature 只在 **同 provider 且同 model** 时回传（见
//!   [`retain_thought_signature`] 与内部 `resolve_thought_signature`）；跨模型不回传。
//! - 错误分两层：内部统一用 [`GoogleFailure`]（message + 可选 HTTP 状态），适配器再把它
//!   转成 `AiError` / 错误事件；[`GoogleFailure::display`] 即 TS
//!   `formatProviderError(normalizeProviderError(error))` 的可观察结果。
//!
//! # 失败模式
//!
//! - 非 2xx：按 `content-type` 决定是否 `JSON.parse` 响应体，再包成 [`GoogleFailure`]；
//!   只有 400–599 才带 `status`，而 `status` 决定 [`retry_google_request`] 是否重试。
//! - 流内错误：每个原始 chunk 先整体试解析，若 `error.code` 在 400–599 立即终止。
//! - 取消：`StreamOptions.signal` 在重试退避与读 chunk 时都会检查，分别产生
//!   `"Request aborted"` 与 `ABORTED_MESSAGE`。
//! - 残缺流：结束时缓冲区仍有非空白内容 → `"Incomplete JSON segment at the end"`。
//!
//! # 阅读顺序
//!
//! 先读纯函数（思考等级、[`convert_messages`]、[`convert_tools`]），再读
//! [`GoogleClientConfig`] / [`request_stream`]（网络边界），然后是 ADC 链，最后是
//! [`read_google_events`] / [`handle_google_chunk`]（流式状态机）。`build_wire_body` 及
//! 其 `t_*` / `*_to_wire` 辅助函数是 SDK 序列化器的逐条复刻，只在排查请求体差异时细读。

use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::kiana_messages::{Utf8StreamDecoder, next_chunk};
use crate::api::transform_messages::{NormalizeToolCallIdFn, transform_messages_with};
use crate::error::{AiError, AiErrorKind};
use crate::http::{ByteStream, HttpRequest};
use crate::types::{
    AssistantContentBlock, ClientIdentity, ImageContent, JsonObject, Message, Model, ProviderEnv,
    ProviderHeaders, StopReason, StreamOptions, TextContent, Tool, TranscriptContext, UserContent,
    UserContentBlock,
};
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{ProviderError, is_retryable_provider_error};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::transcript::{
    collapse_system_messages, get_initial_system_message, without_initial_system_message,
};

/// `GoogleApiThinkingLevel`。
///
/// Gemini 3+ 的离散思考等级，线上值就是 Google SDK 枚举的字符串本身（见
/// [`GoogleApiThinkingLevel::as_str`]）。与 [`ResolvedGoogleThinkingLevel`] 的区别：
/// 后者是 Kiana 内部的小写等级名，本枚举则可以直接写进
/// `generationConfig.thinkingConfig.thinkingLevel`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GoogleApiThinkingLevel {
    /// 未指定（`THINKING_LEVEL_UNSPECIFIED`）；[`GoogleApiThinkingLevel::parse`] 对未知
    /// 字符串也返回它，因此“解析失败”与“调用方显式传未指定”不可区分。
    Unspecified,
    /// Kiana `minimal` 映射到的档位。
    Minimal,
    /// Kiana `low` 映射到的档位。
    Low,
    /// Kiana `medium` 映射到的档位。
    Medium,
    /// Kiana `high` 映射到的档位。
    High,
}

impl GoogleApiThinkingLevel {
    /// SDK 枚举的字符串值（线上逐字）。
    ///
    /// 与 [`GoogleApiThinkingLevel::parse`] 互为逆映射（仅限五个已知值）。
    pub fn as_str(self) -> &'static str {
        match self {
            GoogleApiThinkingLevel::Unspecified => "THINKING_LEVEL_UNSPECIFIED",
            GoogleApiThinkingLevel::Minimal => "MINIMAL",
            GoogleApiThinkingLevel::Low => "LOW",
            GoogleApiThinkingLevel::Medium => "MEDIUM",
            GoogleApiThinkingLevel::High => "HIGH",
        }
    }

    /// 从线上字符串解析（**区分大小写**；未知值按 `Unspecified`）。
    ///
    /// 只接受 [`GoogleApiThinkingLevel::as_str`] 产出的五个全大写值；小写或空串都会落到
    /// `Unspecified`，不会报错。调用方是 `google_generative_ai::parse_google_thinking`，
    /// 用来解析 `extra.thinking.level`。
    pub fn parse(value: &str) -> Self {
        match value {
            "MINIMAL" => GoogleApiThinkingLevel::Minimal,
            "LOW" => GoogleApiThinkingLevel::Low,
            "MEDIUM" => GoogleApiThinkingLevel::Medium,
            "HIGH" => GoogleApiThinkingLevel::High,
            _ => GoogleApiThinkingLevel::Unspecified,
        }
    }
}

/// `ResolvedGoogleThinkingLevel = Exclude<ThinkingLevel, "xhigh" | "max">`。
///
/// Google 只认四档思考等级；`xhigh`/`max` 必须由模型的 `thinking_level_map` 先折到某一档，
/// 否则 [`resolve_google_thinking_level`] 会返回错误。该类型只在小写等级名之间转换
/// （见 [`ResolvedGoogleThinkingLevel::as_str`]），不会直接上线。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolvedGoogleThinkingLevel {
    /// Kiana 等级名 `minimal`。
    Minimal,
    /// Kiana 等级名 `low`。
    Low,
    /// Kiana 等级名 `medium`。
    Medium,
    /// Kiana 等级名 `high`。
    High,
}

impl ResolvedGoogleThinkingLevel {
    /// Kiana 内部的小写等级名，用于与 `Model.thinking_level_map` 的值比较；
    /// 上线前还要经 [`to_google_thinking_level`] 转成 SDK 枚举。
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedGoogleThinkingLevel::Minimal => "minimal",
            ResolvedGoogleThinkingLevel::Low => "low",
            ResolvedGoogleThinkingLevel::Medium => "medium",
            ResolvedGoogleThinkingLevel::High => "high",
        }
    }
}

/// `resolveGoogleThinkingLevel(model, level)`。
///
/// 把 Kiana 的 [`crate::types::ThinkingLevel`] 解析成 Google 支持的四个档位之一。解析顺序：
///
/// 1. 用 `model.thinking_level_map` 做模型级覆盖：查到字符串就取小写值；字段缺失或映射值
///    为 `null` 都回退到 Kiana 等级名本身（对应 TS 的 `typeof mapped === "string"`）；
/// 2. 只接受 `minimal` / `low` / `medium` / `high`。`xhigh`、`max` 没有 Google 对应档位，
///    除非模型映射把它们折到上述四档，否则报错。
///
/// # 错误
///
/// 映射结果不受支持时返回 [`AiErrorKind::Request`]，消息里同时给出
/// `provider/id`、原始等级与映射值（缺失的映射值按 JS 规则显示成 `undefined`，显式
/// `null` 显示成 `null`）。这条消息是用户可见文案，改动会影响快照测试。
pub fn resolve_google_thinking_level(
    model: &Model,
    level: crate::types::ThinkingLevel,
) -> Result<ResolvedGoogleThinkingLevel, AiError> {
    // 先查模型级覆盖；`Some(None)` 表示映射表里显式写了 null，与缺省一样回退。
    let mapped = model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&google_model_thinking_level(level)));
    let resolved = match mapped {
        Some(Some(value)) => value.to_lowercase(),
        _ => thinking_level_name(level).to_string(),
    };
    match resolved.as_str() {
        "minimal" => Ok(ResolvedGoogleThinkingLevel::Minimal),
        "low" => Ok(ResolvedGoogleThinkingLevel::Low),
        "medium" => Ok(ResolvedGoogleThinkingLevel::Medium),
        "high" => Ok(ResolvedGoogleThinkingLevel::High),
        _ => Err(AiError::new(
            AiErrorKind::Request,
            format!(
                "Unsupported Google thinking level mapping for {}/{}: {} -> {}",
                model.provider,
                model.id,
                thinking_level_name(level),
                js_string_of_option(mapped)
            ),
        )),
    }
}

fn google_model_thinking_level(
    level: crate::types::ThinkingLevel,
) -> crate::types::ModelThinkingLevel {
    match level {
        crate::types::ThinkingLevel::Minimal => crate::types::ModelThinkingLevel::Minimal,
        crate::types::ThinkingLevel::Low => crate::types::ModelThinkingLevel::Low,
        crate::types::ThinkingLevel::Medium => crate::types::ModelThinkingLevel::Medium,
        crate::types::ThinkingLevel::High => crate::types::ModelThinkingLevel::High,
        crate::types::ThinkingLevel::Xhigh => crate::types::ModelThinkingLevel::Xhigh,
        crate::types::ThinkingLevel::Max => crate::types::ModelThinkingLevel::Max,
    }
}

fn thinking_level_name(level: crate::types::ThinkingLevel) -> &'static str {
    match level {
        crate::types::ThinkingLevel::Minimal => "minimal",
        crate::types::ThinkingLevel::Low => "low",
        crate::types::ThinkingLevel::Medium => "medium",
        crate::types::ThinkingLevel::High => "high",
        crate::types::ThinkingLevel::Xhigh => "xhigh",
        crate::types::ThinkingLevel::Max => "max",
    }
}

/// `String(mapped)`：`undefined` → `"undefined"`、`null` → `"null"`、字符串原样。
fn js_string_of_option(value: Option<&Option<String>>) -> String {
    match value {
        Some(Some(value)) => value.clone(),
        Some(None) => "null".to_string(),
        None => "undefined".to_string(),
    }
}

/// JS 的 `String(value)`（模板字符串内插）。
pub(crate) fn js_string_of_value(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => {
            kiana_json::js_num::to_radix_string(value.as_f64().unwrap_or(f64::NAN), 10)
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string_of_value(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// JS 的 `ToNumber(value)`（关系比较用）。
fn js_to_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(value)) => value.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(value)) => kiana_json::js_num::parse_js_number(value),
        Some(Value::Array(_)) => kiana_json::js_num::parse_js_number(&js_string_of_value(value)),
        Some(Value::Object(_)) => f64::NAN,
    }
}

/// `usesGoogleThinkingLevel(model)`。
///
/// 决定模型该用离散的 `thinkingLevel` 还是 token 预算 `thinkingBudget`：这是两套互斥的
/// 线上协议，前者用于 Gemini 3+ / Gemma 4。判断只看 `model.id`（转小写），不看 provider：
///
/// - `gemini-3` 后可跟可选的 `.<数字>` 次版本，再跟 `-pro` 或 `-flash`；
/// - `gemini-<4..9>`，或 1–9 后跟一位数字（正则 `[1-9]\d` 不锚定结尾，因此
///   `gemini-100` 也会命中前两位——保持与 TS 正则一致）；
/// - 两个固定别名 `gemini-flash-latest`、`gemini-flash-lite-latest`；
/// - `gemma-4...` 与 `gemma4...` 两种 Gemma 4 命名。
///
/// 原实现用三个正则；这里改成逐字符扫描，注释里保留了对应正则。修改时必须保证与正则
/// 等价，否则同一模型会在两套协议之间切换。
pub fn uses_google_thinking_level(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    let gemini3 = {
        // /gemini-3(?:\.\d+)?-(?:pro|flash)/
        // 次版本号必须至少有一位数字；`gemini-3.-pro` 不算命中。
        id.match_indices("gemini-3").any(|(index, _)| {
            let rest = &id[index + "gemini-3".len()..];
            let rest = if let Some(stripped) = rest.strip_prefix('.') {
                let digits = stripped.chars().take_while(char::is_ascii_digit).count();
                if digits == 0 { "" } else { &stripped[digits..] }
            } else {
                rest
            };
            rest.starts_with("-pro") || rest.starts_with("-flash")
        })
    };
    let gemini4plus = {
        // /gemini-(?:[4-9]|[1-9]\d)/
        // 单个 4–9，或 1–9 后紧跟第二位数字。正则不要求结尾，`gemini-100` 同样命中。
        id.match_indices("gemini-").any(|(index, _)| {
            let rest = &id[index + "gemini-".len()..];
            let mut chars = rest.chars();
            match chars.next() {
                Some(first) if ('4'..='9').contains(&first) => true,
                Some(first) if ('1'..='9').contains(&first) => {
                    chars.next().is_some_and(|second| second.is_ascii_digit())
                }
                _ => false,
            }
        })
    };
    let gemma4 = {
        // /gemma-?4/
        // `gemma-4...` 与 `gemma4...` 两种写法都命中。
        id.match_indices("gemma").any(|(index, _)| {
            let rest = &id[index + "gemma".len()..];
            rest.starts_with('4') || rest.starts_with("-4")
        })
    };
    // 两个滚动别名无法被上面的前缀模式覆盖，单独比较。
    gemini3
        || gemini4plus
        || id == "gemini-flash-latest"
        || id == "gemini-flash-lite-latest"
        || gemma4
}

/// `toGoogleThinkingLevel(level)`。
///
/// 四档一一映射（`minimal` → `MINIMAL` …）。输入已由 [`resolve_google_thinking_level`]
/// 校验过，所以本函数不会失败，也没有兜底分支。
pub fn to_google_thinking_level(level: ResolvedGoogleThinkingLevel) -> GoogleApiThinkingLevel {
    match level {
        ResolvedGoogleThinkingLevel::Minimal => GoogleApiThinkingLevel::Minimal,
        ResolvedGoogleThinkingLevel::Low => GoogleApiThinkingLevel::Low,
        ResolvedGoogleThinkingLevel::Medium => GoogleApiThinkingLevel::Medium,
        ResolvedGoogleThinkingLevel::High => GoogleApiThinkingLevel::High,
    }
}

/// `toGoogleSdkThinkingLevel(level)`：线上就是枚举字符串本身。
///
/// TS 侧查 `GOOGLE_SDK_THINKING_LEVEL_MAP` 表；两边枚举的字符串值同名，所以这里直接返回
/// [`GoogleApiThinkingLevel::as_str`]。`Unspecified` 会原样发出
/// `THINKING_LEVEL_UNSPECIFIED`，不会省略字段。
pub fn to_google_sdk_thinking_level(level: GoogleApiThinkingLevel) -> &'static str {
    level.as_str()
}

/// `getDisabledGoogleThinkingConfig(model)`。
///
/// TS: `clampThinkingLevel(model, "off")` → `resolveGoogleThinkingLevel(model, fallback)`，
/// 因此自定义 `thinkingLevelMap` 会参与映射，映射非法时抛错。
///
/// 返回要写进 `generationConfig.thinkingConfig` 的对象，三种结果：
///
/// - 模型不支持离散等级 → `{"thinkingBudget": 0}`（用预算关闭思考）；
/// - 支持离散等级、且 `clampThinkingLevel(model, "off")` 仍是 `off` → 同样发
///   `thinkingBudget: 0`；
/// - `off` 被模型钳到更高的最低档 → 发该档的 `{"thinkingLevel": "..."}`，
///   因为这类模型无法真正关闭思考。
///
/// # 错误
///
/// 仅当模型级映射把回退档映射到四档之外时返回 [`AiErrorKind::Request`]。
pub fn get_disabled_google_thinking_config(model: &Model) -> Result<Value, AiError> {
    // 旧协议模型没有 thinkingLevel 字段，只能发预算 0。
    if !uses_google_thinking_level(model) {
        return Ok(object([("thinkingBudget", Value::from(0))]));
    }
    let fallback =
        crate::models::clamp_thinking_level(model, crate::types::ModelThinkingLevel::Off);
    // 钳完仍是 off：模型允许真正关闭思考。
    if fallback == crate::types::ModelThinkingLevel::Off {
        return Ok(object([("thinkingBudget", Value::from(0))]));
    }
    let resolved = resolve_google_thinking_level(model, model_level_to_thinking_level(fallback))?;
    Ok(object([(
        "thinkingLevel",
        Value::String(to_google_thinking_level(resolved).as_str().into()),
    )]))
}

fn model_level_to_thinking_level(
    level: crate::types::ModelThinkingLevel,
) -> crate::types::ThinkingLevel {
    match level {
        crate::types::ModelThinkingLevel::Off => crate::types::ThinkingLevel::Minimal,
        crate::types::ModelThinkingLevel::Minimal => crate::types::ThinkingLevel::Minimal,
        crate::types::ModelThinkingLevel::Low => crate::types::ThinkingLevel::Low,
        crate::types::ModelThinkingLevel::Medium => crate::types::ThinkingLevel::Medium,
        crate::types::ModelThinkingLevel::High => crate::types::ThinkingLevel::High,
        crate::types::ModelThinkingLevel::Xhigh => crate::types::ThinkingLevel::Xhigh,
        crate::types::ModelThinkingLevel::Max => crate::types::ThinkingLevel::Max,
    }
}

/// `isThinkingPart(part)`：`part.thought === true`。
///
/// 只有 `thought` 严格等于 `true` 才算思考内容。`thoughtSignature` 是加密的思考上下文，
/// 可以挂在任意 part（text / functionCall）上，**不**表示该 part 是思考块；把带
/// signature 的普通文本误判成 thinking 会改变回放时的消息结构。
pub fn is_thinking_part(part: &Value) -> bool {
    part.get("thought").and_then(Value::as_bool) == Some(true)
}

/// `retainThoughtSignature(existing, incoming)`。
///
/// 流式响应里同一块的签名可能只在第一个 delta 出现，后续 delta 不带；本函数保证块内
/// 已有的非空签名不会被 `None` 覆盖。它**不**跨 part 合并或移动签名：调用方必须按块
/// 分别保存（见 [`handle_google_chunk`] 的 text / thinking 分支）。
pub fn retain_thought_signature(existing: Option<&str>, incoming: Option<&str>) -> Option<String> {
    match incoming {
        Some(incoming) if !incoming.is_empty() => Some(incoming.to_string()),
        _ => existing.map(str::to_string),
    }
}

/// `isValidThoughtSignature(signature)`。
///
/// 只接受标准 Base64（`A-Za-z0-9+/`，至多两个尾部 `=`）且长度为 4 的倍数。Google 把
/// signature 声明为 `TYPE_BYTES`，格式不合法的值回传会被拒绝；空串与 `None` 都视为无效。
fn is_valid_thought_signature(signature: Option<&str>) -> bool {
    let Some(signature) = signature.filter(|signature| !signature.is_empty()) else {
        return false;
    };
    if signature.len() % 4 != 0 {
        return false;
    }
    static BASE64: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[A-Za-z0-9+/]+={0,2}$").expect("valid regex")
    });
    BASE64.is_match(signature)
}

/// `resolveThoughtSignature(isSameProviderAndModel, signature)`。
///
/// 跨 provider / 跨模型的签名无法被当前模型验证，直接丢弃；只有同源且格式合法时才
/// 回传。`convert_messages` 用它决定 text / thinking / toolCall 各块是否带签名。
fn resolve_thought_signature(
    is_same_provider_and_model: bool,
    signature: Option<&str>,
) -> Option<String> {
    if is_same_provider_and_model && is_valid_thought_signature(signature) {
        signature.map(str::to_string)
    } else {
        None
    }
}

/// `requiresToolCallId(modelId)`。
///
/// 判断模型是否要求 `functionCall` / `functionResponse` 携带显式 `id`：Gemini 3 及以上、
/// 以及挂在 Google API 后面的 `claude-*` / `gpt-oss-*`。为 `true` 时，
/// [`convert_messages`] 还会把历史 tool call id 清洗成 `[A-Za-z0-9_-]` 并截断到 64 字符，
/// 保证请求里的 id 与结果消息里的 id 仍然对得上（`transform_messages_with` 的回调）。
pub fn requires_tool_call_id(model_id: &str) -> bool {
    let major = get_gemini_major_version(model_id);
    model_id.starts_with("claude-")
        || model_id.starts_with("gpt-oss-")
        || major.is_some_and(|major| major >= 3)
}

/// `getGeminiMajorVersion(modelId)`。
///
/// 从 `gemini-<数字>...` 或 `gemini-live-<数字>...` 中取主版本号（大小写不敏感）。没有
/// 该前缀或前缀后不是数字时返回 `None`——包括 `claude-*`、`gpt-oss-*` 这类非 Gemini
/// 模型。两个能力判定（[`requires_tool_call_id`]、内部
/// `supports_multimodal_function_response`）都建立在这个结果上。
pub fn get_gemini_major_version(model_id: &str) -> Option<u32> {
    let lower = model_id.to_lowercase();
    let rest = lower
        .strip_prefix("gemini-live-")
        .or_else(|| lower.strip_prefix("gemini-"))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// `supportsMultimodalFunctionResponse(modelId)`。
///
/// Gemini 3+ 允许把图片嵌在 `functionResponse.parts` 里；更老的 Gemini 需要单独一轮
/// user 消息。无法解析出版本号的 id（如 `claude-*`、`gpt-oss-*`）返回 `true`，即按
/// “支持嵌套”处理——这是 TS 的原行为，测试
/// `google_shared_image_tool_result_routing` 锁定了 Gemini 2.x 的独立图片轮。
fn supports_multimodal_function_response(model_id: &str) -> bool {
    match get_gemini_major_version(model_id) {
        Some(major) => major >= 3,
        None => true,
    }
}

/// `convertMessages(model, context)`：内部消息 → Gemini `Content[]`。
///
/// 完整流程：
///
/// 1. `collapse_system_messages` 合并分散的 system 消息，再剥掉首条 system——它由
///    [`build_params`] 单独放进 `systemInstruction`，不能留在 `contents` 里；
/// 2. `transform_messages_with` 做跨 provider 归一化，并顺带按同一规则清洗 tool call id；
/// 3. 逐条映射：
///    - user 文本 → 一个 `text` part；user 块 → `text` / `inlineData`（图片 base64 原样）；
///    - assistant：**同 provider 且同 model** 时保留 thinking 块与 thought signature，
///      否则 thinking 降级成普通文本、签名丢弃；空白文本/思考块只有在带合法签名时才保留
///      （Gemini 会把签名挂在文本为空的 part 上，丢掉它会让推理链断裂）；
///    - toolResult：文本用 `\n` 拼接后放进 `functionResponse.response.output`
///      （错误则用 `error` 键），图片按模型能力嵌进 `functionResponse.parts` 或另起一轮
///      user 消息；
/// 4. 连续的 toolResult 会追加到上一个“含 functionResponse 的 user 轮”，而不是新建一轮。
///
/// 返回值是 `Content[]`。part 的键序由 `Map`（`preserve_order`）的插入顺序决定，模块内
/// 测试对请求体做逐字节字符串断言。
pub fn convert_messages(model: &Model, context: &TranscriptContext) -> Value {
    // Gemini 没有会话中途的 system 消息；首部 prompt 作为 systemInstruction 发送。
    let collapsed = collapse_system_messages(context);
    let conversation = without_initial_system_message(&collapsed.0.messages);
    let mut contents: Vec<Value> = Vec::new();

    // Gemini 3+ 的 tool call id 只允许 `[A-Za-z0-9_-]`、最长 64 字符；这里的清洗规则
    // 必须与结果消息一侧一致，transform_messages_with 会把映射同时套到两边。
    let normalize_ref: &NormalizeToolCallIdFn =
        &|id: &str, callback_model: &Model, _message: &crate::types::AssistantMessage| {
            if !requires_tool_call_id(&callback_model.id) {
                return id.to_string();
            }
            let sanitized: String = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            sanitized.chars().take(64).collect()
        };
    let transformed = transform_messages_with(&conversation, model, Some(normalize_ref));

    // 首条 system 已被剥离；其余 System 在 Gemini 协议里没有位置，直接忽略。
    for message in &transformed {
        match message {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    contents.push(object([
                        (
                            "parts",
                            Value::Array(vec![object([(
                                "text",
                                Value::String(sanitize_surrogates(text)),
                            )])]),
                        ),
                        ("role", Value::String("user".into())),
                    ]));
                }
                UserContent::Blocks(blocks) => {
                    let parts: Vec<Value> = blocks
                        .iter()
                        .map(|item| match item {
                            UserContentBlock::Text(text) => {
                                object([("text", Value::String(sanitize_surrogates(&text.text)))])
                            }
                            UserContentBlock::Image(image) => object([(
                                "inlineData",
                                object([
                                    ("mimeType", Value::String(image.mime_type.clone())),
                                    ("data", Value::String(image.data.clone())),
                                ]),
                            )]),
                        })
                        .collect();
                    if parts.is_empty() {
                        continue;
                    }
                    contents.push(object([
                        ("parts", Value::Array(parts)),
                        ("role", Value::String("user".into())),
                    ]));
                }
            },
            Message::Assistant(assistant) => {
                let mut parts: Vec<Value> = Vec::new();
                // 只有同 provider + 同 model 的历史块才能回传思考内容与签名；判断条件与
                // TS 一致（只看 provider/model，不看 api）。
                let is_same_provider_and_model =
                    assistant.provider == model.provider && assistant.model == model.id;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            let thought_signature = resolve_thought_signature(
                                is_same_provider_and_model,
                                text.text_signature.as_deref(),
                            );
                            if kiana_json::js_string::js_trim(&text.text).is_empty()
                                && thought_signature.is_none()
                            {
                                continue;
                            }
                            let mut part = Map::new();
                            part.insert(
                                "text".into(),
                                Value::String(sanitize_surrogates(&text.text)),
                            );
                            if let Some(signature) = thought_signature {
                                part.insert("thoughtSignature".into(), Value::String(signature));
                            }
                            parts.push(Value::Object(part));
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            if is_same_provider_and_model {
                                let thought_signature = resolve_thought_signature(
                                    true,
                                    thinking.thinking_signature.as_deref(),
                                );
                                if kiana_json::js_string::js_trim(&thinking.thinking).is_empty()
                                    && thought_signature.is_none()
                                {
                                    continue;
                                }
                                let mut part = Map::new();
                                part.insert("thought".into(), Value::Bool(true));
                                part.insert(
                                    "text".into(),
                                    Value::String(sanitize_surrogates(&thinking.thinking)),
                                );
                                if let Some(signature) = thought_signature {
                                    part.insert(
                                        "thoughtSignature".into(),
                                        Value::String(signature),
                                    );
                                }
                                parts.push(Value::Object(part));
                            } else {
                                if kiana_json::js_string::js_trim(&thinking.thinking).is_empty() {
                                    continue;
                                }
                                parts.push(object([(
                                    "text",
                                    Value::String(sanitize_surrogates(&thinking.thinking)),
                                )]));
                            }
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            let thought_signature = resolve_thought_signature(
                                is_same_provider_and_model,
                                call.thought_signature.as_deref(),
                            );
                            let mut function_call = Map::new();
                            function_call.insert("name".into(), Value::String(call.name.clone()));
                            function_call.insert(
                                "args".into(),
                                Value::Object(
                                    call.arguments
                                        .iter()
                                        .map(|(key, value)| (key.clone(), value.clone()))
                                        .collect(),
                                ),
                            );
                            if requires_tool_call_id(&model.id) {
                                function_call.insert("id".into(), Value::String(call.id.clone()));
                            }
                            let mut part = Map::new();
                            part.insert("functionCall".into(), Value::Object(function_call));
                            if let Some(signature) = thought_signature {
                                part.insert("thoughtSignature".into(), Value::String(signature));
                            }
                            parts.push(Value::Object(part));
                        }
                    }
                }
                if parts.is_empty() {
                    continue;
                }
                contents.push(object([
                    ("role", Value::String("model".into())),
                    ("parts", Value::Array(parts)),
                ]));
            }
            Message::ToolResult(tool_result) => {
                // Gemini 的 tool 结果不是独立 role，而是 role=user 的 functionResponse part。
                let text_content: Vec<&TextContent> = tool_result
                    .content
                    .iter()
                    .filter_map(|item| match item {
                        UserContentBlock::Text(text) => Some(text),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect();
                let text_result = text_content
                    .iter()
                    .map(|text| text.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                // 模型不支持 image 输入时，图片部分被直接丢弃（不报错）。
                let supports_image = model
                    .input
                    .contains(&crate::types::ModelInputModality::Image);
                let image_content: Vec<&ImageContent> = if supports_image {
                    tool_result
                        .content
                        .iter()
                        .filter_map(|item| match item {
                            UserContentBlock::Image(image) => Some(image),
                            UserContentBlock::Text(_) => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                // TS 的取值优先级：非空文本 → 仅图片时的占位串 → 空串。
                let has_text = !kiana_json::js_string::js_trim(&text_result).is_empty();
                let has_images = !image_content.is_empty();
                let supports_multimodal = supports_multimodal_function_response(&model.id);
                let response_value = if has_text {
                    sanitize_surrogates(&text_result)
                } else if has_images {
                    "(see attached image)".to_string()
                } else {
                    String::new()
                };
                let image_parts: Vec<Value> = image_content
                    .iter()
                    .map(|image| {
                        object([(
                            "inlineData",
                            object([
                                ("mimeType", Value::String(image.mime_type.clone())),
                                ("data", Value::String(image.data.clone())),
                            ]),
                        )])
                    })
                    .collect();

                let include_id = requires_tool_call_id(&model.id);
                let mut function_response = Map::new();
                function_response
                    .insert("name".into(), Value::String(tool_result.tool_name.clone()));
                function_response.insert(
                    "response".into(),
                    if tool_result.is_error {
                        object([("error", Value::String(response_value))])
                    } else {
                        object([("output", Value::String(response_value))])
                    },
                );
                if has_images && supports_multimodal {
                    function_response.insert("parts".into(), Value::Array(image_parts.clone()));
                }
                if include_id {
                    function_response
                        .insert("id".into(), Value::String(tool_result.tool_call_id.clone()));
                }
                let function_response_part =
                    object([("functionResponse", Value::Object(function_response))]);

                // Cloud Code Assist 要求所有 functionResponse 落在同一个 user 轮：若上一轮
                // 已经是带 functionResponse 的 user 消息，就追加而不是新开一轮。
                let last_is_user_with_function_response = contents.last().is_some_and(|last| {
                    last.get("role").and_then(Value::as_str) == Some("user")
                        && last
                            .get("parts")
                            .and_then(Value::as_array)
                            .is_some_and(|parts| {
                                parts
                                    .iter()
                                    .any(|part| part.get("functionResponse").is_some())
                            })
                });
                if last_is_user_with_function_response {
                    if let Some(Value::Array(parts)) =
                        contents.last_mut().and_then(|last| last.get_mut("parts"))
                    {
                        parts.push(function_response_part);
                    }
                } else {
                    contents.push(object([
                        ("parts", Value::Array(vec![function_response_part])),
                        ("role", Value::String("user".into())),
                    ]));
                }

                // Gemini < 3 不能把图片嵌进 functionResponse，只能另起一轮 user 消息，
                // 用一句固定提示引出图片。
                if has_images && !supports_multimodal {
                    let mut parts = vec![object([(
                        "text",
                        Value::String("Tool result image:".into()),
                    )])];
                    parts.extend(image_parts);
                    contents.push(object([
                        ("parts", Value::Array(parts)),
                        ("role", Value::String("user".into())),
                    ]));
                }
            }
            Message::System(_) => {}
        }
    }
    Value::Array(contents)
}

/// `JSON_SCHEMA_META_DECLARATIONS`。
const JSON_SCHEMA_META_DECLARATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    "definitions",
];

/// `sanitizeForOpenApi(schema)`。
///
/// 递归删除 OpenAPI 不接受的 JSON Schema 元声明（`$schema`、`$id`、`$defs` 等，完整列表
/// 见 `JSON_SCHEMA_META_DECLARATIONS`），用于 [`convert_tools`] 的旧版 `parameters` 字段。
/// 数组按字面数据原样返回、不递归——`enum`/`examples` 里的内容不是 schema 节点，这一点与
/// TS 一致，不要改成递归。
pub fn sanitize_for_openapi(schema: &Value) -> Value {
    match schema {
        // TS 对数组原样返回（不递归）——数组元素是调用方数据形态，不是 schema 节点。
        Value::Array(_) => schema.clone(),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, value) in map {
                if JSON_SCHEMA_META_DECLARATIONS.contains(&key.as_str()) {
                    continue;
                }
                result.insert(key.clone(), sanitize_for_openapi(value));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

/// `JSON_SCHEMA_NAME_MAPS`。
const JSON_SCHEMA_NAME_MAPS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "dependencies",
];

/// `JSON_SCHEMA_SUBSCHEMAS`。
const JSON_SCHEMA_SUBSCHEMAS: &[&str] = &[
    "additionalProperties",
    "unevaluatedProperties",
    "propertyNames",
    "items",
    "prefixItems",
    "additionalItems",
    "unevaluatedItems",
    "contains",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
    "contentSchema",
];

/// `rewriteConstAsEnum(schema, isNameMap = false)`。
///
/// Google 在 `parametersJsonSchema` 里忽略 `const`，但认 `enum`，所以把字符串/数字的
/// `const` 改写成单值 `enum`（值放在数组末尾，键序与 TS 的 delete + assign 一致）。
///
/// 递归规则：
/// - `properties`/`$defs` 等 name-map 的键是**属性名**而不是 schema 关键字，进入后不再把
///   这些键当关键字处理（`is_name_map = true`）；
/// - `items`/`anyOf`/`not` 等 subschema 位置继续递归；
/// - 其它值（`default`、`examples`、`enum` 内部）是字面数据，原样保留。
///
/// 只有字符串/数字的 `const` 会被改写；对象、数组、布尔、`null` 保持原样。
pub fn rewrite_const_as_enum(schema: &Value, is_name_map: bool) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| rewrite_const_as_enum(item, false))
                .collect(),
        ),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, value) in map {
                if is_name_map || JSON_SCHEMA_SUBSCHEMAS.contains(&key.as_str()) {
                    result.insert(key.clone(), rewrite_const_as_enum(value, false));
                } else if JSON_SCHEMA_NAME_MAPS.contains(&key.as_str()) {
                    result.insert(key.clone(), rewrite_const_as_enum(value, true));
                } else {
                    result.insert(key.clone(), value.clone());
                }
            }
            if !is_name_map && result.contains_key("const") && !result.contains_key("enum") {
                let value = result.get("const").cloned().unwrap_or(Value::Null);
                if value.is_string() || value.is_number() {
                    result.remove("const");
                    result.insert("enum".into(), Value::Array(vec![value]));
                }
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

/// `convertTools(tools, useParameters = false, supportsStrictMode = true)`。
///
/// 返回 SDK 的 `Tool[]` 形状 `[{ "functionDeclarations": [...] }]`；工具列表为空时返回
/// `Ok(None)`（对应 TS 的 `undefined`，序列化后整个 `tools` 字段消失）。
///
/// 每个声明先由 `resolve_json_schema_strict_sampling` 结合模型能力求得 `strict`，再交给
/// `get_json_schema_tool_parameters` 生成 schema：
///
/// - `use_parameters = false`（默认）→ 写 `parametersJsonSchema`，并做
///   [`rewrite_const_as_enum`]；
/// - `use_parameters = true`（Cloud Code Assist 上给 Claude 用的旧字段）→ 写 `parameters`，
///   并做 [`sanitize_for_openapi`]。
///
/// # 错误
///
/// strict 约束无法满足时原样返回 `AiError`；[`build_params`] 同样向上传递，最终由两个
/// 适配器在 `run_stream` 里转成 `GoogleFailure`。
pub fn convert_tools(
    tools: &[Tool],
    use_parameters: bool,
    supports_strict_mode: bool,
) -> Result<Option<Value>, AiError> {
    if tools.is_empty() {
        return Ok(None);
    }
    let mut declarations = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?;
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        // 键序 name → description → parameters*：SDK 序列化器按此顺序输出，测试逐字节锁定。
        let mut declaration = Map::new();
        declaration.insert("name".into(), Value::String(tool.name.clone()));
        declaration.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        if use_parameters {
            declaration.insert("parameters".into(), sanitize_for_openapi(&parameters));
        } else {
            declaration.insert(
                "parametersJsonSchema".into(),
                rewrite_const_as_enum(&parameters, false),
            );
        }
        declarations.push(Value::Object(declaration));
    }
    Ok(Some(Value::Array(vec![object([(
        "functionDeclarations",
        Value::Array(declarations),
    )])])))
}

/// `supportsGoogleStrictToolSampling(modelId)`。
///
/// Gemini 3+ 在 `VALIDATED` / `ANY` 模式下会强制校验 required 参数，因此可以为工具开启
/// strict；版本号解析不出来（非 Gemini 模型）时视为不支持。
pub fn supports_google_strict_tool_sampling(model_id: &str) -> bool {
    get_gemini_major_version(model_id).is_some_and(|major| major >= 3)
}

/// `mapToolChoice(choice)`：`auto` → `AUTO`、`none` → `NONE`、`any` → `ANY`、其它 → `AUTO`。
///
/// 未知字符串不报错，回退 `AUTO`（TS 的 `default` 分支）。返回 `&'static str` 是为了让
/// 调用方无需再分配字符串。
pub fn map_tool_choice(choice: &str) -> &'static str {
    match choice {
        "auto" => "AUTO",
        "none" => "NONE",
        "any" => "ANY",
        _ => "AUTO",
    }
}

/// `resolveGoogleFunctionCallingMode(tools, toolChoice, supportsStrictMode)`。
///
/// 返回要写进 `toolConfig.functionCallingConfig.mode` 的字符串；`None` 表示整个
/// `toolConfig` 都不发。优先级如下：
///
/// 1. `tool_choice` 为 `none` / `any` → 直接映射（显式禁用/强制调用优先于 strict）；
/// 2. 任一工具解析出 `strict = true` → `VALIDATED`（strict 工具要求校验模式；`AUTO`
///    不会强制 required 参数）；
/// 3. 其余：非空 `tool_choice` 映射，空串或未设置 → `None`（JS 里空串是假值）。
///
/// # 错误
///
/// strict 解析失败时原样返回 `AiError`。
pub fn resolve_google_function_calling_mode(
    tools: &[Tool],
    tool_choice: Option<&str>,
    supports_strict_mode: bool,
) -> Result<Option<&'static str>, AiError> {
    let mut use_strict_mode = false;
    for tool in tools {
        if resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)? == Some(true) {
            use_strict_mode = true;
            break;
        }
    }
    if matches!(tool_choice, Some("none") | Some("any")) {
        return Ok(Some(map_tool_choice(tool_choice.unwrap_or("auto"))));
    }
    if use_strict_mode {
        return Ok(Some("VALIDATED"));
    }
    // TS: `return toolChoice ? mapToolChoice(toolChoice) : undefined;`——空字符串是假值。
    Ok(tool_choice
        .filter(|choice| !choice.is_empty())
        .map(map_tool_choice))
}

/// `mapStopReason(reason)`（Gemini `FinishReason`）。
///
/// 映射关系：`STOP` → `Stop`，`MAX_TOKENS` → `Length`；安全过滤、内容政策、内部错误等
/// 一整组（见 `match` 的分支）→ `Error`，调用方会把 `raw_stop_reason` 拼成用户可见错误。
/// 不在表里的值返回 [`AiErrorKind::Other`]，消息为
/// `Unhandled stop reason: ...`（TS 侧的 `never` 穷尽检查在这里变成运行时错误）。
pub fn map_stop_reason(reason: &str) -> Result<StopReason, AiError> {
    match reason {
        "STOP" => Ok(StopReason::Stop),
        "MAX_TOKENS" => Ok(StopReason::Length),
        "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "SAFETY"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION"
        | "IMAGE_OTHER"
        | "RECITATION"
        | "FINISH_REASON_UNSPECIFIED"
        | "OTHER"
        | "LANGUAGE"
        | "MALFORMED_FUNCTION_CALL"
        | "UNEXPECTED_TOOL_CALL"
        | "TOO_MANY_TOOL_CALLS"
        | "NO_IMAGE" => Ok(StopReason::Error),
        other => Err(AiError::new(
            AiErrorKind::Other,
            format!("Unhandled stop reason: {other}"),
        )),
    }
}

/// `mapStopReasonString(reason)`（原始 API 字符串）。
///
/// 只识别 `STOP` / `MAX_TOKENS`，其余（包括空串与未知值）都归为 `Error`；不返回错误，
/// 与 [`map_stop_reason`] 的严格版本相对。
pub fn map_stop_reason_string(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::Stop,
        "MAX_TOKENS" => StopReason::Length,
        _ => StopReason::Error,
    }
}

// ---------------------------------------------------------------------------
// 客户端配置（@google/genai 构造参数的等价物）
// ---------------------------------------------------------------------------

/// `HttpOptions` 的可观察字段。
///
/// 这是 SDK 构造参数 `httpOptions` 的 Rust 视图：客户端级由
/// [`create_gemini_client`] / [`build_vertex_http_options`] 填充，请求级由
/// `patch_http_options` 用 `config.httpOptions` 覆盖。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoogleHttpOptions {
    /// 自定义基地址（`model.baseUrl` 或请求级 `httpOptions.baseUrl`）；末尾的 `/` 会被
    /// 规范化掉，拼接时不再重复。
    pub base_url: Option<String>,
    /// 线上值 `"COLLECTION"`。
    ///
    /// 表示 `base_url` 已经是资源集合根：Vertex 不再补
    /// `projects/<project>/locations/<location>` 前缀（逻辑在
    /// `GoogleClientConfig::should_prepend_project_location`）。
    pub base_url_resource_scope: Option<String>,
    /// URL 中的版本段。`Some("")` 表示 baseUrl 已自带版本、不再追加；`None` 使用默认值
    /// （Vertex `v1`，Gemini `v1beta`）。
    pub api_version: Option<String>,
    /// 合并后的请求头（身份头 → `model.headers` → 调用方头，后者覆盖前者；名按小写去重）。
    pub headers: Option<IndexMap<String, String>>,
    /// 请求级 `config.httpOptions.timeout`（毫秒）。
    ///
    /// 同一次请求既用作传输超时，也会转成 `X-Server-Timeout`（向上取整到秒）；
    /// `<= 0` 视为未设置。
    pub timeout: Option<f64>,
    /// 请求级 `config.httpOptions.extraBody`（深合并进请求体）。
    ///
    /// 在 [`build_wire_body`] 之后应用，可覆盖或新增任意请求体字段。
    pub extra_body: Option<Value>,
}

/// `GoogleGenAI` 构造参数的可观察视图。
///
/// 两个适配器分别用 [`create_gemini_client`]（Gemini Developer API）和
/// [`build_vertex_http_options`] + `google_vertex::create_client`（Vertex）构造它，再交给
/// [`request_stream`]。本结构只保存构造参数，不持有 HTTP 连接或 token 缓存——
/// 每次请求都会重新走一遍凭据链（见 `vertex_access_token`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoogleClientConfig {
    /// `true` 走 Vertex AI（`aiplatform.googleapis.com` 或区域域名 + `projects/...` 路径）；
    /// `false` 走 Generative Language API。
    pub vertexai: bool,
    /// Gemini Developer API 的 key；Vertex 模式下若存在也优先于 ADC，直接作
    /// `x-goog-api-key`。
    pub api_key: Option<String>,
    /// Vertex 默认项目；`None` 时 URL 不拼项目前缀（由 baseUrl 或调用方给完整路径）。
    pub project: Option<String>,
    /// Vertex 默认位置；同时影响主机名（`us`/`eu` 用 `rep.googleapis.com`）与资源路径。
    pub location: Option<String>,
    /// 构造时指定的 API 版本，用于对齐 SDK 的参数。当前 URL 的版本段只由
    /// `GoogleHttpOptions::api_version` 决定（缺失时 Vertex 默认 `v1`、Gemini 默认
    /// `v1beta`）；`GoogleClientConfig::request_base_with` 从不读取本字段。
    pub api_version: Option<String>,
    /// `googleAuthOptions.keyFilename`（仅 Vertex ADC 客户端）。
    pub google_auth_key_filename: Option<String>,
    /// 客户端级 HTTP 选项；请求级 `config.httpOptions` 会在 [`request_stream`] 里再
    /// patch 一次（逐键覆盖或合并）。
    pub http_options: Option<GoogleHttpOptions>,
}

impl GoogleClientConfig {
    /// 计算请求基地址（`ApiClient.constructUrl` 的等价物）。
    ///
    /// 基地址优先级：请求级（或客户端级）`http.base_url` → Vertex 规则主机 → Gemini
    /// 固定主机。Vertex 且没有 API key 时按 `location` 选主机：`global` 或缺失用
    /// `aiplatform.googleapis.com`，`us`/`eu` 用 `aiplatform.<loc>.rep.googleapis.com`，
    /// 其它区域用 `<loc>-aiplatform.googleapis.com`；有 API key 时一律用
    /// `aiplatform.googleapis.com`。
    ///
    /// 版本段：`http.api_version` 优先；缺失时 Vertex 默认 `v1`、Gemini 默认 `v1beta`；
    /// `Some("")` 表示版本已包含在 baseUrl 里，拼接时跳过。
    fn request_base_with(&self, http: &GoogleHttpOptions) -> (String, Option<String>) {
        let base_url = match &http.base_url {
            Some(url) => url.clone(),
            None => {
                if self.vertexai {
                    if self.api_key.is_some() {
                        "https://aiplatform.googleapis.com/".to_string()
                    } else {
                        match self.location.as_deref() {
                            Some("global") => "https://aiplatform.googleapis.com/".to_string(),
                            Some(location) if location == "us" || location == "eu" => {
                                format!("https://aiplatform.{location}.rep.googleapis.com/")
                            }
                            Some(location) => {
                                format!("https://{location}-aiplatform.googleapis.com/")
                            }
                            None => "https://aiplatform.googleapis.com/".to_string(),
                        }
                    }
                } else {
                    "https://generativelanguage.googleapis.com/".to_string()
                }
            }
        };
        let api_version = http
            .api_version
            .clone()
            .or_else(|| Some(if self.vertexai { "v1" } else { "v1beta" }.to_string()));
        (base_url, api_version)
    }

    /// `shouldPrependVertexProjectPath(request, httpOptions)`。
    ///
    /// 只有四个条件同时成立才补 `projects/<project>/locations/<location>`：
    /// 不是 `COLLECTION` 资源根、是 Vertex、project 与 location 都有值、路径还不是完整
    /// 资源名（不以 `projects/` 开头）。
    fn should_prepend_project_location(&self, path: &str, http: &GoogleHttpOptions) -> bool {
        if http.base_url.is_some() && http.base_url_resource_scope.as_deref() == Some("COLLECTION")
        {
            return false;
        }
        if !self.vertexai {
            return false;
        }
        if self.project.is_none() || self.location.is_none() {
            return false;
        }
        // 路径已经是完整资源名（`projects/...`）时不再补前缀。
        if path.starts_with("projects/") {
            return false;
        }
        true
    }

    /// `tModel(apiClient, model)` 的 `_url.model` 片段。
    ///
    /// 校验：`model` 必须是非空字符串，且不含 `..`、`?`、`&`（防路径穿越与查询串注入）。
    /// 之后按模式补全：
    ///
    /// - Vertex：已带 `publishers/`、`projects/`、`models/` 前缀的原样返回；含 `/` 的
    ///   `publisher/model` 改写成 `publishers/<p>/models/<m>`（只看前两段）；其余补
    ///   `publishers/google/models/`；
    /// - Gemini：`models/`、`tunedModels/` 原样，其余补 `models/`。
    fn model_path(&self, model: &Value) -> Result<String, GoogleFailure> {
        let Some(model) = model.as_str().filter(|model| !model.is_empty()) else {
            return Err(GoogleFailure::new("model is required and must be a string"));
        };
        if model.contains("..") || model.contains('?') || model.contains('&') {
            return Err(GoogleFailure::new("invalid model parameter"));
        }
        if self.vertexai {
            if model.starts_with("publishers/")
                || model.starts_with("projects/")
                || model.starts_with("models/")
            {
                return Ok(model.to_string());
            }
            if model.contains('/') {
                // JS `model.split('/', 2)`：只保留前两段。
                let mut parts = model.splitn(3, '/');
                let publisher = parts.next().unwrap_or("");
                let name = parts.next().unwrap_or("");
                return Ok(format!("publishers/{publisher}/models/{name}"));
            }
            Ok(format!("publishers/google/models/{model}"))
        } else if model.starts_with("models/") || model.starts_with("tunedModels/") {
            Ok(model.to_string())
        } else {
            Ok(format!("models/{model}"))
        }
    }

    /// `constructUrl(path, httpOptions, prependProjectLocation)`。
    ///
    /// 按 `[base, apiVersion?, projects/<p>/locations/<l>?, path]` 的顺序用 `/` 连接，
    /// 再交给 `url::Url::parse`。空版本段会被跳过；基地址末尾的 `/` 先裁掉，避免出现
    /// `//`。任何解析失败都转成 [`GoogleFailure`]（无 status）。
    fn construct_url(
        &self,
        path: &str,
        http: &GoogleHttpOptions,
    ) -> Result<url::Url, GoogleFailure> {
        let (base_url, api_version) = self.request_base_with(http);
        let base = base_url.trim_end_matches('/');
        let mut parts = vec![base.to_string()];
        // `Some("")` 表示 baseUrl 已自带版本，这里不追加。
        if let Some(api_version) = api_version.as_deref().filter(|v| !v.is_empty()) {
            parts.push(api_version.to_string());
        }
        if self.should_prepend_project_location(path, http) {
            parts.push(format!(
                "projects/{}/locations/{}",
                self.project.as_deref().unwrap_or(""),
                self.location.as_deref().unwrap_or("")
            ));
        }
        parts.push(path.to_string());
        url::Url::parse(&parts.join("/")).map_err(|error| GoogleFailure::new(error.to_string()))
    }

    /// `constructUrl('{model}:streamGenerateContent?alt=sse', ...)`。
    ///
    /// 便捷入口：只为某个 model id 生成 SSE URL，错误统一转成 [`AiErrorKind::Request`]。
    /// 它使用**客户端级** `http_options`，不包含请求级 `config.httpOptions`；真正发请求时
    /// [`request_stream`] 会自己用 patch 后的选项重算 URL。模块测试用它校验
    /// `tModel` 的路径拼接规则。
    pub fn stream_url(&self, model_id: &str) -> Result<url::Url, AiError> {
        let http = self.http_options.clone().unwrap_or_default();
        let path = self
            .model_path(&Value::String(model_id.to_string()))
            .map_err(|error| AiError::new(AiErrorKind::Request, error.message))?;
        self.construct_url(&format!("{path}:streamGenerateContent?alt=sse"), &http)
            .map_err(|error| AiError::new(AiErrorKind::Request, error.message))
    }
}

/// `providerHeadersToRecord(getIdentityHeaders(model, identity), model.headers, optionsHeaders)`。
///
/// 三个来源按顺序合并，后面的覆盖前面的（`provider_headers_to_record` 按小写名去重并
/// 保留最后一次写入的原始大小写）；结果为空时返回 `None`，调用方据此不设置
/// `httpOptions.headers`。
fn client_headers(
    model: &Model,
    options_headers: Option<&ProviderHeaders>,
    identity: Option<&ClientIdentity>,
) -> Option<IndexMap<String, String>> {
    let identity_headers = get_identity_headers(
        &ClientIdentityTarget::new(&model.provider, &model.base_url),
        identity,
    );
    let model_headers: Option<ProviderHeaders> = model.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(name, value)| (name.clone(), Some(value.clone())))
            .collect()
    });
    crate::utils::headers::provider_headers_to_record(&[
        Some(&identity_headers),
        model_headers.as_ref(),
        options_headers,
    ])
}

/// `createClient(model, apiKey, optionsHeaders, identity)`（Google Generative AI）。
///
/// Gemini 模式固定 `vertexai = false`、没有 project/location。若 `model.base_url` 非空，
/// 就把它当作完整基地址，同时把 `api_version` 设为 `Some("")`——约定是 baseUrl 已包含
/// 版本路径，不再追加 `v1beta`。请求头由 `client_headers` 合并三种来源。
pub fn create_gemini_client(
    model: &Model,
    api_key: Option<&str>,
    options_headers: Option<&ProviderHeaders>,
    identity: Option<&ClientIdentity>,
) -> GoogleClientConfig {
    let mut http_options = GoogleHttpOptions::default();
    if !model.base_url.is_empty() {
        http_options.base_url = Some(model.base_url.clone());
        // baseUrl 已包含版本路径，不再追加。
        http_options.api_version = Some(String::new());
    }
    let headers = client_headers(model, options_headers, identity);
    if headers.is_some() {
        http_options.headers = headers;
    }
    GoogleClientConfig {
        vertexai: false,
        api_key: api_key.map(str::to_string),
        project: None,
        location: None,
        api_version: None,
        google_auth_key_filename: None,
        http_options: Some(http_options),
    }
}

/// `buildHttpOptions(model, optionsHeaders, identity)`（Vertex）。
///
/// 与 Gemini 版不同，只有**可用**的自定义 baseUrl 才会进入选项（空白或含 `{location}`
/// 模板的会被 [`resolve_custom_base_url`] 丢弃），并标记 `base_url_resource_scope =
/// "COLLECTION"`；若 baseUrl 已含版本段，再把 `api_version` 置空。
///
/// 返回 `None` 表示四个字段全空：此时 Vertex 客户端完全依赖 [`GoogleClientConfig`] 的
/// 默认主机/版本，不产生空的 `httpOptions`（与 SDK 的 `undefined` 语义一致）。
pub fn build_vertex_http_options(
    model: &Model,
    options_headers: Option<&ProviderHeaders>,
    identity: Option<&ClientIdentity>,
) -> Option<GoogleHttpOptions> {
    let mut http_options = GoogleHttpOptions::default();
    if let Some(base_url) = resolve_custom_base_url(&model.base_url) {
        http_options.base_url = Some(base_url.clone());
        http_options.base_url_resource_scope = Some("COLLECTION".into());
        if base_url_includes_api_version(&base_url) {
            http_options.api_version = Some(String::new());
        }
    }
    let headers = client_headers(model, options_headers, identity);
    if headers.is_some() {
        http_options.headers = headers;
    }
    if http_options.base_url.is_none()
        && http_options.base_url_resource_scope.is_none()
        && http_options.api_version.is_none()
        && http_options.headers.is_none()
    {
        None
    } else {
        Some(http_options)
    }
}

/// `resolveCustomBaseUrl(baseUrl)`。
///
/// 去掉首尾 JS 空白；结果为空、或含 `{location}` 占位符时返回 `None`。
///
/// 占位符分支很重要：模型目录里 Vertex 条目的 `baseUrl` 是
/// `https://{location}-aiplatform.googleapis.com` 模板，Rust 侧不做模板替换。若把它原样
/// 当作 baseUrl 发出去会得到非法主机；返回 `None` 后由
/// `GoogleClientConfig::request_base_with` 按 `location` 生成真实主机。测试
/// `does_not_forward_generated_vertex_base_url_placeholders` 锁定了这个行为。
pub fn resolve_custom_base_url(base_url: &str) -> Option<String> {
    let trimmed = kiana_json::js_string::js_trim(base_url);
    if trimmed.is_empty() || trimmed.contains("{location}") {
        return None;
    }
    Some(trimmed.to_string())
}

/// `baseUrlIncludesApiVersion(baseUrl)`。
///
/// 能解析成 URL 时，逐段检查 path 中是否出现 `v<数字>`、`v<数字>beta` 或
/// `v<数字>beta<数字>`（正则里的 `beta\d*` 允许 0 位数字，所以常见的 `v1beta` 也命中）；
/// 解析失败时退回整串正则，以兼容没有 scheme 的 baseUrl。用于决定是否把 `apiVersion`
/// 置空，避免拼出 `/v1/v1beta/...`。
pub fn base_url_includes_api_version(base_url: &str) -> bool {
    static VERSION: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^v\d+(?:beta\d*)?$").expect("valid regex"));
    static FALLBACK: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:^|/)v\d+(?:beta\d*)?(?:/|$)").expect("valid regex")
    });
    match url::Url::parse(base_url) {
        Ok(url) => url.path().split('/').any(|part| VERSION.is_match(part)),
        Err(_) => FALLBACK.is_match(base_url),
    }
}

// ---------------------------------------------------------------------------
// 传输
// ---------------------------------------------------------------------------

/// Google 请求失败（`ApiError` 的可观察形态）。
///
/// TS 的 `@google/genai` 抛 `ApiError`：有 `status` 属性、没有 `headers`。Rust 用这个轻量
/// 结构承载同样的两个可观察字段：`status` 供 [`retry_google_request`] 判断可重试性，
/// [`GoogleFailure::display`] 供适配器写进错误事件的 `error_message`。
#[derive(Debug, Clone, PartialEq)]
pub struct GoogleFailure {
    /// 错误文本；可能是 `JSON.stringify(errorBody)` 的结果（见 `throw_error_if_not_ok`）。
    pub message: String,
    /// HTTP 状态。主请求的 `throw_error_if_not_ok` 与流内 `error.code` 只在 400–599 设置；
    /// ADC token、impersonation、metadata 等辅助路径对任意非 2xx（如 302/304）也会通过
    /// [`GoogleFailure::with_status`] 设置。`None` 表示传输错误或 JSON 解析错误，
    /// **不会**被重试。
    pub status: Option<u16>,
}

impl GoogleFailure {
    /// 构造不带状态的失败（传输/解析错误），这类错误不会被重试。
    pub fn new(message: impl Into<String>) -> Self {
        GoogleFailure {
            message: message.into(),
            status: None,
        }
    }

    /// 构造带 HTTP 状态的失败；只有带状态且状态可重试的错误才会被
    /// [`retry_google_request`] 重试。
    pub fn with_status(message: impl Into<String>, status: u16) -> Self {
        GoogleFailure {
            message: message.into(),
            status: Some(status),
        }
    }

    /// 适配器写进 `error_message` 的文本。
    ///
    /// 返回 `message` 原样——SDK 的 `normalizeProviderError` 对没有 body 的 `ApiError`
    /// 不做额外加工。
    pub fn display(&self) -> String {
        // `formatProviderError(normalizeProviderError(error))`：ApiError 只有 status，
        // 没有 body，message 原样输出。
        self.message.clone()
    }
}

/// `retryGoogleRequest(request, options)`。
///
/// 重试条件与 `utils::provider_retry` 相同（`max_retries` 默认 0；只重试
/// 408/409/429/5xx），区别是 Google 的 `ApiError` 没有 `headers`，所以永远走指数退避：
/// 不读 `retry-after`，也不会用到 `max_retry_delay_ms`。
///
/// 只重试 `status.is_some()` 的错误：TS 用 `"status" in error` 区分 SDK 的 `ApiError`
/// 与 fetch 抛出的 `TypeError`，后者第一次就原样返回。退避公式：
/// `min(0.5 * 2^retry_index, 8) 秒 * (1 - random * 0.25)`，`random` 是 `[0, 1)`。
///
/// # 取消
///
/// 请求失败后先检查取消；退避期间用 `tokio::select!` 与取消令牌竞争，收到取消立即返回
/// `"Request aborted"`，而不是等睡眠结束。调用方（[`request_stream`]）用它包住「发出请求
/// 直到拿到响应头」的过程：非 2xx 时错误体会在每次尝试内读完，成功响应的流式 body 读取
/// 不在重试范围内。
///
/// # 错误
///
/// 不可重试或重试耗尽时返回**最后一次**的 [`GoogleFailure`]（不改写消息）；取消时返回
/// 新建的 `GoogleFailure("Request aborted")`。
pub async fn retry_google_request<T, F, Fut>(
    mut request: F,
    options: &StreamOptions,
) -> Result<T, GoogleFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, GoogleFailure>>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut retries_remaining = max_retries;
    let mut retry_index: u32 = 0;
    loop {
        match request().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if options.signal.as_ref().is_some_and(|s| s.is_cancelled()) {
                    return Err(GoogleFailure::new("Request aborted"));
                }
                // GoogleFailure 没有响应头，`header` 留空，于是只按 status 判断可重试性。
                let provider_error = ProviderError {
                    message: error.message.clone(),
                    status: error.status,
                    header: None,
                };
                // TS 只重试 `"status" in error` 的 SDK 错误（ApiError）；fetch 抛出的
                // TypeError 没有 status 属性，第一次就原样返回。
                if error.status.is_none()
                    || retries_remaining == 0
                    || !is_retryable_provider_error(&provider_error)
                {
                    return Err(error);
                }
                retries_remaining -= 1;
                // 指数退避：0.5s、1s、2s、4s、8s，封顶 8s；再乘 0.75–1.0 的抖动。
                let exponential = (0.5 * 2f64.powi(retry_index as i32)).min(8.0) * 1000.0;
                let jitter = random_fraction();
                let delay_ms = exponential * (1.0 - jitter * 0.25);
                retry_index += 1;
                // TS 的 `abortableSleep`：退避期间 abort 会立刻清掉定时器并以
                // `AbortError("Request aborted")` 结束，而不是等睡眠跑完。
                if let Some(signal) = options.signal.as_ref() {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(
                            delay_ms.max(0.0) as u64,
                        )) => {}
                        _ = signal.cancelled() => {
                            return Err(GoogleFailure::new("Request aborted"));
                        }
                    }
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms.max(0.0) as u64))
                        .await;
                }
            }
        }
    }
}

/// `Math.random()` 的等价物：`[0, 1)` 上的均匀小数。
///
/// 用 8 字节随机数的高 53 位构造 f64；`getrandom` 失败时返回 0.5，让退避抖动取中间值，
/// 不影响重试的正确性。
fn random_fraction() -> f64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    let value = u64::from_le_bytes(bytes) >> 11;
    value as f64 / (1u64 << 53) as f64
}

/// `patchHttpOptions(base, request)`：请求级 `config.httpOptions` 覆盖客户端选项。
///
/// 逐键处理（不是整体 `Object.assign`）：
///
/// - `headers`：按名合并进已有头；值统一转成字符串（非字符串走 JS `String()`）；
/// - `baseUrl` / `baseUrlResourceScope` / `apiVersion` / `timeout`：只有值不是对象时才替换；
///   类型不符（例如 `timeout` 传字符串）时该字段被置为未设置（`None`），**不会**保留客户端值；
/// - `extraBody`：与已有值深合并（对象递归，数组与标量整体替换）；
/// - 未知键忽略。
///
/// request 不是对象（缺省、`null`、数组）时原样返回客户端选项。
fn patch_http_options(base: &GoogleHttpOptions, request: Option<&Value>) -> GoogleHttpOptions {
    let mut patched = base.clone();
    let Some(Value::Object(request)) = request else {
        return patched;
    };
    for (key, value) in request {
        let is_object = value.is_object();
        match key.as_str() {
            "headers" => {
                // 头按名合并而不是整体替换；非字符串值在这里提前字符串化。
                let mut merged = patched.headers.take().unwrap_or_default();
                if let Value::Object(headers) = value {
                    for (name, value) in headers {
                        let value = match value {
                            Value::String(value) => value.clone(),
                            other => js_string_of_value(Some(other)),
                        };
                        merged.insert(name.clone(), value);
                    }
                }
                patched.headers = Some(merged);
            }
            "baseUrl" if !is_object => {
                patched.base_url = value.as_str().map(str::to_string);
            }
            "baseUrlResourceScope" if !is_object => {
                patched.base_url_resource_scope = value.as_str().map(str::to_string);
            }
            "apiVersion" if !is_object => {
                patched.api_version = value.as_str().map(str::to_string);
            }
            "timeout" if !is_object => {
                patched.timeout = value.as_f64();
            }
            "extraBody" => {
                // 与已有 extraBody 深合并，可覆盖请求体里的同名字段。
                patched.extra_body = Some(match (&patched.extra_body, value) {
                    (Some(Value::Object(base)), Value::Object(source)) => {
                        Value::Object(merge_json_objects(base, source))
                    }
                    _ => value.clone(),
                });
            }
            _ => {}
        }
    }
    patched
}

/// 深合并两个 JSON 对象：只有两边同名值都是对象时才递归，否则整体替换为 source 的值。
///
/// 键序与 JS 的 `{...target, ...source}` 一致：target 原有键保持原位，source 新增的键
/// 追加在末尾（`IndexMap::insert` 对已存在的键不改变位置）。
fn merge_json_objects(
    target: &Map<String, Value>,
    source: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = target.clone();
    for (key, source_value) in source {
        let next = match (merged.get(key), source_value) {
            (Some(Value::Object(target_value)), Value::Object(source_value)) => {
                Value::Object(merge_json_objects(target_value, source_value))
            }
            _ => source_value.clone(),
        };
        merged.insert(key.clone(), next);
    }
    merged
}

/// `includeExtraBodyToRequestInit` 的深合并（只处理非数组对象）。
///
/// 请求体不是对象、或 extraBody 为空对象时什么都不做；否则用 `merge_json_objects`
/// 原地替换 `body`。数组类型的 extraBody 会被忽略（不是对象）。
fn apply_extra_body(body: &mut Value, extra: &Value) {
    let (Value::Object(body), Value::Object(extra)) = (body, extra) else {
        return;
    };
    if extra.is_empty() {
        return;
    }
    *body = merge_json_objects(body, extra);
}

/// 大小写不敏感地查找请求头。
fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// 发出 `generateContentStream` 请求并返回字节流（重试只覆盖等响应头阶段）。
///
/// # 输入
///
/// - `params`：[`build_params`]（可能被 `on_payload` 改写）的产物；`model` 字段决定 URL
///   路径，`config.httpOptions` 是请求级覆盖；
/// - `client`：模式、API key、ADC 配置与客户端级头；
/// - `options`：取 `fetch`（`HttpClient` 注入点）、`signal`、`max_retries`、`env`（ADC）。
///
/// # 流程
///
/// 1. 从 `params.model` 取 model id（**不是** `model.id`），生成
///    `...:streamGenerateContent?alt=sse` URL；
/// 2. 组装头：客户端/请求级头 → `X-Server-Timeout`（毫秒向上取整到秒）→ API key 或
///    Vertex ADC 的 Bearer → 默认 `content-type`。`X-Server-Timeout` 总是追加，
///    api key / authorization / content-type 仅在缺失时添加（查找大小写不敏感）；
/// 3. 用 [`build_wire_body`] 生成请求体，深合并 `extraBody`，再用 JS `JSON.stringify`
///    序列化（不是 serde 的序列化，保证数字格式与字符串转义和 V8 一致）；
/// 4. 在 [`retry_google_request`] 里执行 POST；非 2xx 时读完整 body（可取消）并转成
///    [`GoogleFailure`]。
///
/// # 约束与失败模式
///
/// - 返回的是响应体流，**尚未读取**响应体；读取阶段的错误由 [`read_google_events`]
///   处理，因此不会被重试。
/// - `content-type` 从响应头按小写键查找——默认 reqwest 客户端会把头名小写化；自定义
///   `HttpClient` 若保留原始大小写，可能取不到该头（按“非 JSON”处理）。
/// - 传输错误在取消令牌已触发时统一改写成 `ABORTED_MESSAGE`，否则保留原始 message。
/// - 没有 API key 且不是 Vertex 时不会添加 `authorization`，由服务端返回 401。
pub async fn request_stream(
    client: &GoogleClientConfig,
    params: &Value,
    options: &StreamOptions,
) -> Result<ByteStream, GoogleFailure> {
    // model 来自 params（on_payload 可改写），不是 model.id；非法 id 在这里就失败。
    let model = params.get("model").cloned().unwrap_or(Value::Null);
    let model_path = client.model_path(&model)?;
    // 请求级 httpOptions 只从 config 里取，patch 到客户端选项之上。
    let request_http = params
        .get("config")
        .and_then(|config| get_field(config, "httpOptions"))
        .cloned();
    let base_http = client.http_options.clone().unwrap_or_default();
    let http = patch_http_options(&base_http, request_http.as_ref());
    let url = client.construct_url(
        &format!("{model_path}:streamGenerateContent?alt=sse"),
        &http,
    )?;

    // 头按 SDK 的顺序拼接：已有头优先，默认头只在缺失时补；`X-Server-Timeout` 例外，总是追加。
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(header_map) = &http.headers {
        headers.extend(
            header_map
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
    }
    if let Some(timeout) = http.timeout.filter(|timeout| *timeout > 0.0) {
        // 毫秒 → 秒，向上取整，按 JS 数字格式化（避免出现 "2.0"）。
        let seconds = kiana_json::js_num::to_radix_string((timeout / 1000.0).ceil(), 10);
        headers.push(("X-Server-Timeout".into(), seconds));
    }
    if let Some(api_key) = &client.api_key {
        // Vertex 配了 API key 时也走这个分支；ADC 只在没有 key 时使用。
        if header_value(&headers, "x-goog-api-key").is_none() {
            headers.push(("x-goog-api-key".into(), api_key.clone()));
        }
    } else if client.vertexai {
        let token = vertex_access_token(client, options).await?;
        if header_value(&headers, "authorization").is_none() {
            headers.push(("authorization".into(), format!("Bearer {token}")));
        }
    }
    if header_value(&headers, "content-type").is_none() {
        headers.push(("content-type".into(), "application/json".into()));
    }

    let mut body = build_wire_body(params, client.vertexai, client)?;
    // extraBody 在 SDK 转换之后应用，因此可以覆盖任何生成出来的字段。
    if let Some(extra_body) = &http.extra_body {
        apply_extra_body(&mut body, extra_body);
    }
    let body_bytes = kiana_json::js_object::stringify(&body).into_bytes();
    let url_string = url.to_string();
    let timeout_ms = http
        .timeout
        .filter(|timeout| *timeout > 0.0)
        .map(|timeout| timeout as u64);

    // fetch 注入点：默认 reqwest，测试用 CaptureClient；解析失败直接变成无状态失败。
    let client_handle = crate::api::kiana_messages::resolve_client(options)
        .map_err(|error| GoogleFailure::new(error.message))?;

    retry_google_request(
        || {
            // 每次重试都重新构造 HttpRequest（头与体各克隆一次），失败后可安全重发。
            let request = HttpRequest {
                method: "POST".into(),
                url: url_string.clone(),
                headers: headers.clone(),
                body: Some(body_bytes.clone()),
                timeout_ms,
            };
            let client_handle = client_handle.clone();
            let signal = options.signal.clone();
            async move {
                let response = client_handle
                    .execute(request, signal.as_ref())
                    .await
                    .map_err(|error| {
                        if signal
                            .as_ref()
                            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                        {
                            GoogleFailure::new(crate::http::reqwest_client::ABORTED_MESSAGE)
                        } else {
                            GoogleFailure::new(error.message)
                        }
                    })?;
                if !(200..300).contains(&response.response.status) {
                    // 非 2xx：先按取消感知的方式读完 body，再决定怎么解析错误。
                    let status = response.response.status;
                    // 头名按小写查找（默认 reqwest 客户端会把响应头名小写化）。
                    let content_type = response.response.headers.get("content-type").cloned();
                    let body =
                        crate::api::kiana_messages::read_body_text(response.body, signal.as_ref())
                            .await
                            .map_err(|error| GoogleFailure::new(error.message))?;
                    return Err(throw_error_if_not_ok(
                        status,
                        content_type.as_deref(),
                        &body,
                    ));
                }
                Ok(response.body)
            }
        },
        options,
    )
    .await
}

/// `throwErrorIfNotOK(response)`。
///
/// SDK 只在 `content-type` 含 `application/json` 时 `response.json()`（解析失败抛出
/// `SyntaxError`，没有 status → 不重试）；否则把原始文本包成
/// `{error:{message, code, status}}`。message 是 `JSON.stringify(errorBody)`，只有
/// 400 <= status < 600 才是带 `status` 的 `ApiError`。
fn throw_error_if_not_ok(status: u16, content_type: Option<&str>, body: &str) -> GoogleFailure {
    // SDK 用 `content-type.includes("application/json")`（大小写敏感）决定是否解析 JSON。
    let is_json = content_type.is_some_and(|value| value.contains("application/json"));
    let error_message = if is_json {
        // 解析失败等价于 `response.json()` 抛 SyntaxError：没有 status，因此不重试。
        match kiana_json::js_parse::parse(body) {
            Ok(parsed) => kiana_json::js_object::stringify(&parsed),
            Err(error) => return GoogleFailure::new(error.message),
        }
    } else {
        // 非 JSON：把原文包成 SDK 的 `{error:{message, code, status}}` 形状，
        // status 用标准原因短语（Node 的 `Response.statusText`）。
        kiana_json::js_object::stringify(&object([(
            "error",
            object([
                ("message", Value::String(body.to_string())),
                ("code", Value::from(status)),
                (
                    "status",
                    Value::String(crate::api::kiana_messages::reason_phrase(status)),
                ),
            ]),
        )]))
    };
    // 只有 4xx/5xx 才构造成带 status 的 ApiError；304 之类没有 status，因此不重试。
    if (400..600).contains(&status) {
        GoogleFailure::with_status(error_message, status)
    } else {
        GoogleFailure::new(error_message)
    }
}

// 以下是一组 JS 语义小工具：SDK 转换器大量依赖 `undefined` / `null` / 真值的区别，
// 用它们逐条复刻分支条件，避免用 Rust 的 `Option` 语义想当然。

/// JS 的 `typeof`（JSON 值域）。
fn js_typeof(value: &Value) -> &'static str {
    match value {
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) | Value::Object(_) => "object",
    }
}

/// JS 真值判断（`if (value)`）。
///
/// 与 Rust 直觉不同：空数组、空对象为**真**；`0`、`NaN`、空串、`null` 为假。
pub(crate) fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value
            .as_f64()
            .is_some_and(|value| value != 0.0 && !value.is_nan()),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `getValueByPath(fromObject, [key])`：非对象上取属性得到 `undefined`。
///
/// 返回 `None` 表示 `undefined`，`Some(Value::Null)` 表示显式 `null`；SDK 的不同分支
/// 对这二者并不等价（`!= null` 与 `!== undefined` 的区别）。
fn get_field<'a>(object: &'a Value, key: &str) -> Option<&'a Value> {
    object.as_object().and_then(|object| object.get(key))
}

/// `getValueByPath(...) != null` 的字段拷贝。
///
/// 只拷贝非 `null` 的值：`undefined`（键不存在）与显式 `null` 都不会写进目标对象。
fn copy_field(from: &Value, key: &str, to: &mut Map<String, Value>) {
    if let Some(value) = get_field(from, key).filter(|value| !value.is_null()) {
        to.insert(key.to_string(), value.clone());
    }
}

/// `getValueByPath(...) !== undefined`（null 也算存在）。
fn has_field(object: &Value, key: &str) -> bool {
    get_field(object, key).is_some()
}

/// `Object.keys(object)` 是否包含 `key`。
///
/// 只对 JSON 对象返回真；当前调用点（工具声明的 `$schema` 检查）只传对象。
fn has_own_key(object: &Value, key: &str) -> bool {
    match object {
        Value::Object(map) => map.contains_key(key),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// SDK 请求转换器（`t*` / `*ToMldev` / `*ToVertex`）
// ---------------------------------------------------------------------------
//
// 这些函数逐条复刻 `@google/genai` 的内部转换器：输入是宽松 JSON（`build_params` 的
// 产物），输出是线上请求体。两种模式的差异有两类：
// - 字段写入顺序固定，模块测试对序列化结果做逐字节比较；
// - 某些字段只属于一种模式，用错模式时 SDK 抛错，这里返回等文案的 GoogleFailure。

/// MLDev（Gemini Developer API）专属参数被用在 Vertex 模式时的错误。
fn gemini_only_parameter(name: &str) -> GoogleFailure {
    GoogleFailure::new(format!(
        "{name} parameter is only supported in Gemini Developer API mode, not in Gemini Enterprise Agent Platform mode."
    ))
}

/// Vertex 专属参数被用在 MLDev 模式时的错误。
fn vertex_only_parameter(name: &str) -> GoogleFailure {
    GoogleFailure::new(format!(
        "{name} parameter is only supported in Gemini Enterprise Agent Platform mode, not in Gemini Developer API mode."
    ))
}

/// `isContent(origin)`：带 `parts` 数组的对象视为一个 Content（而不是单个 Part）。
fn is_content(value: &Value) -> bool {
    matches!(get_field(value, "parts"), Some(Value::Array(_)))
}

/// `tPart(origin)`。
///
/// 单个 Part 的宽松输入：对象原样使用，字符串包装成 `{text}`；`null` 报
/// `PartUnion is required`，其它非对象/非字符串值（含数组）报
/// `Unsupported part type: <typeof>`，其中数组经 JS `typeof` 显示为 `object`。
fn t_part(value: &Value) -> Result<Value, GoogleFailure> {
    match value {
        Value::Null => Err(GoogleFailure::new("PartUnion is required")),
        Value::Object(_) => Ok(value.clone()),
        Value::String(text) => Ok(object([("text", Value::String(text.clone()))])),
        other => Err(GoogleFailure::new(format!(
            "Unsupported part type: {}",
            js_typeof(other)
        ))),
    }
}

/// `tParts(origin)`。
///
/// 接受单个 Part 或 Part 数组；`null` 与空数组都报 `PartListUnion is required`。
fn t_parts(value: &Value) -> Result<Vec<Value>, GoogleFailure> {
    if value.is_null() {
        return Err(GoogleFailure::new("PartListUnion is required"));
    }
    if let Value::Array(items) = value {
        if items.is_empty() {
            return Err(GoogleFailure::new("PartListUnion is required"));
        }
        return items.iter().map(t_part).collect();
    }
    Ok(vec![t_part(value)?])
}

/// `tContent(origin)`。
///
/// 已是 Content（有 `parts` 数组）就原样返回；否则把输入当成 Part/Part[]，包装成
/// `role = "user"` 的 Content。这是 SDK 对「只给内容不给角色」的兜底。
fn t_content(value: &Value) -> Result<Value, GoogleFailure> {
    if value.is_null() {
        return Err(GoogleFailure::new("ContentUnion is required"));
    }
    if is_content(value) {
        return Ok(value.clone());
    }
    let parts = t_parts(value)?;
    Ok(object([
        ("role", Value::String("user".into())),
        ("parts", Value::Array(parts)),
    ]))
}

/// `tContents(origin)`。
///
/// 规范化 `contents`：单个 Content/Part 或它们的数组。规则：
///
/// - `null` / 空数组 → `contents are required`；
/// - 数组内不能混用 Content 与 Part（首元素决定整组类型），否则报错；
/// - 单独的 `functionCall` / `functionResponse` part 必须包在带角色的 Content 里，
///   否则报错（它们是模型轮/工具轮的一部分，缺 role 时 API 无法判断归属）；
/// - 纯 Part 数组会被合并成一个 `role = "user"` 的 Content。
fn t_contents(value: &Value) -> Result<Vec<Value>, GoogleFailure> {
    if value.is_null() {
        return Err(GoogleFailure::new("contents are required"));
    }
    let Value::Array(items) = value else {
        if has_field(value, "functionCall") || has_field(value, "functionResponse") {
            return Err(GoogleFailure::new(
                "To specify functionCall or functionResponse parts, please wrap them in a Content object, specifying the role for them",
            ));
        }
        return Ok(vec![t_content(value)?]);
    };
    if items.is_empty() {
        return Err(GoogleFailure::new("contents are required"));
    }
    let is_content_array = items.first().is_some_and(is_content);
    let mut result = Vec::new();
    let mut accumulated = Vec::new();
    for item in items {
        let item_is_content = is_content(item);
        if item_is_content != is_content_array {
            return Err(GoogleFailure::new(
                "Mixing Content and Parts is not supported, please group the parts into a the appropriate Content objects and specify the roles for them",
            ));
        }
        if item_is_content {
            result.push(item.clone());
        } else if has_field(item, "functionCall") || has_field(item, "functionResponse") {
            return Err(GoogleFailure::new(
                "To specify functionCall or functionResponse parts, please wrap them, and any other parts, in Content objects as appropriate, specifying the role for them",
            ));
        } else {
            accumulated.push(item.clone());
        }
    }
    if !is_content_array {
        result.push(object([
            ("role", Value::String("user".into())),
            ("parts", Value::Array(t_parts(&Value::Array(accumulated))?)),
        ]));
    }
    Ok(result)
}

/// `contentToMldev` / `contentToVertex`：`{parts, role}`。
///
/// 只输出 `parts`（逐个经 `part_to_wire` 转换）与 `role` 两个非 null 字段，键序
/// parts → role 与 SDK 一致；两个字段都缺时输出空对象也是合法的。
fn content_to_wire(content: &Value, vertex: bool) -> Result<Value, GoogleFailure> {
    let mut out = Map::new();
    if let Some(parts) = get_field(content, "parts").filter(|value| !value.is_null()) {
        let converted = if let Value::Array(items) = parts {
            Value::Array(
                items
                    .iter()
                    .map(|part| part_to_wire(part, vertex))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        } else {
            parts.clone()
        };
        out.insert("parts".into(), converted);
    }
    if let Some(role) = get_field(content, "role").filter(|value| !value.is_null()) {
        out.insert("role".into(), role.clone());
    }
    Ok(Value::Object(out))
}

/// `functionCallToMldev`：`args, id, name`。
///
/// `partialArgs` / `willContinue` 只属于 Vertex，在 MLDev 下直接报错；剩余字段按
/// args → id → name 顺序拷贝。
fn function_call_to_mldev(value: &Value) -> Result<Value, GoogleFailure> {
    if has_field(value, "partialArgs") {
        return Err(vertex_only_parameter("partialArgs"));
    }
    if has_field(value, "willContinue") {
        return Err(vertex_only_parameter("willContinue"));
    }
    let mut out = Map::new();
    copy_field(value, "args", &mut out);
    copy_field(value, "id", &mut out);
    copy_field(value, "name", &mut out);
    Ok(Value::Object(out))
}

/// `blobToMldev`：`data, mimeType`。
///
/// `displayName` 是 Vertex 专属，在 MLDev 下报错。
fn blob_to_mldev(value: &Value) -> Result<Value, GoogleFailure> {
    if has_field(value, "displayName") {
        return Err(vertex_only_parameter("displayName"));
    }
    let mut out = Map::new();
    copy_field(value, "data", &mut out);
    copy_field(value, "mimeType", &mut out);
    Ok(Value::Object(out))
}

/// `fileDataToMldev`：`fileUri, mimeType`。
///
/// 与 `blob_to_mldev` 一样，`displayName` 是 Vertex 专属，在 MLDev 下报错。
fn file_data_to_mldev(value: &Value) -> Result<Value, GoogleFailure> {
    if has_field(value, "displayName") {
        return Err(vertex_only_parameter("displayName"));
    }
    let mut out = Map::new();
    copy_field(value, "fileUri", &mut out);
    copy_field(value, "mimeType", &mut out);
    Ok(Value::Object(out))
}

/// `partToMldev` / `partToVertex`（两种模式字段顺序与禁用字段不同）。
///
/// 两种模式都按固定顺序拷贝字段；`copy_field` 跳过 null，所以缺省字段不会出现在线上
/// JSON 里。MLDev 分支把 `fileData` / `functionCall` / `inlineData` 交给各自的子转换器
/// （这些子对象含模式专属字段），其余字段原样拷贝；Vertex 分支直接拷贝，遇到 MLDev
/// 专属字段（`toolCall`、`toolResponse`、`partMetadata`）时报错。字段顺序属于线上字节，
/// 不要调整。
fn part_to_wire(part: &Value, vertex: bool) -> Result<Value, GoogleFailure> {
    let mut out = Map::new();
    if !vertex {
        for key in [
            "mediaResolution",
            "toolCall",
            "toolResponse",
            "audioTranscription",
            "codeExecutionResult",
            "executableCode",
        ] {
            copy_field(part, key, &mut out);
        }
        if let Some(value) = get_field(part, "fileData").filter(|value| !value.is_null()) {
            out.insert("fileData".into(), file_data_to_mldev(value)?);
        }
        if let Some(value) = get_field(part, "functionCall").filter(|value| !value.is_null()) {
            out.insert("functionCall".into(), function_call_to_mldev(value)?);
        }
        copy_field(part, "functionResponse", &mut out);
        if let Some(value) = get_field(part, "inlineData").filter(|value| !value.is_null()) {
            out.insert("inlineData".into(), blob_to_mldev(value)?);
        }
        for key in [
            "text",
            "thought",
            "thoughtSignature",
            "videoMetadata",
            "partMetadata",
            "mediaProcessing",
        ] {
            copy_field(part, key, &mut out);
        }
        return Ok(Value::Object(out));
    }
    for key in ["toolCall", "toolResponse", "partMetadata"] {
        if has_field(part, key) {
            return Err(gemini_only_parameter(key));
        }
    }
    for key in [
        "mediaResolution",
        "audioTranscription",
        "codeExecutionResult",
        "executableCode",
        "fileData",
        "functionCall",
        "functionResponse",
        "inlineData",
        "text",
        "thought",
        "thoughtSignature",
        "videoMetadata",
        "mediaProcessing",
    ] {
        copy_field(part, key, &mut out);
    }
    Ok(Value::Object(out))
}

/// `processJsonSchema(schema)`：SDK 的 JSON Schema → Gemini Schema 转换。
///
/// 关键规则（顺序即处理顺序）：
///
/// 1. `type` 与 `anyOf` 同时为真 → 报错；
/// 2. `anyOf: [X, {type:"null"}]`（或反向）折叠成 `nullable: true` + 另一个 schema；
/// 3. `type` 是数组时：含 `"null"` 就置 `nullable`；去掉 null 后只剩一个类型就写 `type`，
///    否则把每个类型展开成 `anyOf: [{type}, ...]`；
/// 4. 递归 `items` / `anyOf` / `properties`；`additionalProperties` 被丢弃（Gemini
///    Schema 不支持）；未识别的字段原样透传。
///
/// 类型名统一大写，落在 `TYPE_VALUES` 之外的记为 `TYPE_UNSPECIFIED`。
fn process_json_schema(schema: &Value) -> Result<Value, GoogleFailure> {
    const TYPE_VALUES: &[&str] = &[
        "TYPE_UNSPECIFIED",
        "STRING",
        "NUMBER",
        "INTEGER",
        "BOOLEAN",
        "ARRAY",
        "OBJECT",
        "NULL",
    ];
    let normalize_type = |value: &str| -> String {
        let upper = value.to_uppercase();
        if TYPE_VALUES.contains(&upper.as_str()) {
            upper
        } else {
            "TYPE_UNSPECIFIED".to_string()
        }
    };
    if js_truthy(get_field(schema, "type").unwrap_or(&Value::Null))
        && js_truthy(get_field(schema, "anyOf").unwrap_or(&Value::Null))
    {
        return Err(GoogleFailure::new(
            "type and anyOf cannot be both populated.",
        ));
    }
    let mut out = Map::new();
    let mut current = schema.clone();
    if let Some(Value::Array(any_of)) = get_field(schema, "anyOf")
        && any_of.len() == 2
    {
        let first_is_null = get_field(&any_of[0], "type").and_then(Value::as_str) == Some("null");
        let second_is_null = get_field(&any_of[1], "type").and_then(Value::as_str) == Some("null");
        if first_is_null {
            out.insert("nullable".into(), Value::Bool(true));
            current = any_of[1].clone();
        } else if second_is_null {
            out.insert("nullable".into(), Value::Bool(true));
            current = any_of[0].clone();
        }
    }
    if let Some(Value::Array(types)) = get_field(&current, "type") {
        // `flattenTypeArrayToAnyOf`
        if types.iter().any(|value| value.as_str() == Some("null")) {
            out.insert("nullable".into(), Value::Bool(true));
        }
        let without_null: Vec<&Value> = types
            .iter()
            .filter(|value| value.as_str() != Some("null"))
            .collect();
        if without_null.len() == 1 {
            let name = without_null[0].as_str().unwrap_or("");
            out.insert("type".into(), Value::String(normalize_type(name)));
        } else {
            let any_of: Vec<Value> = without_null
                .iter()
                .map(|value| {
                    object([(
                        "type",
                        Value::String(normalize_type(value.as_str().unwrap_or(""))),
                    )])
                })
                .collect();
            out.insert("anyOf".into(), Value::Array(any_of));
        }
    }
    if let Value::Object(map) = &current {
        for (field_name, field_value) in map {
            if field_value.is_null() {
                continue;
            }
            match field_name.as_str() {
                "type" => {
                    if field_value.as_str() == Some("null") {
                        return Err(GoogleFailure::new(
                            "type: null can not be the only possible type for the field.",
                        ));
                    }
                    if field_value.is_array() {
                        continue;
                    }
                    let name = field_value.as_str().unwrap_or("");
                    out.insert("type".into(), Value::String(normalize_type(name)));
                }
                "items" => {
                    out.insert("items".into(), process_json_schema(field_value)?);
                }
                "anyOf" => {
                    let mut list = Vec::new();
                    if let Value::Array(items) = field_value {
                        for item in items {
                            if get_field(item, "type").and_then(Value::as_str) == Some("null") {
                                out.insert("nullable".into(), Value::Bool(true));
                                continue;
                            }
                            list.push(process_json_schema(item)?);
                        }
                    }
                    out.insert("anyOf".into(), Value::Array(list));
                }
                "properties" => {
                    let mut dict = Map::new();
                    if let Value::Object(properties) = field_value {
                        for (key, value) in properties {
                            dict.insert(key.clone(), process_json_schema(value)?);
                        }
                    }
                    out.insert("properties".into(), Value::Object(dict));
                }
                "additionalProperties" => {}
                other => {
                    out.insert(other.to_string(), field_value.clone());
                }
            }
        }
    }
    Ok(Value::Object(out))
}

/// `tTool` 的 functionDeclarations 处理（`parameters`/`response` 的 `$schema` 迁移）。
///
/// 若 `parameters`/`response` 带 `$schema`，说明调用方给的是 JSON Schema，迁移到
/// `parametersJsonSchema`/`responseJsonSchema` 键（并删掉旧键）；否则按 Gemini Schema
/// 转换。目标键已有真值时保留目标、不迁移。
fn t_tool_declaration(declaration: &mut Map<String, Value>) -> Result<(), GoogleFailure> {
    for (parameters_key, json_schema_key) in [
        ("parameters", "parametersJsonSchema"),
        ("response", "responseJsonSchema"),
    ] {
        let Some(value) = declaration.get(parameters_key).cloned() else {
            continue;
        };
        if !js_truthy(&value) {
            continue;
        }
        if !has_own_key(&value, "$schema") {
            declaration.insert(parameters_key.to_string(), process_json_schema(&value)?);
        } else if !declaration.get(json_schema_key).is_some_and(js_truthy) {
            declaration.insert(json_schema_key.to_string(), value);
            declaration.remove(parameters_key);
        }
    }
    Ok(())
}

/// `tTool(tool)`。
///
/// 只处理 `functionDeclarations` 数组里的每个声明；`googleSearch` 等其它工具类型不转换，
/// 原样返回。
fn t_tool(tool: &Value) -> Result<Value, GoogleFailure> {
    let mut tool = tool.clone();
    if let Value::Object(map) = &mut tool
        && let Some(Value::Array(declarations)) = map.get_mut("functionDeclarations")
    {
        for declaration in declarations {
            if let Value::Object(declaration) = declaration {
                t_tool_declaration(declaration)?;
            }
        }
    }
    Ok(tool)
}

/// `tTools(tools)`。
///
/// `tools` 必须是数组；`null` 报 `tools is required`，其它类型报
/// `tools is required and must be an array of Tools`。
fn t_tools(tools: &Value) -> Result<Vec<Value>, GoogleFailure> {
    if tools.is_null() {
        return Err(GoogleFailure::new("tools is required"));
    }
    let Value::Array(items) = tools else {
        return Err(GoogleFailure::new(
            "tools is required and must be an array of Tools",
        ));
    };
    items.iter().map(t_tool).collect()
}

/// `toolToMldev` / `toolToVertex`。
///
/// kiana 只生成 `functionDeclarations`；SDK 的其它工具类型按各模式的白名单拷贝，白名单
/// 之外的字段被丢弃。MLDev 下出现 Vertex 专属工具（`retrieval` 等）、或 Vertex 下出现
/// `fileSearch` 时会报错。键序按白名单顺序；模块测试不覆盖这些工具类型。
fn tool_to_wire(tool: &Value, vertex: bool) -> Result<Value, GoogleFailure> {
    let mut out = Map::new();
    if !vertex {
        for key in [
            "retrieval",
            "enterpriseWebSearch",
            "exaAiSearch",
            "parallelAiSearch",
        ] {
            if has_field(tool, key) {
                return Err(vertex_only_parameter(key));
            }
        }
        for key in [
            "googleMaps",
            "mcpServers",
            "codeExecution",
            "computerUse",
            "functionDeclarations",
            "googleSearch",
            "googleSearchRetrieval",
            "urlContext",
            "fileSearch",
        ] {
            copy_field(tool, key, &mut out);
        }
        return Ok(Value::Object(out));
    }
    if has_field(tool, "fileSearch") {
        return Err(gemini_only_parameter("fileSearch"));
    }
    for key in [
        "retrieval",
        "googleMaps",
        "mcpServers",
        "codeExecution",
        "computerUse",
        "enterpriseWebSearch",
        "exaAiSearch",
        "functionDeclarations",
        "googleSearch",
        "googleSearchRetrieval",
        "parallelAiSearch",
        "urlContext",
    ] {
        copy_field(tool, key, &mut out);
    }
    Ok(Value::Object(out))
}

/// `functionCallingConfigToMldev`：`allowedFunctionNames, mode`。
///
/// `streamFunctionCallArguments` 是 Vertex 专属，在 MLDev 下报错。
fn function_calling_config_to_mldev(value: &Value) -> Result<Value, GoogleFailure> {
    if has_field(value, "streamFunctionCallArguments") {
        return Err(vertex_only_parameter("streamFunctionCallArguments"));
    }
    let mut out = Map::new();
    copy_field(value, "allowedFunctionNames", &mut out);
    copy_field(value, "mode", &mut out);
    Ok(Value::Object(out))
}

/// `toolConfigToMldev` / `toolConfigToVertex`。
///
/// Vertex 原样使用 `functionCallingConfig`，MLDev 走子转换；
/// `includeServerSideToolInvocations` 只允许 MLDev（Vertex 下出现则报错）。
fn tool_config_to_wire(value: &Value, vertex: bool) -> Result<Value, GoogleFailure> {
    let mut out = Map::new();
    if let Some(config) = get_field(value, "functionCallingConfig").filter(|value| !value.is_null())
    {
        out.insert(
            "functionCallingConfig".into(),
            if vertex {
                config.clone()
            } else {
                function_calling_config_to_mldev(config)?
            },
        );
    }
    copy_field(value, "retrievalConfig", &mut out);
    if vertex {
        if has_field(value, "includeServerSideToolInvocations") {
            return Err(gemini_only_parameter("includeServerSideToolInvocations"));
        }
    } else {
        copy_field(value, "includeServerSideToolInvocations", &mut out);
    }
    Ok(Value::Object(out))
}

/// `safetySettings` 转换（MLDev 逐项过滤 `method`；Vertex 原样）。
///
/// 非数组输入原样返回（SDK 的宽松处理）。MLDev 下每项若含 `method` 直接报错，只保留
/// `category` 与 `threshold`。
fn safety_settings_to_wire(value: &Value, vertex: bool) -> Result<Value, GoogleFailure> {
    if vertex {
        return Ok(value.clone());
    }
    let Value::Array(items) = value else {
        return Ok(value.clone());
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if has_field(item, "method") {
            return Err(vertex_only_parameter("method"));
        }
        let mut setting = Map::new();
        copy_field(item, "category", &mut setting);
        copy_field(item, "threshold", &mut setting);
        out.push(Value::Object(setting));
    }
    Ok(Value::Array(out))
}

/// `tCachedContentName(apiClient, name)`。
///
/// `name` 必须是字符串；随后按 `resource_name` 的规则补全成 `cachedContents` 资源名。
/// 非字符串（数字、null）报 `name must be a string`。
fn cached_content_name(client: &GoogleClientConfig, name: &Value) -> Result<Value, GoogleFailure> {
    let Some(name) = name.as_str() else {
        return Err(GoogleFailure::new("name must be a string"));
    };
    Ok(Value::String(resource_name(client, name, "cachedContents")))
}

/// `resourceName(client, name, prefix)`。
///
/// 把短资源名补全成全名，规则按模式不同：
///
/// - Vertex：`projects/` 开头原样；`locations/` 开头补 `projects/<project>/`；
///   `<prefix>/` 开头补 `projects/<project>/locations/<location>/`；只有一段的名字补成
///   `projects/<project>/locations/<location>/<prefix>/<name>`；其余原样；
/// - MLDev：只有一段的名字补 `<prefix>/`，其余原样。
///
/// `project`/`location` 缺失时用空串拼接（SDK 同样不做校验，最终由服务端拒绝）。
fn resource_name(client: &GoogleClientConfig, name: &str, prefix: &str) -> String {
    let should_append_prefix =
        !name.starts_with(&format!("{prefix}/")) && name.split('/').count() == 1;
    if client.vertexai {
        if name.starts_with("projects/") {
            name.to_string()
        } else if name.starts_with("locations/") {
            format!(
                "projects/{}/{}",
                client.project.as_deref().unwrap_or(""),
                name
            )
        } else if name.starts_with(&format!("{prefix}/")) {
            format!(
                "projects/{}/locations/{}/{}",
                client.project.as_deref().unwrap_or(""),
                client.location.as_deref().unwrap_or(""),
                name
            )
        } else if should_append_prefix {
            format!(
                "projects/{}/locations/{}/{}/{}",
                client.project.as_deref().unwrap_or(""),
                client.location.as_deref().unwrap_or(""),
                prefix,
                name
            )
        } else {
            name.to_string()
        }
    } else if should_append_prefix {
        format!("{prefix}/{name}")
    } else {
        name.to_string()
    }
}

/// `tSpeechConfig(speechConfig)`。
///
/// 对象原样返回；字符串包装成 `voiceConfig.prebuiltVoiceConfig.voiceName`；其它类型
/// 报 `Unsupported speechConfig type: <typeof>`。
fn t_speech_config(value: &Value) -> Result<Value, GoogleFailure> {
    match value {
        Value::Object(_) => Ok(value.clone()),
        Value::String(name) => Ok(object([(
            "voiceConfig",
            object([(
                "prebuiltVoiceConfig",
                object([("voiceName", Value::String(name.clone()))]),
            )]),
        )])),
        other => Err(GoogleFailure::new(format!(
            "Unsupported speechConfig type: {}",
            js_typeof(other)
        ))),
    }
}

/// `imageConfigToMldev` / `imageConfigToVertex`。
///
/// MLDev 只允许 `aspectRatio`/`imageSize`，出现 Vertex 专属字段（`personGeneration` 等）
/// 时报错。Vertex 还会把 `outputMimeType`/`outputCompressionQuality` 折叠进
/// `imageOutputOptions`（mimeType / compressionQuality），并保留调用方显式给的
/// `imageOutputOptions`（按字段写入顺序覆盖）。
fn image_config_to_wire(value: &Value, vertex: bool) -> Result<Value, GoogleFailure> {
    if !vertex {
        for key in [
            "personGeneration",
            "outputMimeType",
            "outputCompressionQuality",
            "imageOutputOptions",
            "prominentPeople",
        ] {
            if has_field(value, key) {
                return Err(vertex_only_parameter(key));
            }
        }
        let mut out = Map::new();
        copy_field(value, "aspectRatio", &mut out);
        copy_field(value, "imageSize", &mut out);
        return Ok(Value::Object(out));
    }
    let mut out = Map::new();
    copy_field(value, "aspectRatio", &mut out);
    copy_field(value, "imageSize", &mut out);
    copy_field(value, "personGeneration", &mut out);
    if let Some(mime_type) = get_field(value, "outputMimeType").filter(|value| !value.is_null()) {
        let mut options = match out.remove("imageOutputOptions") {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };
        options.insert("mimeType".into(), mime_type.clone());
        out.insert("imageOutputOptions".into(), Value::Object(options));
    }
    if let Some(quality) =
        get_field(value, "outputCompressionQuality").filter(|value| !value.is_null())
    {
        let mut options = match out.remove("imageOutputOptions") {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };
        options.insert("compressionQuality".into(), quality.clone());
        out.insert("imageOutputOptions".into(), Value::Object(options));
    }
    if let Some(options) = get_field(value, "imageOutputOptions").filter(|value| !value.is_null()) {
        out.insert("imageOutputOptions".into(), options.clone());
    }
    copy_field(value, "prominentPeople", &mut out);
    Ok(Value::Object(out))
}

/// `generateContentConfigToMldev` / `generateContentConfigToVertex`。
///
/// 返回 `generationConfig` 对象；父级字段（systemInstruction/tools/safetySettings/...）
/// 按 SDK 转换器的顺序写入 `parent`（即请求体）。
///
/// 两个模式的差异集中在这里：
/// - `serviceTier`、`systemInstruction`、`safetySettings`、`tools`、`toolConfig`、
///   `cachedContent`、`labels`、`modelArmorConfig` 属于父级字段；
/// - `routingConfig`/`modelSelectionConfig`/`audioTimestamp`/`labels` 是 Vertex 专属，
///   MLDev 下出现即报错；`enableEnhancedCivicAnswers` 反过来只属于 MLDev；
/// - Vertex 的 `modelSelectionConfig` 会改名为 `modelConfig`。
fn generation_config_to_wire(
    config: &Value,
    vertex: bool,
    client: &GoogleClientConfig,
    parent: &mut Map<String, Value>,
) -> Result<Map<String, Value>, GoogleFailure> {
    let mut generation = Map::new();
    copy_field(config, "serviceTier", parent);
    if let Some(system_instruction) =
        get_field(config, "systemInstruction").filter(|value| !value.is_null())
    {
        parent.insert(
            "systemInstruction".into(),
            content_to_wire(&t_content(system_instruction)?, vertex)?,
        );
    }
    for key in [
        "temperature",
        "topP",
        "topK",
        "candidateCount",
        "maxOutputTokens",
        "stopSequences",
        "responseLogprobs",
        "logprobs",
        "presencePenalty",
        "frequencyPenalty",
        "seed",
        "responseMimeType",
    ] {
        copy_field(config, key, &mut generation);
    }
    if let Some(schema) = get_field(config, "responseSchema").filter(|value| !value.is_null()) {
        generation.insert("responseSchema".into(), process_json_schema(schema)?);
    }
    copy_field(config, "responseJsonSchema", &mut generation);
    if vertex {
        copy_field(config, "routingConfig", &mut generation);
        if let Some(value) =
            get_field(config, "modelSelectionConfig").filter(|value| !value.is_null())
        {
            generation.insert("modelConfig".into(), value.clone());
        }
    } else {
        for key in ["routingConfig", "modelSelectionConfig"] {
            if has_field(config, key) {
                return Err(vertex_only_parameter(key));
            }
        }
    }
    if let Some(settings) = get_field(config, "safetySettings").filter(|value| !value.is_null()) {
        parent.insert(
            "safetySettings".into(),
            safety_settings_to_wire(settings, vertex)?,
        );
    }
    if let Some(tools) = get_field(config, "tools").filter(|value| !value.is_null()) {
        let converted: Vec<Value> = t_tools(tools)?
            .iter()
            .map(|tool| tool_to_wire(tool, vertex))
            .collect::<Result<_, _>>()?;
        parent.insert("tools".into(), Value::Array(converted));
    }
    if let Some(tool_config) = get_field(config, "toolConfig").filter(|value| !value.is_null()) {
        parent.insert(
            "toolConfig".into(),
            tool_config_to_wire(tool_config, vertex)?,
        );
    }
    if vertex {
        copy_field(config, "labels", parent);
    } else if has_field(config, "labels") {
        return Err(vertex_only_parameter("labels"));
    }
    if let Some(cached) = get_field(config, "cachedContent").filter(|value| !value.is_null()) {
        parent.insert("cachedContent".into(), cached_content_name(client, cached)?);
    }
    copy_field(config, "responseModalities", &mut generation);
    copy_field(config, "mediaResolution", &mut generation);
    if let Some(speech) = get_field(config, "speechConfig").filter(|value| !value.is_null()) {
        generation.insert("speechConfig".into(), t_speech_config(speech)?);
    }
    if vertex {
        copy_field(config, "audioTimestamp", &mut generation);
    } else if has_field(config, "audioTimestamp") {
        return Err(vertex_only_parameter("audioTimestamp"));
    }
    copy_field(config, "thinkingConfig", &mut generation);
    copy_field(config, "audioTranscriptionConfig", &mut generation);
    if let Some(image) = get_field(config, "imageConfig").filter(|value| !value.is_null()) {
        generation.insert("imageConfig".into(), image_config_to_wire(image, vertex)?);
    }
    if vertex {
        if has_field(config, "enableEnhancedCivicAnswers") {
            return Err(gemini_only_parameter("enableEnhancedCivicAnswers"));
        }
        copy_field(config, "modelArmorConfig", parent);
    } else {
        copy_field(config, "enableEnhancedCivicAnswers", &mut generation);
        if has_field(config, "modelArmorConfig") {
            return Err(vertex_only_parameter("modelArmorConfig"));
        }
    }
    Ok(generation)
}

/// `generateContentParametersToMldev` / `generateContentParametersToVertex` 的请求体。
///
/// 顶层字段（存在时才写）：`contents`、`generationConfig`，以及 `config` 中列出的父级字段
/// ——`serviceTier`、`systemInstruction`、`safetySettings`、`tools`、`toolConfig`、
/// `labels`、`cachedContent`、`modelArmorConfig`。`model` 不进请求体，它已经写进 URL 路径。
/// `contents` 先经 `t_contents` 规范化，再逐个 `content_to_wire`；`config` 存在时
/// `generation_config_to_wire` 先按 SDK 顺序把父级字段写进 `body`，再返回
/// `generationConfig` 的 map 由本函数最后插入。
///
/// 实际顶层键序为 contents → 父级字段 → generationConfig，与 SDK 一致。
pub fn build_wire_body(
    params: &Value,
    vertex: bool,
    client: &GoogleClientConfig,
) -> Result<Value, GoogleFailure> {
    let mut body = Map::new();
    if let Some(contents) = get_field(params, "contents").filter(|value| !value.is_null()) {
        let contents: Vec<Value> = t_contents(contents)?
            .iter()
            .map(|content| content_to_wire(content, vertex))
            .collect::<Result<_, _>>()?;
        body.insert("contents".into(), Value::Array(contents));
    }
    if let Some(config) = get_field(params, "config").filter(|value| !value.is_null()) {
        let generation = generation_config_to_wire(config, vertex, client, &mut body)?;
        body.insert("generationConfig".into(), Value::Object(generation));
    }
    Ok(Value::Object(body))
}

/// `GoogleAuth` 的默认凭据链（`getApplicationDefault` 的等价物）。
///
/// 顺序与 google-auth-library 一致：`keyFilename` / `GOOGLE_APPLICATION_CREDENTIALS`
/// → `~/.config/gcloud/application_default_credentials.json` → GCE metadata server
/// → `NO_ADC_FOUND`。
///
/// 注意：一旦选中某个凭据文件，读取或换取 token 失败就立即返回错误，**不会**悄悄回退到
/// 下一种凭据（与 google-auth-library 的行为一致）。显式 `keyFilename` /
/// `GOOGLE_APPLICATION_CREDENTIALS` 指向的文件读取失败会直接报错；只有默认 ADC 文件不存在
/// （`is_file()` 为假）时才继续到 metadata server。
///
/// 返回 `Bearer` 后面的 token 字符串。`api_key` 存在时直接返回它——正常情况下
/// [`request_stream`] 已提前处理，这里是防御性分支。
async fn vertex_access_token(
    client: &GoogleClientConfig,
    options: &StreamOptions,
) -> Result<String, GoogleFailure> {
    if let Some(api_key) = &client.api_key {
        return Ok(api_key.clone());
    }
    let env = options.env.as_ref();
    let key_filename = client
        .google_auth_key_filename
        .clone()
        .or_else(|| get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env))
        .or_else(|| get_provider_env_value("google_application_credentials", env));
    if let Some(path) = key_filename {
        let credentials = read_adc_credentials(&path).map_err(|error| {
            GoogleFailure::new(format!(
                "Unable to read the credential file specified by the GOOGLE_APPLICATION_CREDENTIALS environment variable: {}",
                error.message
            ))
        })?;
        return adc_credentials_access_token(&credentials, options).await;
    }
    if let Some(path) = default_adc_path(env)
        && std::path::Path::new(&path).is_file()
    {
        let credentials = read_adc_credentials(&path)?;
        return adc_credentials_access_token(&credentials, options).await;
    }
    if google_metadata_available(options).await? {
        return google_metadata_access_token(options).await;
    }
    Err(GoogleFailure::new(NO_ADC_FOUND))
}

/// `NO_ADC_FOUND`。
const NO_ADC_FOUND: &str = "Could not load the default credentials. Browse to https://cloud.google.com/docs/authentication/getting-started for more information.";
/// `REQUIRED_VERTEX_AI_SCOPE`（`AuthClient` 的 scopes）。
const GOOGLE_CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// gtoken 硬编码的 token endpoint（`GOOGLE_TOKEN_URL`，也用作 JWT 的 `aud`）。
const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// `BaseExternalAccountClient` 的默认 STS endpoint。
const GOOGLE_STS_TOKEN_URL: &str = "https://sts.googleapis.com/v1/token";
/// gtoken 的 JWT bearer grant。
const GOOGLE_JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// RFC 8693 token exchange grant。
const GOOGLE_STS_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
/// RFC 8693 请求的 token 类型。
const GOOGLE_STS_REQUESTED_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// 默认 ADC 文件路径：`$HOME/.config/gcloud/application_default_credentials.json`。
///
/// `HOME` 从 [`ProviderEnv`]（可覆盖进程环境）读取；读不到时返回 `None`，调用方跳过该
/// 来源。这里不处理 Windows 的 `APPDATA` 路径。
fn default_adc_path(env: Option<&ProviderEnv>) -> Option<String> {
    let home = get_provider_env_value("HOME", env)?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("gcloud")
            .join("application_default_credentials.json")
            .to_string_lossy()
            .into_owned(),
    )
}

/// 读取并解析 ADC JSON 文件。
///
/// 用 `serde_json` 而不是 `kiana_json`：这是本地配置文件，错误文案只用于诊断，不要求与
/// V8 的 `JSON.parse` 一致。文件不存在与 JSON 非法给出两种不同的消息。
fn read_adc_credentials(path: &str) -> Result<Value, GoogleFailure> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        GoogleFailure::new(format!(
            "The file at {path} does not exist, or it is not a file. {error}"
        ))
    })?;
    serde_json::from_str(&text)
        .map_err(|error| GoogleFailure::new(format!("Invalid JSON key file: {error}")))
}

/// `GoogleAuth.fromJSON` + 各 client 的 `getRequestHeaders`。
///
/// 按 ADC 文件的 `type` 字段分派：
///
/// - `authorized_user` 与 `external_account_authorized_user` → refresh_token grant；
/// - `external_account` → STS token exchange（可选 service account impersonation）；
/// - `impersonated_service_account` → 明确不支持；
/// - 其它（含 `service_account`）走 JWT bearer grant 的兜底分支。
///
/// 未知 `type` 不报错而是按 service account 处理，这是 `fromJSON` 的兜底语义。
async fn adc_credentials_access_token(
    credentials: &Value,
    options: &StreamOptions,
) -> Result<String, GoogleFailure> {
    match credentials.get("type").and_then(Value::as_str) {
        Some("authorized_user") => refresh_authorized_user_token(credentials, options).await,
        Some("external_account") => external_account_access_token(credentials, options).await,
        // `external_account_authorized_user` 与 authorized_user 共用 refresh_token grant，
        // 只是 endpoint 字段叫 `token_url`。
        Some("external_account_authorized_user") => {
            refresh_authorized_user_token(credentials, options).await
        }
        Some("impersonated_service_account") => Err(GoogleFailure::new(
            "Google ADC type impersonated_service_account is not supported by the Rust adapter.",
        )),
        // `fromJSON` 的兜底分支：其它 type（含 `service_account`）都按 JWT client 处理。
        _ => service_account_access_token(credentials, options).await,
    }
}

/// token endpoint：优先 `token_uri`，其次 `token_url`（external_account_authorized_user
/// 用后者），都没有时用 gtoken 硬编码的 `GOOGLE_OAUTH_TOKEN_URL`。
fn adc_token_uri(credentials: &Value) -> &str {
    credentials
        .get("token_uri")
        .or_else(|| credentials.get("token_url"))
        .and_then(Value::as_str)
        .unwrap_or(GOOGLE_OAUTH_TOKEN_URL)
}

/// `JWT.refreshToken` → gtoken 的 JWT bearer grant。
///
/// 用 ADC 的 `client_email` / `private_key` 签一个 RS256 JWT，claims 顺序固定为
/// iss、scope、aud、exp、iat、sub（有 `subject` 时），其中：
///
/// - `scope` 固定为 cloud-platform；
/// - `aud` 固定为 `GOOGLE_OAUTH_TOKEN_URL`（gtoken 不读 ADC 的 `token_uri`）；
/// - `exp` = now + 3600 秒。
///
/// 然后向同一 endpoint POST `grant_type=...jwt-bearer`，取 `access_token`。
/// `private_key` 或 `client_email` 缺失时报 `private_key and client_email are required.`。
async fn service_account_access_token(
    credentials: &Value,
    options: &StreamOptions,
) -> Result<String, GoogleFailure> {
    let client_email = credentials
        .get("client_email")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let private_key = credentials
        .get("private_key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if client_email.is_empty() || private_key.is_empty() {
        return Err(GoogleFailure::new(
            "private_key and client_email are required.",
        ));
    }
    let now = now_unix_ms() / 1000;
    // gtoken 的 claims 顺序：iss, scope, aud, exp, iat, sub, ...additionalClaims。
    let mut claims = Map::new();
    claims.insert("iss".into(), Value::String(client_email.to_string()));
    claims.insert(
        "scope".into(),
        Value::String(GOOGLE_CLOUD_PLATFORM_SCOPE.to_string()),
    );
    // gtoken 使用硬编码的 GOOGLE_TOKEN_URL 作为 aud 与请求地址，不读 ADC 的 token_uri。
    claims.insert(
        "aud".into(),
        Value::String(GOOGLE_OAUTH_TOKEN_URL.to_string()),
    );
    claims.insert("exp".into(), Value::from(now + 3600));
    claims.insert("iat".into(), Value::from(now));
    if let Some(subject) = credentials.get("subject").and_then(Value::as_str) {
        claims.insert("sub".into(), Value::String(subject.to_string()));
    }
    let header = serde_json::json!({ "alg": "RS256" });
    let assertion = sign_rs256_jwt(private_key, &header, &Value::Object(claims))?;
    let form = encode_form(&[
        ("grant_type".into(), GOOGLE_JWT_BEARER_GRANT.to_string()),
        ("assertion".into(), assertion),
    ]);
    let response = post_google_form(GOOGLE_OAUTH_TOKEN_URL, &form, Vec::new(), options).await?;
    response
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GoogleFailure::new("Google ADC token response has no access_token"))
}

/// `BaseExternalAccountClient` 的 STS exchange（+ 可选的 service account impersonation）。
///
/// 流程：
///
/// 1. 校验 `audience` 与 `subject_token_type`（缺失报
///    `The incoming JSON object does not contain a valid external account configuration.`）；
/// 2. 取得 subject token（文件或 URL，见 `external_account_subject_token`）；
/// 3. 向 `token_url`（默认 STS endpoint）POST RFC 8693 表单。`client_id` 存在时用
///    HTTP Basic 认证，否则若有 `workforce_pool_user_project` 就加 `options` 字段；
/// 4. 有 `service_account_impersonation_url` 时再调用一次 generateAccessToken，
///    lifetime 取 `service_account_impersonation.token_lifetime_seconds`（默认 3600 秒）。
///
/// 任一响应缺少非空 `access_token`/`accessToken` 都报错。
async fn external_account_access_token(
    credentials: &Value,
    options: &StreamOptions,
) -> Result<String, GoogleFailure> {
    let audience = credentials
        .get("audience")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let subject_token_type = credentials
        .get("subject_token_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if audience.is_empty() || subject_token_type.is_empty() {
        return Err(GoogleFailure::new(
            "The incoming JSON object does not contain a valid external account configuration.",
        ));
    }
    let token_url = credentials
        .get("token_url")
        .and_then(Value::as_str)
        .unwrap_or(GOOGLE_STS_TOKEN_URL);
    let subject_token = external_account_subject_token(credentials, options).await?;
    let impersonation_url = credentials
        .get("service_account_impersonation_url")
        .and_then(Value::as_str);
    // GoogleGenAI 把 scopes 设为 [cloud-platform]；`getScopesArray()` 的默认值也是它。
    let scope = GOOGLE_CLOUD_PLATFORM_SCOPE.to_string();
    let mut form: Vec<(String, String)> = vec![
        ("grant_type".into(), GOOGLE_STS_GRANT.to_string()),
        ("audience".into(), audience.to_string()),
        ("scope".into(), scope),
        (
            "requested_token_type".into(),
            GOOGLE_STS_REQUESTED_TOKEN_TYPE.to_string(),
        ),
        ("subject_token".into(), subject_token),
        ("subject_token_type".into(), subject_token_type.to_string()),
    ];
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(client_id) = credentials.get("client_id").and_then(Value::as_str) {
        let client_secret = credentials
            .get("client_secret")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{client_id}:{client_secret}"));
        headers.push(("authorization".into(), format!("Basic {encoded}")));
    } else if let Some(user_project) = credentials
        .get("workforce_pool_user_project")
        .and_then(Value::as_str)
    {
        let options_json = serde_json::json!({ "userProject": user_project });
        form.push((
            "options".into(),
            kiana_json::js_object::stringify(&options_json),
        ));
    }
    let response = post_google_form(token_url, &encode_form(&form), headers, options).await?;
    let sts_token = response
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GoogleFailure::new("Google STS token response has no access_token"))?;
    let Some(impersonation_url) = impersonation_url else {
        return Ok(sts_token);
    };
    let lifetime = credentials
        .get("service_account_impersonation")
        .and_then(|value| value.get("token_lifetime_seconds"))
        .and_then(Value::as_f64)
        .filter(|seconds| *seconds > 0.0)
        .unwrap_or(3600.0);
    let lifetime_text = kiana_json::js_num::to_radix_string(lifetime, 10);
    let body = serde_json::json!({
        "scope": [GOOGLE_CLOUD_PLATFORM_SCOPE],
        "lifetime": format!("{lifetime_text}s"),
    });
    let request = HttpRequest {
        method: "POST".into(),
        url: impersonation_url.to_string(),
        headers: vec![
            ("content-type".into(), "application/json".into()),
            ("authorization".into(), format!("Bearer {sts_token}")),
        ],
        body: Some(kiana_json::js_object::stringify(&body).into_bytes()),
        timeout_ms: None,
    };
    let (status, text) = execute_google_http(request, options).await?;
    if !(200..300).contains(&status) {
        return Err(GoogleFailure::with_status(text, status));
    }
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| GoogleFailure::new(format!("Invalid JSON response: {error}")))?;
    parsed
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GoogleFailure::new("Google impersonation response has no accessToken"))
}

/// `IdentityPoolClient.retrieveSubjectToken` 的 file/url credential_source 分支。
///
/// `format.type` 只能是 `text`（默认）或 `json`；`json` 必须给
/// `subject_token_field_name`，否则报错。credential_source 里 file 优先于 url；两者都没有
/// 时返回 SDK 原文的 “must be either file, url, or certificate” 错误（certificate 分支
/// Rust 侧未实现）。
///
/// URL 分支会把 `credential_source.headers` 里值为字符串的头原样带上；响应非 2xx 时报带
/// status 的错误。
async fn external_account_subject_token(
    credentials: &Value,
    options: &StreamOptions,
) -> Result<String, GoogleFailure> {
    let source = credentials.get("credential_source").ok_or_else(|| {
        GoogleFailure::new("A credential source or subject token supplier must be specified.")
    })?;
    let format = source.get("format");
    let format_type = format
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("text");
    let field = format
        .and_then(|value| value.get("subject_token_field_name"))
        .and_then(Value::as_str);
    if format_type != "text" && format_type != "json" {
        return Err(GoogleFailure::new(format!(
            "Invalid credential_source format \"{format_type}\""
        )));
    }
    if format_type == "json" && field.is_none() {
        return Err(GoogleFailure::new(
            "Missing subject_token_field_name for JSON credential_source format",
        ));
    }
    if let Some(file) = source.get("file").and_then(Value::as_str) {
        let text = std::fs::read_to_string(file).map_err(|error| {
            GoogleFailure::new(format!(
                "The file at {file} does not exist, or it is not a file. {error}"
            ))
        })?;
        return subject_token_from_text(&text, format_type, field, "file");
    }
    if let Some(url) = source.get("url").and_then(Value::as_str) {
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(source_headers) = source.get("headers").and_then(Value::as_object) {
            for (name, value) in source_headers {
                let Some(value) = value.as_str() else {
                    continue;
                };
                headers.push((name.clone(), value.to_string()));
            }
        }
        let request = HttpRequest {
            method: "GET".into(),
            url: url.to_string(),
            headers,
            body: None,
            timeout_ms: None,
        };
        let (status, text) = execute_google_http(request, options).await?;
        if !(200..300).contains(&status) {
            return Err(GoogleFailure::with_status(text, status));
        }
        return subject_token_from_text(&text, format_type, field, "URL");
    }
    Err(GoogleFailure::new(
        "No valid Identity Pool \"credential_source\" provided, must be either file, url, or certificate.",
    ))
}

/// 从 credential_source 的响应文本里取 subject token。
///
/// `format_type == "json"` 时按 `subject_token_field_name` 取字段（必须是非空字符串），
/// 否则整段文本就是 token。空 token 报
/// `Unable to parse the subject_token from the credential_source <source>`，`source` 只用于
/// 消息（`file` / `URL`）。
fn subject_token_from_text(
    text: &str,
    format_type: &str,
    field: Option<&str>,
    source: &str,
) -> Result<String, GoogleFailure> {
    let token = if format_type == "json" {
        let parsed: Value = serde_json::from_str(text)
            .map_err(|error| GoogleFailure::new(format!("Invalid JSON response: {error}")))?;
        field
            .and_then(|field| parsed.get(field))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        text.to_string()
    };
    if token.is_empty() {
        return Err(GoogleFailure::new(format!(
            "Unable to parse the subject_token from the credential_source {source}"
        )));
    }
    Ok(token)
}

/// metadata server 的基地址。
///
/// 主机优先取 `GCE_METADATA_IP`，其次 `GCE_METADATA_HOST`，默认
/// `metadata.google.internal`；没写 scheme 时补 `http://`。返回
/// `<base>/computeMetadata/v1`（去掉多余尾斜杠）。
fn metadata_base_url(env: Option<&ProviderEnv>) -> String {
    let host = get_provider_env_value("GCE_METADATA_IP", env)
        .or_else(|| get_provider_env_value("GCE_METADATA_HOST", env))
        .unwrap_or_else(|| "metadata.google.internal".to_string());
    let base = if host.starts_with("http://") || host.starts_with("https://") {
        host
    } else {
        format!("http://{host}")
    };
    format!("{}/computeMetadata/v1", base.trim_end_matches('/'))
}

/// `_checkIsGCE()`：`METADATA_SERVER_DETECTION` 覆盖 + metadata server ping。
///
/// `METADATA_SERVER_DETECTION` 的取值：`assume-present` 直接返回真、`none` 直接返回假、
/// `bios-only` 只看 `gcp_residency`、`ping-only` 或未设置才真的去 ping；其它值报错。
///
/// ping 是 `GET <base>/instance`（3 秒超时）。网络错误、非 2xx、缺少
/// `Metadata-Flavor: Google` 都当作“不可用”返回 `false`，不向上抛错。
async fn google_metadata_available(options: &StreamOptions) -> Result<bool, GoogleFailure> {
    let detection = get_provider_env_value("METADATA_SERVER_DETECTION", options.env.as_ref());
    if let Some(value) = detection {
        match value.trim().to_lowercase().as_str() {
            "assume-present" => return Ok(true),
            "none" => return Ok(false),
            "bios-only" => return Ok(gcp_residency(options.env.as_ref())),
            "ping-only" => {}
            other => {
                return Err(GoogleFailure::new(format!(
                    "Unknown `METADATA_SERVER_DETECTION` env variable. Got `{other}`, but it should be `assume-present`, `none`, `bios-only`, `ping-only`, or unset"
                )));
            }
        }
    }
    let request = HttpRequest {
        method: "GET".into(),
        url: format!("{}/instance", metadata_base_url(options.env.as_ref())),
        headers: vec![("metadata-flavor".into(), "Google".into())],
        body: None,
        timeout_ms: Some(3000),
    };
    let client = match crate::api::kiana_messages::resolve_client(options) {
        Ok(client) => client,
        Err(_) => return Ok(false),
    };
    let Ok(response) = client.execute(request, options.signal.as_ref()).await else {
        return Ok(false);
    };
    if !(200..300).contains(&response.response.status) {
        return Ok(false);
    }
    // `metadataAccessor` 要求响应带 `Metadata-Flavor: Google`。
    let flavor = response
        .response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("metadata-flavor"))
        .map(|(_, value)| value.as_str());
    Ok(flavor == Some("Google"))
}

/// bios 检测：先看常见 Cloud Run / Functions 环境变量，再读 DMI。
///
/// 任一环境变量存在即为真；否则要求 `/sys/class/dmi/id/bios_date` 存在且
/// `/sys/class/dmi/id/bios_vendor` 含 `Google`。读文件失败按假处理（不报错）。
fn gcp_residency(env: Option<&ProviderEnv>) -> bool {
    for name in ["CLOUD_RUN_JOB", "FUNCTION_NAME", "K_SERVICE"] {
        if get_provider_env_value(name, env).is_some() {
            return true;
        }
    }
    if !std::path::Path::new("/sys/class/dmi/id/bios_date").exists() {
        return false;
    }
    std::fs::read_to_string("/sys/class/dmi/id/bios_vendor")
        .map(|vendor| vendor.contains("Google"))
        .unwrap_or(false)
}

/// `Compute.refreshToken`：metadata server 的默认服务账号令牌。
///
/// GET `<base>/instance/service-accounts/default/token?scopes=<cloud-platform>`（scope 做
/// form percent-encode），带 `Metadata-Flavor: Google`，3 秒超时。非 2xx 报带 status 的
/// 错误；响应缺少非空 `access_token` 也报错。
async fn google_metadata_access_token(options: &StreamOptions) -> Result<String, GoogleFailure> {
    let mut url = format!(
        "{}/instance/service-accounts/default/token",
        metadata_base_url(options.env.as_ref())
    );
    url.push_str("?scopes=");
    url.push_str(&encode_form_component(GOOGLE_CLOUD_PLATFORM_SCOPE));
    let request = HttpRequest {
        method: "GET".into(),
        url,
        headers: vec![("metadata-flavor".into(), "Google".into())],
        body: None,
        timeout_ms: Some(3000),
    };
    let (status, text) = execute_google_http(request, options).await?;
    if !(200..300).contains(&status) {
        return Err(GoogleFailure::with_status(text, status));
    }
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| GoogleFailure::new(format!("Invalid JSON response: {error}")))?;
    parsed
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GoogleFailure::new("Google metadata token response has no access_token"))
}

/// `UserRefreshClient.refreshToken`：refresh_token grant。
///
/// 表单字段顺序 grant_type → client_id → client_secret → refresh_token；endpoint 由
/// `adc_token_uri` 决定。适用于 `authorized_user` 与
/// `external_account_authorized_user` 两种字典（后者只有 endpoint 字段名不同）。
/// 缺字段时按空串发送，最终由服务端拒绝。
async fn refresh_authorized_user_token(
    credentials: &Value,
    options: &StreamOptions,
) -> Result<String, GoogleFailure> {
    let client_id = credentials
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let client_secret = credentials
        .get("client_secret")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let refresh_token = credentials
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let token_uri = adc_token_uri(credentials);
    let form = encode_form(&[
        ("grant_type".into(), "refresh_token".to_string()),
        ("client_id".into(), client_id.to_string()),
        ("client_secret".into(), client_secret.to_string()),
        ("refresh_token".into(), refresh_token.to_string()),
    ]);
    let response = post_google_form(token_uri, &form, Vec::new(), options).await?;
    response
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| GoogleFailure::new("Google ADC token response has no access_token"))
}

/// `URLSearchParams` 的 `application/x-www-form-urlencoded` percent-encode set：
/// 字母数字与 `*-._` 之外的字节全部转义，空格写成 `+`。
fn encode_form_component(value: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'*')
        .remove(b'-')
        .remove(b'.')
        .remove(b'_');
    percent_encoding::utf8_percent_encode(value, SET)
        .to_string()
        .replace("%20", "+")
}

/// 把 `(key, value)` 列表编码成 `application/x-www-form-urlencoded` 字符串，用 `&` 连接。
fn encode_form(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                encode_form_component(key),
                encode_form_component(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// 用 `StreamOptions` 里的 `HttpClient` 执行一次请求并读完整个 body。
///
/// 返回 `(status, text)`；body 按 `response.text()` 语义解码（非法 UTF-8 变成 U+FFFD）。
/// 传输或读取错误都转成不带 status 的 [`GoogleFailure`]，调用方据此按普通失败处理。
async fn execute_google_http(
    request: HttpRequest,
    options: &StreamOptions,
) -> Result<(u16, String), GoogleFailure> {
    let client = crate::api::kiana_messages::resolve_client(options)
        .map_err(|error| GoogleFailure::new(error.message))?;
    let response = client
        .execute(request, options.signal.as_ref())
        .await
        .map_err(|error| GoogleFailure::new(error.message))?;
    let status = response.response.status;
    let body = crate::api::kiana_messages::read_body_text(response.body, options.signal.as_ref())
        .await
        .map_err(|error| GoogleFailure::new(error.message))?;
    Ok((status, body))
}

/// 以 `application/x-www-form-urlencoded` POST 一个表单，并解析 JSON 响应。
///
/// 非 2xx 时返回带 status 的 [`GoogleFailure`]（body 原文即 message）；2xx 但 JSON 非法
/// 时报 `Invalid JSON response: ...`。ADC 的 token endpoint 都用它。
async fn post_google_form(
    url: &str,
    form: &str,
    extra_headers: Vec<(String, String)>,
    options: &StreamOptions,
) -> Result<Value, GoogleFailure> {
    let mut headers = vec![(
        "content-type".into(),
        "application/x-www-form-urlencoded".into(),
    )];
    headers.extend(extra_headers);
    let request = HttpRequest {
        method: "POST".into(),
        url: url.to_string(),
        headers,
        body: Some(form.as_bytes().to_vec()),
        timeout_ms: None,
    };
    let (status, body) = execute_google_http(request, options).await?;
    if !(200..300).contains(&status) {
        return Err(GoogleFailure::with_status(body, status));
    }
    serde_json::from_str(&body)
        .map_err(|error| GoogleFailure::new(format!("Invalid JSON response: {error}")))
}

// ---------------------------------------------------------------------------
// RS256（service account ADC 的 JWT 签名）
// ---------------------------------------------------------------------------

/// 只保留 RS256 签名所需的 RSA 参数（模数、私钥指数）。
///
/// 不使用 CRT（p/q/dp/dq/qinv），也不校验素数与密钥一致性：签名直接算 `m^d mod n`。
/// 输入是本地 ADC 凭据文件里的 `private_key` 字段。
struct RsaPrivateKey {
    /// 模数 n（大端字节）。
    modulus: num_bigint::BigUint,
    /// 私钥指数 d。
    private_exponent: num_bigint::BigUint,
    /// n 的字节长度；签名结果要左补零到这个长度。
    modulus_bytes: usize,
}

/// 读取 PKCS#8（`BEGIN PRIVATE KEY`）或 PKCS#1（`BEGIN RSA PRIVATE KEY`）的 RSA 私钥。
///
/// 步骤：去掉 PEM 头尾行 → Base64 解码 → 解析最外层 SEQUENCE → 读取第一个 TLV 的
/// version → 用第二个 TLV 的 tag 区分格式（`0x30` 是 PKCS#8 的 AlgorithmIdentifier，
/// `0x02` 是 PKCS#1 的 modulus INTEGER，version 已在前面读取）→
/// 依次读 version、modulus、publicExponent、privateExponent。
///
/// 返回 `Err(String)` 而不是 [`GoogleFailure`]：调用方 `sign_rs256_jwt` 会把它包进带
/// 上下文的消息。加密的 PKCS#8（EncryptedPrivateKeyInfo）不受支持，会在版本字段处失败。
fn rsa_private_key_from_pem(pem: &str) -> Result<RsaPrivateKey, String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.trim_start().starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|error| format!("Invalid service account private key: {error}"))?;
    let (tag, content, _) = der_read_tlv(&der, 0)
        .ok_or_else(|| "Invalid service account private key: malformed DER".to_string())?;
    if tag != 0x30 {
        return Err("Invalid service account private key: expected a SEQUENCE".into());
    }
    let (version_tag, _, after_version) = der_read_tlv(content, 0)
        .ok_or_else(|| "Invalid service account private key: missing version".to_string())?;
    if version_tag != 0x02 {
        return Err("Invalid service account private key: expected a version INTEGER".into());
    }
    let (second_tag, _, after_second) = der_read_tlv(content, after_version)
        .ok_or_else(|| "Invalid service account private key: truncated".to_string())?;
    let rsa_content = if second_tag == 0x30 {
        // PKCS#8 PrivateKeyInfo：version, algorithm, OCTET STRING privateKey。
        let (octet_tag, octet_body, _) = der_read_tlv(content, after_second)
            .ok_or_else(|| "Invalid service account private key: missing key bytes".to_string())?;
        if octet_tag != 0x04 {
            return Err("Invalid service account private key: expected an OCTET STRING".into());
        }
        let (rsa_tag, rsa_body, _) = der_read_tlv(octet_body, 0).ok_or_else(|| {
            "Invalid service account private key: malformed RSAPrivateKey".to_string()
        })?;
        if rsa_tag != 0x30 {
            return Err("Invalid service account private key: expected an RSAPrivateKey".into());
        }
        rsa_body
    } else if second_tag == 0x02 {
        // PKCS#1 RSAPrivateKey：顶层就是字段序列。
        content
    } else {
        return Err("Invalid service account private key: unsupported encoding".into());
    };
    let (_, _, after_rsa_version) = der_read_tlv(rsa_content, 0).ok_or_else(|| {
        "Invalid service account private key: missing RSAPrivateKey version".to_string()
    })?;
    let (modulus_tag, modulus_body, after_modulus) =
        der_read_tlv(rsa_content, after_rsa_version)
            .ok_or_else(|| "Invalid service account private key: missing modulus".to_string())?;
    let (_, _, after_public_exponent) =
        der_read_tlv(rsa_content, after_modulus).ok_or_else(|| {
            "Invalid service account private key: missing public exponent".to_string()
        })?;
    let (private_tag, private_body, _) = der_read_tlv(rsa_content, after_public_exponent)
        .ok_or_else(|| {
            "Invalid service account private key: missing private exponent".to_string()
        })?;
    if modulus_tag != 0x02 || private_tag != 0x02 {
        return Err("Invalid service account private key: expected INTEGER fields".into());
    }
    let modulus = num_bigint::BigUint::from_bytes_be(modulus_body);
    let private_exponent = num_bigint::BigUint::from_bytes_be(private_body);
    let modulus_bytes = modulus.to_bytes_be().len();
    if modulus_bytes == 0 || private_exponent.bits() == 0 {
        return Err("Invalid service account private key: empty RSA parameters".into());
    }
    Ok(RsaPrivateKey {
        modulus,
        private_exponent,
        modulus_bytes,
    })
}

/// 读一个 DER TLV：返回 `(tag, value 切片, 下一个字节的偏移)`。
///
/// 长度支持短格式与 1–4 字节长格式；所有越界与非法长度都返回 `None` 而不是 panic。
/// 不做 tag 合法性与 DER 规范性检查——这些由调用方按字段校验。
fn der_read_tlv(input: &[u8], offset: usize) -> Option<(u8, &[u8], usize)> {
    let tag = *input.get(offset)?;
    let first = *input.get(offset + 1)? as usize;
    let (length, header) = if first < 0x80 {
        (first, 2usize)
    } else {
        let count = first & 0x7f;
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for index in 0..count {
            length = (length << 8) | *input.get(offset + 2 + index)? as usize;
        }
        (length, 2 + count)
    };
    let start = offset.checked_add(header)?;
    let end = start.checked_add(length)?;
    if end > input.len() {
        return None;
    }
    Some((tag, &input[start..end], end))
}

/// 生成 RS256 JWT：`base64url(header).base64url(payload).base64url(signature)`。
///
/// header/payload 用 `kiana_json::js_object::stringify` 序列化，保证与 JS 的键序和转义
/// 一致；Base64 用 URL-safe 且不带 padding（JWT 要求）。签名失败统一包装成
/// `Unable to sign the Google ADC JWT: ...`。
fn sign_rs256_jwt(
    private_key_pem: &str,
    header: &Value,
    payload: &Value,
) -> Result<String, GoogleFailure> {
    let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(kiana_json::js_object::stringify(header).as_bytes());
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(kiana_json::js_object::stringify(payload).as_bytes());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature =
        rsa_sha256_sign(private_key_pem, signing_input.as_bytes()).map_err(|error| {
            GoogleFailure::new(format!("Unable to sign the Google ADC JWT: {error}"))
        })?;
    Ok(format!(
        "{signing_input}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&signature)
    ))
}

/// RSASSA-PKCS1-v1_5 + SHA-256（`jwa` 的 RS256）。
///
/// 步骤：SHA-256(message) → 拼 DER `DigestInfo` → EMSA-PKCS1-v1_5 编码
/// （`0x00 0x01 0xFF... 0x00 || T`）→ `m^d mod n` → 左补零到模数字节长度。
///
/// 实现是确定性的（符合标准），测试用 OpenSSL 的已知答案锁定。注意：`num-bigint` 的
/// `modpow` 未做盲化，也不是恒定时间实现；本函数只用于本地 ADC 私钥签名。
fn rsa_sha256_sign(private_key_pem: &str, message: &[u8]) -> Result<Vec<u8>, String> {
    use sha2::Digest as _;

    let key = rsa_private_key_from_pem(private_key_pem)?;
    let digest = sha2::Sha256::digest(message);
    // DER 编码的 DigestInfo（SHA-256）。
    const SHA256_DIGEST_INFO: [u8; 19] = [
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
        0x05, 0x00, 0x04, 0x20,
    ];
    let mut encoded = Vec::with_capacity(SHA256_DIGEST_INFO.len() + digest.len());
    encoded.extend_from_slice(&SHA256_DIGEST_INFO);
    encoded.extend_from_slice(&digest);
    let key_bytes = key.modulus_bytes;
    if key_bytes < encoded.len() + 11 {
        return Err("RSA modulus too short".into());
    }
    let mut em = vec![0u8; key_bytes];
    em[1] = 0x01;
    let padding = key_bytes - encoded.len() - 3;
    em[2..2 + padding].fill(0xff);
    em[2 + padding] = 0x00;
    em[3 + padding..].copy_from_slice(&encoded);
    let message = num_bigint::BigUint::from_bytes_be(&em);
    let signature = message.modpow(&key.private_exponent, &key.modulus);
    let bytes = signature.to_bytes_be();
    if bytes.len() > key_bytes {
        return Err("RSA signature longer than the modulus".into());
    }
    let mut out = vec![0u8; key_bytes - bytes.len()];
    out.extend_from_slice(&bytes);
    Ok(out)
}

// ---------------------------------------------------------------------------
// SSE 解析
// ---------------------------------------------------------------------------

/// `processStreamResponse` 里对原始 chunk 的 error 检查。
///
/// TS 用 `errorJson['status']` 内插（缺失 → `undefined`）并按 `code >= 400 && code < 600`
/// 决定是否抛 `ApiError`（JSON 里的 `"500"`/`true` 也会被 JS 关系比较强制转换）。
fn google_chunk_error(parsed: &Value) -> Option<GoogleFailure> {
    let error = parsed.get("error")?;
    let code = js_to_number(error.get("code"));
    if (400.0..600.0).contains(&code) {
        // `${status}` 是 JS 模板字符串：字段缺失时输出 `undefined`。
        let status = js_string_of_value(error.get("status"));
        return Some(GoogleFailure::with_status(
            format!(
                "got status: {status}. {}",
                kiana_json::js_object::stringify(parsed)
            ),
            code as u16,
        ));
    }
    None
}

/// SDK `processStreamResponse` 的等价物：逐条产出 chunk JSON。
///
/// 输入是响应体字节流（既不保证按 UTF-8 边界切分，也不保证按 SSE 事件边界切分），因此：
///
/// 1. 用 `Utf8StreamDecoder` 做流式解码，跨 chunk 保留不完整的字符；
/// 2. 每收到一段文本，先整体尝试 `JSON.parse` 并检查 `error`（Google 有时直接返回一个
///    非 SSE 的错误 JSON，见 `google_chunk_error`）；
/// 3. 把文本积攒进 buffer，按三种分隔符中**最先出现**的一个切事件（`\n\n`、`\r\r`、
///    `\r\n\r\n`）；只有以 `data:` 开头的事件会被解析，其余行忽略；
/// 4. 回调返回 `Ok(true)` 表示调用方已终止（例如已拿到终态），本函数立即返回 `Ok(())`；
/// 5. 流结束时缓冲区仍有非空白内容 → `"Incomplete JSON segment at the end"`。
///
/// # 错误
///
/// - 读取错误：取消令牌已触发时返回 `ABORTED_MESSAGE`，否则透传原始 message；
/// - 事件 JSON 非法：返回 V8 风格的 `SyntaxError` 文案（来自 `kiana_json::js_parse`）；
/// - 流内 `error.code` 在 400–599：转为带 status 的 [`GoogleFailure`]。
pub async fn read_google_events(
    mut body: ByteStream,
    signal: Option<tokio_util::sync::CancellationToken>,
    mut on_event: impl FnMut(Value) -> Result<bool, GoogleFailure>,
) -> Result<(), GoogleFailure> {
    // 跨 chunk 的 UTF-8 解码；done 时用 stream=false 刷出残留字节。
    let mut decoder = Utf8StreamDecoder::new();
    let mut buffer = String::new();
    loop {
        let (done, bytes) = match next_chunk(&mut body, signal.as_ref()).await {
            Ok(Some(bytes)) => (false, bytes),
            Ok(None) => (true, Vec::new()),
            Err(error) => {
                if signal.as_ref().is_some_and(|token| token.is_cancelled()) {
                    return Err(GoogleFailure::new(
                        crate::http::reqwest_client::ABORTED_MESSAGE,
                    ));
                }
                return Err(GoogleFailure::new(error.message));
            }
        };
        let chunk_string = decoder.decode(&bytes, !done);
        // SDK 先对**原始 chunk** 尝试整体 JSON.parse 并检查 error（SSE 之外的服务端错误）。
        // 用 `js_parse`（JSON.parse 等价物）：孤立代理项转义、溢出数字等 V8 接受的输入
        // 这里也必须接受，否则错误 chunk 会被静默漏掉。
        if !chunk_string.is_empty()
            && let Ok(parsed) = kiana_json::js_parse::parse(&chunk_string)
            && let Some(failure) = google_chunk_error(&parsed)
        {
            return Err(failure);
        }
        buffer.push_str(&chunk_string);
        // 一条事件 = 上一个分隔符之后、下一个分隔符之前的文本；drain 掉已消费的前缀。
        while let Some((index, length)) = find_google_delimiter(&buffer) {
            let event_string = buffer[..index].to_string();
            buffer.drain(..index + length);
            let trimmed = kiana_json::js_string::js_trim(&event_string);
            // 非 `data:` 行（注释、event:、id:）被忽略。
            if let Some(data) = trimmed.strip_prefix("data:") {
                let processed = kiana_json::js_string::js_trim(data);
                // `chunk.json()`（标准 Response.json → JSON.parse）：错误文本就是 V8 的
                // SyntaxError 文案，SDK 不会把它改写成 `exception parsing stream chunk ...`
                // （那段包装只覆盖 `new Response(...)` 构造，构造不会解析 JSON）。
                let parsed: Value = kiana_json::js_parse::parse(processed)
                    .map_err(|error| GoogleFailure::new(error.message))?;
                if on_event(parsed)? {
                    return Ok(());
                }
            }
        }
        if done {
            break;
        }
    }
    if !kiana_json::js_string::js_trim(&buffer).is_empty() {
        return Err(GoogleFailure::new("Incomplete JSON segment at the end"));
    }
    Ok(())
}

/// SDK 的三种分隔符里**最先出现**的一个，返回 `(起始下标, 分隔符字节长度)`。
///
/// 三种都找不到时返回 `None`；同时出现多个时取下标最小的那个，保证不跳过更早的事件边界。
fn find_google_delimiter(buffer: &str) -> Option<(usize, usize)> {
    ["\n\n", "\r\r", "\r\n\r\n"]
        .iter()
        .filter_map(|delimiter| buffer.find(delimiter).map(|index| (index, delimiter.len())))
        .min_by_key(|(index, _)| *index)
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 按给定键序构造 JSON 对象。
///
/// `serde_json` 开启了 `preserve_order`，因此插入顺序就是序列化顺序；调用方用它固定
/// 线上 JSON 的键序（测试按逐字节字符串断言）。`pairs` 里重复的键保留后一次写入，
/// 但位置仍是第一次插入的位置。
pub fn object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// 模块级工具调用计数器（TS 的 `let toolCallCounter = 0`）。
///
/// 进程级共享、跨会话不重置（与 TS 模块级变量一致）；用 `SeqCst` 保证多线程下生成的
/// 序号不重复。`AtomicU64` 在溢出时回绕，实际使用中不可能达到。
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成唯一工具调用 id：`${name}_${Date.now()}_${++counter}`。
///
/// 用于 Google 没有给 `functionCall.id`（或 id 与已有调用重复）的场合。序号来自进程级
/// 计数器，时间戳来自系统时钟，因此同一进程内不会重复；不同进程在同一毫秒内可能碰撞。
/// `name` 为空时 id 以 `_` 开头。
pub fn generate_tool_call_id(name: &str) -> String {
    let counter = TOOL_CALL_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
    format!("{name}_{}_{counter}", now_unix_ms())
}

/// `Date.now()`：Unix 毫秒时间戳；系统时钟早于 epoch 时返回 0（不 panic）。
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 供 `google-generative-ai` / `google-vertex` 复用的 `SystemInstruction` 文本。
///
/// 等价于 `getInitialSystemMessage(...)` → `getSystemMessageText(...)` 两步。当前两个
/// 适配器在 [`build_params`] 内用同样两步自行取值；本函数是这一逻辑的公开封装。
pub fn initial_system_instruction(context: &TranscriptContext) -> Option<String> {
    get_initial_system_message(&context.0.messages).map(crate::utils::text::get_system_message_text)
}

// ---------------------------------------------------------------------------
// buildParams / 事件循环（两个 Google 适配器共用）
// ---------------------------------------------------------------------------

/// `BuildParamsOptions`：`GoogleOptions` / `GoogleVertexOptions` 的公共字段。
#[derive(Debug, Clone, Default)]
pub struct GoogleParamsOptions {
    /// 采样温度；`Some` 时才写 `generationConfig.temperature`。
    pub temperature: Option<f64>,
    /// 输出上限；写 `generationConfig.maxOutputTokens`。
    pub max_tokens: Option<u64>,
    /// `"auto"` / `"none"` / `"any"`，处理规则见
    /// [`resolve_google_function_calling_mode`]。
    pub tool_choice: Option<String>,
    /// 思考配置；`None` 表示调用方完全没设置，不会发 `thinkingConfig`。
    pub thinking: Option<GoogleThinkingOption>,
    /// 构造参数之前就检查过的取消状态；为真时 [`build_params`] 直接返回
    /// [`AiErrorKind::Aborted`]，不构建请求。
    pub signal_aborted: bool,
}

/// `thinking?: { enabled: boolean; budgetTokens?: number; level?: GoogleApiThinkingLevel }`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoogleThinkingOption {
    /// 是否请求思考。`false` 且模型支持思考时，会发一个“关闭思考”的配置
    /// （见 [`get_disabled_google_thinking_config`]）；模型不支持思考时忽略。
    pub enabled: bool,
    /// token 预算；`-1` 表示动态预算（TS 注释），`0` 表示关闭。`level` 缺省时才生效。
    pub budget_tokens: Option<f64>,
    /// 离散等级；设置时优先于 `budget_tokens`。
    pub level: Option<GoogleApiThinkingLevel>,
}

/// `getGoogleBudget` 的两种变体（Vertex 没有 `2.5-flash-lite` 分支）。
///
/// 变体只影响 2.5 系列的默认预算表；自定义 `ThinkingBudgets` 与 `2.5-pro`/`2.5-flash`
/// 分支两者相同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleBudgetFlavor {
    /// `google-generative-ai`：pro / flash-lite / flash 三个分支。
    GenerativeAi,
    /// `google-vertex`：pro / flash 两个分支。
    Vertex,
}

/// `buildParams(model, context, options)`。
///
/// 汇总一次 Gemini 请求的 `GenerateContentParameters`：
///
/// - `contents`：来自 [`convert_messages`]；
/// - `generationConfig`：temperature / maxOutputTokens / thinkingConfig；
/// - 其它父级字段：`systemInstruction`（首条 system 文本，经 surrogate 清洗）、`tools`
///   （[`convert_tools`]）、`toolConfig`（[`resolve_google_function_calling_mode`]）；
/// - `model` 字段只用于拼 URL，不进请求体。
///
/// 键序按 TS 的构造顺序：generation 字段先拷进 config，再依次追加 systemInstruction、
/// tools、toolConfig、thinkingConfig。适配器随后可能用 `on_payload` 整体替换这个 Value。
///
/// # 错误
///
/// - 已取消（`signal_aborted`）→ [`AiErrorKind::Aborted`]；
/// - tool strict 约束或 thinking 映射非法 → 对应的 `AiError`。
///
/// 取消检查位于函数末尾，与 TS 的 `if (options.signal) { if (aborted) throw }` 位置一致。
pub fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &GoogleParamsOptions,
) -> Result<Value, AiError> {
    let contents = convert_messages(model, context);
    let initial_system_message = get_initial_system_message(&context.0.messages);
    // 只取当前上下文里可见的工具，而不是历史累积的全部工具。
    let current_tools = crate::utils::transcript::get_current_tools(&context.0.messages);

    let mut generation_config = Map::new();
    if let Some(temperature) = options.temperature {
        generation_config.insert("temperature".into(), number_value(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        generation_config.insert("maxOutputTokens".into(), Value::from(max_tokens));
    }

    let supports_strict_mode = supports_google_strict_tool_sampling(&model.id);
    // 没有工具时不发 toolConfig（空数组也不发）。
    let function_calling_mode = if current_tools.is_empty() {
        None
    } else {
        resolve_google_function_calling_mode(
            &current_tools,
            options.tool_choice.as_deref(),
            supports_strict_mode,
        )?
    };
    let system_instruction = initial_system_message
        .map(crate::utils::text::get_system_message_text)
        .unwrap_or_default();

    // 先放 generation 字段，再按 TS 顺序追加父级字段，键序即线上 JSON 顺序。
    let mut config = Map::new();
    for (key, value) in generation_config {
        config.insert(key, value);
    }
    if !system_instruction.is_empty() {
        config.insert(
            "systemInstruction".into(),
            Value::String(sanitize_surrogates(&system_instruction)),
        );
    }
    if !current_tools.is_empty()
        && let Some(tools) = convert_tools(&current_tools, false, supports_strict_mode)?
    {
        config.insert("tools".into(), tools);
    }
    if let Some(mode) = function_calling_mode {
        config.insert(
            "toolConfig".into(),
            object([(
                "functionCallingConfig",
                object([("mode", Value::String(mode.into()))]),
            )]),
        );
    }

    if let Some(thinking) = &options.thinking {
        // `level` 优先于 `budgetTokens`；模型不支持 reasoning 时两个分支都不发。
        if thinking.enabled && model.reasoning == Some(true) {
            let mut thinking_config = Map::new();
            thinking_config.insert("includeThoughts".into(), Value::Bool(true));
            if let Some(level) = thinking.level {
                thinking_config.insert(
                    "thinkingLevel".into(),
                    Value::String(to_google_sdk_thinking_level(level).into()),
                );
            } else if let Some(budget_tokens) = thinking.budget_tokens {
                thinking_config.insert("thinkingBudget".into(), number_value(budget_tokens));
            }
            config.insert("thinkingConfig".into(), Value::Object(thinking_config));
        } else if model.reasoning == Some(true) && !thinking.enabled {
            config.insert(
                "thinkingConfig".into(),
                get_disabled_google_thinking_config(model)?,
            );
        }
    }

    // 与 TS 一样，在 config 构建完之后才检查取消。
    if options.signal_aborted {
        return Err(AiError::new(AiErrorKind::Aborted, "Request aborted"));
    }

    let mut params = Map::new();
    params.insert("model".into(), Value::String(model.id.clone()));
    params.insert("contents".into(), contents);
    params.insert("config".into(), Value::Object(config));
    Ok(Value::Object(params))
}

/// `getGoogleBudget(model, level, customBudgets?)`。
///
/// 返回 token 预算，优先级：
///
/// 1. `custom_budgets` 中对应档位的值；
/// 2. 按模型 id 的默认表：`2.5-pro` → 128/2048/8192/32768；`2.5-flash-lite`（仅
///    [`GoogleBudgetFlavor::GenerativeAi`]）→ 512/2048/8192/24576；`2.5-flash` →
///    128/2048/8192/24576；
/// 3. 都不匹配 → `-1.0`（动态预算哨兵，来自 TS 的 `budgetTokens: -1 for dynamic`）。
///
/// 匹配是子串判断，`2.5-flash-lite` 必须排在 `2.5-flash` 之前检查，否则会被后者吞掉——
/// 代码顺序就是这条约束。
pub fn get_google_budget(
    model: &Model,
    level: ResolvedGoogleThinkingLevel,
    custom_budgets: Option<&crate::types::ThinkingBudgets>,
    flavor: GoogleBudgetFlavor,
) -> f64 {
    let custom = custom_budgets.and_then(|budgets| match level {
        ResolvedGoogleThinkingLevel::Minimal => budgets.minimal,
        ResolvedGoogleThinkingLevel::Low => budgets.low,
        ResolvedGoogleThinkingLevel::Medium => budgets.medium,
        ResolvedGoogleThinkingLevel::High => budgets.high,
    });
    if let Some(custom) = custom {
        return custom as f64;
    }
    let pick = |minimal: f64, low: f64, medium: f64, high: f64| match level {
        ResolvedGoogleThinkingLevel::Minimal => minimal,
        ResolvedGoogleThinkingLevel::Low => low,
        ResolvedGoogleThinkingLevel::Medium => medium,
        ResolvedGoogleThinkingLevel::High => high,
    };
    if model.id.contains("2.5-pro") {
        return pick(128.0, 2048.0, 8192.0, 32768.0);
    }
    if flavor == GoogleBudgetFlavor::GenerativeAi && model.id.contains("2.5-flash-lite") {
        return pick(512.0, 2048.0, 8192.0, 24576.0);
    }
    if model.id.contains("2.5-flash") {
        return pick(128.0, 2048.0, 8192.0, 24576.0);
    }
    -1.0
}

/// 把 f64 写成 JSON 数字：整数值用 i64（避免序列化成 `100.0`），其余用最短往返表示。
///
/// 非有限值（NaN / Infinity）在 JSON 里没有表示，写成 `null`——正常请求不会出现，
/// 但调用方误传时不会 panic。
fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value.is_finite() && value.abs() < 9.007_199_254_740_992e15 {
        Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// `consumeGoogleStream` 的可变状态：当前打开的文本/思考块。
///
/// 不变量：`current_block` 为 `Some(kind)` 时，`AssistantMessage.content` 的**最后一个**
/// 块就是该类型的块。所有事件里的 `content_index` 都用 `content.len() - 1` 计算，
/// 依赖这条不变量；调用方不能在两次调用之间往 `output.content` 末尾塞别的东西。
#[derive(Debug, Default)]
pub struct GoogleStreamState {
    /// 尚未收到 end 事件的块类型；`None` 表示当前没有打开的块。
    pub current_block: Option<GoogleBlockKind>,
}

/// 当前打开的块类型。
///
/// Google 的 part 只有两类可以流式累积：普通文本与 `thought: true` 的思考文本。
/// `functionCall` 是完整对象，不进这个状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleBlockKind {
    /// 文本块（`text_start` / `text_delta` / `text_end`）。
    Text,
    /// 思考块（`thinking_start` / `thinking_delta` / `thinking_end`）。
    Thinking,
}

/// 处理一个 `GenerateContentResponse` chunk（`stream.push` 的 `start` 已由调用方发出）。
///
/// 这是流式状态机的核心：输入单个 chunk，副作用有三类——修改 `output`、更新 `state`、
/// 向 `stream` 推送增量事件。
///
/// # 状态机（文本/思考块）
///
/// 状态集合 `{None, Text, Thinking}`：
///
/// - 收到带 `text` 的 part：若没有打开的块、或 part 的 `thought` 类型与当前块不同，
///   先给旧块发 end 事件，再 push 新块并发出 start 事件；
/// - 类型相同则直接追加到当前块并发 delta 事件；
/// - 收到 `functionCall`：先关闭当前块（`take`），再推 toolCall 的
///   start/delta/end 三个事件——toolCall 没有增量累积。
///
/// # 其它字段
///
/// - `responseId`：只保留第一个非空值（`||=` 语义）；
/// - `finishReason`：写 `raw_stop_reason` → [`map_stop_reason`]；若结果是 `Stop` 但内容
///   里已有 toolCall，则改判 `ToolUse`；
/// - `usageMetadata`：覆盖式写 `usage`（每个 chunk 带的是累计值），随后重算 cost。
///
/// # 错误
///
/// 未知 `finishReason` 由 [`map_stop_reason`] 返回 `AiError`，适配器会把它转成错误事件
/// 并终止流。`stream.push` 的错误被 `let _ =` 丢弃，本函数不会因此中止。
pub fn handle_google_chunk(
    model: &Model,
    output: &mut crate::types::AssistantMessage,
    stream: &crate::event_stream::AssistantMessageEventStream,
    state: &mut GoogleStreamState,
    chunk: &Value,
) -> Result<(), AiError> {
    use crate::types::{
        AssistantContentBlock, AssistantMessageEvent, TextContent, TextKind, ThinkingContent,
        ThinkingKind, ToolCall, ToolCallKind,
    };

    // `output.responseId ||= chunk.responseId`
    if output.response_id.as_deref().is_none_or(str::is_empty)
        && let Some(response_id) = chunk.get("responseId").and_then(Value::as_str)
    {
        output.response_id = Some(response_id.to_string());
    }

    // SDK 只看 candidates[0]；其它 candidate 被忽略。
    let candidate = chunk
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first());

    // parts 里可能混合 text / thought / functionCall，按数组顺序逐个处理。
    if let Some(parts) = candidate
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                let is_thinking = is_thinking_part(part);
                // 块类型切换点：None → 任意类型，Text ↔ Thinking 互相切换。
                let needs_new_block = match state.current_block {
                    None => true,
                    Some(GoogleBlockKind::Thinking) => !is_thinking,
                    Some(GoogleBlockKind::Text) => is_thinking,
                };
                if needs_new_block {
                    if let Some(kind) = state.current_block {
                        finish_google_block(kind, output, stream);
                    }
                    if is_thinking {
                        output
                            .content
                            .push(AssistantContentBlock::Thinking(ThinkingContent {
                                kind_thinking: ThinkingKind::Thinking,
                                thinking: String::new(),
                                thinking_signature: None,
                                redacted: None,
                            }));
                        state.current_block = Some(GoogleBlockKind::Thinking);
                        let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                            content_index: output.content.len() - 1,
                            partial: Box::new(output.clone()),
                        });
                    } else {
                        output
                            .content
                            .push(AssistantContentBlock::Text(TextContent {
                                kind_text: TextKind::Text,
                                text: String::new(),
                                text_signature: None,
                            }));
                        state.current_block = Some(GoogleBlockKind::Text);
                        let _ = stream.push(AssistantMessageEvent::TextStart {
                            content_index: output.content.len() - 1,
                            partial: Box::new(output.clone()),
                        });
                    }
                }
                let incoming_signature = part.get("thoughtSignature").and_then(Value::as_str);
                if state.current_block == Some(GoogleBlockKind::Thinking) {
                    if let Some(AssistantContentBlock::Thinking(block)) = output.content.last_mut()
                    {
                        block.thinking.push_str(text);
                        block.thinking_signature = retain_thought_signature(
                            block.thinking_signature.as_deref(),
                            incoming_signature,
                        );
                    }
                    let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: output.content.len() - 1,
                        delta: text.to_string(),
                        partial: Box::new(output.clone()),
                    });
                } else {
                    if let Some(AssistantContentBlock::Text(block)) = output.content.last_mut() {
                        block.text.push_str(text);
                        block.text_signature = retain_thought_signature(
                            block.text_signature.as_deref(),
                            incoming_signature,
                        );
                    }
                    let _ = stream.push(AssistantMessageEvent::TextDelta {
                        content_index: output.content.len() - 1,
                        delta: text.to_string(),
                        partial: Box::new(output.clone()),
                    });
                }
            }

            if let Some(function_call) = part.get("functionCall") {
                // toolCall 是完整对象，不走增量状态机；先结束仍打开的文本/思考块。
                if let Some(kind) = state.current_block.take() {
                    finish_google_block(kind, output, stream);
                }
                let provided_id = function_call.get("id").and_then(Value::as_str);
                // 没有 id、空 id，或 id 与本条消息里已有的 toolCall 重复时才生成新 id。
                let needs_new_id = provided_id.is_none_or(|id| {
                    id.is_empty()
                        || output.content.iter().any(|block| {
                            matches!(block, AssistantContentBlock::ToolCall(call) if call.id == id)
                        })
                });
                let name = function_call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let tool_call_id = if needs_new_id {
                    generate_tool_call_id(name)
                } else {
                    provided_id.unwrap_or_default().to_string()
                };
                let arguments: JsonObject = match function_call.get("args") {
                    Some(Value::Object(map)) => map
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                    _ => JsonObject::new(),
                };
                let thought_signature = part
                    .get("thoughtSignature")
                    .and_then(Value::as_str)
                    .filter(|signature| !signature.is_empty())
                    .map(str::to_string);
                let tool_call = ToolCall {
                    kind_tool_call: ToolCallKind::ToolCall,
                    id: tool_call_id,
                    name: name.to_string(),
                    arguments: arguments.clone(),
                    thought_signature,
                    namespace: None,
                    arguments_error: None,
                };
                output
                    .content
                    .push(AssistantContentBlock::ToolCall(tool_call.clone()));
                let content_index = output.content.len() - 1;
                let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                    content_index,
                    partial: Box::new(output.clone()),
                });
                let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                    content_index,
                    delta: kiana_json::js_object::stringify(&Value::Object(
                        arguments
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                    )),
                    partial: Box::new(output.clone()),
                });
                let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                    content_index,
                    tool_call,
                    partial: Box::new(output.clone()),
                });
            }
        }
    }

    // finishReason 可能出现在任意 chunk；后到的值覆盖先到的（通常只在最后一个 chunk）。
    if let Some(finish_reason) = candidate
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
    {
        output.raw_stop_reason = Some(finish_reason.to_string());
        output.stop_reason = map_stop_reason(finish_reason)?;
        // 有工具调用但服务端给 STOP 时改判 ToolUse（适配器以它决定 DoneReason）。
        if output.stop_reason == StopReason::Stop
            && output
                .content
                .iter()
                .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
        {
            output.stop_reason = StopReason::ToolUse;
        }
    }

    // usageMetadata 是累计值，直接覆盖；非对象（null、数组）忽略。
    if let Some(usage) = chunk.get("usageMetadata").filter(|value| value.is_object()) {
        let prompt = json_number(usage.get("promptTokenCount"));
        let cached = json_number(usage.get("cachedContentTokenCount"));
        let candidates = json_number(usage.get("candidatesTokenCount"));
        let thoughts = json_number(usage.get("thoughtsTokenCount"));
        // TS 保留原始 JS 数字：`input = (promptTokenCount || 0) - (cachedContentTokenCount || 0)`
        // 可以是负数或小数。`Usage.input` 是冻死的 `u64`（ARCHITECTURE §4.9），因此负值饱和到
        // 0、小数被截断（f64→u64 的 `as` 是饱和转换，不会 panic）。这是类型层面的被迫偏差。
        output.usage = crate::types::Usage {
            input: (prompt - cached) as u64,
            output: (candidates + thoughts) as u64,
            cache_read: cached as u64,
            cache_write: 0,
            cache_write1h: None,
            reasoning: Some(thoughts as u64),
            total_tokens: json_number(usage.get("totalTokenCount")) as u64,
            cost: crate::types::UsageCost::default(),
        };
        crate::models::calculate_cost(model, &mut output.usage);
    }
    Ok(())
}

/// `json_number`（TS 的 `|| 0`）。
///
/// 缺失、`null`、非数字、`NaN`、`Infinity` 都返回 0；负数原样保留（JS 里负数是真值），
/// 后续转 `u64` 时按饱和转换变成 0。
fn json_number(value: Option<&Value>) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(number) if number.is_finite() => number,
        _ => 0.0,
    }
}

/// 结束当前块并 push 对应的 end 事件。
///
/// `kind` 必须对应 `output.content` 的**最后一个**块（由调用方保证，见
/// [`GoogleStreamState`] 的不变量）：下标固定取 `content.len() - 1`（空内容时饱和到 0）。
/// 如果最后一个块类型与 `kind` 不匹配，`content` 字段退化为空串而不是 panic。
///
/// 本函数只发事件、不修改 `output`；把 `state.current_block` 置空是调用方的事。
pub fn finish_google_block(
    kind: GoogleBlockKind,
    output: &crate::types::AssistantMessage,
    stream: &crate::event_stream::AssistantMessageEventStream,
) {
    use crate::types::{AssistantContentBlock, AssistantMessageEvent};
    let index = output.content.len().saturating_sub(1);
    match kind {
        GoogleBlockKind::Text => {
            let content = match output.content.get(index) {
                Some(AssistantContentBlock::Text(text)) => text.text.clone(),
                _ => String::new(),
            };
            let _ = stream.push(AssistantMessageEvent::TextEnd {
                content_index: index,
                content,
                partial: Box::new(output.clone()),
            });
        }
        GoogleBlockKind::Thinking => {
            let content = match output.content.get(index) {
                Some(AssistantContentBlock::Thinking(thinking)) => thinking.thinking.clone(),
                _ => String::new(),
            };
            let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index: index,
                content,
                partial: Box::new(output.clone()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    //! 模块内单测：锁定错误体形状（JSON/非 JSON/无 status）、wire body 的键序与
    //! 模式差异、Vertex URL 拼接、请求级 httpOptions 的覆盖，以及 RS256 的 OpenSSL
    //! 已知答案。跨模块的 convertMessages/tools/retry 用例在 `tests/google_shared_*.rs`。

    use super::*;
    use serde_json::json;

    #[test]
    fn thinking_level_detection_checks_every_model_id_occurrence() {
        let uses = |id: &str| {
            uses_google_thinking_level(&Model {
                id: id.into(),
                ..Model::default()
            })
        };
        // 第一个 `gemini-` 是旧版本，后面才是 4.x：不能只看第一次出现。
        assert!(uses("gemini-2.5-gemini-4-pro"));
        assert!(uses("prefix-gemini-3-flash"));
        assert!(uses("gemini-2.5-gemma-4-x"));
        assert!(!uses("gemini-2.5-pro"));
    }

    #[test]
    fn error_body_is_only_json_parsed_when_the_content_type_says_json() {
        // `content-type: application/json`：错误体就是解析后的 JSON（重新 stringify）。
        let failure = throw_error_if_not_ok(
            400,
            Some("application/json; charset=utf-8"),
            r#"{"error":{"message":"bad","code":400,"status":"INVALID_ARGUMENT"}}"#,
        );
        assert_eq!(failure.status, Some(400));
        assert_eq!(
            failure.message,
            r#"{"error":{"message":"bad","code":400,"status":"INVALID_ARGUMENT"}}"#
        );

        // 非 JSON content-type：原始文本被包成 `{error:{message, code, status}}`。
        let failure = throw_error_if_not_ok(502, Some("text/html"), "<html>bad gateway</html>");
        assert_eq!(failure.status, Some(502));
        assert_eq!(
            failure.message,
            r#"{"error":{"message":"<html>bad gateway</html>","code":502,"status":"Bad Gateway"}}"#
        );

        // 没有 content-type 也走文本包装。
        let failure = throw_error_if_not_ok(503, None, "busy");
        assert_eq!(
            failure.message,
            r#"{"error":{"message":"busy","code":503,"status":"Service Unavailable"}}"#
        );
    }

    #[test]
    fn invalid_json_with_a_json_content_type_surfaces_the_parse_error_without_a_status() {
        let failure = throw_error_if_not_ok(500, Some("application/json"), "not json");
        assert_eq!(failure.status, None);
        assert_eq!(
            failure.message,
            "Unexpected token 'o', \"not json\" is not valid JSON"
        );
    }

    #[test]
    fn non_4xx_5xx_statuses_do_not_create_an_api_error() {
        let failure = throw_error_if_not_ok(304, Some("text/plain"), "");
        assert_eq!(failure.status, None);
        assert_eq!(
            failure.message,
            r#"{"error":{"message":"","code":304,"status":"Not Modified"}}"#
        );
    }

    #[test]
    fn stream_error_templates_a_missing_status_as_undefined() {
        let failure = google_chunk_error(&json!({
            "error": {"code": 500, "message": "boom"}
        }))
        .expect("failure");
        assert_eq!(failure.status, Some(500));
        assert_eq!(
            failure.message,
            r#"got status: undefined. {"error":{"code":500,"message":"boom"}}"#
        );
    }

    fn vertex_client() -> GoogleClientConfig {
        GoogleClientConfig {
            vertexai: true,
            project: Some("p".into()),
            location: Some("us-central1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn mldev_wire_body_serializes_parts_in_sdk_key_order() {
        let params = json!({
            "model": "gemini-3-flash-preview",
            "contents": [{
                "role": "model",
                "parts": [
                    {"thought": true, "text": "t", "thoughtSignature": "sig"},
                    {"functionCall": {"name": "f", "args": {"a": 1}, "id": "x"}},
                    {"inlineData": {"mimeType": "image/png", "data": "AA=="}}
                ]
            }],
            "config": {}
        });
        let client = GoogleClientConfig {
            vertexai: false,
            api_key: Some("k".into()),
            ..Default::default()
        };
        let body = build_wire_body(&params, false, &client).expect("body");
        assert_eq!(
            kiana_json::js_object::stringify(&body),
            concat!(
                r#"{"contents":[{"parts":["#,
                r#"{"text":"t","thought":true,"thoughtSignature":"sig"},"#,
                r#"{"functionCall":{"args":{"a":1},"id":"x","name":"f"}},"#,
                r#"{"inlineData":{"data":"AA==","mimeType":"image/png"}}"#,
                r#"],"role":"model"}],"generationConfig":{}}"#
            )
        );
    }

    #[test]
    fn vertex_wire_body_keeps_passthrough_part_objects() {
        let params = json!({
            "model": "gemini-3-flash-preview",
            "contents": [{
                "role": "model",
                "parts": [
                    {"thought": true, "text": "t", "thoughtSignature": "sig"},
                    {"functionCall": {"name": "f", "args": {"a": 1}, "id": "x"}},
                    {"inlineData": {"mimeType": "image/png", "data": "AA=="}}
                ]
            }],
            "config": {}
        });
        let body = build_wire_body(&params, true, &vertex_client()).expect("body");
        assert_eq!(
            kiana_json::js_object::stringify(&body),
            concat!(
                r#"{"contents":[{"parts":["#,
                r#"{"text":"t","thought":true,"thoughtSignature":"sig"},"#,
                r#"{"functionCall":{"name":"f","args":{"a":1},"id":"x"}},"#,
                r#"{"inlineData":{"mimeType":"image/png","data":"AA=="}}"#,
                r#"],"role":"model"}],"generationConfig":{}}"#
            )
        );
    }

    #[test]
    fn wire_body_maps_every_generate_content_config_field_from_on_payload() {
        let params = json!({
            "model": "gemini-3-flash-preview",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "config": {
                "temperature": 0.5,
                "topP": 0.9,
                "topK": 40,
                "candidateCount": 2,
                "maxOutputTokens": 100,
                "stopSequences": ["x"],
                "responseLogprobs": true,
                "logprobs": 3,
                "presencePenalty": 0.1,
                "frequencyPenalty": 0.2,
                "seed": 7,
                "responseMimeType": "application/json",
                "responseJsonSchema": {"type": "object"},
                "safetySettings": [{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}],
                "responseModalities": ["TEXT"],
                "mediaResolution": "MEDIA_RESOLUTION_HIGH",
                "speechConfig": "Kore",
                "audioTranscriptionConfig": {"languageCodes": ["en-US"]},
                "imageConfig": {"aspectRatio": "1:1"},
                "serviceTier": "flex",
                "labels": {"team": "ai"},
                "cachedContent": "cache-1",
                "thinkingConfig": {"includeThoughts": true}
            }
        });
        let body = build_wire_body(&params, true, &vertex_client()).expect("body");
        let generation = &body["generationConfig"];
        assert_eq!(generation["temperature"], json!(0.5));
        assert_eq!(generation["topP"], json!(0.9));
        assert_eq!(generation["topK"], json!(40));
        assert_eq!(generation["candidateCount"], json!(2));
        assert_eq!(generation["maxOutputTokens"], json!(100));
        assert_eq!(generation["stopSequences"], json!(["x"]));
        assert_eq!(generation["responseLogprobs"], json!(true));
        assert_eq!(generation["logprobs"], json!(3));
        assert_eq!(generation["presencePenalty"], json!(0.1));
        assert_eq!(generation["frequencyPenalty"], json!(0.2));
        assert_eq!(generation["seed"], json!(7));
        assert_eq!(generation["responseMimeType"], json!("application/json"));
        assert_eq!(generation["responseJsonSchema"], json!({"type": "object"}));
        assert_eq!(generation["responseModalities"], json!(["TEXT"]));
        assert_eq!(
            generation["mediaResolution"],
            json!("MEDIA_RESOLUTION_HIGH")
        );
        assert_eq!(
            generation["speechConfig"],
            json!({"voiceConfig": {"prebuiltVoiceConfig": {"voiceName": "Kore"}}})
        );
        assert_eq!(
            generation["audioTranscriptionConfig"],
            json!({"languageCodes": ["en-US"]})
        );
        assert_eq!(generation["imageConfig"], json!({"aspectRatio": "1:1"}));
        assert_eq!(
            generation["thinkingConfig"],
            json!({"includeThoughts": true})
        );
        assert_eq!(body["serviceTier"], json!("flex"));
        assert_eq!(body["labels"], json!({"team": "ai"}));
        // Vertex: 纯名字会被补成 projects/<project>/locations/<location>/cachedContents/<name>。
        assert_eq!(
            body["cachedContent"],
            json!("projects/p/locations/us-central1/cachedContents/cache-1")
        );
        assert_eq!(
            body["safetySettings"],
            json!([{"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}])
        );
    }

    #[test]
    fn wire_body_rejects_fields_the_sdk_rejects_per_mode() {
        let gemini = GoogleClientConfig {
            vertexai: false,
            api_key: Some("k".into()),
            ..Default::default()
        };
        let params = json!({
            "model": "m",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "config": {"labels": {}}
        });
        assert_eq!(
            build_wire_body(&params, false, &gemini)
                .expect_err("labels is Vertex-only")
                .message,
            "labels parameter is only supported in Gemini Enterprise Agent Platform mode, not in Gemini Developer API mode."
        );

        let params = json!({
            "model": "m",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "config": {"enableEnhancedCivicAnswers": true}
        });
        assert_eq!(
            build_wire_body(&params, true, &vertex_client())
                .expect_err("gemini-only field")
                .message,
            "enableEnhancedCivicAnswers parameter is only supported in Gemini Developer API mode, not in Gemini Enterprise Agent Platform mode."
        );
    }

    #[test]
    fn vertex_model_path_matches_tmodel() {
        let client = vertex_client();
        // 已带 projects/ 前缀：不再重复拼 project/location，`split('/', 2)` 截断。
        assert_eq!(
            client
                .stream_url("projects/p2/locations/l2/endpoints/e")
                .expect("url")
                .as_str(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p2/locations/l2/endpoints/e:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            client.stream_url("a/b/c").expect("url").as_str(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1/publishers/a/models/b:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            client
                .stream_url("gemini-3-flash-preview")
                .expect("url")
                .as_str(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1/publishers/google/models/gemini-3-flash-preview:streamGenerateContent?alt=sse"
        );
        for invalid in ["", "a?b", "a&b", "a..b"] {
            assert_eq!(
                client.stream_url(invalid).expect_err("invalid").message,
                if invalid.is_empty() {
                    "model is required and must be a string"
                } else {
                    "invalid model parameter"
                },
                "{invalid}"
            );
        }
    }

    struct CaptureClient {
        requests: std::sync::Mutex<Vec<crate::http::HttpRequest>>,
    }

    impl crate::http::HttpClient for CaptureClient {
        fn execute<'a>(
            &'a self,
            request: crate::http::HttpRequest,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
        ) -> crate::http::BoxFuture<'a, Result<crate::http::HttpResponse, crate::http::HttpError>>
        {
            self.requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request);
            Box::pin(async move {
                let body: crate::http::ByteStream = Box::pin(futures::stream::empty());
                Ok(crate::http::HttpResponse {
                    response: crate::types::ProviderResponse {
                        status: 200,
                        headers: Default::default(),
                    },
                    body,
                })
            })
        }
    }

    #[tokio::test]
    async fn request_stream_uses_params_model_and_request_http_options() {
        use std::sync::Arc;

        let client = GoogleClientConfig {
            vertexai: false,
            api_key: Some("key".into()),
            ..Default::default()
        };
        let capture = Arc::new(CaptureClient {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let params = json!({
            "model": "on-payload-model",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "config": {
                "httpOptions": {
                    "headers": {"x-test": "1"},
                    "timeout": 1500,
                    "extraBody": {"foo": {"bar": 2}}
                }
            }
        });
        let options = crate::types::StreamOptions {
            api_key: Some("key".into()),
            fetch: Some(capture.clone()),
            ..Default::default()
        };
        let _stream = request_stream(&client, &params, &options)
            .await
            .expect("stream");
        let requests = capture
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let request = &requests[0];
        assert_eq!(
            request.url,
            "https://generativelanguage.googleapis.com/v1beta/models/on-payload-model:streamGenerateContent?alt=sse"
        );
        assert_eq!(request.timeout_ms, Some(1500));
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "x-test" && value == "1")
        );
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "x-goog-api-key" && value == "key")
        );
        assert!(request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-server-timeout") && value == "2"
        }));
        let body = String::from_utf8(request.body.clone().expect("body")).expect("utf8");
        assert!(body.contains(r#""foo":{"bar":2}"#), "{body}");
        assert!(
            body.contains(r#""contents":[{"parts":[{"text":"hi"}],"role":"user"}]"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn request_stream_honors_request_base_url_override() {
        use std::sync::Arc;

        let client = GoogleClientConfig {
            vertexai: true,
            api_key: Some("key".into()),
            project: Some("p".into()),
            location: Some("us-central1".into()),
            ..Default::default()
        };
        let capture = Arc::new(CaptureClient {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let params = json!({
            "model": "projects/p2/locations/l2/endpoints/e",
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
            "config": {"httpOptions": {"baseUrl": "https://proxy.example.com"}}
        });
        let options = crate::types::StreamOptions {
            api_key: Some("key".into()),
            fetch: Some(capture.clone()),
            ..Default::default()
        };
        let _stream = request_stream(&client, &params, &options)
            .await
            .expect("stream");
        let requests = capture
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            requests[0].url,
            "https://proxy.example.com/v1/projects/p2/locations/l2/endpoints/e:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn stream_error_coerces_string_codes_like_the_sdk_relational_comparison() {
        let failure = google_chunk_error(&json!({
            "error": {"code": "503", "status": "UNAVAILABLE"}
        }))
        .expect("failure");
        assert_eq!(failure.status, Some(503));
        assert!(failure.message.starts_with("got status: UNAVAILABLE. "));

        assert!(google_chunk_error(&json!({"error": {"code": "oops"}})).is_none());
        assert!(google_chunk_error(&json!({"error": {"code": 399}})).is_none());
    }

    #[test]
    fn rsa_sha256_signature_matches_the_openssl_known_answer() {
        // `openssl dgst -sha256 -sign` 对同一条消息的输出（RSASSA-PKCS1-v1_5 是确定性的）。
        let key = include_str!("../../tests/fixtures/api-other/google_adc_service_account_key.pem");
        let signature = super::rsa_sha256_sign(key, b"hello").expect("signature");
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature),
            "IH9yjy-65xyUfy5goPPgDH7sM-GSug_T2d07ASBZ93MrhrtGxJugPVSjtn2tKJ2uW7kReRkoOYVDvoIKHeOqDDm79MTX8B7HKKtSh2Lezv8SLkIZ6uxUj1N8CJ4vB_lbXPkmhky8ZHsoSOZfaNFsS0pIk68ulgG1yg9vZPYKtbcp9yfmjA4j5L8rChuhkVx-FT8kU6Xmi-zQ7WYHKrprXtQDxVjbZ87dKlnlZzfy-GKt5WxOnLXvVoHRGkIpyB1wVY12YJBG9x6IMQmRLwcw_6fz2j4PsiVASgvGpb0m53wC8nBYo0uyibfvryeRHvvEmyT1s8ttbTzTyyCJXht2oQ"
        );
    }
}
