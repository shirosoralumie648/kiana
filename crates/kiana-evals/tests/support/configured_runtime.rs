//! `packages/evals/evals/configured-runtime.ts` 的移植（测试支持，不属于 kiana-evals 公开 API）。
//!
//! 规范 §8.1：`kiana-evals-fixtures` **未采纳**为 crate，configured-runtime 保留为 kiana-evals
//! 内的 test fixture。这里保持 `ModelRuntime.create` 的三个路径参数、错误作为数据
//! 返回（不抛出）、以及 `contentText` / `stopReason` / 用量的映射。
//!
//! TS 源：`packages/evals/evals/configured-runtime.ts`（120 行）。

use std::path::Path;

use kiana_ai::index::content_text_assistant;
use kiana_ai::models::{ModelsRefreshOptions, ModelsSimpleStreamOptions};
use kiana_ai::types::{Context, Model, ModelInputModality};
use kiana_coding_agent::sdk_exports::{CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption};
use serde::{Deserialize, Serialize};

/// `modelFields(model)` 的 `cost`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostFields {
    /// 输入费率。
    pub input: f64,
    /// 输出费率。
    pub output: f64,
    /// 缓存读费率。
    pub cache_read: f64,
    /// 缓存写费率。
    pub cache_write: f64,
}

/// `modelFields(model)` 的逐字段复制。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFields {
    /// model id。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// provider id。
    pub provider: String,
    /// `model.reasoning ?? false`。
    pub reasoning: bool,
    /// `[...model.input]`（`"text"` / `"image"`）。
    pub input: Vec<String>,
    /// 费率。
    pub cost: ModelCostFields,
    /// `model.contextWindow ?? 0`。
    pub context_window: u64,
    /// `model.maxTokens ?? 0`。
    pub max_tokens: u64,
}

/// 成功 completion 的字段。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderResponseFields {
    /// `contentText(response.content)`。
    pub text: String,
    /// `response.stopReason`（TS 字面量字符串）。
    pub stop_reason: String,
    /// `response.usage.input`。
    pub input_tokens: u64,
    /// `response.usage.output`。
    pub output_tokens: u64,
}

/// `inspectProvider` 的成功分支。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSuccess {
    /// probe 请求是否被 fixture 服务器确认为有效探针。
    pub valid_request_received: bool,
    /// 模型字段。
    pub model: ModelFields,
    /// 响应字段。
    pub response: ProviderResponseFields,
}

/// `{error}` 分支。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResult {
    /// 错误文本（TS 的 `error.message`）。
    pub error: String,
}

/// `ProviderRuntimeOutput["result"]`（untagged 联合）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderRuntimeResult {
    /// 成功。
    Success(ProviderSuccess),
    /// 结构化错误。
    Error(ErrorResult),
}

/// TS: `ProviderRuntimeOutput`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRuntimeOutput {
    /// union 结果。
    pub result: ProviderRuntimeResult,
}

/// `inspectAddedModel` 的成功分支。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedModelSuccess {
    /// 新增模型的字段。
    pub model: ModelFields,
    /// 内置模型是否全部仍在。
    pub existing_models_preserved: bool,
}

/// `AddedModelOutput["result"]`（untagged 联合）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AddedModelResult {
    /// 成功。
    Success(AddedModelSuccess),
    /// 结构化错误。
    Error(ErrorResult),
}

/// TS: `AddedModelOutput`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedModelOutput {
    /// union 结果。
    pub result: AddedModelResult,
}

/// TS: `ProviderScenario`。
pub struct ProviderScenario {
    /// provider id。
    pub provider_id: String,
    /// model id。
    pub model_id: String,
    /// 每次调用构造一个新的 `Context`。
    pub create_context: Box<dyn Fn() -> Context>,
    /// `completeSimple` 的选项。
    pub options: Option<ModelsSimpleStreamOptions>,
    /// fixture 服务器是否收到了有效探针。
    pub valid_request_received: Box<dyn Fn() -> bool>,
}

/// `loadConfiguredModelRuntime` / `inspectAddedModel` 的错误（`Display` 为错误文本）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredRuntimeError(pub String);

impl std::fmt::Display for ConfiguredRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfiguredRuntimeError {}

/// `modelFields(model)`。
pub fn model_fields(model: &Model) -> ModelFields {
    ModelFields {
        id: model.id.clone(),
        name: model.name.clone(),
        provider: model.provider.clone(),
        reasoning: model.reasoning.unwrap_or(false),
        input: model
            .input
            .iter()
            .map(|modality| match modality {
                ModelInputModality::Text => "text".to_string(),
                ModelInputModality::Image => "image".to_string(),
            })
            .collect(),
        cost: ModelCostFields {
            input: model.cost.input,
            output: model.cost.output,
            cache_read: model.cost.cache_read,
            cache_write: model.cost.cache_write,
        },
        context_window: model.context_window.unwrap_or(0),
        max_tokens: model.max_tokens.unwrap_or(0),
    }
}

fn join_path(agent_dir: &str, name: &str) -> String {
    Path::new(agent_dir)
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// TS: `loadConfiguredModelRuntime(agentDir)`。
pub async fn load_configured_model_runtime(
    agent_dir: &str,
) -> Result<ModelRuntime, ConfiguredRuntimeError> {
    ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: ModelsPathOption::Path(join_path(agent_dir, "models.json")),
        auth_path: Some(join_path(agent_dir, "auth.json")),
        models_store_path: Some(join_path(agent_dir, "models-store.json")),
        allow_model_network: false,
        ..Default::default()
    })
    .await
    .map_err(|error| ConfiguredRuntimeError(error.message))
}

/// TS: `inspectProvider(runtime, scenario)`。
///
/// 只有在 `refresh` 本身失败时才返回 `Err`（TS 里那会 reject）；配置解析错误通过
/// `runtime.getError()` 以 `{result:{error}}` 形式返回。
pub async fn inspect_provider(
    runtime: &ModelRuntime,
    scenario: ProviderScenario,
) -> Result<ProviderRuntimeOutput, ConfiguredRuntimeError> {
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .map_err(|error| ConfiguredRuntimeError(error.message))?;
    if let Some(error) = runtime.get_error() {
        return Ok(ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Error(ErrorResult { error }),
        });
    }
    let Some(model) = runtime.get_model(&scenario.provider_id, &scenario.model_id) else {
        return Ok(ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Error(ErrorResult {
                error: format!(
                    "Model {}/{} is unavailable after reload.",
                    scenario.provider_id, scenario.model_id
                ),
            }),
        });
    };
    let context = (scenario.create_context)();
    let response = runtime
        .complete_simple(&model, &context, scenario.options.unwrap_or_default())
        .await;
    Ok(ProviderRuntimeOutput {
        result: ProviderRuntimeResult::Success(ProviderSuccess {
            valid_request_received: (scenario.valid_request_received)(),
            model: model_fields(&model),
            response: ProviderResponseFields {
                text: content_text_assistant(&response.content, "\n"),
                stop_reason: kiana_evals::harness::stop_reason_str(response.stop_reason)
                    .to_string(),
                input_tokens: response.usage.input,
                output_tokens: response.usage.output,
            },
        }),
    })
}

/// TS: `inspectAddedModel(runtime, providerId, modelId)`。
pub async fn inspect_added_model(
    runtime: &ModelRuntime,
    provider_id: &str,
    model_id: &str,
) -> Result<AddedModelOutput, ConfiguredRuntimeError> {
    let pristine_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: ModelsPathOption::Disabled,
        allow_model_network: false,
        ..Default::default()
    })
    .await
    .map_err(|error| ConfiguredRuntimeError(error.message))?;
    let Some(pristine_provider) = pristine_runtime.get_provider(provider_id) else {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error(ErrorResult {
                error: format!("Built-in provider {provider_id} is unavailable."),
            }),
        });
    };
    let existing_model_ids: Vec<String> = pristine_provider
        .get_models()
        .into_iter()
        .map(|model| model.id)
        .collect();
    if existing_model_ids.is_empty() {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error(ErrorResult {
                error: format!("Built-in provider {provider_id} has no models."),
            }),
        });
    }
    runtime
        .refresh(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        })
        .await
        .map_err(|error| ConfiguredRuntimeError(error.message))?;
    if let Some(error) = runtime.get_error() {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error(ErrorResult { error }),
        });
    }
    let Some(model) = runtime.get_model(provider_id, model_id) else {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error(ErrorResult {
                error: format!("Model {provider_id}/{model_id} is unavailable after reload."),
            }),
        });
    };
    let existing_models_preserved = existing_model_ids
        .iter()
        .all(|id| runtime.get_model(provider_id, id).is_some());
    Ok(AddedModelOutput {
        result: AddedModelResult::Success(AddedModelSuccess {
            model: model_fields(&model),
            existing_models_preserved,
        }),
    })
}
