//! WebSearch / WebFetch 工具定义（第 12 周 web 集成的工具表接线）。
//!
//! `web::search`/`web::fetch` 是库层实现；本模块把它们包装成
//! [`ToolDefinition`](kiana_ext_api::types::ToolDefinition)，由
//! [`web_tools_extension`] 注册到扩展工具表。
//!
//! # 默认激活
//!
//! `web_fetch` 只依赖网络与 SSRF 校验，默认激活。
//!
//! `web_search` 需要 `BRAVE_API_KEY`/`GOOGLE_CSE_API_KEY`+`GOOGLE_CSE_CX`/
//! `BING_API_KEY` 之一：**注册时**按环境变量决定 `default_active`，有 key 才默认激活，
//! 避免没有 key 的会话多出永远失败的工具。需要强制启用/禁用时仍可用
//! `--tools web_search` / `--exclude-tools web_search` 覆盖。
//!
//! # 安全边界
//!
//! WebFetch 的每个 URL 都经过 `fetch::fetch_urls` 的三层 SSRF 校验；本模块不直接
//! 调用传输层。WebSearch 的端点来自引擎常量/配置，不做 DNS 预解析。

use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::types::{AgentToolError, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{TextContent, TextKind, UserContentBlock};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute};
use kiana_ext_api::types::{ExtensionApi, ExtensionFactory, InlineExtension};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::web::fetch::{FetchOptions, MAX_FETCH_URLS, fetch_urls};
use crate::web::http::{ReqwestWebTransport, WebTransport};
use crate::web::search::{
    BingSearchEngine, BraveSearchEngine, GoogleCseEngine, SearchChain, SearchEngine, SearchQuery,
};

/// 注册 `web_search`/`web_fetch` 的内联扩展工厂。
pub fn web_tools_extension() -> InlineExtension {
    let factory: ExtensionFactory = Arc::new(|kiana: Arc<dyn ExtensionApi>| {
        Box::pin(async move {
            kiana.register_tool(web_search_tool_definition());
            kiana.register_tool(web_fetch_tool_definition());
        })
    });
    InlineExtension::Named {
        name: "web-tools".to_string(),
        factory,
        // 扩展本身保持隐藏：它的价值在工具表里，不需要在扩展列表占一行。
        hidden: Some(true),
        replaceable: None,
        builtin: None,
    }
}

/// 是否至少配置了一个官方搜索引擎 API key。
///
/// Brave 只需 `BRAVE_API_KEY`；Google CSE 需要 `GOOGLE_CSE_API_KEY` +
/// `GOOGLE_CSE_CX` 成对出现；Bing 只需 `BING_API_KEY`。
pub fn search_api_key_configured() -> bool {
    fn present(name: &str) -> bool {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    }
    present("BRAVE_API_KEY")
        || present("BING_API_KEY")
        || (present("GOOGLE_CSE_API_KEY") && present("GOOGLE_CSE_CX"))
}

fn tool_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: None,
        stack: None,
    }
}

fn text_result(text: String, details: Value) -> AgentToolResult<Value> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text,
            text_signature: None,
        })],
        details,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn object_schema(properties: Value, required: Vec<&str>) -> Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `web_search` 工具定义。
pub fn web_search_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".to_string(),
        label: "Web Search".to_string(),
        description: "Search the web through the configured search engine chain \
            (Brave / Google CSE / Bing). Returns normalized title, URL and snippet."
            .to_string(),
        prompt_snippet: Some("search the web".to_string()),
        prompt_guidelines: None,
        parameters: object_schema(
            serde_json::json!({
                "query": {"type": "string", "description": "Search query"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20},
            }),
            vec!["query"],
        ),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        // 注册时按环境变量决定：至少配置一个官方搜索 API key 才默认激活。
        default_active: Some(search_api_key_configured()),
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(WebSearchExecutor),
        render_call: None,
        render_result: None,
    }
}

/// `web_fetch` 工具定义。
pub fn web_fetch_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "web_fetch".to_string(),
        label: "Web Fetch".to_string(),
        description: "Fetch one or more http(s) URLs in parallel, convert HTML to text \
            and truncate to 10,000 characters. Private/link-local/cloud-metadata \
            addresses are rejected."
            .to_string(),
        prompt_snippet: Some("fetch web pages".to_string()),
        prompt_guidelines: None,
        parameters: object_schema(
            serde_json::json!({
                "url": {"type": "string"},
                "urls": {
                    "type": "array",
                    "items": {"type": "string"},
                    "maxItems": MAX_FETCH_URLS,
                },
            }),
            Vec::new(),
        ),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        // 只依赖网络与 SSRF 校验，默认激活。
        default_active: Some(true),
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(WebFetchExecutor),
        render_call: None,
        render_result: None,
    }
}

struct WebSearchExecutor;

#[async_trait]
impl ToolDefinitionExecute for WebSearchExecutor {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: Value,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<Value>, AgentToolError>> {
        Box::pin(async move {
            let work = async {
                let query = params
                    .get("query")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or_else(|| tool_error("web_search requires a non-empty `query`"))?;
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|limit| (limit as usize).clamp(1, 20))
                    .unwrap_or(10);
                let chain = build_search_chain();
                let response = chain
                    .run(&SearchQuery {
                        query: query.to_string(),
                        limit,
                        recency: None,
                    })
                    .await
                    .map_err(|error| tool_error(error.to_string()))?;
                let mut text = String::new();
                for (index, result) in response.results.iter().enumerate() {
                    text.push_str(&format!(
                        "{}. {}\n   {}\n   {}\n",
                        index + 1,
                        result.title,
                        result.url,
                        result.snippet
                    ));
                }
                if text.is_empty() {
                    text.push_str("(no results)");
                }
                Ok(text_result(
                    text,
                    serde_json::to_value(&response).unwrap_or(Value::Null),
                ))
            };
            cancellable(signal, work).await
        })
    }
}

fn build_search_chain() -> SearchChain {
    let transport: Arc<dyn WebTransport> = Arc::new(ReqwestWebTransport::new());
    let brave = std::env::var("BRAVE_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(|key| {
            Arc::new(BraveSearchEngine::new(Arc::clone(&transport), key)) as Arc<dyn SearchEngine>
        });
    let google = match (
        std::env::var("GOOGLE_CSE_API_KEY"),
        std::env::var("GOOGLE_CSE_CX"),
    ) {
        (Ok(key), Ok(cx)) if !key.is_empty() && !cx.is_empty() => Some(Arc::new(
            GoogleCseEngine::new(Arc::clone(&transport), key, cx),
        )
            as Arc<dyn SearchEngine>),
        _ => None,
    };
    let bing = std::env::var("BING_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(|key| {
            Arc::new(BingSearchEngine::new(Arc::clone(&transport), key)) as Arc<dyn SearchEngine>
        });
    SearchChain::new(
        brave,
        google.into_iter().collect(),
        bing.into_iter().collect(),
    )
}

struct WebFetchExecutor;

#[async_trait]
impl ToolDefinitionExecute for WebFetchExecutor {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: Value,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn kiana_ext_api::types::ExtensionToolContextApi>,
    ) -> kiana_ext_api::BoxFuture<'a, Result<AgentToolResult<Value>, AgentToolError>> {
        Box::pin(async move {
            let work = async {
                let urls = fetch_urls_from_params(&params)?;
                if urls.is_empty() {
                    return Err(tool_error("web_fetch requires `url` or a non-empty `urls`"));
                }
                if urls.len() > MAX_FETCH_URLS {
                    return Err(tool_error(format!(
                        "web_fetch accepts at most {MAX_FETCH_URLS} URLs per call"
                    )));
                }
                let options = FetchOptions::new(Arc::new(ReqwestWebTransport::new()));
                let batch = fetch_urls(&urls, &options).await;
                let mut text = String::new();
                for page in &batch.pages {
                    text.push_str(&format!(
                        "## {} (status {})\n{}\n\n",
                        page.final_url, page.status, page.content
                    ));
                }
                for failure in &batch.failures {
                    text.push_str(&format!(
                        "## {} (failed)\n{}\n\n",
                        failure.url, failure.reason
                    ));
                }
                if text.is_empty() {
                    text.push_str("(no pages)");
                }
                let is_error = batch.pages.is_empty();
                let mut result =
                    text_result(text, serde_json::to_value(&batch).unwrap_or(Value::Null));
                if is_error {
                    result.is_error = Some(true);
                }
                Ok(result)
            };
            cancellable(signal, work).await
        })
    }
}

/// 在取消信号与工具工作之间做 `biased` 选择：已取消的调用即使工作已经就绪也优先
/// 返回 `Operation aborted`，与 read/find 等工具一致。
async fn cancellable<T, F>(signal: Option<&CancellationToken>, work: F) -> Result<T, AgentToolError>
where
    F: std::future::Future<Output = Result<T, AgentToolError>>,
{
    match signal {
        Some(token) => tokio::select! {
            biased;
            _ = token.cancelled() => Err(tool_error("Operation aborted")),
            result = work => result,
        },
        None => work.await,
    }
}

fn fetch_urls_from_params(params: &Value) -> Result<Vec<String>, AgentToolError> {
    if let Some(urls) = params.get("urls").and_then(Value::as_array)
        && !urls.is_empty()
    {
        return urls
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| tool_error("`urls` must contain strings"))
            })
            .collect();
    }
    if let Some(url) = params.get("url").and_then(Value::as_str) {
        return Ok(vec![url.to_string()]);
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_have_object_schemas() {
        for tool in [web_search_tool_definition(), web_fetch_tool_definition()] {
            assert!(tool.parameters.is_object(), "{}", tool.name);
            assert!(!tool.name.is_empty());
        }
    }

    #[test]
    fn web_fetch_is_active_by_default() {
        assert_eq!(web_fetch_tool_definition().default_active, Some(true));
    }

    #[test]
    fn web_search_default_activation_follows_api_key_presence() {
        // `search_api_key_configured` 与工具定义的默认值必须使用同一判定，避免
        // “有 key 但工具被隐藏”或“无 key 但工具默认激活”两类漂移。
        assert_eq!(
            web_search_tool_definition().default_active,
            Some(search_api_key_configured())
        );
    }

    #[test]
    fn fetch_params_accept_url_or_urls() {
        assert_eq!(
            fetch_urls_from_params(&serde_json::json!({"url": "https://example.com"}))
                .expect("url"),
            vec!["https://example.com".to_string()]
        );
        assert_eq!(
            fetch_urls_from_params(&serde_json::json!({"urls": ["https://a", "https://b"]}))
                .expect("urls"),
            vec!["https://a".to_string(), "https://b".to_string()]
        );
        // 空的 `urls` 不应遮蔽有效的 `url`（模型可能同时给出两个字段）。
        assert_eq!(
            fetch_urls_from_params(&serde_json::json!({"url": "https://example.com", "urls": []}))
                .expect("fallback to url"),
            vec!["https://example.com".to_string()]
        );
        assert!(
            fetch_urls_from_params(&serde_json::json!({"urls": [1]}))
                .expect_err("non-string")
                .message
                .contains("must contain strings")
        );
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn cancelled_signal_wins_over_ready_work() {
        let token = CancellationToken::new();
        token.cancel();
        let polls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&polls);
        let result = cancellable(Some(&token), async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, AgentToolError>(())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().message, "Operation aborted");
        assert_eq!(
            polls.load(Ordering::SeqCst),
            0,
            "cancelled work must not run"
        );
    }

    #[tokio::test]
    async fn work_runs_without_a_signal() {
        let result = cancellable(None, async { Ok::<_, AgentToolError>(42) }).await;
        assert_eq!(result.expect("work"), 42);
    }
}
