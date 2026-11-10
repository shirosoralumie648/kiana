//! Provider 能力矩阵：manifest 声明与 adapter 声明的双重能力门。
//!
//! 一次请求在真正发网络之前要同时满足两边：
//!
//! 1. provider manifest 声明模型支持该 operation（以及 tools / JSON 输出 / 并行工具 /
//!    vision）；
//! 2. 实际选择的 adapter 也声明实现了同一能力。
//!
//! 两边取交集：任何一边缺失都返回 [`UnsupportedCapability`]，错误里逐项列出缺失的能力
//! 以及缺失方（manifest / adapter）。这样能区分「模型目录写错」与「适配器没实现」，
//! 也让 text-only provider 的 image 请求在发出 HTTP 之前就失败。
//!
//! 设计参考（只借鉴行为，不复制代码）：cline `packages/llms/src/providers/model-operations.ts`
//! 的 `operationCapabilitiesSupportModel` 与 `providers/model-tools.ts` 的 fail-closed
//! 路由检查。Kiana 的模型目录没有 operation 字段，因此这里用显式的
//! [`ProviderCapabilityManifest`] 承载声明，由配置层/适配器注册表构造。

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{AiError, AiErrorKind};

/// provider 可执行的模型 operation。
///
/// 字符串与 cline 的 `ModelOperation` 一致（kebab-case）；未知值在反序列化时报错，
/// 避免把拼写错误静默降级成 `language`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelOperation {
    /// 文本/多模态对话。
    Language,
    /// 文生图 / 图生图。
    ImageGeneration,
    /// 语音合成。
    SpeechGeneration,
    /// 语音转文字。
    Transcription,
    /// 实时双工（WebSocket 等）。
    Realtime,
    /// 视频生成。
    VideoGeneration,
}

impl ModelOperation {
    /// 全部已知 operation，顺序固定。
    pub const ALL: [ModelOperation; 6] = [
        ModelOperation::Language,
        ModelOperation::ImageGeneration,
        ModelOperation::SpeechGeneration,
        ModelOperation::Transcription,
        ModelOperation::Realtime,
        ModelOperation::VideoGeneration,
    ];

    /// 稳定的 kebab-case 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ModelOperation::Language => "language",
            ModelOperation::ImageGeneration => "image-generation",
            ModelOperation::SpeechGeneration => "speech-generation",
            ModelOperation::Transcription => "transcription",
            ModelOperation::Realtime => "realtime",
            ModelOperation::VideoGeneration => "video-generation",
        }
    }

    /// 从字面量解析；未知值返回 `None`（反序列化路径会把它变成错误）。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "language" => Some(ModelOperation::Language),
            "image-generation" => Some(ModelOperation::ImageGeneration),
            "speech-generation" => Some(ModelOperation::SpeechGeneration),
            "transcription" => Some(ModelOperation::Transcription),
            "realtime" => Some(ModelOperation::Realtime),
            "video-generation" => Some(ModelOperation::VideoGeneration),
            _ => None,
        }
    }
}

impl Default for ModelOperation {
    fn default() -> Self {
        ModelOperation::Language
    }
}

impl fmt::Display for ModelOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一组能力声明。
///
/// `operations` 为空表示「未声明任何 operation」——能力门会 fail-closed（而不是把空集
/// 解释成「全部支持」）。布尔字段含义：
///
/// - `tools`：支持原生的函数/工具调用（不是普通文本）；
/// - `json_output`：支持结构化 JSON 输出；
/// - `parallel_tool_calls`：支持一轮并行多个 tool call；
/// - `vision`：支持图片输入。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySet {
    /// 支持的 operation 集合（BTreeSet 保证序列化顺序稳定）。
    #[serde(default)]
    pub operations: BTreeSet<ModelOperation>,
    /// 原生工具调用。
    #[serde(default)]
    pub tools: bool,
    /// 结构化 JSON 输出。
    #[serde(default)]
    pub json_output: bool,
    /// 并行工具调用。
    #[serde(default)]
    pub parallel_tool_calls: bool,
    /// 图片输入。
    #[serde(default)]
    pub vision: bool,
}

impl CapabilitySet {
    /// 空声明（fail-closed 的基线）。
    pub fn none() -> Self {
        Self::default()
    }

    /// 纯文本对话，不声明工具/JSON/并行/vision。
    pub fn text_only() -> Self {
        Self {
            operations: BTreeSet::from([ModelOperation::Language]),
            ..Self::default()
        }
    }

    /// 完整的文本对话能力：语言 operation + 工具 + JSON + 并行工具。
    ///
    /// 这是多数 chat 适配器的保守声明；vision 需要调用方显式打开。
    pub fn language_chat() -> Self {
        Self {
            operations: BTreeSet::from([ModelOperation::Language]),
            tools: true,
            json_output: true,
            parallel_tool_calls: true,
            vision: false,
        }
    }

    /// 图像生成能力（输入文本/图片，输出图片）。
    pub fn image_generation() -> Self {
        Self {
            operations: BTreeSet::from([ModelOperation::ImageGeneration]),
            ..Self::default()
        }
    }

    /// 加入一个 operation（builder 风格）。
    pub fn with_operation(mut self, operation: ModelOperation) -> Self {
        self.operations.insert(operation);
        self
    }

    /// 打开/关闭工具调用声明。
    pub fn with_tools(mut self, tools: bool) -> Self {
        self.tools = tools;
        self
    }

    /// 打开/关闭 JSON 输出声明。
    pub fn with_json_output(mut self, json_output: bool) -> Self {
        self.json_output = json_output;
        self
    }

    /// 打开/关闭并行工具调用声明。
    pub fn with_parallel_tool_calls(mut self, parallel: bool) -> Self {
        self.parallel_tool_calls = parallel;
        self
    }

    /// 打开/关闭 vision 声明。
    pub fn with_vision(mut self, vision: bool) -> Self {
        self.vision = vision;
        self
    }

    /// 是否声明了某个 operation。
    pub fn supports_operation(&self, operation: ModelOperation) -> bool {
        self.operations.contains(&operation)
    }
}

/// 单个模型在 manifest 里的能力声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelCapability {
    /// 模型 id（provider 内唯一）。
    pub id: String,
    /// 该模型声明的能力。
    #[serde(default)]
    pub capabilities: CapabilitySet,
}

/// provider manifest 的能力段。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilityManifest {
    /// provider 级能力（对所有未单独声明的模型生效）。
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// 模型级覆盖；按 id 精确匹配。
    #[serde(default)]
    pub models: Vec<ProviderModelCapability>,
}

impl ProviderCapabilityManifest {
    /// 取某模型的能力：模型级声明优先，缺失时回落到 provider 级。
    pub fn capabilities_for(&self, model_id: &str) -> &CapabilitySet {
        self.models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| &model.capabilities)
            .unwrap_or(&self.capabilities)
    }
}

/// 请求本身需要的能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestRequirements {
    /// 请求的 operation；默认 `language`。
    #[serde(default)]
    pub operation: ModelOperation,
    /// 请求携带工具定义。
    #[serde(default)]
    pub tools: bool,
    /// 请求要求 JSON 输出。
    #[serde(default)]
    pub json_output: bool,
    /// 请求允许并行工具调用。
    #[serde(default)]
    pub parallel_tool_calls: bool,
    /// 请求包含图片输入。
    #[serde(default)]
    pub vision: bool,
}

impl Default for RequestRequirements {
    fn default() -> Self {
        RequestRequirements {
            operation: ModelOperation::Language,
            tools: false,
            json_output: false,
            parallel_tool_calls: false,
            vision: false,
        }
    }
}

impl RequestRequirements {
    /// 对话请求，携带工具。
    pub fn chat_with_tools() -> Self {
        RequestRequirements {
            tools: true,
            ..Self::default()
        }
    }

    /// 图像生成请求。
    pub fn image_generation() -> Self {
        RequestRequirements {
            operation: ModelOperation::ImageGeneration,
            ..Self::default()
        }
    }

    /// 是否声明了任何能力需求。
    pub fn is_empty(&self) -> bool {
        self.operation == ModelOperation::Language
            && !self.tools
            && !self.json_output
            && !self.parallel_tool_calls
            && !self.vision
    }
}

/// 能力缺失的声明方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySide {
    /// provider manifest 没有声明。
    Manifest,
    /// adapter 没有声明实现。
    Adapter,
}

impl CapabilitySide {
    /// 稳定字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            CapabilitySide::Manifest => "manifest",
            CapabilitySide::Adapter => "adapter",
        }
    }
}

/// 一项缺失的能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingCapability {
    /// 能力名（`operation:<name>` / `tools` / `json_output` / `parallel_tool_calls` /
    /// `vision`）。
    pub capability: String,
    /// 缺失的声明方；两边都缺失时各有一条记录。
    pub side: CapabilitySide,
}

/// 请求被能力门拒绝。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsupportedCapability {
    /// 模型 id（诊断用）。
    pub model_id: String,
    /// 缺失项，按 manifest → adapter、声明顺序排列。
    pub missing: Vec<MissingCapability>,
}

impl UnsupportedCapability {
    /// 缺失项的稳定文本列表，例如 `manifest:operation:image-generation`。
    pub fn missing_labels(&self) -> Vec<String> {
        self.missing
            .iter()
            .map(|missing| format!("{}:{}", missing.side.as_str(), missing.capability))
            .collect()
    }
}

impl fmt::Display for UnsupportedCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "model `{}` does not support the request: missing {}",
            self.model_id,
            self.missing_labels().join(", ")
        )
    }
}

impl std::error::Error for UnsupportedCapability {}

impl From<UnsupportedCapability> for AiError {
    fn from(error: UnsupportedCapability) -> Self {
        AiError::new(AiErrorKind::Unsupported, error.to_string())
            .with_detail(serde_json::to_value(&error).unwrap_or(serde_json::Value::Null))
    }
}

/// 能力门：某个模型的 manifest 声明 + 已选 adapter 的声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityGate {
    /// 模型 id（用于错误文本）。
    pub model_id: String,
    /// manifest 侧声明。
    pub manifest: CapabilitySet,
    /// adapter 侧声明。
    pub adapter: CapabilitySet,
}

impl CapabilityGate {
    /// 用 manifest + adapter 构造门。
    pub fn new(
        model_id: impl Into<String>,
        manifest: CapabilitySet,
        adapter: CapabilitySet,
    ) -> Self {
        CapabilityGate {
            model_id: model_id.into(),
            manifest,
            adapter,
        }
    }

    /// 从 provider 级 manifest 与模型 id 构造。
    pub fn from_manifest(
        manifest: &ProviderCapabilityManifest,
        adapter: CapabilitySet,
        model_id: impl Into<String>,
    ) -> Self {
        let model_id = model_id.into();
        let capabilities = manifest.capabilities_for(&model_id).clone();
        CapabilityGate {
            model_id,
            manifest: capabilities,
            adapter,
        }
    }

    /// 请求必须同时满足 manifest 与 adapter；否则在发网络前返回
    /// [`UnsupportedCapability`]。
    pub fn authorize(
        &self,
        requirements: &RequestRequirements,
    ) -> Result<(), UnsupportedCapability> {
        let mut missing = Vec::new();

        if !self.manifest.supports_operation(requirements.operation) {
            missing.push(MissingCapability {
                capability: format!("operation:{}", requirements.operation),
                side: CapabilitySide::Manifest,
            });
        }
        if !self.adapter.supports_operation(requirements.operation) {
            missing.push(MissingCapability {
                capability: format!("operation:{}", requirements.operation),
                side: CapabilitySide::Adapter,
            });
        }

        let checks = [
            (
                "tools",
                requirements.tools,
                self.manifest.tools,
                self.adapter.tools,
            ),
            (
                "json_output",
                requirements.json_output,
                self.manifest.json_output,
                self.adapter.json_output,
            ),
            (
                "parallel_tool_calls",
                requirements.parallel_tool_calls,
                self.manifest.parallel_tool_calls,
                self.adapter.parallel_tool_calls,
            ),
            (
                "vision",
                requirements.vision,
                self.manifest.vision,
                self.adapter.vision,
            ),
        ];
        for (name, required, manifest, adapter) in checks {
            if !required {
                continue;
            }
            if !manifest {
                missing.push(MissingCapability {
                    capability: name.to_string(),
                    side: CapabilitySide::Manifest,
                });
            }
            if !adapter {
                missing.push(MissingCapability {
                    capability: name.to_string(),
                    side: CapabilitySide::Adapter,
                });
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            Err(UnsupportedCapability {
                model_id: self.model_id.clone(),
                missing,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_provider_rejects_image_generation_before_dispatch() {
        let gate = CapabilityGate::new(
            "text-model",
            CapabilitySet::text_only(),
            CapabilitySet::text_only(),
        );
        let error = gate
            .authorize(&RequestRequirements::image_generation())
            .expect_err("image generation is unsupported");
        assert_eq!(
            error.missing_labels(),
            vec![
                "manifest:operation:image-generation",
                "adapter:operation:image-generation",
            ]
        );
        let message = error.to_string();
        assert!(message.contains("image-generation"));
        assert!(message.contains("manifest"));
        assert!(message.contains("adapter"));
    }

    #[test]
    fn both_sides_must_declare_the_same_boolean_capability() {
        let manifest = CapabilitySet::language_chat();
        let adapter = CapabilitySet::language_chat().with_parallel_tool_calls(false);
        let gate = CapabilityGate::new("m", manifest.clone(), adapter);
        let requirements = RequestRequirements {
            parallel_tool_calls: true,
            ..RequestRequirements::default()
        };
        let error = gate.authorize(&requirements).expect_err("adapter missing");
        assert_eq!(error.missing_labels(), vec!["adapter:parallel_tool_calls"]);

        // 回头验证 manifest 缺失也能被指出。
        let gate = CapabilityGate::new(
            "m",
            CapabilitySet::text_only(),
            CapabilitySet::language_chat(),
        );
        let error = gate
            .authorize(&RequestRequirements::chat_with_tools())
            .expect_err("manifest missing tools");
        assert_eq!(error.missing_labels(), vec!["manifest:tools"]);
    }

    #[test]
    fn intersection_allows_matching_declarations() {
        let gate = CapabilityGate::new(
            "m",
            CapabilitySet::language_chat().with_vision(true),
            CapabilitySet::language_chat().with_vision(true),
        );
        let requirements = RequestRequirements {
            tools: true,
            json_output: true,
            parallel_tool_calls: true,
            vision: true,
            ..RequestRequirements::default()
        };
        assert!(gate.authorize(&requirements).is_ok());
    }

    #[test]
    fn model_level_manifest_overrides_provider_level() {
        let manifest = ProviderCapabilityManifest {
            capabilities: CapabilitySet::language_chat(),
            models: vec![ProviderModelCapability {
                id: "image-only".into(),
                capabilities: CapabilitySet::image_generation(),
            }],
        };
        let gate = CapabilityGate::from_manifest(
            &manifest,
            CapabilitySet::image_generation(),
            "image-only",
        );
        assert!(
            gate.authorize(&RequestRequirements::image_generation())
                .is_ok()
        );
        assert!(
            gate.authorize(&RequestRequirements::default()).is_err(),
            "language is not declared for the image-only model"
        );
    }

    #[test]
    fn unsupported_capability_maps_to_ai_error_kind_unsupported() {
        let error = UnsupportedCapability {
            model_id: "m".into(),
            missing: vec![MissingCapability {
                capability: "tools".into(),
                side: CapabilitySide::Manifest,
            }],
        };
        let ai_error: AiError = error.into();
        assert_eq!(ai_error.kind, AiErrorKind::Unsupported);
        assert!(ai_error.message.contains("tools"));
    }
}
