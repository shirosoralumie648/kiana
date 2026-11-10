//! api/typesafe-system-one.ts。
//!
//! TypeSafe 的原生 System One 协议。OpenRouter 提供同样的协议，因此两个 provider 共用本
//! API（只是 base URL 不同）。
//!
//! TypeSafe 把图片作为 `assets`，实验性且需要 `TypeSafe-Danger-Zone-Images` opt-in 头；
//! OpenRouter 会静默丢弃 `assets`，改为把图片作为 state 里的 `image_url` part。
//!
//! TS: `packages/ai/src/api/typesafe-system-one.ts`。
//!
//! # 被谁调用
//!
//! `providers::typesafe`（TypeSafe 官方）、`providers::openrouter`、
//! `providers::opencode`、`providers::vercel_ai_gateway` 把 [`typesafe_system_one_api`]
//! 注册到 `classifiers` 的 `"typesafe-system-one"` 槽位，由 `CreatedProvider` 对
//! `Provider::classify` 的实现按 `model.api` 查表分派；`kiana-builtin-extensions` 的
//! llama.cpp provider 对 decision 模型直接持有同一个实现：它自己实现 `Provider::classify`
//! 并按 `model.api` 选择，绕过的是 `CreatedProvider` 的 `classifiers` 查表，而不是这个
//! trait 方法本身。
//!
//! # 设计原因
//!
//! System One 是一个「分类问答」协议：请求体为 `{ model, state, questions }`，响应体为
//! `{ answers, usage? }`。各服务在 API 名/label、URL、请求信封、图片传递方式、opt-in 头
//! 与响应信封上有差异，因此公共流程抽到 `system_one_shared`。本文件实现
//! [`SystemOneTransport`] 的 6 个方法：`api` 与 `label` 返回固定常量
//! （`"typesafe-system-one"`、`"System One API"`），其余四个钩子随服务变化：
//!
//! - `detect_compat`：图片形式与 opt-in 头的**默认值**；
//! - `url`：把 `base_url` 拼成 `{base}/systemone`；
//! - `payload`：平铺请求体（`model` 在最前，没有 `input` 信封）；
//! - `output`：TypeSafe/OpenRouter 的输出就在响应体顶层，只校验它是 JSON 对象。
//!
//! OpenRouter 兼容 TypeSafe 的 `/systemone` 路径与请求/响应形状，所以两者共用本文件；
//! 差异全部收敛在 `detect_compat` 与 `model.compat` 上。
//!
//! # 执行流程
//!
//! 1. [`classify`] 把 `TypesafeTransport` 交给 [`classify_system_one`]。
//! 2. 共享层校验 `model.api == "typesafe-system-one"`，并把 `context.questions` 序列化为
//!    线上问题（`bool` 题的 `type` 改写为 `noul`）。
//! 3. 有图片时依次做：模型是否声明 `Image` 输入、`detect_compat` 的默认值、
//!    `model.compat.imageInput` / `sendImageOptInHeader` 的逐字段覆盖。
//! 4. 发请求；`output` 取回顶层对象后，共享层先解析 `usage`、再解析 `answers`。
//! 5. 任何失败都写进 [`ClassifierResult`] 的 `stop_reason` / `error_message`，不向外抛
//!    `Err`。
//!
//! # 注意事项
//!
//! - `detect_compat` 返回的是默认值；显式配置了 `compat` 的模型会覆盖它，改这里对它们
//!   无效。
//! - OpenRouter 检测是「provider id 相等**或** `base_url` 子串包含 `openrouter.ai`」，不是
//!   主机名解析；代理转发的 OpenRouter 应通过 provider id 或 `compat` 声明。
//! - `temperature` 等采样参数在 System One 协议里没有对应字段，会被忽略
//!   （`tests/typesafe_system_one.rs` 断言请求体不含 `temperature`）。
//! - 错误文案里的服务名固定为 `LABEL`（`System One API`），改名会改变用户可见文案与测试
//!   断言。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use crate::models::{ClassifierOptions, ProviderClassifier};
use crate::types::{ClassifierContext, ClassifierResult, Model};

use super::classifier_shared::ClassifierHttpError;
use super::system_one_shared::{
    ResolvedSystemOneCompat, SystemOneImageInput, SystemOneTransport, SystemOneWireRequest,
    classify_system_one, payload_with_model, require_output_record, transport_url,
};

/// `label`：错误消息里的服务名。
const LABEL: &str = "System One API";

/// TypeSafe 的 System One transport。
///
/// 无字段，因此也无状态：每个钩子只读传入的 `model` / wire request。本文件的
/// [`classify`] 在每次调用里用 `&TypesafeTransport` 临时构造一个实例，不需要注册或初始化。
struct TypesafeTransport;

impl SystemOneTransport for TypesafeTransport {
    fn api(&self) -> &'static str {
        // 必须与 `Model.api`、provider 分类器映射的键（如 `providers::typesafe` 里的
        // `"typesafe-system-one"`）以及模型目录里的 api 名一致；不一致时共享层会以
        // `Unsupported classifier API: <model.api>` 失败。
        "typesafe-system-one"
    }

    fn label(&self) -> &'static str {
        LABEL
    }

    fn detect_compat(&self, model: &Model) -> ResolvedSystemOneCompat {
        // OpenRouter by provider ID, which survives baseUrl overrides, or by host, which
        // covers regional endpoints.
        // 返回的是默认值：共享层 `run` 还会用 `model.compat` 的同名字段逐项覆盖。有图片时，
        // 结果决定图片按 `assets`（TypeSafe 及未知代理）还是 state 里的 `image_url`
        // （OpenRouter）发送，以及是否带 `TypeSafe-Danger-Zone-Images: true`。
        if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
            ResolvedSystemOneCompat {
                image_input: SystemOneImageInput::StateImageUrls,
                send_image_opt_in_header: false,
            }
        } else {
            ResolvedSystemOneCompat {
                image_input: SystemOneImageInput::Assets,
                send_image_opt_in_header: true,
            }
        }
    }

    fn url(&self, model: &Model) -> Result<Url, ClassifierHttpError> {
        // 按相对 URL 解析规则把 `systemone` 拼到 baseUrl 上（baseUrl 先去掉末尾所有 `/`
        // 再补一个 `/`）：`.../v1` 与 `.../v1/` 都得到 `.../v1/systemone`。base URL 无法
        // 解析时返回 `Invalid URL`。
        transport_url(&model.base_url, "systemone")
    }

    fn payload(&self, model: &Model, request: &SystemOneWireRequest) -> Value {
        // TypeSafe/OpenRouter 的请求体是平铺的 `{ model, state, questions, ... }`，没有
        // Cloudflare 那种 `input` 信封；`model` 由共享层放在最前，其余字段来自 wire request。
        payload_with_model(model, request)
    }

    fn output(
        &self,
        body: &Value,
    ) -> Result<indexmap::IndexMap<String, Value>, ClassifierHttpError> {
        // 两个服务的输出都在响应体顶层（`{ answers, usage }`），没有额外信封；这里只校验它
        // 是 JSON 对象并保留键序，`answers` / `usage` 的字段解析由共享层完成。
        require_output_record(LABEL, body)
    }
}

/// TypeSafe System One classification with public `bool` values mapped to wire-level `noul`.
///
/// 这是 TS 模块 `typesafe-system-one.ts` 的 `classify` 导出；provider 工厂通过
/// [`typesafe_system_one_api`] 拿到同一实现。
///
/// # 参数
///
/// - `model`：必须是 `api == "typesafe-system-one"` 的分类模型。`base_url` 是 API 根路径；
///   `provider == "openrouter"` 或 `base_url` 含 `openrouter.ai` 时默认改用 OpenRouter 的
///   图片形式（见 `TypesafeTransport::detect_compat`）。
/// - `context`：无图片时（`images` 为 `None` 或 `Some([])`）`state` 原样发给服务端；有图片
///   时按解析出的图片形态改写——`assets`/未知值把资产名数组放在原 state 之前，
///   `state-image-urls` 把 `image_url` part 数组放在原 state 之前，`images` 形态则保持
///   state 不变、图片进独立的 `images` 字段（见 `add_images`）。`questions` 与 `answers`
///   都按 JS 对象键序处理：整数索引键按数值升序排在前，其余键保持插入序（`js_entries`）。
/// - `options`：API key、请求级 header（与 `model.headers` 合并）、超时、重试、取消信号以及
///   `on_payload` / `on_response` 回调都从这里读取。若其中没有非空 `api_key`（包括
///   `options == None`），请求不会发出，直接以
///   `No API key for provider: <model.provider>` 失败。
///
/// # 返回
///
/// 总是返回 [`ClassifierResult`]，调用方不需要处理 `Result`：
///
/// - 成功：`stop_reason == Stop`，`answers` 按 `context.questions` 的 JS 对象键序填充
///   （整数索引键升序在前、其余保持插入序，与线上问题顺序一致）；`usage` 仅在
///   响应带 `usage` 且可解析时存在，并按模型目录价格计算费用。
/// - 失败：`stop_reason == Error`；调用方通过 `options.signal` 取消时为 `Aborted`。
///   `answers` 为空，`error_message` 是面向用户的文案。`usage` 仍可能存在——请求已被计费
///   时，即使 `answers` 解析失败也保留用量（共享层先设 usage 再解析 answers）。
///
/// # 错误
///
/// 函数本身不返回 `Err`。可观察的失败包括：API 名不匹配（`Unsupported classifier API`）、
/// 缺 API key、模型未声明图片输入、图片格式/数量不符、HTTP 非 2xx、超时、响应不是 JSON
/// 对象，以及缺少答案或答案类型错误。
pub async fn classify(
    model: &Model,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
) -> ClassifierResult {
    classify_system_one(&TypesafeTransport, model, context, options).await
}

/// `typesafeSystemOneApi()`：惰性包装在 Rust 里就是同一个实现（静态链接，无需动态 import）。
///
/// 每次调用返回一个新的 `Arc`，指向无状态的 `TypesafeSystemOneApi`；它只把
/// [`ProviderClassifier::classify`] 转给自由函数 [`classify`]。provider 工厂把它放进分类器
/// 映射即可（见 `providers::typesafe`）。
pub fn typesafe_system_one_api() -> Arc<dyn ProviderClassifier> {
    Arc::new(TypesafeSystemOneApi)
}

/// [`ProviderClassifier`] 适配器：把自由函数 [`classify`] 包装成可放进
/// `Arc<dyn ProviderClassifier>` 的对象（对应 TS 的 `typesafeSystemOneApi()` 返回值）。
struct TypesafeSystemOneApi;

// `ProviderClassifier` 本身用 `#[async_trait]` 声明；`async fn` 不是 dyn-compatible（不能
// 直接通过 trait object 调用），所以实现侧必须同样加宏，把 future 装箱后调用。
#[async_trait]
impl ProviderClassifier for TypesafeSystemOneApi {
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        classify(model, context, options).await
    }
}
