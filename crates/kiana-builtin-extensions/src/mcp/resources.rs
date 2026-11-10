//! `extensions/mcp/resources.ts`：三个 MCP 资源工具，沿用 Codex 与 opencode 的工具形状。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/resources.ts`。
//!
//! `list_mcp_resources`、`list_mcp_resource_templates` 与 `read_mcp_resource` 接受一个 `server`
//! 参数，覆盖所有有资源的已连接服务器。列表是 JSON（与 Codex 一致）：
//! `{ server?, resources: [{ server, ...resource }], nextCursor? }`。指定 `server` 时列出一页，
//! `cursor` 续页；不指定时列出每个服务器的所有页。MCP App 资源（`ui://` URI 与
//! `profile=mcp-app` 的 HTML）以及图标不列出。读取的资源变成模型的文本与图片；二进制资源存到
//! 临时文件。脚本拿到的是 JSON 载荷。
//!
//! 已知差异：列出的条目与 `read_mcp_resource` 的 `contents` 来自 kiana-mcp 冻结的类型化
//! `Resource`/`ResourceContents`，已知字段的键序是结构体声明序而**不是**服务端的原始键序
//! （kiana-mcp 记录在案的契约限制）。整数样式的键已按 JS 自身键序（升序在前）重排；值相同，
//! 只有其余键的顺序可能不同。
//!
//! # 这个文件解决什么问题
//! MCP 服务器可以对外分享静态资源与参数化资源模板（文件、数据库 schema、应用数据等）。本文件
//! 把 `resources/list`、`resources/templates/list` 与 `resources/read` 包装成三个只读工具，
//! 沿用 Codex 与 opencode 的工具名和参数形状。
//!
//! # 在系统中的位置
//! 这三个工具由 `crate::mcp::index` 的 `sync_resource_tools` 注册：它计算带资源服务器的
//! 最宽 exposure，只在最宽 exposure 变化时才调用 [`create_mcp_resource_tool_definitions`]
//! 重建并重新注册；服务器的增删只通过实时连接表（[`McpResourceToolOptions`] 的 `servers`
//! 闭包）反映，不触发重建。本文件只依赖 [`McpResourceServer`] 抽象，
//! 生产实现是 `crate::mcp::runtime` 的 `McpServerConnection`；传输与重连由那一层负责，请求超时
//! 通过 `McpRequestOptions` 传下去，本文件不直接接触网络。
//!
//! # 一次调用的流程
//! 1. `ResourceToolExecutable::execute` 按工具种类分派到 `list_payload` 或 `read_resource`；
//! 2. `list_payload` 解析 `server` / `cursor`：指定服务器时只取一页，传输错误直接让工具失败；
//!    未指定时按服务器名排序后并行取每个服务器的所有页，单个失败只记入 `errors` 数组；
//! 3. `listed_value` 去掉 `_meta`/`icons`、补上 `server` 字段，`is_mcp_app_resource` 负责过滤；
//! 4. 载荷转成 `AgentToolResult`：列表工具把载荷序列化成 JSON 文本，读取工具把资源内容块
//!    （文本/图片等）交给模型；两者都经 `limit_mcp_content` 截断，未截断的载荷对象放进
//!    `structured_content` 供脚本读取。
//!
//! # 关键约束
//! - 参数与输出 schema 是模型可见的工具声明，是当前稳定契约（list 的参数对象在
//!   单元测试里被逐字断言）；改动键名、描述或 `required` 会改变工具契约。
//! - `servers` 闭包在每次调用时求值，不在工具定义创建时快照；只要最宽 exposure 不变，
//!   服务器增删无需重建定义（exposure 变化时 `index.rs` 会重建）。
//! - 列表与读取结果都是本文件重建的 JSON，不保证字节级等于服务端响应，键序限制见上一节。

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use kiana_agent_core::{AgentToolError, AgentToolResult};
use kiana_ai::types::JsonValue;
use kiana_ext_api::tool_types::{ToolAnnotations, ToolDefinition, ToolDefinitionExecute};
use kiana_ext_api::{BoxFuture, ExtensionToolContextApi};
use kiana_mcp::McpRequestOptions;
use kiana_mcp::auth_provider::McpTransportError;
use kiana_mcp::protocol_types::{
    ContentBlock, EmbeddedResourceContent, EmbeddedResourceKind, ListResourceTemplatesResult,
    ListResourcesResult, ReadResourceResult, Resource, ResourceTemplate,
};
use kiana_tui::autocomplete::locale_compare;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

pub use kiana_core::mcp_servers::{
    LIST_MCP_RESOURCE_TEMPLATES_TOOL, LIST_MCP_RESOURCES_TOOL, READ_MCP_RESOURCE_TOOL,
};

use crate::mcp::config::McpExposure;
use crate::mcp::tools::{
    ConvertMcpResultOptions, agent_error, details_json, limit_mcp_content, text_block,
    to_model_content, to_tool_exposure,
};

/// 一个提供资源的已连接服务器。TS: `resources.ts#McpResourceServer`。
///
/// 这是资源工具与传输层之间唯一的接缝：工具只通过本 trait 列目录和读内容，因此生产连接与
/// 测试替身可以互换。`name` 必须返回配置中的服务器名——工具的 `server` 参数按它精确匹配，
/// 结果里的 `server` 字段也由它填充；其余方法应把 `options` 原样交给底层客户端，让取消令牌
/// 与超时设置继续生效。
// `async-trait` 给每个方法生成 `#[must_use]`，clippy 的 `double_must_use` 看不到宏内属性
// （与工作区其它 `async_trait` trait 相同的局部例外）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait McpResourceServer: Send + Sync {
    /// 服务器名。
    ///
    /// 调用方用它与 `server` 参数做精确（区分大小写）比较，并把它写进每条结果的 `server`
    /// 字段；名字重复时 `find_server` 只取列表中的第一个。
    fn name(&self) -> &str;
    /// 每次请求的超时（毫秒）。
    ///
    /// 原样填入每个子请求的 `McpRequestOptions.timeout_ms`；`f64` 用于承载 TS number 的语义。
    fn timeout_ms(&self) -> f64;
    /// `resources/list` 的一页。
    ///
    /// `cursor` 为 `None` 表示第一页；返回的 `next_cursor` 会原样写进载荷的 `nextCursor`。
    /// 错误在单服务器路径上直接变成工具错误（整个调用失败），不会进入 `errors` 数组。
    async fn resources_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> Result<ListResourcesResult, McpTransportError>;
    /// `resources/templates/list` 的一页。
    ///
    /// 分页语义同 [`McpResourceServer::resources_page`]，只是结果类型换成模板。
    async fn resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> Result<ListResourceTemplatesResult, McpTransportError>;
    /// 所有资源页的合并结果。
    ///
    /// 只在未指定 `server` 的列表调用里使用，实现负责自行翻页并合并。这里返回的错误会被
    /// `list_payload` 记成 `{ server, error }` 条目，其余服务器继续处理。
    async fn all_resources(
        &self,
        options: &McpRequestOptions,
    ) -> Result<Vec<Resource>, McpTransportError>;
    /// 所有模板页的合并结果。
    ///
    /// 错误处理同 [`McpResourceServer::all_resources`]。
    async fn all_resource_templates(
        &self,
        options: &McpRequestOptions,
    ) -> Result<Vec<ResourceTemplate>, McpTransportError>;
    /// `resources/read`。
    ///
    /// `uri` 直接来自模型输入，本文件不校验它是否存在或是否属于该服务器；服务器返回的
    /// `contents` 会全部转换后交给模型。传输错误变成工具错误。
    async fn read_resource(
        &self,
        uri: &str,
        options: &McpRequestOptions,
    ) -> Result<ReadResourceResult, McpTransportError>;
}

/// MCP App 的用户界面（只有能渲染它们的宿主才用得上）。
/// TS: `resources.ts#isMcpAppResource` 的 `/;\s*profile\s*=\s*"?mcp-app"?/i`。
///
/// 显式写出 ECMAScript 的 `\s` 集合：Rust regex 的 `\s` 是 Unicode `White_Space`
/// （多出 U+0085），而 JS 的 `\s` 还包含 U+FEFF（ECMAScript WhiteSpace）。
///
/// 边界：模式里的 `;` 不是可选的，`text/html profile=mcp-app`（无分号）不会命中，与 TS 的
/// 同一正则一致；匹配只要求 `profile` 前有分号，不解析 MIME 的其余结构。
static MCP_APP_PROFILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        "(?i);[\\t\\n\\u{000B}\\u{000C}\\r \\u{00A0}\\u{1680}\\u{2000}-\\u{200A}\
         \\u{2028}\\u{2029}\\u{202F}\\u{205F}\\u{3000}\\u{FEFF}]*profile\
         [\\t\\n\\u{000B}\\u{000C}\\r \\u{00A0}\\u{1680}\\u{2000}-\\u{200A}\
         \\u{2028}\\u{2029}\\u{202F}\\u{205F}\\u{3000}\\u{FEFF}]*=\
         [\\t\\n\\u{000B}\\u{000C}\\r \\u{00A0}\\u{1680}\\u{2000}-\\u{200A}\
         \\u{2028}\\u{2029}\\u{202F}\\u{205F}\\u{3000}\\u{FEFF}]*\"?mcp-app\"?",
    )
    .expect("MCP App profile regex is valid")
});

/// 按给定顺序构造一个 JSON 对象。
///
/// 工作区的 `serde_json` 启用了 `preserve_order`，插入顺序就是序列化顺序；本文件的载荷与
/// schema 依赖这一点来复刻 TS 对象字面量的键序。
fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// `{ type: "string", description }`。
fn string_property(description: &str) -> Value {
    object([
        ("type", Value::String("string".into())),
        ("description", Value::String(description.into())),
    ])
}

/// `{ type: "string" }`。
fn optional_string() -> Value {
    object([("type", Value::String("string".into()))])
}

/// MCP App 资源不列出。TS: `resources.ts#isMcpAppResource`。
///
/// `item.uri ?? item.uriTemplate ?? ""`：模板可以带一个非标准的 `uri` 属性，它在 TS 里
/// **优先于** `uriTemplate`（`kiana-mcp` 把这类值留在 `extra` 里）。`uri` 不是字符串时 TS 的
/// `uri.startsWith` 抛 TypeError，整个列表调用失败。
///
/// 返回 `Ok(true)` 表示该条目应从列表中隐藏；唯一的错误来源是非字符串的 `extra["uri"]`
/// （见上）。`typed_mime_type` 只在 `extra` 没有 `mimeType` 时使用。
fn is_mcp_app_resource(
    typed_uri: Option<&str>,
    uri_template: Option<&str>,
    extra: &Map<String, Value>,
    typed_mime_type: Option<&str>,
) -> Result<bool, AgentToolError> {
    let extra_uri = match extra.get("uri") {
        None | Some(Value::Null) => None,
        Some(Value::String(uri)) => Some(uri.as_str()),
        Some(_) => return Err(agent_error("uri.startsWith is not a function")),
    };
    let uri = extra_uri.or(typed_uri).or(uri_template).unwrap_or("");
    if uri.starts_with("ui://") {
        return Ok(true);
    }
    // `item.mimeType ?? ""`：非字符串按 `String()` 转换后交给 `RegExp.test`（JS 的正则
    // 也会先把参数转成字符串）。
    let mime_type = match extra.get("mimeType") {
        None | Some(Value::Null) => typed_mime_type.unwrap_or("").to_string(),
        Some(value) => kiana_json::js_string::js_string(Some(value)),
    };
    Ok(MCP_APP_PROFILE.is_match(&mime_type))
}

/// 列出的资源/模板：去掉 `_meta` 与 `icons`，带上服务器名（`server` 在前）。
/// TS: `resources.ts#listed`（键序按 JS 自身键序：整数样式键升序在前）。
///
/// 只删除顶层的 `_meta`/`icons`；嵌套对象的字段不会被删除，但整个对象的键序仍会被
/// `sort_js_keys` 递归重排（见下方调用）。
fn listed_value<T: Serialize>(server: &str, item: &T) -> Value {
    let value = serde_json::to_value(item).unwrap_or(Value::Null);
    let mut listed = Map::new();
    listed.insert("server".to_string(), Value::String(server.to_string()));
    if let Value::Object(map) = value {
        for (key, item) in map {
            if key == "_meta" || key == "icons" {
                continue;
            }
            listed.insert(key, item);
        }
    }
    kiana_json::js_object::sort_js_keys(&Value::Object(listed))
}

/// 一个服务器的可见资源（过滤 MCP App；过滤时的 TypeError 向上传播）。
/// TS: `resources.ts#list` 的 `result.items.filter(visible).map(...)`。
///
/// 输入顺序保持不变（只筛选，不排序）；过滤出错会中止整个列表调用，而不是跳过那一项。
fn listed_resources(server: &str, resources: &[Resource]) -> Result<Vec<Value>, AgentToolError> {
    let mut items = Vec::with_capacity(resources.len());
    for item in resources {
        if !is_mcp_app_resource(
            Some(&item.uri),
            None,
            &item.extra,
            item.mime_type.as_deref(),
        )? {
            items.push(listed_value(server, item));
        }
    }
    Ok(items)
}

/// 一个服务器的可见模板（过滤规则同 [`listed_resources`]）。
fn listed_templates(
    server: &str,
    templates: &[ResourceTemplate],
) -> Result<Vec<Value>, AgentToolError> {
    let mut items = Vec::with_capacity(templates.len());
    for item in templates {
        if !is_mcp_app_resource(
            None,
            Some(&item.uri_template),
            &item.extra,
            item.mime_type.as_deref(),
        )? {
            items.push(listed_value(server, item));
        }
    }
    Ok(items)
}

/// `{ server, error }`。
fn error_entry(server: &str, error: String) -> Value {
    object([
        ("server", Value::String(server.to_string())),
        ("error", Value::String(error)),
    ])
}

/// `stringArgument(params, key)`：`null`/缺失 → `None`，非字符串抛错，空白 → `None`。
///
/// `params` 不是对象时按缺失处理。空白判定用 JS `trim()` 的字符集
/// （`kiana_json::js_string::js_trim`：去掉 U+FEFF，**不**去掉 U+0085 等），因此只含这些字符的
/// 字符串等同未提供。
fn string_argument(params: &Value, key: &str) -> Result<Option<String>, AgentToolError> {
    let value = params.as_object().and_then(|params| params.get(key));
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = kiana_json::js_string::js_trim(value);
            Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
        }
        Some(_) => Err(agent_error(format!("{key} must be a string"))),
    }
}

/// 找到有资源的服务器；找不到时按 TS 文案报错。
///
/// 服务器列表由调用方传入（通常是本次调用对 `servers` 闭包的一次快照）；名字按精确字符串
/// 匹配，错误消息里按传入顺序用 `, ` 拼接所有候选名。
fn find_server(
    servers: &[Arc<dyn McpResourceServer>],
    name: &str,
) -> Result<Arc<dyn McpResourceServer>, AgentToolError> {
    if let Some(server) = servers.iter().find(|server| server.name() == name) {
        return Ok(server.clone());
    }
    let available = servers
        .iter()
        .map(|server| server.name())
        .collect::<Vec<_>>()
        .join(", ");
    Err(agent_error(format!(
        "MCP server \"{name}\" has no resources{}",
        if available.is_empty() {
            String::new()
        } else {
            format!(". Servers with resources: {available}")
        }
    )))
}

/// 一次请求的选项：`{ signal, timeoutMs: server.timeoutMs }`。
///
/// `signal_reason` 与 `on_progress` 固定为 `None`：资源工具不注册进度回调，取消也不携带
/// 自定义原因（与 TS 只传 `signal`、`timeoutMs` 一致）。
fn request_options(signal: Option<&CancellationToken>, timeout_ms: f64) -> McpRequestOptions {
    McpRequestOptions {
        signal: signal.cloned(),
        signal_reason: None,
        timeout_ms: Some(timeout_ms),
        on_progress: None,
    }
}

/// JSON 结果（紧凑 JSON 作为模型文本；载荷作为 `structuredContent`）。
/// TS: `resources.ts#jsonResult`。
///
/// 模型文本是 JS 兼容的紧凑 JSON（键序按载荷构造顺序），超过上限且全文保存成功时由
/// `limit_mcp_content` 截断并把全文路径写进 `details.fullOutputPath`；保存失败时 details
/// 不带 `fullOutputPath`，模型文本改为"[Could not save the full output: …]"提示。
/// `structured_content` 始终是**未截断**的原始载荷。`server` 只用于 details，缺省写空串。
async fn json_result(
    tool: &str,
    server: Option<&str>,
    payload: Value,
) -> AgentToolResult<JsonValue> {
    let text = kiana_json::js_object::stringify(&payload);
    let (content, full_output_path) = limit_mcp_content(vec![text_block(text)], None).await;
    AgentToolResult {
        content,
        details: details_json(server.unwrap_or(""), tool, full_output_path.as_deref()),
        structured_content: Some(payload),
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// `list_mcp_resources` / `list_mcp_resource_templates` 的共享参数。
///
/// 两个列表工具只有三处不同：调用的 trait 方法、载荷键名（`resources` /
/// `resourceTemplates`）与结果类型；分页、排序、错误汇总与过滤逻辑完全共用。
#[derive(Clone, Copy)]
enum ListKind {
    Resources,
    Templates,
}

/// 返回当前可触达服务器的回调。TS: `servers: () => readonly McpResourceServer[]`。
///
/// 每次工具调用会求值一次并对其结果做快照，因此只要最宽 exposure 不变，会话中途的连接变化
/// 无需重建工具定义（exposure 变化时 `index.rs` 的 `sync_resource_tools` 会重建）。类型要求
/// `Send + Sync`，使同一个闭包能在异步执行器里被共享调用。
pub type McpResourceServersFn = Arc<dyn Fn() -> Vec<Arc<dyn McpResourceServer>> + Send + Sync>;

/// 一个资源工具的运行参数。TS: `createMcpResourceToolDefinitions({ exposure, servers })`。
///
/// `exposure` 对三个工具同时生效，由调用方按当前可用服务器的 exposure 取最宽值（见
/// `crate::mcp::index` 的 `sync_resource_tools`）；`servers` 是调用时闭包，允许配置热变化。
#[derive(Clone)]
pub struct McpResourceToolOptions {
    /// 三个工具的 exposure（`codemode` 会映射成 `deferred`）。
    pub exposure: McpExposure,
    /// 调用时可触达的服务器（每次调用重新取）。
    pub servers: McpResourceServersFn,
}

/// 执行一个资源工具。
///
/// 三个工具定义共用这一实现，用 `kind` 区分；`options` 是创建定义时克隆进来的不可变副本
/// （其中的 `servers` 闭包仍是实时的）。`execute` 不使用 `tool_call_id`、`on_update` 与
/// `ctx`：资源工具不上报进度（`request_options` 的 `on_progress` 固定为 `None`），也不
/// 调用其它工具，所以没有需要会话上下文的地方。
struct ResourceToolExecutable {
    options: McpResourceToolOptions,
    kind: ResourceToolKind,
}

/// 工具种类；决定 `execute` 的分派，也决定创建定义时选哪套参数/输出 schema。
#[derive(Clone, Copy)]
enum ResourceToolKind {
    ListResources,
    ListTemplates,
    ReadResource,
}

impl ToolDefinitionExecute for ResourceToolExecutable {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            // 列表工具先取载荷（这一步会读一次 `servers` 快照），再单独解析 `server`，只为
            // details 里的服务器名；读取工具的结果在 `read_resource` 里完整构造。
            match self.kind {
                ResourceToolKind::ListResources => {
                    let payload =
                        list_payload(&self.options, &params, signal, ListKind::Resources).await?;
                    let server = string_argument(&params, "server")?;
                    Ok(json_result(LIST_MCP_RESOURCES_TOOL, server.as_deref(), payload).await)
                }
                ResourceToolKind::ListTemplates => {
                    let payload =
                        list_payload(&self.options, &params, signal, ListKind::Templates).await?;
                    let server = string_argument(&params, "server")?;
                    Ok(
                        json_result(LIST_MCP_RESOURCE_TEMPLATES_TOOL, server.as_deref(), payload)
                            .await,
                    )
                }
                ResourceToolKind::ReadResource => {
                    read_resource(&self.options, &params, signal).await
                }
            }
        })
    }
}

/// 一个服务器的一页，或每个服务器的所有页。TS: `resources.ts#list`。
async fn list_payload(
    options: &McpResourceToolOptions,
    params: &Value,
    signal: Option<&CancellationToken>,
    kind: ListKind,
) -> Result<Value, AgentToolError> {
    let server_name = string_argument(params, "server")?;
    let cursor = string_argument(params, "cursor")?;
    // 路径 A：指定 server，只取一页。cursor 原样转发；传输错误直接让工具失败（与 TS 的
    // await 抛错一致），所以这里没有 `errors` 数组。
    if let Some(server_name) = server_name {
        let servers = (options.servers)();
        let server = find_server(&servers, &server_name)?;
        let request = request_options(signal, server.timeout_ms());
        let mut payload = Map::new();
        payload.insert(
            "server".to_string(),
            Value::String(server.name().to_string()),
        );
        match kind {
            ListKind::Resources => {
                let result = server
                    .resources_page(cursor.as_deref(), &request)
                    .await
                    .map_err(|error| agent_error(error.to_string()))?;
                let items = listed_resources(server.name(), &result.resources)?;
                payload.insert("resources".to_string(), Value::Array(items));
                if let Some(next_cursor) = result.next_cursor {
                    payload.insert("nextCursor".to_string(), Value::String(next_cursor));
                }
            }
            ListKind::Templates => {
                let result = server
                    .resource_templates_page(cursor.as_deref(), &request)
                    .await
                    .map_err(|error| agent_error(error.to_string()))?;
                let items = listed_templates(server.name(), &result.resource_templates)?;
                payload.insert("resourceTemplates".to_string(), Value::Array(items));
                if let Some(next_cursor) = result.next_cursor {
                    payload.insert("nextCursor".to_string(), Value::String(next_cursor));
                }
            }
        }
        return Ok(Value::Object(payload));
    }
    // 路径 B 前的校验：cursor 只能配合 server 使用。空白 cursor 已被 `string_argument` 归一成
    // `None`，所以这里不会把空串误报成"使用了 cursor"。
    if cursor.is_some() {
        return Err(agent_error(
            "cursor can only be used when a server is specified",
        ));
    }
    // 路径 B：未指定 server。先对本次快照按 `localeCompare` 排序，让 items/errors 中服务器的
    // 顺序稳定；`join_all` 保持输入顺序，因此可以用下标把结果对回服务器名。
    let mut servers = (options.servers)();
    servers.sort_by(|left, right| locale_compare(left.name(), right.name()));
    let requests = servers
        .iter()
        .map(|server| request_options(signal, server.timeout_ms()))
        .collect::<Vec<_>>();
    // TS 的 `Promise.allSettled`：所有服务器的请求并行；传输失败变成 `errors` 条目，但过滤器
    // （`isMcpAppResource`）的 TypeError 在集合之后同步抛出，整个调用失败。
    let mut items = Vec::new();
    let mut errors = Vec::new();
    match kind {
        ListKind::Resources => {
            let futures = servers.iter().zip(&requests).map(|(server, request)| {
                let server = server.clone();
                async move {
                    server
                        .all_resources(request)
                        .await
                        .map_err(|error| error.to_string())
                }
            });
            let results = futures::future::join_all(futures).await;
            for (index, result) in results.into_iter().enumerate() {
                let server = &servers[index];
                match result {
                    Ok(resources) => items.extend(listed_resources(server.name(), &resources)?),
                    Err(error) => errors.push(error_entry(server.name(), error)),
                }
            }
        }
        ListKind::Templates => {
            let futures = servers.iter().zip(&requests).map(|(server, request)| {
                let server = server.clone();
                async move {
                    server
                        .all_resource_templates(request)
                        .await
                        .map_err(|error| error.to_string())
                }
            });
            let results = futures::future::join_all(futures).await;
            for (index, result) in results.into_iter().enumerate() {
                let server = &servers[index];
                match result {
                    Ok(templates) => items.extend(listed_templates(server.name(), &templates)?),
                    Err(error) => errors.push(error_entry(server.name(), error)),
                }
            }
        }
    }
    let mut payload = Map::new();
    payload.insert(
        match kind {
            ListKind::Resources => "resources",
            ListKind::Templates => "resourceTemplates",
        }
        .to_string(),
        Value::Array(items),
    );
    if !errors.is_empty() {
        payload.insert("errors".to_string(), Value::Array(errors));
    }
    Ok(Value::Object(payload))
}

/// `read_mcp_resource.execute`。TS: `resources.ts#readResource.execute`。
async fn read_resource(
    options: &McpResourceToolOptions,
    params: &Value,
    signal: Option<&CancellationToken>,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    let server_name = string_argument(params, "server")?;
    let uri = string_argument(params, "uri")?;
    let Some(server_name) = server_name else {
        return Err(agent_error("server must be provided"));
    };
    let Some(uri) = uri else {
        return Err(agent_error("uri must be provided"));
    };
    let servers = (options.servers)();
    let server = find_server(&servers, &server_name)?;
    let request = request_options(signal, server.timeout_ms());
    let result = server
        .read_resource(&uri, &request)
        .await
        .map_err(|error| agent_error(error.to_string()))?;
    // 阶段 1：contents → 内嵌资源内容块；多于一条时先插入 `${uri}:` 文本标签，
    // 让模型能区分目录等返回的多份内容（单条时 TS 不加标签）。
    let multiple = result.contents.len() > 1;
    let mut blocks: Vec<ContentBlock> = Vec::new();
    for contents in &result.contents {
        if multiple {
            // TS: `${contents.uri}:`——缺失时是 `undefined:`，非字符串按 `String(value)`。
            let value = serde_json::to_value(contents).unwrap_or(Value::Null);
            let label = kiana_json::js_string::js_string(value.get("uri"));
            blocks.push(ContentBlock::Text(
                kiana_mcp::protocol_types::TextContent::new(format!("{label}:")),
            ));
        }
        blocks.push(ContentBlock::EmbeddedResource(EmbeddedResourceContent {
            kind: EmbeddedResourceKind::Resource,
            resource: contents.clone(),
            annotations: None,
            meta: None,
            extra: Map::new(),
        }));
    }
    // 阶段 2：MCP 内容块 → 模型内容。默认选项下，非文本、非图片的二进制资源写到临时文件；
    // 任一块转换抛错（例如 blob 字段类型不是字符串/数组）会让整个读取失败，而不是只丢那
    // 一块；注意非法 base64 字符会被宽松解码跳过，不构成失败。
    let converted =
        to_model_content(server.name(), &blocks, &ConvertMcpResultOptions::default()).await?;
    // 服务器返回零个内容块时给模型一句可读提示，避免空结果。
    let converted = if converted.is_empty() {
        vec![text_block(format!("Resource {uri} is empty."))]
    } else {
        converted
    };
    let (content, full_output_path) = limit_mcp_content(converted, None).await;
    // 阶段 3：脚本载荷。逐条去掉 `_meta`，再按 JS 键序重排（类型化重序列化的键序限制见
    // 模块说明）。
    let contents: Vec<Value> = result
        .contents
        .iter()
        .map(|contents| {
            let mut value = serde_json::to_value(contents).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut value {
                map.shift_remove("_meta");
            }
            kiana_json::js_object::sort_js_keys(&value)
        })
        .collect();
    let payload = object([
        ("server", Value::String(server.name().to_string())),
        ("uri", Value::String(uri)),
        ("contents", Value::Array(contents)),
    ]);
    Ok(AgentToolResult {
        content,
        details: details_json(
            server.name(),
            READ_MCP_RESOURCE_TOOL,
            full_output_path.as_deref(),
        ),
        structured_content: Some(payload),
        usage: None,
        is_error: None,
        terminate: None,
    })
}

/// 构造三个资源工具的定义。`servers` 在调用时返回它们能触达的服务器。
/// TS: `resources.ts#createMcpResourceToolDefinitions`。
///
/// 三个定义共享同一个 `options.exposure`（经 `to_tool_exposure` 映射，`codemode` 变成
/// `deferred`）、`label == name` 与 `read_only_hint: true`，并各带自己的输入/输出 schema。
/// 每次调用都返回新的执行器 `Arc`，但内部只持有 options 的克隆；定义本身不缓存服务器列表。
pub fn create_mcp_resource_tool_definitions(
    options: McpResourceToolOptions,
) -> Vec<ToolDefinition> {
    [
        (
            ResourceToolKind::ListResources,
            LIST_MCP_RESOURCES_TOOL,
            "Lists resources provided by MCP servers. Resources allow servers to share data that provides context to language models, such as files, database schemas, or application-specific information. Prefer resources over web search when possible.",
        ),
        (
            ResourceToolKind::ListTemplates,
            LIST_MCP_RESOURCE_TEMPLATES_TOOL,
            "Lists resource templates provided by MCP servers. Parameterized resource templates allow servers to share data that takes parameters and provides context to language models, such as files, database schemas, or application-specific information. Prefer resource templates over web search when possible.",
        ),
        (
            ResourceToolKind::ReadResource,
            READ_MCP_RESOURCE_TOOL,
            "Read a specific resource from an MCP server given the server name and resource URI.",
        ),
    ]
    .into_iter()
    .map(|(kind, name, description)| {
        let parameters = match kind {
            ResourceToolKind::ReadResource => read_parameters(),
            _ => list_parameters(),
        };
        let output_schema = match kind {
            ResourceToolKind::ListResources => list_output_schema(),
            ResourceToolKind::ListTemplates => list_templates_output_schema(),
            ResourceToolKind::ReadResource => read_output_schema(),
        };
        ToolDefinition {
            name: name.to_string(),
            label: name.to_string(),
            description: description.to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters,
            constrained_sampling: None,
            render_shell: None,
            prepare_arguments: None,
            output_schema: Some(output_schema),
            exposure: Some(to_tool_exposure(options.exposure)),
            namespace: None,
            reaches_tools: None,
            annotations: Some(ToolAnnotations {
                read_only_hint: Some(true),
                ..ToolAnnotations::default()
            }),
            default_active: None,
            prepare_loadout: None,
            execution_mode: None,
            execute: Arc::new(ResourceToolExecutable {
                options: options.clone(),
                kind,
            }),
            render_call: None,
            render_result: None,
        }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// 参数与输出 schema（逐字）
// ---------------------------------------------------------------------------

// 这些函数按 TS 对象字面量的插入顺序构造 JSON。schema 是模型看到的工具声明：list 的参数
// 对象在单元测试 `definitions_match_the_three_codex_tools` 里被逐字断言，read 的参数只断言
// `required`，输出 schema 无测试断言、仅按 TS 常量移植。不要为了"整理"重排键、合并分支或
// 改写描述文字。

/// `LIST_PARAMETERS`。
fn list_parameters() -> Value {
    object([
        ("type", Value::String("object".into())),
        (
            "properties",
            object([
                (
                    "server",
                    string_property("MCP server name. Omit to list every server with resources."),
                ),
                (
                    "cursor",
                    string_property(
                        "Opaque cursor from a previous call with the same server; omit for the first page.",
                    ),
                ),
            ]),
        ),
        ("additionalProperties", Value::Bool(false)),
    ])
}

/// `READ_PARAMETERS`。
fn read_parameters() -> Value {
    object([
        ("type", Value::String("object".into())),
        (
            "properties",
            object([
                (
                    "server",
                    string_property(
                        "MCP server name exactly as configured. Must match the 'server' field returned by list_mcp_resources.",
                    ),
                ),
                (
                    "uri",
                    string_property(
                        "Resource URI to read. Must be one of the URIs returned by list_mcp_resources.",
                    ),
                ),
            ]),
        ),
        (
            "required",
            Value::Array(vec![
                Value::String("server".into()),
                Value::String("uri".into()),
            ]),
        ),
        ("additionalProperties", Value::Bool(false)),
    ])
}

/// `LISTING_ERRORS`。
fn listing_errors() -> Value {
    object([
        ("type", Value::String("array".into())),
        (
            "description",
            Value::String("Servers that could not be listed".into()),
        ),
        (
            "items",
            object([
                ("type", Value::String("object".into())),
                (
                    "properties",
                    object([
                        ("server", object([("type", Value::String("string".into()))])),
                        ("error", object([("type", Value::String("string".into()))])),
                    ]),
                ),
                (
                    "required",
                    Value::Array(vec![
                        Value::String("server".into()),
                        Value::String("error".into()),
                    ]),
                ),
            ]),
        ),
    ])
}

/// `LIST_OUTPUT_SCHEMA`。
fn list_output_schema() -> Value {
    object([
        ("type", Value::String("object".into())),
        (
            "properties",
            object([
                ("server", optional_string()),
                (
                    "resources",
                    object([
                        ("type", Value::String("array".into())),
                        (
                            "items",
                            object([
                                ("type", Value::String("object".into())),
                                (
                                    "properties",
                                    object([
                                        (
                                            "server",
                                            object([("type", Value::String("string".into()))]),
                                        ),
                                        ("uri", object([("type", Value::String("string".into()))])),
                                        (
                                            "name",
                                            object([("type", Value::String("string".into()))]),
                                        ),
                                        ("title", optional_string()),
                                        ("description", optional_string()),
                                        ("mimeType", optional_string()),
                                        (
                                            "size",
                                            object([("type", Value::String("number".into()))]),
                                        ),
                                    ]),
                                ),
                                (
                                    "required",
                                    Value::Array(vec![
                                        Value::String("server".into()),
                                        Value::String("uri".into()),
                                        Value::String("name".into()),
                                    ]),
                                ),
                            ]),
                        ),
                    ]),
                ),
                ("nextCursor", optional_string()),
                ("errors", listing_errors()),
            ]),
        ),
        (
            "required",
            Value::Array(vec![Value::String("resources".into())]),
        ),
    ])
}

/// `LIST_TEMPLATES_OUTPUT_SCHEMA`。
fn list_templates_output_schema() -> Value {
    object([
        ("type", Value::String("object".into())),
        (
            "properties",
            object([
                ("server", optional_string()),
                (
                    "resourceTemplates",
                    object([
                        ("type", Value::String("array".into())),
                        (
                            "items",
                            object([
                                ("type", Value::String("object".into())),
                                (
                                    "properties",
                                    object([
                                        (
                                            "server",
                                            object([("type", Value::String("string".into()))]),
                                        ),
                                        (
                                            "uriTemplate",
                                            object([
                                                ("type", Value::String("string".into())),
                                                (
                                                    "description",
                                                    Value::String("RFC 6570 URI template".into()),
                                                ),
                                            ]),
                                        ),
                                        (
                                            "name",
                                            object([("type", Value::String("string".into()))]),
                                        ),
                                        ("title", optional_string()),
                                        ("description", optional_string()),
                                        ("mimeType", optional_string()),
                                    ]),
                                ),
                                (
                                    "required",
                                    Value::Array(vec![
                                        Value::String("server".into()),
                                        Value::String("uriTemplate".into()),
                                        Value::String("name".into()),
                                    ]),
                                ),
                            ]),
                        ),
                    ]),
                ),
                ("nextCursor", optional_string()),
                ("errors", listing_errors()),
            ]),
        ),
        (
            "required",
            Value::Array(vec![Value::String("resourceTemplates".into())]),
        ),
    ])
}

/// `READ_OUTPUT_SCHEMA`。
fn read_output_schema() -> Value {
    object([
        ("type", Value::String("object".into())),
        (
            "properties",
            object([
                ("server", object([("type", Value::String("string".into()))])),
                ("uri", object([("type", Value::String("string".into()))])),
                (
                    "contents",
                    object([
                        ("type", Value::String("array".into())),
                        (
                            "items",
                            object([(
                                "anyOf",
                                Value::Array(vec![
                                    object([
                                        ("type", Value::String("object".into())),
                                        (
                                            "properties",
                                            object([
                                                (
                                                    "uri",
                                                    object([(
                                                        "type",
                                                        Value::String("string".into()),
                                                    )]),
                                                ),
                                                ("mimeType", optional_string()),
                                                (
                                                    "text",
                                                    object([(
                                                        "type",
                                                        Value::String("string".into()),
                                                    )]),
                                                ),
                                            ]),
                                        ),
                                        (
                                            "required",
                                            Value::Array(vec![
                                                Value::String("uri".into()),
                                                Value::String("text".into()),
                                            ]),
                                        ),
                                    ]),
                                    object([
                                        ("type", Value::String("object".into())),
                                        (
                                            "properties",
                                            object([
                                                (
                                                    "uri",
                                                    object([(
                                                        "type",
                                                        Value::String("string".into()),
                                                    )]),
                                                ),
                                                ("mimeType", optional_string()),
                                                (
                                                    "blob",
                                                    object([
                                                        ("type", Value::String("string".into())),
                                                        (
                                                            "description",
                                                            Value::String("base64".into()),
                                                        ),
                                                    ]),
                                                ),
                                            ]),
                                        ),
                                        (
                                            "required",
                                            Value::Array(vec![
                                                Value::String("uri".into()),
                                                Value::String("blob".into()),
                                            ]),
                                        ),
                                    ]),
                                ]),
                            )]),
                        ),
                    ]),
                ),
            ]),
        ),
        (
            "required",
            Value::Array(vec![
                Value::String("server".into()),
                Value::String("uri".into()),
                Value::String("contents".into()),
            ]),
        ),
    ])
}

// 以下单元测试锁定 TS 对齐的可见行为：三个工具的名称/exposure/readOnly 标注、单服务器分页、
// 全服务器排序与 errors 汇总、MCP App 过滤（含 JS `\s` 字符集）、模板 URI 的优先级，以及
// read_mcp_resource 的多内容标签与空资源提示。`FakeServer` 只实现本文件的 McpResourceServer，
// 不经过真实传输层。
#[cfg(test)]
mod tests {
    use super::*;
    use kiana_mcp::protocol_types::{
        BlobResourceContents, ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult,
        Resource, ResourceContents, ResourceTemplate, TextResourceContents,
    };
    use serde_json::json;

    /// 一个可配置的假服务器。
    struct FakeServer {
        name: String,
        resources: Vec<Resource>,
        templates: Vec<ResourceTemplate>,
        contents: Vec<ResourceContents>,
        fail: bool,
    }

    impl FakeServer {
        fn new(name: &str) -> Self {
            FakeServer {
                name: name.to_string(),
                resources: Vec::new(),
                templates: Vec::new(),
                contents: Vec::new(),
                fail: false,
            }
        }
    }

    #[async_trait]
    impl McpResourceServer for FakeServer {
        fn name(&self) -> &str {
            &self.name
        }

        fn timeout_ms(&self) -> f64 {
            60_000.0
        }

        async fn resources_page(
            &self,
            cursor: Option<&str>,
            _options: &McpRequestOptions,
        ) -> Result<ListResourcesResult, McpTransportError> {
            if self.fail {
                return Err(McpTransportError::new("boom"));
            }
            Ok(ListResourcesResult {
                resources: self.resources.clone(),
                next_cursor: cursor.is_none().then(|| "next".to_string()),
                ..ListResourcesResult::default()
            })
        }

        async fn resource_templates_page(
            &self,
            cursor: Option<&str>,
            _options: &McpRequestOptions,
        ) -> Result<ListResourceTemplatesResult, McpTransportError> {
            if self.fail {
                return Err(McpTransportError::new("boom"));
            }
            Ok(ListResourceTemplatesResult {
                resource_templates: self.templates.clone(),
                next_cursor: cursor.is_none().then(|| "next".to_string()),
                ..ListResourceTemplatesResult::default()
            })
        }

        async fn all_resources(
            &self,
            _options: &McpRequestOptions,
        ) -> Result<Vec<Resource>, McpTransportError> {
            if self.fail {
                return Err(McpTransportError::new("boom"));
            }
            Ok(self.resources.clone())
        }

        async fn all_resource_templates(
            &self,
            _options: &McpRequestOptions,
        ) -> Result<Vec<ResourceTemplate>, McpTransportError> {
            if self.fail {
                return Err(McpTransportError::new("boom"));
            }
            Ok(self.templates.clone())
        }

        async fn read_resource(
            &self,
            _uri: &str,
            _options: &McpRequestOptions,
        ) -> Result<ReadResourceResult, McpTransportError> {
            if self.fail {
                return Err(McpTransportError::new("boom"));
            }
            Ok(ReadResourceResult {
                contents: self.contents.clone(),
                ..ReadResourceResult::default()
            })
        }
    }

    fn options(servers: Vec<Arc<dyn McpResourceServer>>) -> McpResourceToolOptions {
        McpResourceToolOptions {
            exposure: McpExposure::Codemode,
            servers: Arc::new(move || servers.clone()),
        }
    }

    fn resource(uri: &str, name: &str) -> Resource {
        Resource {
            uri: uri.to_string(),
            name: name.to_string(),
            ..Resource::default()
        }
    }

    fn text_contents(uri: &str, text: &str) -> ResourceContents {
        ResourceContents::Text(TextResourceContents {
            uri: uri.to_string(),
            mime_type: None,
            text: text.to_string(),
            meta: None,
            extra: Map::new(),
        })
    }

    #[test]
    fn definitions_match_the_three_codex_tools() {
        let definitions = create_mcp_resource_tool_definitions(options(Vec::new()));
        assert_eq!(definitions.len(), 3);
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                LIST_MCP_RESOURCES_TOOL,
                LIST_MCP_RESOURCE_TEMPLATES_TOOL,
                READ_MCP_RESOURCE_TOOL
            ]
        );
        for definition in &definitions {
            assert_eq!(definition.label, definition.name);
            assert_eq!(
                definition.exposure,
                Some(kiana_ext_api::tool_types::ToolExposure::Deferred)
            );
            assert_eq!(
                definition
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.read_only_hint),
                Some(true)
            );
        }
        assert_eq!(
            definitions[0].parameters,
            json!({
                "type": "object",
                "properties": {
                    "server": { "type": "string", "description": "MCP server name. Omit to list every server with resources." },
                    "cursor": { "type": "string", "description": "Opaque cursor from a previous call with the same server; omit for the first page." }
                },
                "additionalProperties": false
            })
        );
        assert_eq!(
            definitions[2].parameters["required"],
            json!(["server", "uri"])
        );
    }

    #[tokio::test]
    async fn lists_one_page_for_one_server_and_filters_mcp_apps_and_icons() {
        let mut app = resource("ui://widget", "widget");
        app.mime_type = Some("text/html; profile=mcp-app".to_string());
        let mut with_icons = resource("docs://guide", "guide");
        with_icons
            .extra
            .insert("icons".to_string(), json!([{ "src": "x" }]));
        let mut server = FakeServer::new("docs");
        server.resources = vec![resource("docs://a", "a"), app, with_icons];
        let options = options(vec![Arc::new(server)]);
        let payload = list_payload(
            &options,
            &json!({ "server": "docs" }),
            None,
            ListKind::Resources,
        )
        .await
        .expect("list");
        assert_eq!(
            payload,
            json!({
                "server": "docs",
                "resources": [
                    { "server": "docs", "uri": "docs://a", "name": "a" },
                    { "server": "docs", "uri": "docs://guide", "name": "guide" }
                ],
                "nextCursor": "next"
            })
        );
    }

    #[tokio::test]
    async fn lists_every_page_of_every_server_sorted_and_reports_failures() {
        let mut zed = FakeServer::new("zed");
        zed.resources = vec![resource("z://one", "one")];
        let mut alpha = FakeServer::new("alpha");
        alpha.resources = vec![resource("a://one", "one")];
        let mut broken = FakeServer::new("broken");
        broken.fail = true;
        let options = options(vec![Arc::new(zed), Arc::new(broken), Arc::new(alpha)]);
        let payload = list_payload(&options, &json!({}), None, ListKind::Resources)
            .await
            .expect("list");
        assert_eq!(
            payload,
            json!({
                "resources": [
                    { "server": "alpha", "uri": "a://one", "name": "one" },
                    { "server": "zed", "uri": "z://one", "name": "one" }
                ],
                "errors": [{ "server": "broken", "error": "boom" }]
            })
        );
    }

    #[tokio::test]
    async fn lists_resource_templates() {
        let mut server = FakeServer::new("docs");
        server.templates = vec![ResourceTemplate {
            uri_template: "docs://{id}".to_string(),
            name: "by id".to_string(),
            ..ResourceTemplate::default()
        }];
        let options = options(vec![Arc::new(server)]);
        let payload = list_payload(
            &options,
            &json!({ "server": "docs" }),
            None,
            ListKind::Templates,
        )
        .await
        .expect("list");
        assert_eq!(
            payload,
            json!({
                "server": "docs",
                "resourceTemplates": [
                    { "server": "docs", "uriTemplate": "docs://{id}", "name": "by id" }
                ],
                "nextCursor": "next"
            })
        );
    }

    #[tokio::test]
    async fn rejects_cursors_without_a_server_and_unknown_servers() {
        let options = options(vec![Arc::new(FakeServer::new("docs"))]);
        let error = list_payload(
            &options,
            &json!({ "cursor": "next" }),
            None,
            ListKind::Resources,
        )
        .await
        .expect_err("cursor without server");
        assert_eq!(
            error.message,
            "cursor can only be used when a server is specified"
        );

        let error = list_payload(
            &options,
            &json!({ "server": "missing" }),
            None,
            ListKind::Resources,
        )
        .await
        .expect_err("unknown server");
        assert_eq!(
            error.message,
            "MCP server \"missing\" has no resources. Servers with resources: docs"
        );

        let error = list_payload(&options, &json!({ "server": 5 }), None, ListKind::Resources)
            .await
            .expect_err("server must be a string");
        assert_eq!(error.message, "server must be a string");
    }

    #[tokio::test]
    async fn reads_multiple_contents_and_labels_each_with_its_uri() {
        let mut server = FakeServer::new("docs");
        server.contents = vec![
            text_contents("docs://a", "a text"),
            ResourceContents::Blob(BlobResourceContents {
                uri: "docs://b".to_string(),
                mime_type: Some("text/plain".to_string()),
                blob: "YiB0ZXh0".to_string(),
                meta: None,
                extra: Map::new(),
            }),
        ];
        let options = options(vec![Arc::new(server)]);
        let result = read_resource(
            &options,
            &json!({ "server": "docs", "uri": "docs://a" }),
            None,
        )
        .await
        .expect("read");
        assert_eq!(
            serde_json::to_value(&result.content).expect("content"),
            json!([
                { "type": "text", "text": "docs://a:" },
                { "type": "text", "text": "a text" },
                { "type": "text", "text": "docs://b:" },
                { "type": "text", "text": "b text" }
            ])
        );
        assert_eq!(
            result.structured_content,
            Some(json!({
                "server": "docs",
                "uri": "docs://a",
                "contents": [
                    { "uri": "docs://a", "text": "a text" },
                    { "uri": "docs://b", "mimeType": "text/plain", "blob": "YiB0ZXh0" }
                ]
            }))
        );
        assert_eq!(
            result.details,
            json!({ "server": "docs", "tool": READ_MCP_RESOURCE_TOOL })
        );
    }

    #[tokio::test]
    async fn reads_an_empty_resource_and_reports_missing_arguments() {
        let options = options(vec![Arc::new(FakeServer::new("docs"))]);
        let result = read_resource(
            &options,
            &json!({ "server": "docs", "uri": "docs://a" }),
            None,
        )
        .await
        .expect("read");
        assert_eq!(
            serde_json::to_value(&result.content).expect("content"),
            json!([{ "type": "text", "text": "Resource docs://a is empty." }])
        );

        let error = read_resource(&options, &json!({}), None)
            .await
            .expect_err("missing server");
        assert_eq!(error.message, "server must be provided");
        let error = read_resource(&options, &json!({ "server": "docs" }), None)
            .await
            .expect_err("missing uri");
        assert_eq!(error.message, "uri must be provided");
    }

    /// 评审回归：`isMcpAppResource` 的 `item.uri ?? item.uriTemplate ?? ""`——模板可以带一个
    /// 非标准的 `uri`，它优先于 `uriTemplate`。
    #[tokio::test]
    async fn template_uri_takes_precedence_over_uri_template() {
        let mut server = FakeServer::new("docs");
        server.templates = vec![
            ResourceTemplate {
                uri_template: "t://plain".to_string(),
                name: "plain".to_string(),
                ..ResourceTemplate::default()
            },
            ResourceTemplate {
                uri_template: "t://y".to_string(),
                name: "y".to_string(),
                extra: Map::from_iter([("uri".to_string(), json!("ui://extra"))]),
                ..ResourceTemplate::default()
            },
            ResourceTemplate {
                uri_template: "ui://z".to_string(),
                name: "z".to_string(),
                extra: Map::from_iter([("uri".to_string(), json!("custom://e"))]),
                ..ResourceTemplate::default()
            },
            ResourceTemplate {
                uri_template: "ui://hidden".to_string(),
                name: "hidden".to_string(),
                ..ResourceTemplate::default()
            },
        ];
        let options = options(vec![Arc::new(server)]);
        let payload = list_payload(
            &options,
            &json!({ "server": "docs" }),
            None,
            ListKind::Templates,
        )
        .await
        .expect("list");
        assert_eq!(
            payload["resourceTemplates"],
            json!([
                { "server": "docs", "uriTemplate": "t://plain", "name": "plain" },
                { "server": "docs", "uriTemplate": "ui://z", "uri": "custom://e", "name": "z" }
            ])
        );
    }

    /// 评审回归：JS 的 `\s` 含 U+FEFF、不含 U+0085。
    #[tokio::test]
    async fn filters_app_profiles_with_the_javascript_whitespace_set() {
        let mut server = FakeServer::new("docs");
        let template = |name: &str, mime_type: &str| ResourceTemplate {
            uri_template: format!("t://{name}"),
            name: name.to_string(),
            mime_type: Some(mime_type.to_string()),
            ..ResourceTemplate::default()
        };
        server.templates = vec![
            template("plain", "text/html"),
            template("feff", "text/html;\u{FEFF}profile=mcp-app"),
            template("nel", "text/html;\u{0085}profile=mcp-app"),
            template("nbsp", "text/html;\u{00A0}profile = \"mcp-app\""),
            template("ls", "text/html;\u{2028}profile=mcp-app"),
        ];
        let options = options(vec![Arc::new(server)]);
        let payload = list_payload(
            &options,
            &json!({ "server": "docs" }),
            None,
            ListKind::Templates,
        )
        .await
        .expect("list");
        let names = payload["resourceTemplates"]
            .as_array()
            .expect("templates")
            .iter()
            .map(|template| template["name"].as_str().expect("name"))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["plain", "nel"]);
    }

    /// 评审回归：`uri` 不是字符串时 TS 的 `uri.startsWith` 抛 TypeError（整个调用失败，
    /// 不是 `errors` 条目）。
    #[tokio::test]
    async fn rejects_non_string_template_uris_like_the_ts_type_error() {
        let mut server = FakeServer::new("docs");
        server.templates = vec![ResourceTemplate {
            uri_template: "t://x".to_string(),
            name: "x".to_string(),
            extra: Map::from_iter([("uri".to_string(), json!(5))]),
            ..ResourceTemplate::default()
        }];
        let single_options = options(vec![Arc::new(server)]);
        let error = list_payload(
            &single_options,
            &json!({ "server": "docs" }),
            None,
            ListKind::Templates,
        )
        .await
        .expect_err("non-string uri");
        assert_eq!(error.message, "uri.startsWith is not a function");

        // The all-servers branch throws too, instead of reporting a listing error.
        let mut broken = FakeServer::new("broken");
        broken.templates = vec![ResourceTemplate {
            uri_template: "t://x".to_string(),
            name: "x".to_string(),
            extra: Map::from_iter([("uri".to_string(), json!(5))]),
            ..ResourceTemplate::default()
        }];
        let broken_options = options(vec![Arc::new(broken)]);
        let error = list_payload(&broken_options, &json!({}), None, ListKind::Templates)
            .await
            .expect_err("non-string uri");
        assert_eq!(error.message, "uri.startsWith is not a function");
    }
}
