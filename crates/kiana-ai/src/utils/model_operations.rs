//! `utils/model-operations.ts`——模型类型断言与错误结果构造。
//!
//! TS: `packages/ai/src/utils/model-operations.ts`。
//!
//! TS 用 `asserts model is ChatModel` 的断言函数把 `AnyModel` 收窄到具体类型；Rust 侧返回
//! `Result`（把 `assertChatModel` 的 `ModelsError` 原样返回），调用方用 `?` 传播。
//!
//! `ModelsError` 的构造走 [`models_error`]（本单元 [`crate::utils::models_error`]），
//! 保证 `code`/`message` 的映射稳定（`"provider"` 对应 `ModelsErrorCode::Provider`），
//! 这是当前稳定契约，改动需同步测试。
//!
//! # 为什么断言返回 `Result`
//!
//! TS 的 `asserts model is ChatModel` 在失败时 `throw new ModelsError(...)`，调用方靠
//! `try/catch` 处理；Rust 没有异常，因此这里把“类型不符”编码成 `Err(ModelsError)`，
//! 调用方用 `?` 传播（`models::generate_images` / `models::classify` 会把 `Err` 折叠成
//! [`image_error_result`] / [`classifier_error_result`] 的结果，不向调用方抛出）。错误
//! 文本、`code` 与 `name` 是当前稳定契约，改动需同步测试；下游若按这些文本做断言，
//! 可继续成立。
//!
//! 另一个同类入口是 [`crate::types::require_chat`]：它只接受 `&Model`、返回
//! `ModelTypeError`，错误文本与本文件的 [`assert_chat_model`] 相同；新增代码按入参类型
//! 二选一，不要在两处各写一份不同的消息。
//!
//! # 调用方
//!
//! - `crate::models`：`generate_images` / `classify` 在鉴权前先做类型与输入校验；
//! - `kiana-config::models::runtime`：模型运行时按同一顺序复用这些检查；
//! - `kiana-config::models::composer`：`ComposedProvider` 的 `generate_images` / `classify`
//!   兜底分支调用 [`image_error_result`] / [`classifier_error_result`]，类型判定用
//!   [`get_model_type`] / [`is_model_type`]；
//! - `crate::providers::openrouter`：用 [`is_model_type`] 把非 chat 模型排除在个性化过滤
//!   之外（原样保留）。
//!
//! # 错误结果的形状
//!
//! [`image_error_result`] / [`classifier_error_result`] 不返回 `Err`：它们把失败编码进
//! 结果对象（TS 的 `catch` 分支返回同形对象）。`aborted = true` 只表示“调用方取消”，
//! 与普通失败共用同一个构造函数；两者的 `usage` 都是 `None`，`output` / `answers` 为空。
//! `timestamp` 用当前 Unix 毫秒（TS 的 `Date.now()`）。

use crate::types::{
    AnyModel, AssistantImages, ClassifierContext, ClassifierResult, ClassifierStopReason,
    ImageContent, ImagesStopReason, Model, ModelType, Usage, UserContentBlock,
};
use crate::utils::diagnostics::ThrownValue;
use crate::utils::models_error::{TsModelsErrorCode, models_error};

/// `getModelType(model)`：没有 `type` 的是 chat 模型。
///
/// 直接转发 [`AnyModel::model_type`]：`Chat`/`Image`/`Classifier` 变体按变体本身判定；
/// `Remote` 读投影出的 `Model.model_type`，字段为 `None`（JSON 里省略 `type`）时按
/// `ModelType::Chat` 处理，与 TS 的 `model.type ?? "chat"` 一致。
pub fn get_model_type(model: &AnyModel) -> ModelType {
    model.model_type()
}

/// `isModelType(model, type)`。
///
/// 只是 [`get_model_type`] 的相等比较，没有额外的字段校验；返回 `true` 不代表模型的
/// `api`/`input` 等字段已经满足某家 provider 的要求，那些检查由适配器自己完成。
pub fn is_model_type(model: &AnyModel, model_type: ModelType) -> bool {
    get_model_type(model) == model_type
}

/// `assertChatModel(model)`。
///
/// 要求模型类型是 chat，否则返回 `ModelsError`。
///
/// # 参数与返回
///
/// - `model`：任意 [`AnyModel`]；`Remote` 按投影出的 `Model.model_type` 判定。
/// - 成功返回内部 [`Model`] 的共享借用（不克隆）；调用方随后可读 `api`/`base_url` 等字段。
///
/// # 错误
///
/// 类型不符时返回 `ModelsError { code: Provider, message: "Model {provider}/{id} is not a
/// chat model", cause_detail: None }`；错误文本是当前稳定契约，`provider`/`id`
/// 取投影后的字段。判定只依赖类型，不看 `api` 是否与 provider 支持的实现匹配。
pub fn assert_chat_model(model: &AnyModel) -> Result<&Model, crate::error::ModelsError> {
    if !is_model_type(model, ModelType::Chat) {
        // 失败消息用 `model()` 的投影：对 `Remote` 而言就是远端 JSON 尽力投影出的
        // provider/id（与 TS 从对象上直接读字段的结果一致）。
        let model = model.model();
        return Err(models_error(
            TsModelsErrorCode::Provider,
            &format!("Model {}/{} is not a chat model", model.provider, model.id),
            None,
        ));
    }
    Ok(model.model())
}

/// `assertImageModel(model)`。
///
/// 要求模型类型是 image，否则返回 `ModelsError`。
///
/// 成功返回内部 [`Model`] 的共享借用；失败消息为
/// `"Model {provider}/{id} is not an image model"`，`code` 为 `Provider`。它不检查模型
/// 是否声明了 `output` 模态等图像专属字段。
pub fn assert_image_model(model: &AnyModel) -> Result<&Model, crate::error::ModelsError> {
    if !is_model_type(model, ModelType::Image) {
        let model = model.model();
        return Err(models_error(
            TsModelsErrorCode::Provider,
            &format!(
                "Model {}/{} is not an image model",
                model.provider, model.id
            ),
            None,
        ));
    }
    Ok(model.model())
}

/// `assertClassifierModel(model)`。
///
/// 要求模型类型是 classifier，否则返回 `ModelsError`。
///
/// 成功返回内部 [`Model`] 的共享借用；失败消息为
/// `"Model {provider}/{id} is not a classifier model"`，`code` 为 `Provider`。
pub fn assert_classifier_model(model: &AnyModel) -> Result<&Model, crate::error::ModelsError> {
    if !is_model_type(model, ModelType::Classifier) {
        let model = model.model();
        return Err(models_error(
            TsModelsErrorCode::Provider,
            &format!(
                "Model {}/{} is not a classifier model",
                model.provider, model.id
            ),
            None,
        ));
    }
    Ok(model.model())
}

/// `assertClassifierInputSupported(model, context)`：模型目录项不接受图片输入时拒绝分类器图片。
///
/// # 判定
///
/// 仅当 `context.images` 是 `Some` 且**非空**、同时 `model.input` 不含
/// [`crate::types::ModelInputModality::Image`] 时返回错误；`None` 与 `Some(vec![])` 都
/// 视为“没有图片输入”（对应 TS 的 `context.images?.length` 真值判断）。
///
/// # 错误与调用约定
///
/// 失败返回 `ModelsError { code: Provider, message: "Model {provider}/{id} does not accept
/// image input" }`。本函数只做拒绝，不做图片降级；调用方必须在构造请求前调用它。若跳过，
/// 聊天路径的 `api::transform_messages::downgrade_unsupported_images` 会把不支持的图片
/// 替换成占位文本（不报错），分类器路径则可能把图片发给不认识它的模型。
pub fn assert_classifier_input_supported(
    model: &Model,
    context: &ClassifierContext,
) -> Result<(), crate::error::ModelsError> {
    // TS 的 `context.images?.length` 是数字真值：空数组为 0 → 不进入拒绝分支。
    let has_images = context
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty());
    if has_images
        && !model
            .input
            .contains(&crate::types::ModelInputModality::Image)
    {
        return Err(models_error(
            TsModelsErrorCode::Provider,
            &format!(
                "Model {}/{} does not accept image input",
                model.provider, model.id
            ),
            None,
        ));
    }
    Ok(())
}

/// TS 的 `Date.now()`。
///
/// 返回 Unix 纪元以来的毫秒数。系统时钟早于 1970-01-01 时返回 0（`map_or(0, ...)`），
/// 不 panic、也不会产生负数——使用本函数的错误结果 `timestamp` 因此始终非负。
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `imageErrorResult(model, error, aborted = false)`。
///
/// 构造失败的图像结果：`output` 为空、`response_id` 与 `usage` 为 `None`，
/// `stop_reason` 在 `aborted` 时为 [`ImagesStopReason::Aborted`]、否则为
/// [`ImagesStopReason::Error`]，`error_message` 恒为 `Some(thrown_message(error))`
/// （即使消息是空串），`timestamp` 取当前 Unix 毫秒。
///
/// 该函数不返回 `Result`：调用方（例如 `models::generate_images` 的错误分支、`kiana-config`
/// 的 `models::runtime` 与 `models::composer` 的 `generate_images`，以及 provider 默认实现）
/// 已经处在“把失败折叠成结果对象”的位置。`api`/`provider`/`model` 三个标识从 `model`
/// 复制，不包含请求上下文或中间产物。
pub fn image_error_result(model: &Model, error: &ThrownValue, aborted: bool) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: Vec::new(),
        response_id: None,
        usage: None,
        stop_reason: if aborted {
            ImagesStopReason::Aborted
        } else {
            ImagesStopReason::Error
        },
        error_message: Some(thrown_message(error)),
        timestamp: now_unix_ms(),
    }
}

/// `classifierErrorResult(model, error, aborted = false)`。
///
/// 与 [`image_error_result`] 同构：`answers` 为空表、`usage` 为 `None`，停止原因在
/// [`ClassifierStopReason::Aborted`] 与 [`ClassifierStopReason::Error`] 之间按 `aborted`
/// 切换，`error_message` 恒为 `Some`，`timestamp` 取当前 Unix 毫秒。
pub fn classifier_error_result(
    model: &Model,
    error: &ThrownValue,
    aborted: bool,
) -> ClassifierResult {
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: indexmap::IndexMap::new(),
        usage: None,
        stop_reason: if aborted {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        },
        error_message: Some(thrown_message(error)),
        timestamp: now_unix_ms(),
    }
}

/// TS 的 `error instanceof Error ? error.message : String(error)`。
///
/// 注意与 [`crate::utils::diagnostics::format_thrown_value`] 的区别：TS 这里取 `Error` 的
/// `message` 原值（空字符串就是 `""`），**不**回退到 `name`。
///
/// # 语义
///
/// 非 `Error` 的 JSON 值走 [`kiana_json::js_string::js_string`]（JS 的 `String(v)`）：
/// 对象得到 `"[object Object]"`，数组按逗号连接，`null` 得到 `"null"`。`Error` 分支
/// 无条件取 `info.message`，因此 `Error { name: "TypeError", message: "" }` 的结果是
/// `""`；单测 `uses_the_error_message_even_when_it_is_empty` 锁定该差异。
fn thrown_message(error: &ThrownValue) -> String {
    match error {
        ThrownValue::Error(info) => info.message.clone(),
        ThrownValue::Text(text) => text.clone(),
        ThrownValue::Json(json) => kiana_json::js_string::js_string(Some(json)),
    }
}

/// 便捷重载：成功时的图片结果（TS 侧由 provider 直接构造对象，Rust 侧给出同形构造器）。
///
/// `AssistantImages.output` 是 `(TextContent | ImageContent)[]`；这里把每个
/// [`ImageContent`] 包装成 [`UserContentBlock::Image`]。
///
/// # 参数与不变量
///
/// - `images` 按传入顺序放进 `output`，不做缩放、base64 或 MIME 校验；
/// - `usage` 原样透传（`None` 表示 provider 未报告用量），本函数不做计费；
/// - `response_id` 固定为 `None`、`stop_reason` 固定为 [`ImagesStopReason::Stop`]，
///   因此调用方若有 provider 响应 id，需要在本函数返回后自行填写；
/// - `timestamp` 由调用方决定（与错误结果内部的 `Date.now()` 不同），便于重放/测试。
///
/// 当前仓库内没有调用方；保留它是为了让 Rust 侧能构造与 TS 成功路径相同的对象形状。
pub fn image_result(
    model: &Model,
    images: Vec<ImageContent>,
    usage: Option<Usage>,
    timestamp: i64,
) -> AssistantImages {
    AssistantImages {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        output: images.into_iter().map(UserContentBlock::Image).collect(),
        response_id: None,
        usage,
        stop_reason: ImagesStopReason::Stop,
        error_message: None,
        timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(model_type: Option<ModelType>) -> AnyModel {
        let model = Model {
            id: "m1".into(),
            name: "M1".into(),
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com".into(),
            input: vec![crate::types::ModelInputModality::Text],
            cost: Default::default(),
            model_type,
            ..Default::default()
        };
        match model_type {
            Some(ModelType::Image) => AnyModel::Image(model),
            Some(ModelType::Classifier) => AnyModel::Classifier(model),
            _ => AnyModel::Chat(model),
        }
    }

    #[test]
    fn models_without_a_type_are_chat_models() {
        assert_eq!(get_model_type(&model(None)), ModelType::Chat);
        assert!(is_model_type(&model(None), ModelType::Chat));
        assert!(assert_chat_model(&model(None)).is_ok());
    }

    #[test]
    fn rejects_the_wrong_model_type() {
        let error = assert_chat_model(&model(Some(ModelType::Image))).expect_err("image model");
        assert_eq!(error.message, "Model anthropic/m1 is not a chat model");
        let error = assert_image_model(&model(None)).expect_err("chat model");
        assert_eq!(error.message, "Model anthropic/m1 is not an image model");
        let error = assert_classifier_model(&model(None)).expect_err("chat model");
        assert_eq!(
            error.message,
            "Model anthropic/m1 is not a classifier model"
        );
    }

    #[test]
    fn rejects_classifier_images_when_the_catalog_entry_has_none() {
        let model = self::model(None);
        let context = ClassifierContext {
            state: Default::default(),
            images: Some(vec![ImageContent::default()]),
            questions: Default::default(),
        };
        let error =
            assert_classifier_input_supported(model.model(), &context).expect_err("no image input");
        assert_eq!(
            error.message,
            "Model anthropic/m1 does not accept image input"
        );
        let context = ClassifierContext {
            state: Default::default(),
            images: None,
            questions: Default::default(),
        };
        assert!(assert_classifier_input_supported(model.model(), &context).is_ok());
    }

    #[test]
    fn builds_error_results_with_the_thrown_message() {
        let model = self::model(None);
        let error = ThrownValue::from_text("boom");
        let result = image_error_result(model.model(), &error, false);
        assert_eq!(result.stop_reason, ImagesStopReason::Error);
        assert_eq!(result.error_message.as_deref(), Some("boom"));
        assert!(result.output.is_empty());

        let result = classifier_error_result(model.model(), &error, true);
        assert_eq!(result.stop_reason, ClassifierStopReason::Aborted);
        assert_eq!(result.error_message.as_deref(), Some("boom"));
        assert!(result.answers.is_empty());
    }

    #[test]
    fn uses_the_error_message_even_when_it_is_empty() {
        // TS: `error instanceof Error ? error.message : String(error)`；空 message 保持 `""`，
        // 不回退到 `name`（`formatThrownValue` 的 `message || name` 不适用于这里）。
        let model = self::model(None);
        let error = ThrownValue::Error(crate::types::DiagnosticErrorInfo {
            name: Some("TypeError".into()),
            message: String::new(),
            stack: None,
            code: None,
        });
        assert_eq!(
            image_error_result(model.model(), &error, false)
                .error_message
                .as_deref(),
            Some("")
        );
        assert_eq!(
            classifier_error_result(model.model(), &error, false)
                .error_message
                .as_deref(),
            Some("")
        );

        let json_error = ThrownValue::from_json(serde_json::json!({ "reason": "boom" }));
        assert_eq!(
            image_error_result(model.model(), &json_error, false)
                .error_message
                .as_deref(),
            Some("[object Object]")
        );
    }
}
