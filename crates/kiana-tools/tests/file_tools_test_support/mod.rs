//! file-tools 单元测试的共享上下文。
//!
//! `kiana-tools` 的集成测试各自是独立 crate；这个目录模块提供：
//!
//! - [`TestContext`]：只覆写 `cwd` / `model` 的最小 [`ExtensionContextApi`]，
//!   其余读取委托给 [`kiana_tools::definition::missing_tool_context`]（占位实现）。
//! - [`execute`] / [`execute_with_signal`]：直接调用 `ToolDefinition::execute` 的便捷函数。
//!
//! 该文件是本单元新增的测试辅助（不在 `ownedPaths` 的逐文件清单里，但属于 `tests/`，
//! 命名带 `file_tools_` 前缀以避免与其它单元的测试辅助冲突）。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use kiana_agent_core::{AgentTool, AgentToolCallOutcome, AgentToolError, AgentToolResult};
use kiana_ai::types::{JsonValue, Model};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_ext_api::BoxFuture;
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, ExecuteToolOptions, ExtensionContextApi, ExtensionMode,
    ExtensionToolContextApi,
};
use kiana_ext_api::ui::ExtensionUiContext;
use kiana_session_core::session_manager::ReadonlySessionManager;
use tokio_util::sync::CancellationToken;

/// 占位上下文的进程级实例（`model_registry` 等未覆写成员从这里借用）。
fn fallback() -> &'static dyn ExtensionToolContextApi {
    static MISSING: OnceLock<Arc<dyn ExtensionToolContextApi>> = OnceLock::new();
    MISSING
        .get_or_init(kiana_tools::definition::missing_tool_context)
        .as_ref()
}

/// 只覆写 `cwd` 与 `model` 的测试上下文。
pub struct TestContext {
    /// `ctx.cwd`。
    pub cwd: PathBuf,
    /// `ctx.model`。
    pub model: Option<Model>,
}

impl TestContext {
    /// 以给定 cwd 创建（`Arc<dyn ExtensionToolContextApi>`）。
    ///
    /// 返回 trait object 而不是 `Self` 是有意的：工具执行入口只接受
    /// `Arc<dyn ExtensionToolContextApi>`。
    #[allow(clippy::new_ret_no_self)]
    pub fn new(cwd: impl Into<PathBuf>) -> Arc<dyn ExtensionToolContextApi> {
        Arc::new(TestContext {
            cwd: cwd.into(),
            model: None,
        })
    }

    /// 带模型的上下文。
    pub fn with_model(cwd: impl Into<PathBuf>, model: Model) -> Arc<dyn ExtensionToolContextApi> {
        Arc::new(TestContext {
            cwd: cwd.into(),
            model: Some(model),
        })
    }
}

impl ExtensionContextApi for TestContext {
    fn ui(&self) -> &dyn ExtensionUiContext {
        fallback().ui()
    }

    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Tui
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn session_manager(&self) -> &dyn ReadonlySessionManager {
        fallback().session_manager()
    }

    fn model_registry(&self) -> &ModelRegistry {
        fallback().model_registry()
    }

    fn model(&self) -> Option<Model> {
        self.model.clone()
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        Vec::new()
    }

    fn thinking_level(&self) -> Option<kiana_agent_core::ThinkingLevel> {
        None
    }

    fn is_idle(&self) -> bool {
        true
    }

    fn is_project_trusted(&self) -> bool {
        false
    }

    fn signal(&self) -> Option<CancellationToken> {
        None
    }

    fn abort(&self) {}

    fn has_pending_messages(&self) -> bool {
        false
    }

    fn shutdown(&self) {}

    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }

    fn compact(&self, _options: Option<CompactOptions>) {}

    fn get_system_prompt(&self) -> String {
        String::new()
    }
}

impl ExtensionToolContextApi for TestContext {
    fn tools(&self) -> Vec<AgentTool> {
        Vec::new()
    }

    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        args: JsonValue,
        options: Option<ExecuteToolOptions>,
    ) -> BoxFuture<'a, AgentToolCallOutcome> {
        fallback().execute_tool(name, args, options)
    }
}

/// 执行一次工具调用（无中止信号）。
pub async fn execute(
    definition: &ToolDefinition,
    params: JsonValue,
    ctx: &Arc<dyn ExtensionToolContextApi>,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    definition
        .execute
        .execute("test-call", params, None, None, Arc::clone(ctx))
        .await
}

/// 执行一次工具调用（带中止信号）。
pub async fn execute_with_signal(
    definition: &ToolDefinition,
    params: JsonValue,
    signal: &CancellationToken,
    ctx: &Arc<dyn ExtensionToolContextApi>,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    definition
        .execute
        .execute("test-call", params, Some(signal), None, Arc::clone(ctx))
        .await
}

/// 结果里的文本块拼接（测试辅助，等价 TS 的 `getTextOutput`）。
pub fn text_output(result: &AgentToolResult<JsonValue>) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            kiana_ai::types::UserContentBlock::Text(text) => Some(text.text.clone()),
            kiana_ai::types::UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 结果里的图片块。
pub fn image_blocks(result: &AgentToolResult<JsonValue>) -> Vec<&kiana_ai::types::ImageContent> {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            kiana_ai::types::UserContentBlock::Image(image) => Some(image),
            kiana_ai::types::UserContentBlock::Text(_) => None,
        })
        .collect()
}

/// 带 `input` 模态（或空）的模型，用于图片相关测试。
pub fn test_model(inputs: &[kiana_ai::types::ModelInputModality]) -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: kiana_ai::types::Api::default(),
        provider: kiana_ai::types::ProviderId::default(),
        base_url: "https://example.com".to_string(),
        input: inputs.to_vec(),
        input_limits: None,
        cost: kiana_ai::types::ModelCost::default(),
        headers: None,
        model_type: None,
        reasoning: None,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: None,
        max_tokens: None,
        sampling_params: None,
        ..Model::default()
    }
}
