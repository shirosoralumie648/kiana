//! 声明式 provider 的加载期校验与未知字段往返。
//!
//! provider manifest 可以由配置文件/扩展提供（「声明式 provider」）。加载期必须：
//!
//! 1. 校验 provider id 与模型 id（非空、字符集、唯一性）；
//! 2. 校验已知字段的类型与取值（operation 枚举、URL scheme、正数 token 预算）；
//! 3. **保留未知字段**：manifest 可能是未来版本写的，Kiana 不认识的新字段必须原样往返，
//!    不能因为重新序列化而丢失；
//! 4. 坏模型条目只跳过该条并产生诊断，不连坐同一 provider 的其它模型。
//!
//! 第 4 条是白名单式容错：provider 级结构错误（不是对象、id 非法、没有可用模型）仍返回
//! 错误；单条模型的 id/operation 错误降级成诊断。

use std::fmt;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::request_policy::capabilities::CapabilitySet;

/// 一个声明式 provider 的 manifest。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarativeProvider {
    /// provider id（字符集见 [`validate_provider_id`]）。
    pub provider_id: String,
    /// 展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// API 根地址（http/https）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// API 实现名（如 `openai-completions`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// provider 级默认能力。
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// 模型列表；加载时会剔除坏条目。
    #[serde(default)]
    pub models: Vec<DeclarativeModel>,
    /// 未知的 provider 级字段（原样保留）。
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// 一个声明式模型条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarativeModel {
    /// 模型 id（provider 内唯一）。
    pub id: String,
    /// 展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 模型级能力覆盖。
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// 最大输出 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    /// 上下文窗口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 未知的模型级字段（原样保留）。
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// 诊断码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarativeDiagnosticCode {
    /// 模型 id 非法。
    InvalidModelId,
    /// 模型 id 与前面的条目重复。
    DuplicateModelId,
    /// 模型条目不是对象或缺少必填字段。
    InvalidModelEntry,
    /// 模型能力字段类型/取值非法。
    InvalidCapability,
    /// token 预算字段非法（0 或类型错误）。
    InvalidTokenBudget,
}

impl DeclarativeDiagnosticCode {
    /// 稳定字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            DeclarativeDiagnosticCode::InvalidModelId => "invalid_model_id",
            DeclarativeDiagnosticCode::DuplicateModelId => "duplicate_model_id",
            DeclarativeDiagnosticCode::InvalidModelEntry => "invalid_model_entry",
            DeclarativeDiagnosticCode::InvalidCapability => "invalid_capability",
            DeclarativeDiagnosticCode::InvalidTokenBudget => "invalid_token_budget",
        }
    }
}

/// 一条加载诊断。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarativeDiagnostic {
    /// 模型在原始数组里的下标。
    pub index: usize,
    /// 可读出的模型 id（读不出时为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// 诊断码。
    pub code: DeclarativeDiagnosticCode,
    /// 诊断文本。
    pub message: String,
}

impl fmt::Display for DeclarativeDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "model[{}]{}: {} ({})",
            self.index,
            self.model_id
                .as_deref()
                .map(|id| format!(" ({id})"))
                .unwrap_or_default(),
            self.message,
            self.code.as_str()
        )
    }
}

/// provider 级加载错误（无法降级成逐条诊断）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeclarativeProviderError {
    /// 顶层不是 JSON 对象。
    NotAnObject,
    /// provider id 缺失或非法。
    InvalidProviderId { provider_id: String },
    /// `baseUrl` 不是 http(s) URL。
    InvalidBaseUrl { base_url: String },
    /// `models` 不是数组。
    InvalidModels,
    /// 所有模型条目都被跳过。
    NoUsableModels,
}

impl fmt::Display for DeclarativeProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeclarativeProviderError::NotAnObject => {
                formatter.write_str("declarative provider must be a JSON object")
            }
            DeclarativeProviderError::InvalidProviderId { provider_id } => {
                write!(formatter, "invalid provider id `{provider_id}`")
            }
            DeclarativeProviderError::InvalidBaseUrl { base_url } => {
                write!(formatter, "invalid provider baseUrl `{base_url}`")
            }
            DeclarativeProviderError::InvalidModels => {
                formatter.write_str("declarative provider `models` must be an array")
            }
            DeclarativeProviderError::NoUsableModels => {
                formatter.write_str("declarative provider has no usable models")
            }
        }
    }
}

impl std::error::Error for DeclarativeProviderError {}

/// 加载结果：可用模型 + 被跳过的坏条目诊断。
#[derive(Debug, Clone, PartialEq)]
pub struct DeclarativeLoadResult {
    /// 通过校验的 provider（`models` 只含好条目，未知字段已保留）。
    pub provider: DeclarativeProvider,
    /// 坏条目诊断，按原始下标顺序。
    pub diagnostics: Vec<DeclarativeDiagnostic>,
}

impl DeclarativeLoadResult {
    /// 被跳过的模型数量。
    pub fn skipped_models(&self) -> usize {
        self.diagnostics.len()
    }
}

/// provider id / model id 的字符集：字母数字开头，后续允许 `[A-Za-z0-9._-]`。
pub fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn validate_base_url(base_url: &str) -> bool {
    url::Url::parse(base_url)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
}

fn validate_model_capabilities(capabilities: &CapabilitySet) -> Result<(), String> {
    if capabilities.operations.is_empty() {
        return Err("capabilities.operations must not be empty".to_string());
    }
    Ok(())
}

fn validate_token_budget(name: &str, value: Option<u64>) -> Result<(), String> {
    match value {
        Some(0) => Err(format!("{name} must be greater than zero")),
        _ => Ok(()),
    }
}

/// 解析并校验一个声明式 provider。
///
/// 顶层结构/provider id/base URL/models 形状错误返回 [`DeclarativeProviderError`]；
/// 单条模型错误进入 [`DeclarativeLoadResult::diagnostics`] 并被跳过。
pub fn load_declarative_provider(
    value: &Value,
) -> Result<DeclarativeLoadResult, DeclarativeProviderError> {
    let object = value
        .as_object()
        .ok_or(DeclarativeProviderError::NotAnObject)?;

    let provider_id = object
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_valid_identifier(provider_id) {
        return Err(DeclarativeProviderError::InvalidProviderId {
            provider_id: provider_id.to_string(),
        });
    }
    if let Some(base_url) = object.get("baseUrl").and_then(Value::as_str)
        && !validate_base_url(base_url)
    {
        return Err(DeclarativeProviderError::InvalidBaseUrl {
            base_url: base_url.to_string(),
        });
    }

    let raw_models = match object.get("models") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(models)) => models.clone(),
        Some(_) => return Err(DeclarativeProviderError::InvalidModels),
    };

    let mut models = Vec::with_capacity(raw_models.len());
    let mut diagnostics = Vec::new();
    let mut seen_ids = std::collections::BTreeSet::new();

    for (index, raw_model) in raw_models.iter().enumerate() {
        match parse_model(raw_model) {
            Ok(model) => {
                if !is_valid_identifier(&model.id) {
                    diagnostics.push(DeclarativeDiagnostic {
                        index,
                        model_id: Some(model.id.clone()),
                        code: DeclarativeDiagnosticCode::InvalidModelId,
                        message: format!("invalid model id `{}`", model.id),
                    });
                    continue;
                }
                if !seen_ids.insert(model.id.clone()) {
                    diagnostics.push(DeclarativeDiagnostic {
                        index,
                        model_id: Some(model.id.clone()),
                        code: DeclarativeDiagnosticCode::DuplicateModelId,
                        message: format!("duplicate model id `{}`", model.id),
                    });
                    continue;
                }
                if let Err(message) = validate_model_capabilities(&model.capabilities) {
                    diagnostics.push(DeclarativeDiagnostic {
                        index,
                        model_id: Some(model.id.clone()),
                        code: DeclarativeDiagnosticCode::InvalidCapability,
                        message,
                    });
                    continue;
                }
                if let Err(message) = validate_token_budget("maxOutput", model.max_output)
                    .and_then(|_| validate_token_budget("contextWindow", model.context_window))
                {
                    diagnostics.push(DeclarativeDiagnostic {
                        index,
                        model_id: Some(model.id.clone()),
                        code: DeclarativeDiagnosticCode::InvalidTokenBudget,
                        message,
                    });
                    continue;
                }
                models.push(model);
            }
            Err((model_id, code, message)) => diagnostics.push(DeclarativeDiagnostic {
                index,
                model_id,
                code,
                message,
            }),
        }
    }

    if models.is_empty() {
        return Err(DeclarativeProviderError::NoUsableModels);
    }

    // 顶层已知字段之外的键全部进入 extra；provider 级 models 已由上面的净化结果替换。
    let mut extra = IndexMap::new();
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "providerId" | "name" | "baseUrl" | "api" | "capabilities" | "models"
        ) {
            continue;
        }
        extra.insert(key.clone(), value.clone());
    }

    let provider = DeclarativeProvider {
        provider_id: provider_id.to_string(),
        name: object
            .get("name")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        base_url: object
            .get("baseUrl")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        api: object
            .get("api")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        capabilities: object
            .get("capabilities")
            .cloned()
            .map(|value| serde_json::from_value(value).unwrap_or_default())
            .unwrap_or_default(),
        models,
        extra,
    };

    Ok(DeclarativeLoadResult {
        provider,
        diagnostics,
    })
}

type ModelParseError = (Option<String>, DeclarativeDiagnosticCode, String);

fn parse_model(value: &Value) -> Result<DeclarativeModel, ModelParseError> {
    let object = value.as_object().ok_or_else(|| {
        (
            None,
            DeclarativeDiagnosticCode::InvalidModelEntry,
            "model entry must be an object".to_string(),
        )
    })?;
    let id = object.get("id").and_then(Value::as_str).ok_or_else(|| {
        (
            None,
            DeclarativeDiagnosticCode::InvalidModelEntry,
            "model entry is missing a string `id`".to_string(),
        )
    })?;

    let capabilities = match object.get("capabilities") {
        None | Some(Value::Null) => CapabilitySet::default(),
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            (
                Some(id.to_string()),
                DeclarativeDiagnosticCode::InvalidCapability,
                format!("invalid capabilities: {error}"),
            )
        })?,
    };

    let max_output = optional_u64(object, "maxOutput", id)?;
    let context_window = optional_u64(object, "contextWindow", id)?;

    let mut extra = IndexMap::new();
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "id" | "name" | "capabilities" | "maxOutput" | "contextWindow"
        ) {
            continue;
        }
        extra.insert(key.clone(), value.clone());
    }

    Ok(DeclarativeModel {
        id: id.to_string(),
        name: object
            .get("name")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        capabilities,
        max_output,
        context_window,
        extra,
    })
}

fn optional_u64(
    object: &serde_json::Map<String, Value>,
    key: &str,
    model_id: &str,
) -> Result<Option<u64>, ModelParseError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            (
                Some(model_id.to_string()),
                DeclarativeDiagnosticCode::InvalidTokenBudget,
                format!("`{key}` must be a non-negative integer"),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_manifest_roundtrips_unknown_fields() {
        let raw = json!({
            "providerId": "acme",
            "name": "Acme",
            "baseUrl": "https://api.acme.test/v1",
            "api": "openai-completions",
            "x-vendor": {"routing": "fast"},
            "capabilities": {
                "operations": ["language"],
                "tools": true
            },
            "models": [
                {
                    "id": "acme-chat",
                    "name": "Acme Chat",
                    "contextWindow": 128000,
                    "maxOutput": 16384,
                    "futureField": [1, 2, 3],
                    "capabilities": {
                        "operations": ["language"],
                        "jsonOutput": true
                    }
                }
            ]
        });
        let loaded = load_declarative_provider(&raw).expect("load");
        assert!(loaded.diagnostics.is_empty());
        let provider = loaded.provider;
        assert_eq!(provider.provider_id, "acme");
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].max_output, Some(16_384));
        assert_eq!(
            provider.extra.get("x-vendor"),
            Some(&json!({"routing": "fast"}))
        );
        assert_eq!(
            provider.models[0].extra.get("futureField"),
            Some(&json!([1, 2, 3]))
        );

        let serialized = serde_json::to_value(&provider).expect("serialize");
        assert_eq!(serialized["x-vendor"], json!({"routing": "fast"}));
        assert_eq!(serialized["models"][0]["futureField"], json!([1, 2, 3]));
        // 已知字段没有丢失。
        assert_eq!(serialized["providerId"], json!("acme"));
        assert_eq!(
            serialized["models"][0]["capabilities"]["jsonOutput"],
            json!(true)
        );
        // 值级往返（键序不参与比较）。
        let reparsed: DeclarativeProvider = serde_json::from_value(serialized).expect("reparse");
        assert_eq!(reparsed, provider);
    }

    #[test]
    fn bad_model_entries_are_skipped_with_diagnostics() {
        let raw = json!({
            "providerId": "acme",
            "models": [
                {"id": "good-1", "capabilities": {"operations": ["language"]}},
                {"id": "bad id!", "capabilities": {"operations": ["language"]}},
                {"id": "good-2", "capabilities": {"operations": ["unknown-operation"]}},
                {"id": "good-1", "capabilities": {"operations": ["language"]}},
                {"id": "good-3", "capabilities": {"operations": ["image-generation"]}, "maxOutput": 0},
                {"id": "good-4", "capabilities": {"operations": ["language"]}, "contextWindow": "big"}
            ]
        });
        let loaded = load_declarative_provider(&raw).expect("load");
        assert_eq!(
            loaded
                .provider
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["good-1"]
        );
        let codes = loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            vec![
                DeclarativeDiagnosticCode::InvalidModelId,
                DeclarativeDiagnosticCode::InvalidCapability,
                DeclarativeDiagnosticCode::DuplicateModelId,
                DeclarativeDiagnosticCode::InvalidTokenBudget,
                DeclarativeDiagnosticCode::InvalidTokenBudget,
            ]
        );
        assert_eq!(loaded.skipped_models(), 5);
    }

    #[test]
    fn provider_level_errors_fail_closed() {
        assert_eq!(
            load_declarative_provider(&json!([1, 2])),
            Err(DeclarativeProviderError::NotAnObject)
        );
        assert_eq!(
            load_declarative_provider(&json!({"providerId": "", "models": []})),
            Err(DeclarativeProviderError::InvalidProviderId {
                provider_id: String::new()
            })
        );
        assert_eq!(
            load_declarative_provider(&json!({
                "providerId": "acme",
                "baseUrl": "file:///etc/passwd",
                "models": [{"id": "m"}]
            })),
            Err(DeclarativeProviderError::InvalidBaseUrl {
                base_url: "file:///etc/passwd".to_string()
            })
        );
        assert_eq!(
            load_declarative_provider(&json!({"providerId": "acme", "models": "nope"})),
            Err(DeclarativeProviderError::InvalidModels)
        );
        assert_eq!(
            load_declarative_provider(&json!({
                "providerId": "acme",
                "models": [{"id": "bad id", "capabilities": {"operations": ["language"]}}]
            })),
            Err(DeclarativeProviderError::NoUsableModels)
        );
    }

    #[test]
    fn identifiers_are_checked() {
        assert!(is_valid_identifier("anthropic"));
        assert!(is_valid_identifier("acme-1.2_x"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("-starts-with-dash"));
        assert!(!is_valid_identifier("has space"));
        assert!(!is_valid_identifier("中文"));
    }
}
