//! 第 6 周：工具输出预算 wrapper 的集成测试。
//!
//! 10MB bash 输出经 wrapper 后 < 50k 字符；完整内容落盘为 artifact，可按 offset
//! 逐字节回读。

use std::path::Path;
use std::sync::Arc;

use kiana_agent_core::types::AgentToolResult;
use kiana_agent_core::{AgentToolError, AgentToolUpdateCallback};
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_core::artifact_store::ArtifactStore;
use kiana_ext_api::BoxFuture;
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute};
use kiana_ext_api::types::ExtensionToolContextApi;
use kiana_tools::definition::{OutputBudgetOptions, wrap_tool_definition_with_output_budget};
use kiana_tools::read::create_read_tool_definition;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// 返回固定文本的假执行器。
struct FixedTextExecute {
    text: String,
}

impl ToolDefinitionExecute for FixedTextExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        _params: JsonValue,
        _signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            Ok(AgentToolResult {
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: self.text.clone(),
                    text_signature: None,
                })],
                details: json!({}),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

fn definition_with_text(cwd: &Path, name: &str, text: String) -> ToolDefinition {
    let mut definition = create_read_tool_definition(cwd.to_string_lossy(), None);
    definition.name = name.to_string();
    definition.execute = Arc::new(FixedTextExecute { text });
    definition
}

#[tokio::test]
async fn ten_megabyte_output_is_wrapped_under_50k_and_artifact_is_byte_exact() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path(), "session").unwrap());
    let content = "0123456789abcdef".repeat(640 * 1024); // 10 MiB
    assert_eq!(content.len(), 10 * 1024 * 1024);
    let tool = wrap_tool_definition_with_output_budget(
        definition_with_text(temp.path(), "bash", content.clone()),
        None,
        OutputBudgetOptions {
            store: Arc::clone(&store),
        },
    );
    let result = tool
        .execute
        .execute("call-1", json!({}), None, None)
        .await
        .expect("execute");

    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<String>();
    assert!(text.chars().count() < 50_000, "wrapper too large");
    assert!(text.contains("<persisted-output>"));
    assert!(text.contains("artifact://"));

    let details = result.details.as_object().expect("details object");
    let artifact = details.get("artifact").expect("artifact metadata");
    let id = artifact.get("id").unwrap().as_str().unwrap();
    let path = artifact.get("path").unwrap().as_str().unwrap();
    assert!(Path::new(path).exists());
    assert_eq!(store.read(id).unwrap().len(), content.len());
    let bytes = store.read_range_bytes(id, 123_456, 65_536).unwrap();
    assert_eq!(bytes, content.as_bytes()[123_456..123_456 + 65_536]);
}

#[tokio::test]
async fn small_output_is_returned_unchanged() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path(), "session").unwrap());
    let tool = wrap_tool_definition_with_output_budget(
        definition_with_text(temp.path(), "bash", "hello\n".to_string()),
        None,
        OutputBudgetOptions { store },
    );
    let result = tool
        .execute
        .execute("call-1", json!({}), None, None)
        .await
        .expect("execute");
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<String>();
    assert_eq!(text, "hello\n");
    assert!(
        result
            .details
            .as_object()
            .unwrap()
            .get("artifact")
            .is_none()
    );
}

#[tokio::test]
async fn edit_output_cap_is_one_hundred_thousand() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(ArtifactStore::new(temp.path(), "session").unwrap());
    let content = "e".repeat(120_000);
    let tool = wrap_tool_definition_with_output_budget(
        definition_with_text(temp.path(), "edit", content),
        None,
        OutputBudgetOptions { store },
    );
    let result = tool
        .execute
        .execute("call-1", json!({}), None, None)
        .await
        .expect("execute");
    let text = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<String>();
    assert!(text.chars().count() < 50_000);
    assert!(text.contains("Full output saved to"));
}
