//! api/cloudflare-workers-ai-system-one.ts。
//!
//! Workers AI REST 端点上的 System One 模型：`POST /accounts/{account}/ai/run`，
//! body 为 `{ model, input }`。REST API 用 Cloudflare 的 API 信封包住模型输出：
//! - 第三方模型（如 `typesafe/jev`）多一层 run 记录：
//!   `{ success, result: { state: "Completed", result: { answers, usage } } }`；
//! - Cloudflare 自托管模型（如 `@cf/cloudflare/clef`）直接返回输出：
//!   `{ success, result: { model, answers, usage } }`。
//!
//! TS: `packages/ai/src/api/cloudflare-workers-ai-system-one.ts`。
//!
//! # 这个文件负责的边界
//!
//! System One 协议（wire request、bool ↔ `noul`、answers/usage 解析、图片编码）由
//! `super::system_one_shared` 实现；本文件只提供 Workers AI 的「传输差异」：
//! REST 路径、`{ model, input }` 请求信封、Cloudflare API 响应信封，以及图片模式检测。
//!
//! # 调用链
//!
//! `providers::cloudflare_workers_ai::cloudflare_workers_ai_provider()` 把
//! [`cloudflare_workers_ai_system_one_api`] 注册到 provider 的 `classifiers` 表的
//! `"cloudflare-workers-ai-system-one"` 键下，外层套
//! `providers::cloudflare_stream::cloudflare_classifier`：包装层会先用 provider env 替换
//! `baseUrl` 里的 `{CLOUDFLARE_ACCOUNT_ID}` / `{CLOUDFLARE_GATEWAY_ID}`。
//! 之后 `Models::classify` → `Provider::classify`（按 `model.api` 查表）→ 本文件的
//! [`classify`] → `classify_system_one`。
//!
//! # 一次分类请求
//!
//! 1. 共享层校验 `model.api == "cloudflare-workers-ai-system-one"`，把 context 转成
//!    System One wire request（`bool` 题在线上写作 `noul`）；
//! 2. 有图片时：目录条目的 `compat.imageInput` 优先；否则按模型 id 前缀决定——
//!    `@cf/cloudflare/clef` 开头用 `images`（data URL 列表），其余用 System One
//!    `assets`。Workers AI 的检测值不要求 TypeSafe 的 opt-in 头（目录里的 `compat`
//!    仍可显式覆盖）；
//! 3. `POST {baseUrl}/run`，体为 `{ "model": <id>, "input": <wire request> }`；共享层用
//!    `Authorization: Bearer <apiKey>`，`options` 里没有非空 `api_key` 时在发请求前失败；
//! 4. 解析响应信封（见下），再按 `context.questions` 解析 answers 与 usage。
//!
//! # 响应信封与失败模式
//!
//! - `success === false` 优先于 `result` 检查；错误文本取自 `errors[].message`
//!   （只收字符串项，用 `; ` 连接），一个都没有时退回 `<LABEL> request failed`；
//! - `result` 里有 `answers` → Cloudflare 自托管模型的直接输出，原样返回；
//! - 否则要求 `state === "Completed"`，再取一层 `result`（第三方模型的 run 记录）；
//! - 所有失败都折叠进 [`ClassifierResult`]：`stop_reason` 为 `Error`，取消令牌已触发时
//!   为 `Aborted`，错误文案写入 `error_message`；函数本身不返回 `Result`，与
//!   `ProviderClassifier::classify` 的契约一致。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use crate::models::{ClassifierOptions, ProviderClassifier};
use crate::types::{ClassifierContext, ClassifierResult, Model};
use crate::utils::json_parse::json_object_from_map;

use super::classifier_shared::ClassifierHttpError;
use super::system_one_shared::{
    ResolvedSystemOneCompat, SystemOneImageInput, SystemOneTransport, SystemOneWireRequest,
    classify_system_one, payload_with_input, require_output_record, transport_url,
};

const LABEL: &str = "Cloudflare Workers AI";

/// `cloudflareErrorMessage(errors)`：收集 `errors[].message`，否则用通用文案。
///
/// 只接受 `{ "message": "<字符串>" }` 形状的数组项；非对象、`message` 缺失或不是字符串
/// 的项都被跳过。`errors` 不是数组或全部被跳过时返回 `<LABEL> request failed`。
/// 这条文案随后由共享层的 `error_message` 处理：非 HTTP 失败原样保留，因此用户看到的
/// 就是这里拼出的字符串。
fn cloudflare_error_message(errors: &Value) -> String {
    if let Value::Array(items) = errors {
        let messages: Vec<&str> = items
            .iter()
            .filter_map(|error| {
                let object = error.as_object()?;
                object.get("message")?.as_str()
            })
            .collect();
        if !messages.is_empty() {
            return format!("{LABEL} error: {}", messages.join("; "));
        }
    }
    format!("{LABEL} request failed")
}

/// System One models on the Workers AI REST endpoint.
struct CloudflareWorkersAiTransport;

impl SystemOneTransport for CloudflareWorkersAiTransport {
    fn api(&self) -> &'static str {
        "cloudflare-workers-ai-system-one"
    }

    fn label(&self) -> &'static str {
        LABEL
    }

    fn detect_compat(&self, model: &Model) -> ResolvedSystemOneCompat {
        // 目录里的 `compat.imageInput` 在共享层优先覆盖这里的判断；这里只覆盖没有
        // compat 的模型：Clef 系按 id 前缀识别，其余按 System One `assets` 处理。
        // opt-in 头默认关闭（compat.sendImageOptInHeader 仍可覆盖）。
        ResolvedSystemOneCompat {
            image_input: if model.id.starts_with("@cf/cloudflare/clef") {
                SystemOneImageInput::Images
            } else {
                SystemOneImageInput::Assets
            },
            send_image_opt_in_header: false,
        }
    }

    fn url(&self, model: &Model) -> Result<Url, ClassifierHttpError> {
        transport_url(&model.base_url, "run")
    }

    fn payload(&self, model: &Model, request: &SystemOneWireRequest) -> Value {
        payload_with_input(model, request)
    }

    fn output(
        &self,
        body: &Value,
    ) -> Result<indexmap::IndexMap<String, Value>, ClassifierHttpError> {
        let body = require_output_record(LABEL, body)?;
        // 先看 Cloudflare 信封的成功位：失败时 `result` 可能为 null，细节在 errors。
        if body.get("success") == Some(&Value::Bool(false)) {
            return Err(ClassifierHttpError::new(cloudflare_error_message(
                body.get("errors").unwrap_or(&Value::Null),
            )));
        }
        let result = body.get("result").unwrap_or(&Value::Null);
        let Value::Object(result_map) = result else {
            return Err(ClassifierHttpError::new(format!(
                "{LABEL} returned an unexpected response"
            )));
        };
        // Cloudflare-hosted models return the output directly.
        if result_map.contains_key("answers") {
            return Ok(json_object_from_map(result_map.clone()));
        }
        // Third-party models add a run record.
        // 只有 state == "Completed" 才能取出内层 result；其余状态（Queued/Failed/…）
        // 按错误返回，状态文本按 JS `String()` 规则格式化（缺失写 `undefined`、null 写
        // `null`，字符串原样）。
        if result_map.get("state").and_then(Value::as_str) != Some("Completed") {
            let state = kiana_json::js_string::js_string(result_map.get("state"));
            return Err(ClassifierHttpError::new(format!(
                "{LABEL} run did not complete (state: {state})"
            )));
        }
        match result_map.get("result") {
            Some(Value::Object(inner)) => Ok(json_object_from_map(inner.clone())),
            _ => Err(ClassifierHttpError::new(format!(
                "{LABEL} returned an unexpected response"
            ))),
        }
    }
}

/// Cloudflare Workers AI System One classification with public `bool` values mapped to
/// wire-level `noul`.
///
/// # 输入
///
/// - `model`：`api` 必须是 `"cloudflare-workers-ai-system-one"`（共享层校验），`base_url`
///   必须是已解析账号占位符的 REST 根（目录里以 `/ai` 结尾，请求再加 `/run`）；
/// - `context`：状态、可选图片与问题定义；
/// - `options`：取 `api_key`（必填）、`headers`、`signal`、`fetch`、重试与回调等通用字段
///   （`ClassifierOptions` 就是 `StreamOptions` 的别名）。
///
/// # 返回
///
/// 总是返回 [`ClassifierResult`]：成功时 `stop_reason = Stop` 且 `answers` 已按问题类型
/// 解码；失败时 `stop_reason` 为 `Error`（取消令牌已触发时为 `Aborted`），原因写入
/// `error_message`。调用方不需要（也无法）从这个函数拿到 `Err`。
pub async fn classify(
    model: &Model,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
) -> ClassifierResult {
    classify_system_one(&CloudflareWorkersAiTransport, model, context, options).await
}

/// `cloudflareWorkersAISystemOneApi()`。
///
/// 返回的 [`ProviderClassifier`] 只做转发；传输差异在 `CloudflareWorkersAiTransport`。
/// `providers::cloudflare_workers_ai` 把它注册到 `"cloudflare-workers-ai-system-one"`
/// 键下，并在外层套 `cloudflare_classifier` 做 baseUrl 占位符解析。
pub fn cloudflare_workers_ai_system_one_api() -> Arc<dyn ProviderClassifier> {
    Arc::new(CloudflareWorkersAiSystemOneApi)
}

struct CloudflareWorkersAiSystemOneApi;

#[async_trait]
impl ProviderClassifier for CloudflareWorkersAiSystemOneApi {
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        classify(model, context, options).await
    }
}
