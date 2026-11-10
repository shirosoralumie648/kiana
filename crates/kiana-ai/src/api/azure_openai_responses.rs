//! api/azure-openai-responses.ts。
//!
//! Azure OpenAI Responses 适配器：端点解析（`azure-openai-config.ts`）、`api-key` 鉴权、
//! deployment 名替换与 Responses 协议转换。与 `openai-responses.ts` 的差异逐条见
//! `spec/ai-api-anthropic-openai.md` §2.13。
//!
//! # 这个文件负责什么
//!
//! 把统一上下文 [`TranscriptContext`] 变成 Azure OpenAI Responses 请求，再把 SSE 事件流
//! 转回 [`AssistantMessageEventStream`]。协议转换、工具转换与流式状态机已在
//! `api::openai_responses_shared` 实现；本文件只保留 Azure 专属差异：
//!
//! - 用 `api-key` header 而不是 `Authorization: Bearer`；
//! - 请求 URL 在 base URL 后追加 `/responses`，并带 `api-version` query（合并规则见
//!   `azure_url`）；
//! - 请求体的 `model` 字段发送 deployment 名，`model.id` 只用于目录与输出记录；
//! - Azure SDK 在带 `api-key` 时使用 `redirect: "manual"`，因此未注入 `options.fetch`
//!   时默认 HTTP client 不跟随重定向；
//! - 没有 `serviceTier` 选项，流处理也不传 service tier 相关回调（与 `openai-responses`
//!   不同）。
//!
//! # 调用方
//!
//! - `providers::azure::azure_provider` 把本模块注册为 `azure-openai-responses` 的实现；
//! - `compat` / `legacy_api_aliases` 的 `azureOpenAIResponsesApi()` 别名；
//! - 测试直接调用 [`stream`] / [`stream_simple`]。
//!
//! # 一次请求的执行流程
//!
//! 1. [`stream`] 在调用线程同步执行 `resolveTranscript`（按
//!    `supportsMidConvoSystemMessages` 折叠中途 system 消息），然后把后续工作交给
//!    `spawn_forward` 的后台任务；
//! 2. 后台任务解析 deployment 名、构造 `AssistantMessage`，调用 `run_azure`；
//! 3. `run_azure_inner`：校验 API key → 解析端点 → 构造 headers 与请求体 → 调用
//!    `on_payload` → `POST` → `request_with_retry` → `on_response` → 推送 `start`；
//! 4. `process_responses_stream` 把 SSE 事件写入 `output` 并向流推送增量事件；
//! 5. 终态检查（取消、`pending`、`aborted`/`error`）后推送 `done` 并结束流。
//!
//! 任意阶段失败都会走到 `run_azure` 的收尾：把 `output.stop_reason` 设为 aborted/error、
//! 用 `format_thrown_error` 生成 `error_message`、推送 `error` 事件并 `end`。因此
//! [`stream`] 本身不会因端点/鉴权/网络错误返回 `Err`——错误只出现在流事件里。
//!
//! # 错误与取消
//!
//! - 唯一同步返回 `Err` 的入口是 [`stream_simple`]：API key 缺失或为空时返回
//!   `No API key for provider: <provider>`（`AiErrorKind::Auth`），对应 TS 的同步 throw；
//! - 取消通过 `StreamOptions::signal` 传入：重试层在两次尝试之间、SSE 解码层在传输错误
//!   时都会检查；流结束后若 signal 已取消，错误文本是 `"Request was aborted"`，
//!   `stop_reason` 为 `Aborted`；
//! - 错误文本由 `format_thrown_error` 生成，前缀参数是 `"Azure OpenAI API error"`（与 TS
//!   的 `formatAzureOpenAIError` 一致）；有状态码时前缀总会出现（空 body 时形如
//!   `Azure OpenAI API error (307): 307 status code (no body)`），只有连状态码都没有的错误
//!   （连接失败、取消等）才不带前缀；`last_failure` 只用于还原最后一次 HTTP 失败的状态码
//!   与响应体，避免取消时把陈旧失败当成当前错误。
//!
//! # 注意事项
//!
//! - 返回的流是一次性的：后台任务只写一次 `done`/`error` 并 `end`；`push` 的返回值被
//!   忽略（`let _ =`），`done` 之后的推送不会再产生事件；
//! - `output.api` 固定为字面量 `"azure-openai-responses"`，不是 `model.api`；
//! - 请求体与 TS 逐字节对齐（fixture `tests/fixtures/api-openai/azure_request.json`）：
//!   `store: false` 与字段集合/顺序由该 fixture 锁定；`supportsStrictMode` 与
//!   `max_output_tokens` 下限在该 fixture 中被显式 `true` 和较大的 `maxTokens`（1234）
//!   绕过，并非由它锁定。

// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定（`Display` 必须等于 TS 的
// `error.message`），因此结果类型只按文档要求携带它。
#![allow(clippy::result_large_err)]
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::api::azure_openai_config::{
    AzureEndpointOptions, resolve_azure_config, resolve_deployment_name,
};
use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::lazy::{lazy_api, lazy_stream_error, spawn_forward};
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, HeaderTable,
    OpenAIResponsesStreamOptions, RequestFailure, build_url, convert_responses_messages,
    convert_responses_tools, default_http_client, ensure_response_body, format_thrown_error,
    get_responses_tool_support, header_list, js_string, merge_plain_headers,
    merge_provider_headers, now_ms, process_responses_stream, provider_event_stream,
    provider_response, request_with_retry, set_header, truthy,
};
use crate::api::simple_options::{build_base_options, resolve_sampling_params};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::HttpRequest;
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantMessage, DoneReason, ErrorReason, Model, ModelThinkingLevel, SimpleStreamOptions,
    StopReason, StreamOptions, ThinkingLevel, TranscriptContext,
};
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::transcript::{get_declared_tools, resolve_transcript, resolve_transcript_tools};

/// `AZURE_TOOL_CALL_PROVIDERS`。
///
/// 传给 `convert_responses_messages` 的允许集合：只有目标模型 provider 在这个集合里时，
/// 历史工具调用 id 才按 Responses 的 `call_id|item_id` 两段式解析；否则整串按普通 id
/// 净化。新增 provider 时需确认是否应加入，否则历史工具调用回放格式会退化。
pub const AZURE_TOOL_CALL_PROVIDERS: [&str; 4] = ["openai", "openai-codex", "opencode", "azure"];
/// `OPENAI_RESPONSES_MIN_OUTPUT_TOKENS`。
///
/// `max_output_tokens` 的下限：调用方给了非零 `max_tokens` 时取
/// `max(max_tokens, 16)`，与 TS 的 `Math.max(options.maxTokens, 16)` 一致。
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// `AzureOpenAIResponsesOptions`（TS：`AzureEndpointOptions` 的扩展）。
///
/// Rust 用一个扁平 struct 表达 TS 的三层继承：`base` 承载全部 [`StreamOptions`] 字段
/// （网络、重试、回调、`fetch`、`env` 等），其余字段是 Azure 端点选项与 Responses 专属
/// 的 `reasoningEffort`/`toolChoice`/`reasoningSummary`。
///
/// 通常由 [`AzureOpenAIResponsesOptions::from_stream_options`] 从通用
/// [`StreamOptions`] 构造（`Models` 层的调用路径），也可以直接构造。
#[derive(Default)]
pub struct AzureOpenAIResponsesOptions {
    /// 全部 [`StreamOptions`] 字段。
    ///
    /// `api_key`/`env`/`signal`/`on_payload` 等都从这里读取。
    pub base: StreamOptions,
    /// `azureApiVersion`。
    ///
    /// 显式 API 版本；空串视为未设置，默认 `v1`。
    pub azure_api_version: Option<String>,
    /// `azureResourceName`。
    ///
    /// Azure 资源名；用于拼 `https://<name>.openai.azure.com/openai/v1`，空串视为未设置。
    pub azure_resource_name: Option<String>,
    /// `azureBaseUrl`。
    ///
    /// 完整 base URL；空串/纯空白视为未设置，优先级高于环境变量与 resource name。
    pub azure_base_url: Option<String>,
    /// `azureDeploymentName`。
    ///
    /// 显式 deployment 名；非空时直接作为请求的 `model` 字段，空串视为未设置。
    pub azure_deployment_name: Option<String>,
    /// `reasoningEffort`。
    ///
    /// 思考强度；模型支持推理时写入 `reasoning.effort`，并参与 sampling params 的档位
    /// 选择。`None` 不等于 `off`：若同时给了 `reasoningSummary`，会推导出 `"medium"`。
    pub reasoning_effort: Option<ThinkingLevel>,
    /// `toolChoice`。
    ///
    /// 原样写入请求体的 `tool_choice`；JSON 形状由调用方负责（对应 TS 里 SDK 的联合类型）。
    pub tool_choice: Option<Value>,
    /// `reasoningSummary`。
    ///
    /// truthy 时按 JS `String(value)` 写入 `reasoning.summary`；`null`/空串等假值视为
    /// 未设置。
    pub reasoning_summary: Option<Value>,
}

/// 等价于 [`AzureOpenAIResponsesOptions::from_stream_options`]：把通用选项提升为 Azure 选项。
impl From<StreamOptions> for AzureOpenAIResponsesOptions {
    fn from(base: StreamOptions) -> Self {
        AzureOpenAIResponsesOptions::from_stream_options(base)
    }
}

impl AzureOpenAIResponsesOptions {
    /// 从 [`StreamOptions`] 的 `extra` 读取 API 专属字段。
    ///
    /// `Models` 层只透传 `extra`（键为 TS 字段名），因此这里按 camelCase 键逐个提取：
    /// `azureApiVersion` / `azureResourceName` / `azureBaseUrl` / `azureDeploymentName`
    /// 必须是字符串，否则视为未设置；`reasoningEffort` 反序列化为 [`ThinkingLevel`]，
    /// 非法值同样静默丢弃；`toolChoice` / `reasoningSummary` 按 JSON 原样克隆。缺失的键
    /// 全部为 `None`。
    ///
    /// 返回的 `base` 就是传入的 [`StreamOptions`]（`extra` 保留，但后续不再读取）。
    pub fn from_stream_options(base: StreamOptions) -> Self {
        let azure_api_version = base
            .extra
            .get("azureApiVersion")
            .and_then(Value::as_str)
            .map(str::to_string);
        let azure_resource_name = base
            .extra
            .get("azureResourceName")
            .and_then(Value::as_str)
            .map(str::to_string);
        let azure_base_url = base
            .extra
            .get("azureBaseUrl")
            .and_then(Value::as_str)
            .map(str::to_string);
        let azure_deployment_name = base
            .extra
            .get("azureDeploymentName")
            .and_then(Value::as_str)
            .map(str::to_string);
        let reasoning_effort = base
            .extra
            .get("reasoningEffort")
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        let tool_choice = base.extra.get("toolChoice").cloned();
        let reasoning_summary = base.extra.get("reasoningSummary").cloned();
        AzureOpenAIResponsesOptions {
            base,
            azure_api_version,
            azure_resource_name,
            azure_base_url,
            azure_deployment_name,
            reasoning_effort,
            tool_choice,
            reasoning_summary,
        }
    }

    /// 解析函数只读 `env`；`StreamOptions` 不可 Clone，因此为它构造一份最小副本。
    fn endpoint_options(&self) -> AzureEndpointOptions {
        AzureEndpointOptions {
            base: StreamOptions {
                env: self.base.env.clone(),
                ..StreamOptions::default()
            },
            azure_api_version: self.azure_api_version.clone(),
            azure_resource_name: self.azure_resource_name.clone(),
            azure_base_url: self.azure_base_url.clone(),
            azure_deployment_name: self.azure_deployment_name.clone(),
        }
    }
}

/// 原始 `model.compat` 上的布尔字段（`?? default` 语义）。
///
/// `compat` 是未类型化的 JSON（不同 api 有不同 schema）：字段缺失、不是对象或值不是
/// bool 都返回 `None`，由调用方用 `unwrap_or(default)` 表达 TS 的 `??`。
fn compat_bool(model: &Model, key: &str) -> Option<bool> {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.get(key))
        .and_then(Value::as_bool)
}

fn to_model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// `ModelThinkingLevel` → `Option<ThinkingLevel>`。
///
/// [`ThinkingLevel`] 没有 `Off` 档，因此 `Off` 映射为 `None`；这正好复刻 TS
/// `clampedReasoning === "off" ? undefined : clampedReasoning`（`stream_simple` 依赖它）。
fn from_model_thinking_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    Some(match level {
        ModelThinkingLevel::Off => return None,
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ModelThinkingLevel::Max => ThinkingLevel::Max,
    })
}

/// 请求体 `reasoning.effort` 的字符串形式（与 TS 的字面量一致，`xhigh` 全小写）。
fn thinking_level_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// `model.thinkingLevelMap?.[level]`：`None` = 键不存在；`Some(None)` = `null`。
fn mapped_thinking_level(model: &Model, level: ModelThinkingLevel) -> Option<Option<String>> {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .cloned()
}

/// `model.thinkingLevelMap?.off`。
///
/// `Some(None)` 对应 JSON `null`：此时不写 `reasoning` 字段（TS 用 `!== null` 守卫）；
/// `None`（键缺失）则回落 `"none"`；`Some(Some(text))` 使用模型给的自定义值。
fn mapped_off(model: &Model) -> Option<Option<String>> {
    mapped_thinking_level(model, ModelThinkingLevel::Off)
}

/// 把内部 [`StopReason`] 收敛为 `done` 事件允许的 [`DoneReason`]。
///
/// `Error` / `Aborted` 不会走到这里（`run_azure_inner` 在推送 `done` 前已提前返回），
/// 其余未列出的值统一按 `Stop` 处理。
fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// `buildParams(...)`。
///
/// 构造 Responses 请求体（`serde_json::Map`，插入顺序即序列化顺序；黄金测试按
/// `js_stringify` 逐字节比较）。输入是模型 compat 开关、上下文、选项、已解析的 deployment
/// 名与 grammar 工具的输入属性映射。
///
/// 关键契约：
///
/// - `model` 写 deployment 名而不是 `model.id`；`stream` 恒为 true；`store` 恒为 false；
/// - `prompt_cache_key` 只由 `session_id` 决定且总是经过 clamp（与 openai-responses
///   不同，不看 `cacheRetention`）；`Some("")` 仍会写成空串，只有 `None` 才省略字段；
/// - `max_output_tokens` 仅在 `max_tokens` 非 0 时出现，且不小于 16；
/// - `supportsStrictMode` 缺省为 **true**（`openai-responses` 适配器缺省 false）；
/// - reasoning 相关字段只在 `model.reasoning == Some(true)` 时出现，详见下方分支注释；
/// - sampling params 最后合并，同名键会覆盖前面写入的字段。
///
/// `on_payload` 回调可以在发送前整体替换这个对象，因此上述契约只在回调之前成立。
///
/// # 错误
///
/// 消息/工具转换与 grammar 解析可能失败（约束采样不满足、thinking 签名损坏等），错误
/// 原样返回，由调用方转成流上的 error 事件。
fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: &AzureOpenAIResponsesOptions,
    deployment_name: &str,
    grammar_tool_input_properties: &IndexMap<String, String>,
) -> Result<Value, AiError> {
    // 每个开关独立读取：缺失或非 bool 都按下面的默认值处理（TS 的 `??`）。
    let supports_additional_tools = compat_bool(model, "supportsAdditionalTools").unwrap_or(false);
    let supports_tool_search = compat_bool(model, "supportsToolSearch").unwrap_or(false);
    let supports_mid_convo_system_messages =
        compat_bool(model, "supportsMidConvoSystemMessages").unwrap_or(false);
    let supports_strict_mode = compat_bool(model, "supportsStrictMode").unwrap_or(true);
    let supports_openai_grammar_tools =
        compat_bool(model, "supportsOpenAIGrammarTools").unwrap_or(false);

    // 先固定本次请求的能力开关；get_responses_tool_support 会把它压缩成工具变更策略。
    let convert_options = ConvertResponsesMessagesOptions {
        include_system_prompt: None,
        grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        supports_mid_convo_system_messages: Some(supports_mid_convo_system_messages),
        supports_additional_tools: Some(supports_additional_tools),
        supports_tool_search: Some(supports_tool_search),
        tool_options: Some(ConvertResponsesToolsOptions {
            strict: None,
            supports_strict_mode: Some(supports_strict_mode),
            supports_openai_grammar_tools: Some(supports_openai_grammar_tools),
            tool_search_result: false,
        }),
        configuration_update_effort: None,
    };
    let tool_support = get_responses_tool_support(&convert_options);
    // 顶层工具列表决定 request.tools；历史工具变更由 convert_responses_messages 编码。
    let transcript_tools = resolve_transcript_tools(&context.as_context().messages, tool_support);
    let messages = convert_responses_messages(
        model,
        context,
        &AZURE_TOOL_CALL_PROVIDERS,
        Some(&convert_options),
    )?;

    // 固定字段：deployment 名、输入项、流式、禁止服务端存储。
    let mut params: Map<String, Value> = Map::new();
    params.insert("model".into(), Value::String(deployment_name.to_string()));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), Value::Bool(true));
    // sessionId 为 `None` 时不写；`Some("")` 会按 clamp 契约写入空串。
    if let Some(key) = clamp_openai_prompt_cache_key(options.base.session_id.as_deref()) {
        params.insert("prompt_cache_key".into(), Value::String(key));
    }
    params.insert("store".into(), Value::Bool(false));

    // 0 与 None 都表示未设置；有值时抬到 16 的下限（TS 的 Math.max）。
    if let Some(max_tokens) = options.base.max_tokens.filter(|value| *value != 0) {
        params.insert(
            "max_output_tokens".into(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }
    // temperature 用 `Some` 判断：0.0 也是有效值，不能当 falsy 跳过。
    if let Some(temperature) = options.base.temperature {
        params.insert("temperature".into(), Value::from(temperature));
    }
    // 空工具列表不写字段；转换失败（约束采样）在这里返回错误。
    if !transcript_tools.request_tools.is_empty() {
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                &transcript_tools.request_tools,
                Some(&ConvertResponsesToolsOptions {
                    strict: None,
                    supports_strict_mode: Some(supports_strict_mode),
                    supports_openai_grammar_tools: Some(supports_openai_grammar_tools),
                    tool_search_result: false,
                }),
            )?),
        );
    }
    // `Some(Value::Null)` 也会写入，复刻 TS 的 `!== undefined`。
    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".into(), tool_choice.clone());
    }

    // reasoningEffort 的来源：显式档位，或仅有 reasoningSummary 时推导为 "medium"
    // （TS 的 `?? (reasoningSummary ? "medium" : undefined)`）。
    let reasoning_summary = options
        .reasoning_summary
        .as_ref()
        .filter(|value| truthy(Some(value)));
    let reasoning_effort_raw = options.reasoning_effort;
    let reasoning_effort = reasoning_effort_raw
        .map(|level| thinking_level_str(level).to_string())
        .or_else(|| reasoning_summary.map(|_| "medium".to_string()));
    // 只有 reasoning 模型才写 reasoning/include；非 reasoning 模型即使给了选项也忽略。
    if model.reasoning == Some(true) {
        if let Some(effort) = &reasoning_effort {
            // thinkingLevelMap 的值优先；`Some(None)`（JSON null）与键缺失一样回落到档位
            // 名，复刻 `??` 的 nullish 语义。summary 缺省 "auto"，假值已被上面的 truthy
            // 过滤。
            let effort = match reasoning_effort_raw {
                Some(level) => mapped_thinking_level(model, to_model_thinking_level(level))
                    .flatten()
                    .unwrap_or_else(|| thinking_level_str(level).to_string()),
                None => effort.clone(),
            };
            let summary = match reasoning_summary {
                Some(value) => js_string(Some(value)),
                None => "auto".to_string(),
            };
            let mut reasoning = Map::new();
            reasoning.insert("effort".into(), Value::String(effort));
            reasoning.insert("summary".into(), Value::String(summary));
            params.insert("reasoning".into(), Value::Object(reasoning));
            params.insert(
                "include".into(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".into())]),
            );
        } else if mapped_off(model) != Some(None) {
            // 无显式 effort：thinkingLevelMap.off 为 null 时按 TS 的 `!== null` 守卫完全
            // 跳过 reasoning 字段；键缺失则写 `{ effort: "none" }`。
            let effort = mapped_off(model)
                .flatten()
                .unwrap_or_else(|| "none".to_string());
            let mut reasoning = Map::new();
            reasoning.insert("effort".into(), Value::String(effort));
            params.insert("reasoning".into(), Value::Object(reasoning));
        }
    }

    // 档位选择与 reasoning 字段一致：显式档位 → summary 推导的 medium → off。
    let thinking_level = reasoning_effort_raw
        .map(to_model_thinking_level)
        .or_else(|| reasoning_summary.map(|_| ModelThinkingLevel::Medium))
        .unwrap_or(ModelThinkingLevel::Off);
    // samplingParams 最后 Object.assign；同名键覆盖前面的字段（例如自定义 temperature）。
    let sampling_params =
        resolve_sampling_params(model, thinking_level, options.base.sampling_params.as_ref());
    if let Some(sampling_params) = sampling_params {
        for (key, value) in sampling_params {
            params.insert(key, value);
        }
    }
    Ok(Value::Object(params))
}

// ---------------------------------------------------------------------------
// header
// ---------------------------------------------------------------------------

/// `createClient(...)` 的 header 部分（Azure SDK 用 `api-key` 而不是 Bearer）。
///
/// 合并顺序固定：`Accept`、`api-key` → identity headers（User-Agent / Stainless
/// 等）→ `model.headers` → 调用方 `headers` → `Content-Type`。后写的同名 header 覆盖先写
/// 的（大小写不敏感）；调用方 `ProviderHeaders` 里的 `None` 是显式删除。
///
/// 返回有序 `(name, value)` 列表，顺序是“最后一次设置的顺序”（`set_header` 先
/// `shift_remove` 再插入）；调用方应按名称查找，不应依赖该顺序。
fn build_headers(
    model: &Model,
    config: &crate::api::azure_openai_config::AzureConfig,
    api_key: &str,
    options: &AzureOpenAIResponsesOptions,
) -> Vec<(String, String)> {
    let mut table: HeaderTable = IndexMap::new();
    set_header(&mut table, "Accept", "application/json");
    set_header(&mut table, "api-key", api_key);
    // identity headers 由 provider/base URL 决定（User-Agent 的版本号等）。
    let target = ClientIdentityTarget::new(model.provider.clone(), config.base_url.clone());
    let identity_headers = get_identity_headers(&target, options.base.identity.as_ref());
    merge_provider_headers(&mut table, &identity_headers);
    // model.headers 不能删除 identity 头（纯 map 合并），options.headers 可以（None 删除）。
    if let Some(model_headers) = &model.headers {
        merge_plain_headers(&mut table, model_headers);
    }
    if let Some(options_headers) = &options.base.headers {
        merge_provider_headers(&mut table, options_headers);
    }
    // 最后写 Content-Type，确保调用方提供的同名头被覆盖。
    set_header(&mut table, "Content-Type", "application/json");
    header_list(table)
}

/// `?api-version=<apiVersion>`（Azure SDK 的 `defaultQuery`，由 `buildURL` 合并）。
///
/// 路径固定追加 `/responses`。`api-version` 总是取 `AzureConfig` 的值：base URL 里已有
/// 同名参数时值被覆盖、位置保持在首次出现处（`build_url` 的 query 合并规则）。
fn azure_url(base_url: &str, api_version: &str) -> Result<String, AiError> {
    build_url(base_url, "/responses", &[("api-version", api_version)])
}

/// Azure 请求使用的 `HttpClient`。
///
/// Azure SDK 在请求带 `api-key` 头时设置 `redirect: "manual"`
/// （`node_modules/openai/azure.js#buildRequest`）：**不跟随**重定向，3xx 原样作为响应返回，
/// 由 SDK 变成 `APIError`（`<status> status code (no body)` 等），`api-key` 也绝不会被转发到
/// 重定向目标。冻结的 `HttpClient` 契约没有“逐请求重定向策略”通道，而
/// `ReqwestHttpClient` 的默认 client 会跟随重定向（与 `fetch` 默认一致），因此这里在未注入
/// `options.fetch` 时使用一个不跟随重定向的 reqwest client（其余装配与
/// `ReqwestHttpClient::new` 相同：关闭内建解压、rustls、系统代理）。调用方注入了
/// `options.fetch` 时无法改写其策略——与 TS 里自定义 `fetch` 实现自行决定重定向行为一致。
fn azure_http_client(options: &StreamOptions) -> crate::http::SharedHttpClient {
    if let Some(fetch) = &options.fetch {
        // 注入的 client 完全接管传输；重定向策略由调用方负责（与 TS 自定义 fetch 一致）。
        return fetch.clone();
    }
    // 进程级复用：连接池与 TLS 配置只初始化一次；闭包不读取 options，不会被某个调用方污染。
    static DEFAULT: std::sync::OnceLock<crate::http::SharedHttpClient> = std::sync::OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            match reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_gzip()
                .no_deflate()
                .no_brotli()
                .no_zstd()
                .build()
            {
                Ok(client) => {
                    crate::http::reqwest_client::ReqwestHttpClient::from_client(client).shared()
                }
                // 与 `default_http_client` 的兜底一致（只在 TLS 后端初始化失败时走到）。
                Err(_) => default_http_client(&StreamOptions::default()),
            }
        })
        .clone()
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// `run_azure_inner` 的错误载体。
///
/// `error` 是最终要格式化的错误；`last_failure` 保留最后一次 HTTP 尝试的 SDK 错误快照。
/// `thrown_value` 只有在快照的消息与 `error.message` 完全一致时才采用它来还原状态码与
/// 响应体；取消发生在重试间隙时 `error.message` 是 `Request aborted`，与陈旧 5xx 的消息
/// 不同，因此旧快照会被丢弃并改用 `AiError` 自身重建。两者一起传播是为了在消息匹配的
/// 情形仍能还原状态码与响应体（详见 `request_with_retry` 的注释）。
struct AzureRunError {
    error: AiError,
    last_failure: Option<RequestFailure>,
}

impl From<AiError> for AzureRunError {
    fn from(error: AiError) -> Self {
        AzureRunError {
            error,
            last_failure: None,
        }
    }
}

/// 一次 Azure Responses 请求的完整执行体（在后台任务里调用）。
///
/// 成功时向 `stream` 推送 `start` … `done` 并 `end`；失败时返回 [`AzureRunError`]，由
/// `run_azure` 统一转成 `error` 事件并结束流。成功路径在这里结束流、失败路径不在，保证
/// 任何失败都经过同一段 `output.stop_reason`/`error_message` 处理。
///
/// 时序约束：`start` 事件必须在 `ensure_response_body` 之前推送，以复刻 SDK“先拿到响应
/// 对象、迭代时才因无 body 抛错”的行为（204/205 用例锁定）。
async fn run_azure_inner(
    model: &Model,
    context: &TranscriptContext,
    options: &AzureOpenAIResponsesOptions,
    endpoint_options: &AzureEndpointOptions,
    deployment_name: &str,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<(), AzureRunError> {
    // 与 TS 的 `if (!apiKey)` 一致：空串也视为缺失。
    let Some(api_key) = options
        .base
        .api_key
        .as_deref()
        .filter(|key| !key.is_empty())
    else {
        return Err(AiError::new(
            AiErrorKind::Auth,
            format!("No API key for provider: {}", model.provider),
        )
        .into());
    };
    // 端点解析放在后台任务里：失败会变成 error 事件（TS 的 createClient 在 try 内）。
    let config = resolve_azure_config(model, Some(endpoint_options))?;
    let headers = build_headers(model, &config, api_key, options);
    // grammar 属性在请求前算一次，同时供请求体与流处理的 custom tool 解析使用。
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        Some(&get_declared_tools(&context.as_context().messages)),
        compat_bool(model, "supportsOpenAIGrammarTools").unwrap_or(false),
    )?;
    let mut params = build_params(
        model,
        context,
        options,
        deployment_name,
        &grammar_tool_input_properties,
    )?;
    // 回调返回 `Some` 时整体替换参数；返回 `None` 表示不改（TS 的 undefined）。
    if let Some(next_params) = options.base.apply_on_payload(params.clone(), model).await {
        params = next_params;
    }
    // body 用 crate 的 JSON 序列化，确保与 `JSON.stringify` 字节一致。
    let request = HttpRequest {
        method: "POST".to_string(),
        url: azure_url(&config.base_url, &config.api_version)?,
        headers,
        body: Some(crate::http::json_body(&params)),
        timeout_ms: options.base.timeout_ms,
    };
    let client = azure_http_client(&options.base);
    // 重试、超时与取消都在这一层；返回的 last_failure 是最后一次失败的快照。
    let (response, last_failure) = request_with_retry(
        client,
        request,
        options.base.signal.clone(),
        options.base.max_retries,
        options.base.max_retry_delay_ms,
    )
    .await;
    let response = response.map_err(|error| AzureRunError {
        error,
        last_failure,
    })?;
    // onResponse 在推送 start 之前调用（与 TS 的顺序一致）。
    if let Some(on_response) = &options.base.on_response {
        on_response(
            provider_response(response.response.status, &response.response.headers),
            model,
        );
    }
    let _ = stream.push(crate::types::AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    // 204/205 的响应没有 body：SDK 在迭代流时抛 OpenAIError（start 已推送之后）。
    ensure_response_body(response.response.status)?;

    // Azure 只透传 onProviderStreamEvent 与 grammar props；service tier 槽位保持 None。
    let stream_options = OpenAIResponsesStreamOptions {
        on_provider_stream_event: options.base.on_provider_stream_event.clone(),
        service_tier: None,
        grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        resolve_service_tier: None,
        apply_service_tier_pricing: None,
    };
    let events = provider_event_stream(response.body, options.base.signal.clone());
    process_responses_stream(events, output, stream, model, Some(&stream_options)).await?;

    // 检查顺序固定：取消优先于 stop_reason 检查，因此会盖过 pending/error 文案。
    if options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err(AiError::new(AiErrorKind::Other, "Request was aborted").into());
    }
    if output.stop_reason == StopReason::Pending {
        return Err(AiError::new(
            AiErrorKind::Other,
            "Azure OpenAI Responses stream ended without a stop reason",
        )
        .into());
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let message = output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_string());
        return Err(AiError::new(AiErrorKind::Other, message).into());
    }
    let message = output.clone();
    let _ = stream.push(crate::types::AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: Box::new(message.clone()),
    });
    stream.end(message);
    Ok(())
}

/// 执行 [`run_azure_inner`]，并把 `Err` 转成流的终止 `error` 事件。
///
/// 取消判定只看 `signal.is_cancelled()`：取消 → `StopReason::Aborted` /
/// `ErrorReason::Aborted`，否则 `Error`。错误文本用最后一次 HTTP 失败快照格式化；本函数
/// 不返回 `Result`，调用方（`stream` 的后台任务）无需再处理错误。
async fn run_azure(
    model: &Model,
    context: &TranscriptContext,
    options: &AzureOpenAIResponsesOptions,
    endpoint_options: &AzureEndpointOptions,
    deployment_name: &str,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let result = run_azure_inner(
        model,
        context,
        options,
        endpoint_options,
        deployment_name,
        output,
        stream,
    )
    .await;
    let Err(failure) = result else {
        return;
    };
    // 失败快照与取消状态一起决定终态；error_message 在推送 error 事件前写入 output。
    let aborted = options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled());
    output.stop_reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    output.error_message = Some(format_thrown_error(
        failure.error,
        failure.last_failure,
        Some("Azure OpenAI API error"),
    ));
    let reason = if aborted {
        ErrorReason::Aborted
    } else {
        ErrorReason::Error
    };
    let _ = stream.push(crate::types::AssistantMessageEvent::Error {
        reason,
        error: Box::new(output.clone()),
    });
    stream.end(output.clone());
}

/// `stream`（导出）：完整选项。
///
/// 同步返回一个 [`AssistantMessageEventStream`]：本函数只做 context 归一化、克隆与后台
/// 任务启动，不等待网络。deployment 名与 `output` 的构造在后台任务里完成（与 TS 的
/// async IIFE 位置一致），因此端点解析失败不会从本函数抛出，而是以 `error` 事件结束流。
///
/// `options` 接受 [`AzureOpenAIResponsesOptions`] 本身，或可 `Into` 它的
/// [`StreamOptions`]（见上方的 `From` 实现）。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: impl Into<AzureOpenAIResponsesOptions>,
) -> AssistantMessageEventStream {
    let options = options.into();
    let stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    // 归一化必须在调用线程完成：TS 的 resolveTranscript 在 stream() 里同步执行。
    let normalized_context = resolve_transcript(
        &context,
        Some(compat_bool(&model, "supportsMidConvoSystemMessages").unwrap_or(false)),
    );
    // 后台任务持有流的一份克隆；原 stream 立即返回给调用方。
    let stream_for_task = stream.clone();
    spawn_forward(async move {
        let endpoint_options = options.endpoint_options();
        // deployment 名解析不会失败；它位于 output 构造之前、错误处理之外。
        let deployment_name = resolve_deployment_name(&model, Some(&endpoint_options));
        let mut output = AssistantMessage {
            // api 是字面量而不是 model.api：调用方即使传入错误 api 的模型，输出仍记录
            // 本次实际使用的适配器。
            api: "azure-openai-responses".to_string(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            stop_reason: StopReason::Pending,
            timestamp: now_ms(),
            ..AssistantMessage::default()
        };
        run_azure(
            &model,
            &normalized_context,
            &options,
            &endpoint_options,
            &deployment_name,
            &mut output,
            &stream_for_task,
        )
        .await;
    });
    stream
}

/// `streamSimple`（导出）：缺鉴权时同步返回 `Err`。
///
/// 与 [`stream`] 不同，这个入口在调用线程先校验 API key：缺失或空串立即返回
/// `AiErrorKind::Auth` + `No API key for provider: <provider>`（TS 的同步 throw 契约）。
/// 之后把 [`SimpleStreamOptions`] 转成完整选项：`build_base_options` 负责 maxTokens 钳制
/// 与 sampling params，`reasoning` 先按模型能力 clamp 再映射为 `reasoning_effort`
/// （`off` → `None`），`tool_choice` 序列化成 JSON；序列化失败时该字段被静默丢弃。
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    // 这是整个模块唯一的同步错误出口；消息文本是外部契约（改动需同步测试）。
    let api_key = options
        .base
        .api_key
        .as_deref()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            AiError::new(
                AiErrorKind::Auth,
                format!("No API key for provider: {}", model.provider),
            )
        })?;
    let base = build_base_options(model, context, Some(&options), Some(api_key));
    // 先按模型支持的档位 clamp，再转成 API 档位；clamp 到 Off 时 reasoning_effort 为 None。
    let clamped_reasoning = options
        .reasoning
        .map(|level| clamp_thinking_level(model, to_model_thinking_level(level)));
    let reasoning_effort = clamped_reasoning.and_then(from_model_thinking_level);
    let tool_choice = options
        .tool_choice
        .and_then(|choice| serde_json::to_value(choice).ok());
    Ok(stream(
        model,
        context,
        AzureOpenAIResponsesOptions {
            base,
            reasoning_effort,
            tool_choice,
            ..AzureOpenAIResponsesOptions::default()
        },
    ))
}

/// provider 工厂使用的零状态实现；真正逻辑在模块级 [`stream`] / [`stream_simple`]。
struct AzureOpenAIResponsesStreams;

/// 冻结的 [`ProviderStreams`] 适配：把本模块的两个入口接到 provider 工厂。
#[async_trait]
impl ProviderStreams for AzureOpenAIResponsesStreams {
    /// 把通用 [`StreamOptions`] 提升为 Azure 选项后转调 [`stream`]。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            AzureOpenAIResponsesOptions::from_stream_options(options),
        )
    }

    /// trait 不能返回 `Err`，因此把同步错误转成只含 error 事件的流（错误在流的
    /// setup 阶段出现）。
    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// `azureOpenAIResponsesApi()`。
///
/// 用 [`lazy_api`] 包装本模块的实现：每次 `stream` / `stream_simple` 都会执行一次
/// `load`（这里只是构造零状态的 `AzureOpenAIResponsesStreams`，不会失败），语义等价于
/// TS 的动态 import 包装。返回值可安全跨线程共享。
pub fn azure_openai_responses_api() -> Arc<dyn ProviderStreams> {
    lazy_api(
        Arc::new(|| {
            Box::pin(async {
                Ok(Arc::new(AzureOpenAIResponsesStreams) as Arc<dyn ProviderStreams>)
            })
        }),
        None,
    )
}

#[cfg(test)]
mod golden {
    //! 与 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）逐字节对照的请求黄金测试。
    //!
    //! fixture 同时锁定 URL（含 `api-version` 合并）、请求体字段顺序与关键 header；断言用
    //! `js_stringify` 比较整个参数对象。

    use super::*;
    use crate::types::TranscriptContext;
    use serde_json::json;

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-openai/azure_request.json");

    fn endpoint_options(fixture: &Value) -> AzureEndpointOptions {
        let options = &fixture["options"];
        AzureEndpointOptions {
            azure_api_version: options["azureApiVersion"].as_str().map(str::to_string),
            azure_base_url: options["azureBaseUrl"].as_str().map(str::to_string),
            azure_deployment_name: options["azureDeploymentName"].as_str().map(str::to_string),
            ..AzureEndpointOptions::default()
        }
    }

    fn options(fixture: &Value) -> AzureOpenAIResponsesOptions {
        let options = &fixture["options"];
        AzureOpenAIResponsesOptions {
            base: StreamOptions {
                max_tokens: options["maxTokens"].as_u64(),
                temperature: options["temperature"].as_f64(),
                session_id: options["sessionId"].as_str().map(str::to_string),
                sampling_params: Some(
                    serde_json::from_value(options["samplingParams"].clone()).expect("params"),
                ),
                ..StreamOptions::default()
            },
            azure_api_version: options["azureApiVersion"].as_str().map(str::to_string),
            azure_base_url: options["azureBaseUrl"].as_str().map(str::to_string),
            azure_deployment_name: options["azureDeploymentName"].as_str().map(str::to_string),
            reasoning_effort: Some(ThinkingLevel::High),
            reasoning_summary: Some(json!("detailed")),
            tool_choice: Some(options["toolChoice"].clone()),
            ..AzureOpenAIResponsesOptions::default()
        }
    }

    #[test]
    fn build_params_and_url_match_typescript_reference() {
        let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        let model: Model = serde_json::from_value(fixture["model"].clone()).expect("model");
        let context: TranscriptContext =
            serde_json::from_value(fixture["context"].clone()).expect("context");
        let endpoint_options = endpoint_options(&fixture);
        let config = resolve_azure_config(&model, Some(&endpoint_options)).expect("config");
        assert_eq!(
            azure_url(&config.base_url, &config.api_version).expect("url"),
            fixture["url"].as_str().expect("url")
        );

        let options = options(&fixture);
        let grammar: IndexMap<String, String> = [("query".to_string(), "input".to_string())]
            .into_iter()
            .collect();
        let deployment_name = resolve_deployment_name(&model, Some(&endpoint_options));
        let params = build_params(&model, &context, &options, &deployment_name, &grammar)
            .expect("build params");
        assert_eq!(
            crate::api::openai_responses_shared::js_stringify(&params),
            crate::api::openai_responses_shared::js_stringify(&fixture["params"])
        );

        let headers = build_headers(&model, &config, "azure-key", &options);
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("api-key"), Some("azure-key"));
        assert_eq!(header("content-type"), Some("application/json"));
        assert_eq!(header("accept"), Some("application/json"));
        assert!(header("session_id").is_none());
        assert!(header("x-client-request-id").is_none());
    }
}
