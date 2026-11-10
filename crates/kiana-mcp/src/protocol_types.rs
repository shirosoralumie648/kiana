//! `protocol/types.ts` + `protocol/content.ts`：协议版本、能力、Tool/Resource/Prompt 类型与
//! 内容块 → 模型内容的转换。
//!
//! TS 源：`packages/mcp/src/protocol/types.ts`、`packages/mcp/src/protocol/content.ts`
//! （`spec/telemetry-mcp.md` §10、§11）。
//!
//! # 形状
//!
//! TS 这些都是**结构类型**：客户端只校验一个子集，其余解析后的 JSON 原样传递。Rust 侧因此把
//! 每个结构体建模成「已知字段 + [`extra`](Tool::extra) 兜底」：
//! `#[serde(rename_all = "camelCase")]` 保持线上名字，`#[serde(flatten)] extra: Map<String, Value>`
//! 保留未知字段（调用方会读 `icons`、`_meta` 等），因此服务器发来的对象可以逐字往返。
//!
//! **反序列化是宽松的**（见文件下方的「宽松反序列化」一节）：TS 的接口只在编译期存在，运行期
//! `client.ts` 只校验 `isTool`/`isResource` 之类的子集。因此**参与宽松路径的结构体**不派生
//! `Deserialize`（改由本文件末尾的宏与手写实现提供），而是接受缺失的必填字段，并把类型不符的
//! 已知字段原样留在 `extra` 里 —— 与 TS 一样"不因一个畸形字段就整体失败"。**可选**字段
//! （`Option<T>`）只在类型相符时进类型化字段，否则原值进 `extra` 逐字往返（`optional_string` +
//! `restore_unconsumed`；这两个是私有函数，见本文件中部的「宽松反序列化」小节）。
//!
//! 范围注意：`TextContent`/`ImageContent`/`AudioContent`/`ResourceLinkContent`/
//! `TextResourceContents`/`BlobResourceContents`/`ContentAnnotations` 等**载荷类型**仍然派生
//! 严格 `Deserialize`，直接反序列化它们时字段类型不符会整体失败。只有经 [`ContentBlock`] 的
//! 宽松分派（`content_block_from_value`，内部调用各载荷类型的 `from_object`）时，不匹配的输入
//! 才会降级到 [`ContentBlock::Other`]；[`ResourceContents`] 同理降级到
//! [`ResourceContents::Other`]。
//!
//! # 与 TS 的抛错路径
//!
//! TS 的 `toLlmContent` 对畸形输入抛 `TypeError`，而包根导出的 [`to_llm_content`] 是冻结契约
//! `-> Vec<LlmContent>`，不能抛。因此实现里并存三条路径（都用同一套分支逻辑，不重复）：
//!
//! - [`to_llm_content`] / [`block_to_llm_content`]：降级（异常 → 占位文本）；
//! - [`try_to_llm_content`] / [`try_block_to_llm_content`]：对已解析的 [`CallToolResult`]
//!   给出 `McpTransportError { name: "TypeError" }`；
//! - [`try_to_llm_content_from_value`] / [`try_block_value_to_llm_content`]：接受**任意** JSON
//!   （TS 的签名就是如此），因此 `content` 非数组、`result` 为 `null`、"键缺失 vs 显式 null"
//!   这些类型化枚举无法表达的差别也能逐字复刻。
//!
//! # 已知偏差
//!
//! - `undefined` 与字段缺失在 TS 里是同一件事（`JSON.stringify` 都省略），Rust 用
//!   `Option<T>` + `skip_serializing_if` 表达。**显式 `null`** 在 TS 里是"存在且为 null"，
//!   在 `Option<T>` 里通常退化成 `None`；只有 [`CallToolResult::structured_content`] 例外
//!   （保留 `Some(Value::Null)`，因为 `toLlmContent` 的 `!== undefined` 判定依赖它）。
//! - 结构体字段顺序是声明顺序，而 TS 保留服务器的键序；往返后的对象键序可能与服务器原序不同。
//!   需要逐字字节相等的场景（stdio/HTTP body）在输出边界走 `kiana_json`。
//! - **必填**字段（`uri`、`name`、`text`、`blob`、`inputSchema`、`capabilities`、`clientInfo`、
//!   `serverInfo`、`progressToken`、`requestId` 等）无法缺席，类型不符时仍会被 `String()` /
//!   空串 / 默认值改写：TS 会原样保留 `{uri: 5}`、`{inputSchema: 5}`、`{capabilities: 5}`。
//!   它们也不能在 `extra` 里再放一份（`serde(flatten)` 会写出重复键）。要逐字保留这类对象，
//!   用兜底变体 [`ResourceContents::Other`] / [`ContentBlock::Other`]（内容块与资源已按"能逐字
//!   表达才类型化"处理）；其余情况是 `ARCHITECTURE.md` §4.9 冻结字段面的固有局限。
//! - [`ContentBlock`] 的未知/畸形块落在 [`ContentBlock::Other`]，`to_llm_content` 按原始
//!   `Value` 的 `type` 分支（与 TS 的 `switch (block.type)` 一致）。
//! - [`LlmContent`] 的字段是 `String`，无法表达 TS 里 `undefined`/`null`/数字导致的
//!   "键被省略"或"值不是字符串"。`text`、`data` 这类字段一律按 `String()` 转成文本
//!   （缺失 → 空串），因此 `{text: 0}` 得到 `"0"`、`{text: null}` 得到 `""`（TS 是 `null`）。
//!   受影响的只有 `LlmContent` 的输出。
//! - 数字可选字段 [`ProgressNotification::total`] 没有走 `restore_unconsumed`：非数字原值
//!   既进不了类型化字段，也不会留在 `extra`，往返时丢失（与字符串可选字段的处理不一致）。
//!
//! # 在 crate 中的位置
//!
//! 本文件只做数据建模与纯函数转换：不发起 IO，也不持有运行时状态。同 crate 的 `jsonrpc`
//! 提供消息信封与 `JsonRpcId`，`client` 先校验响应形状再调用这里的 `from_value` 得到类型化
//! 视图；`index.rs` 按 TS 的导出面把名字再导出到包根，`lib.rs` 另外提供
//! `crate::protocol::{content, types}` 别名模块（模块表见 `lib.rs` 的 `//!`）。下游
//! `kiana-builtin-extensions` 的 `mcp::tools` 消费 [`CallToolResult`] 与
//! [`to_llm_content`]/[`try_block_to_llm_content`]。
//!
//! # 阅读顺序
//!
//! 1. 版本与能力：[`LATEST_PROTOCOL_VERSION`]、[`SUPPORTED_PROTOCOL_VERSIONS`]、
//!    [`ServerCapabilities`]；
//! 2. 列表类型：[`Tool`]、[`Resource`]、[`ResourceTemplate`] 与各 `List*Result`；
//! 3. 内容块：[`ContentBlock`]、[`ResourceContents`] 的判别与 `Other` 兜底；
//! 4. 转换：[`to_llm_content`]、[`try_to_llm_content`]、[`try_to_llm_content_from_value`]；
//! 5. 宽松反序列化辅助（文件中部的私有 `field`/`optional_string`/`restore_unconsumed`）；
//! 6. 末尾是 JS 数字的 serde 桥、类型别名与测试。
//!
//! # 宽松反序列化的两阶段实现
//!
//! 每个**手写的宽松** `Deserialize` 实现都先把输入整体读成 `serde_json::Value`，再交给逐字段的
//! `from_value`/`from_object`：这类实现包括本文件末尾 `lenient_deserialize!` /
//! `lenient_deserialize_object!` 生成的全部类型，以及手写的 `ResourceContents`、`ContentBlock`、
//! `EmbeddedResourceContent`。`object_of` 负责读成对象并拒绝非对象（两处宏与
//! `EmbeddedResourceContent`），`ResourceContents`/`ContentBlock` 则直接 `Value::deserialize`
//! 后按键或 `type` 分派。这样「某个已知字段类型不对」可以降级为「原值放进 `extra`」
//! （内容块/资源则放进 `Other` 变体）。若改用派生 `Deserialize`，serde 会在第一个不匹配
//! 的字段处整体失败，与 TS 运行期不校验接口的行为不符。`from_value` 返回 `None` 时（例如 [`CallToolResult`] 的 `content` 存在
//! 但不是数组、通知的 `progressToken`/`requestId` 不是合法 id），宏生成的实现统一报
//! `expected an object`。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::auth_provider::McpTransportError;
use crate::jsonrpc::JsonRpcId;

/// `LATEST_PROTOCOL_VERSION`：客户端默认请求的协议版本。
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// `SUPPORTED_PROTOCOL_VERSIONS`：客户端接受的服务器版本。
///
/// 服务器若不支持请求的版本，会用自己的最新版本作答，所以旧版本仍要接受
/// （对方是用旧 SDK 建的）。
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] = [
    LATEST_PROTOCOL_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// `SupportedProtocolVersion`（`typeof SUPPORTED_PROTOCOL_VERSIONS[number]`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SupportedProtocolVersion {
    #[serde(rename = "2025-11-25")]
    V2025_11_25,
    #[serde(rename = "2025-06-18")]
    V2025_06_18,
    #[serde(rename = "2025-03-26")]
    V2025_03_26,
    #[serde(rename = "2024-11-05")]
    V2024_11_05,
}

impl SupportedProtocolVersion {
    /// 线上字符串（逐字）。
    pub fn as_str(self) -> &'static str {
        match self {
            SupportedProtocolVersion::V2025_11_25 => "2025-11-25",
            SupportedProtocolVersion::V2025_06_18 => "2025-06-18",
            SupportedProtocolVersion::V2025_03_26 => "2025-03-26",
            SupportedProtocolVersion::V2024_11_05 => "2024-11-05",
        }
    }

    /// 全部受支持版本，顺序与 [`SUPPORTED_PROTOCOL_VERSIONS`] 一致。
    pub const ALL: [SupportedProtocolVersion; 4] = [
        SupportedProtocolVersion::V2025_11_25,
        SupportedProtocolVersion::V2025_06_18,
        SupportedProtocolVersion::V2025_03_26,
        SupportedProtocolVersion::V2024_11_05,
    ];
}

impl std::fmt::Display for SupportedProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SupportedProtocolVersion {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        SupportedProtocolVersion::ALL
            .into_iter()
            .find(|version| version.as_str() == value)
            .ok_or(())
    }
}

/// `SUPPORTED_PROTOCOL_VERSIONS.includes(value)`（`client.connect` 的校验）。
pub fn is_supported_protocol_version(value: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&value)
}

/// `Implementation`（clientInfo / serverInfo）。
///
/// `name`/`version` 在线上是必填字段。宽松反序列化（`Implementation::from_value`）不对它们
/// 做类型校验：缺失/`null`/非字符串一律按 `String()` 或空串写入（模块文档的必填字段偏差）。
/// `client.rs` 的 `validate_initialize_result` 会在调用 `from_value` 之前先校验 `serverInfo`
/// 的这两个字段。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 未知字段原样保留。
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Implementation {
    /// `{ name, version }`。
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Implementation {
            name: name.into(),
            version: version.into(),
            title: None,
            extra: Map::new(),
        }
    }
}

/// `Root`（`roots/list` 的元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `{ listChanged?: boolean }`（`ClientCapabilities.roots`、`ServerCapabilities.prompts` 等）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListChangedCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ServerCapabilities.resources`：`{ subscribe?, listChanged? }`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ClientCapabilities`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<ListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ServerCapabilities`。
///
/// 宽松反序列化只把**对象**形态的能力读进类型化字段；`capabilities.tools: true` 这类非对象
/// 原值留在 `extra`。因此判断"服务器是否声明了 tools"必须用 [`ServerCapabilities::has_tools`]
/// （JS truthiness），不能只看 `tools.is_some()`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<ListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completions: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ServerCapabilities {
    /// `serverCapabilities.tools` 是否为真（coding-agent 用它决定是否调用 `tools/list`，§30.5）。
    ///
    /// TS 的判定是 `client.serverCapabilities?.tools ? ... : ...`——**JS truthiness**，不是
    /// "字段是否存在"：`tools: true`/`[]`/`"x"` 为真，`tools: false`/`0`/`""`/`null` 为假。
    /// 非对象值没进 [`tools`](ServerCapabilities::tools)，而是被宽松反序列化留在 `extra` 里，
    /// 因此这里对 `extra` 里的原值再判一次 truthiness。
    pub fn has_tools(&self) -> bool {
        self.tools.is_some() || kiana_json::js_string::is_truthy(self.extra.get("tools"))
    }
}

/// `InitializeParams`（`initialize` 请求的 params）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: Implementation,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `InitializeResult`（`initialize` 的 result）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: Implementation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ProgressNotification`。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressNotification {
    pub progress_token: JsonRpcId,
    /// 进度值。宽松反序列化把缺失/非数字读成 `NaN`（`ProgressNotification::from_value`），
    /// 序列化时经 `serialize_js_number` 写成 `null`（`JSON.stringify` 对 NaN 的行为）。
    #[serde(serialize_with = "serialize_js_number")]
    pub progress: f64,
    /// 总量；缺失/`null`/非数字都读成 `None`（`number_field`）。注意非数字原值不会进
    /// `extra`：`total` 属于已知键，`ProgressNotification::from_value` 也没有为它调用
    /// `restore_unconsumed`，因此该原值在往返中丢失。
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number_opt"
    )]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `CancelledNotification`。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelledNotification {
    pub request_id: JsonRpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ToolAnnotations`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ToolExecution.taskSupport`。
///
/// 线上值是 `"forbidden"` / `"optional"` / `"required"`。未知字符串不算错误：它留在
/// `ToolExecution::extra` 的 `taskSupport` 里逐字往返（见私有函数
/// `ToolExecution::from_object`；`ToolExecution` 本身没有公开的 `from_value`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskSupport {
    Forbidden,
    Optional,
    Required,
}

/// `ToolExecution`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecution {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_support: Option<TaskSupport>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `Tool`（`tools/list` 的元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecution>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Tool {
    /// `{ name, inputSchema }`。
    pub fn new(name: impl Into<String>, input_schema: Map<String, Value>) -> Self {
        Tool {
            name: name.into(),
            input_schema,
            ..Tool::default()
        }
    }
}

/// `ListToolsResult`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `PromptArgument`（`prompts/list` 元素的参数声明）。
///
/// MCP 规范的 `arguments` 是可选数组；每项的 `name` 必填，`description`/`required`
/// 可选。与其它协议结构一样，未知键留在 [`PromptArgument::extra`] 里逐字往返。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptArgument {
    /// 参数名（调用 `prompts/get` 时 `arguments` 的键）。
    pub name: String,
    /// 参数说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 是否必填。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PromptArgument {
    /// `{ name }`。
    pub fn new(name: impl Into<String>) -> Self {
        PromptArgument {
            name: name.into(),
            ..PromptArgument::default()
        }
    }

    /// 宽松构造：非对象或无字符串 `name` 时返回 `None`。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let name = map.get("name").and_then(Value::as_str)?;
        let description = optional_string(map, "description");
        let mut extra = extra_fields(map, keys::PROMPT_ARGUMENT);
        restore_unconsumed(&mut extra, map, &[("description", description.is_some())]);
        Some(PromptArgument {
            name: name.to_string(),
            description,
            required: bool_field(map, "required"),
            extra,
        })
    }
}

/// `Prompt`（`prompts/list` 的元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// prompt 名（`/mcp__<server>__<prompt>` 的后半段）。
    pub name: String,
    /// 标题（可选，供 UI 显示）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 一句话说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 参数声明；服务器没有参数时省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Prompt {
    /// `{ name }`。
    pub fn new(name: impl Into<String>) -> Self {
        Prompt {
            name: name.into(),
            ..Prompt::default()
        }
    }

    /// 宽松构造：非对象或无字符串 `name` 时返回 `None`。
    ///
    /// `arguments` 存在但不是数组时读成 `None` 并把原值留在 `extra`；数组中不是对象的
    /// 成员被跳过（与 TS 的 `list_of` 语义一致）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let meta = map_field(map, "_meta");
        let arguments = match map.get("arguments") {
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .filter_map(PromptArgument::from_value)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        let mut extra = extra_fields(map, keys::PROMPT);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("arguments", arguments.is_some()),
                ("_meta", meta.is_some()),
            ],
        );
        let title = optional_string(map, "title");
        let description = optional_string(map, "description");
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("title", title.is_some()),
                ("description", description.is_some()),
            ],
        );
        Some(Prompt {
            name: required_string(map, "name"),
            title,
            description,
            arguments,
            meta,
            extra,
        })
    }
}

/// `ListPromptsResult`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListPromptsResult {
    pub prompts: Vec<Prompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ListPromptsResult {
    /// 宽松构造：`prompts` 非数组 → 空列表；`nextCursor` 非字符串原值留在 `extra`。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let meta = map_field(map, "_meta");
        let next_cursor = optional_string(map, "nextCursor");
        let mut extra = extra_fields(map, keys::LIST_PROMPTS);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("_meta", meta.is_some()),
                ("nextCursor", next_cursor.is_some()),
            ],
        );
        Some(ListPromptsResult {
            prompts: list_of(map, "prompts", Prompt::from_value),
            next_cursor,
            meta,
            extra,
        })
    }
}

/// `PromptMessage.role`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PromptRole {
    /// `"user"`。
    #[default]
    User,
    /// `"assistant"`。
    Assistant,
}

impl PromptRole {
    /// `prompts/get` 校验接受的字符串。
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "user" => Some(PromptRole::User),
            "assistant" => Some(PromptRole::Assistant),
            _ => None,
        }
    }

    /// 线上字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            PromptRole::User => "user",
            PromptRole::Assistant => "assistant",
        }
    }
}

/// `PromptMessage`（`prompts/get` 的消息）。
///
/// `content` 复用 [`ContentBlock`] 的宽松反序列化；未知内容块保留为
/// [`ContentBlock::Other`]，由调用方决定如何转成模型内容。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMessage {
    /// 消息角色。
    pub role: PromptRole,
    /// 消息内容。
    pub content: ContentBlock,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl PromptMessage {
    /// 宽松构造：非对象、`role` 不是 `user`/`assistant`、或缺 `content` 时返回 `None`。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let role = map
            .get("role")
            .and_then(Value::as_str)
            .and_then(PromptRole::from_str)?;
        let content = content_block_from_value(map.get("content")?);
        let mut extra = extra_fields(map, keys::PROMPT_MESSAGE);
        restore_unconsumed(&mut extra, map, &[]);
        Some(PromptMessage {
            role,
            content,
            extra,
        })
    }
}

/// `GetPromptResult`（`prompts/get` 的结果）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetPromptResult {
    /// 可选说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 消息列表（可以是空数组）。
    pub messages: Vec<PromptMessage>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl GetPromptResult {
    /// 宽松构造：非对象或 `messages` 不是数组时返回 `None`；成员不是合法消息时跳过。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let messages = map.get("messages")?.as_array()?;
        let description = optional_string(map, "description");
        let meta = map_field(map, "_meta");
        let mut extra = extra_fields(map, keys::GET_PROMPT);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("description", description.is_some()),
                ("_meta", meta.is_some()),
            ],
        );
        Some(GetPromptResult {
            description,
            messages: messages
                .iter()
                .filter_map(PromptMessage::from_value)
                .collect(),
            meta,
            extra,
        })
    }
}

/// `Resource`（`resources/list` 的元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number_opt"
    )]
    pub size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ResourceTemplate`（`resources/templates/list` 的元素）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ListResourcesResult`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListResourcesResult {
    pub resources: Vec<Resource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ListResourceTemplatesResult`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListResourceTemplatesResult {
    pub resource_templates: Vec<ResourceTemplate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ReadResourceResult`。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReadResourceResult {
    pub contents: Vec<ResourceContents>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ---------------------------------------------------------------------------
// protocol/content.ts
// ---------------------------------------------------------------------------

/// `ContentAnnotations`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContentAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<Audience>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number_opt"
    )]
    pub priority: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ContentAnnotations.audience` 的元素。
///
/// 经父结构体的宽松反序列化（`Resource`/`ResourceTemplate` 等的 `annotations` 字段）时走私有的
/// `ContentAnnotations::from_object`：数组里只识别 `"user"` / `"assistant"`，其他成员被丢弃
/// （不会进 `extra`）；`audience` 不是数组时整体读成 `None`，**非 `null`** 的原值由
/// `restore_unconsumed` 放回 `extra`。该函数在 `!value.is_null()` 处显式跳过 `null`，所以显式
/// `audience: null` 会被当作缺失、`from_object`/`from_value` 路径不会保留它；客户端路径由
/// `client.rs` 的 `patch_content_annotations`/`patch_nulls` 另行补回。注意直接对
/// `ContentAnnotations` 使用派生 `Deserialize` 是严格模式，未知成员会报错——两条路径行为不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    User,
    Assistant,
}

/// `TextContent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    #[serde(rename = "type")]
    pub kind: TextKind,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `TextContent.type` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextKind {
    #[default]
    #[serde(rename = "text")]
    Text,
}

impl TextContent {
    /// `{ type: "text", text }`。
    pub fn new(text: impl Into<String>) -> Self {
        TextContent {
            kind: TextKind::Text,
            text: text.into(),
            ..TextContent::default()
        }
    }
}

/// `ImageContent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub kind: ImageKind,
    pub data: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ImageContent.type` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImageKind {
    #[default]
    #[serde(rename = "image")]
    Image,
}

impl ImageContent {
    /// `{ type: "image", data, mimeType }`。
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        ImageContent {
            kind: ImageKind::Image,
            data: data.into(),
            mime_type: mime_type.into(),
            ..ImageContent::default()
        }
    }
}

/// `AudioContent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioContent {
    #[serde(rename = "type")]
    pub kind: AudioKind,
    pub data: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `AudioContent.type` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioKind {
    #[default]
    #[serde(rename = "audio")]
    Audio,
}

impl AudioContent {
    /// `{ type: "audio", data, mimeType }`。
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        AudioContent {
            kind: AudioKind::Audio,
            data: data.into(),
            mime_type: mime_type.into(),
            ..AudioContent::default()
        }
    }
}

/// `ResourceLinkContent`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLinkContent {
    #[serde(rename = "type")]
    pub kind: ResourceLinkKind,
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number_opt"
    )]
    pub size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `ResourceLinkContent.type` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ResourceLinkKind {
    #[default]
    #[serde(rename = "resource_link")]
    ResourceLink,
}

impl ResourceLinkContent {
    /// `{ type: "resource_link", uri, name }`。
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        ResourceLinkContent {
            kind: ResourceLinkKind::ResourceLink,
            uri: uri.into(),
            name: name.into(),
            ..ResourceLinkContent::default()
        }
    }
}

/// `TextResourceContents`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TextResourceContents {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `BlobResourceContents`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BlobResourceContents {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub blob: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `TextResourceContents | BlobResourceContents`（外加畸形值的兜底）。
///
/// TS 用 `"text" in resource` 判别（**键存在**即可，即使值是 `undefined`）；手写的
/// `Deserialize`（私有函数 `resource_contents_from_value`）同样 text 优先，字段类型不符时
/// 落到 [`ResourceContents::Other`] 保留原值。枚举上的 `untagged` 只影响序列化，不参与判别。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResourceContents {
    Text(TextResourceContents),
    Blob(BlobResourceContents),
    Other(Value),
}

impl ResourceContents {
    /// `uri`（三种变体都有；非对象时为 `None`）。
    pub fn uri(&self) -> Option<&str> {
        match self {
            ResourceContents::Text(text) => Some(&text.uri),
            ResourceContents::Blob(blob) => Some(&blob.uri),
            ResourceContents::Other(value) => value.get("uri").and_then(Value::as_str),
        }
    }

    /// 原始 JSON 视图（`Other` 变体返回自身）。
    ///
    /// 类型化变体是**重新序列化**得到的对象：键序是结构体字段声明序，`None` 字段被省略，
    /// 与服务器原始字节/键序可能不同；需要逐字原样时必须用 `Other`。序列化失败时按
    /// `Value::Null` 兜底（`unwrap_or`）。
    pub fn as_value(&self) -> Value {
        match self {
            ResourceContents::Text(text) => serde_json::to_value(text).unwrap_or(Value::Null),
            ResourceContents::Blob(blob) => serde_json::to_value(blob).unwrap_or(Value::Null),
            ResourceContents::Other(value) => value.clone(),
        }
    }
}

/// `EmbeddedResourceContent`。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResourceContent {
    #[serde(rename = "type")]
    pub kind: EmbeddedResourceKind,
    pub resource: ResourceContents,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ContentAnnotations>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `EmbeddedResourceContent.type` 的字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EmbeddedResourceKind {
    #[default]
    #[serde(rename = "resource")]
    Resource,
}

/// `ContentBlock`：`TextContent | ImageContent | AudioContent | ResourceLinkContent |
/// EmbeddedResourceContent`，外加未知/畸形块的兜底。
///
/// 手写的 `Deserialize`（私有函数 `content_block_from_value`）按 `type` 字符串分派，与 TS 的
/// `switch (block.type)` 等价；枚举上的 `untagged` 只影响序列化，变体排列顺序仅作阅读提示。
/// 字段不符合已知变体（例如 `{type:"text"}` 缺 `text`）时落到 [`ContentBlock::Other`]，
/// `to_llm_content` 仍按原始 `type` 分派。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ContentBlock {
    Text(TextContent),
    Image(ImageContent),
    Audio(AudioContent),
    ResourceLink(ResourceLinkContent),
    EmbeddedResource(EmbeddedResourceContent),
    Other(Value),
}

impl ContentBlock {
    /// `block.type`（缺失或非字符串时为 `None`；`to_llm_content` 用 `String()` 版本）。
    pub fn type_name(&self) -> Option<&str> {
        match self {
            ContentBlock::Text(_) => Some("text"),
            ContentBlock::Image(_) => Some("image"),
            ContentBlock::Audio(_) => Some("audio"),
            ContentBlock::ResourceLink(_) => Some("resource_link"),
            ContentBlock::EmbeddedResource(_) => Some("resource"),
            ContentBlock::Other(value) => value.get("type").and_then(Value::as_str),
        }
    }

    /// 原始 JSON 视图（`to_llm_content` 对 `Other` 变体需要）。
    ///
    /// 类型化变体是**重新序列化**得到的对象（键序为字段声明序、`None` 字段省略），不是服务器
    /// 的原始字节；`Other` 才逐字返回。下游按原始字段分派（如
    /// `kiana-builtin-extensions` 的 `block_to_content`）依赖的就是这一视图。
    pub fn as_value(&self) -> Value {
        match self {
            ContentBlock::Text(text) => serde_json::to_value(text).unwrap_or(Value::Null),
            ContentBlock::Image(image) => serde_json::to_value(image).unwrap_or(Value::Null),
            ContentBlock::Audio(audio) => serde_json::to_value(audio).unwrap_or(Value::Null),
            ContentBlock::ResourceLink(link) => serde_json::to_value(link).unwrap_or(Value::Null),
            ContentBlock::EmbeddedResource(resource) => {
                serde_json::to_value(resource).unwrap_or(Value::Null)
            }
            ContentBlock::Other(value) => value.clone(),
        }
    }
}

/// `CallToolResult`。
///
/// `content` 在类型上必填，但运行时用 `result.content ?? []`（§30.12）；因此反序列化时允许缺失
/// （→ 空数组）。`structuredContent` 可以是**任意** JSON 值（MCP 2026-07-28），包括 `null`：
/// 这里用 `Option<Value>` 保留 `Some(Value::Null)`，因为 `toLlmContent` 的判定是
/// `structuredContent !== undefined`（`None` == 缺省）。
///
/// # 与 TS 的键序差异（已记录，未修）
///
/// TS 的 `validateCallToolResult` 返回 `{...value, content: []}`（服务器给了 `content` 时原样
/// 返回），因此键序是服务器的原序、`content` 缺失时才追加到末尾。Rust 侧按结构体声明序
/// 序列化（`content` 在前）。两者**语义相同**（TS 的测试都是深比较），只有逐字节比较才可见；
/// 要消除它需要保存原始 `Value` 并改 `Serialize`，而 `CallToolResult` 的字段面是
/// `ARCHITECTURE.md` §4.9 冻结的契约，不由本单元单方面扩展。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// `content` 在 TS 类型上必填，但运行时用 `result.content ?? []`（§30.12）：缺失**与**
    /// 显式 `null` 都读成空数组。序列化时总是写出（TS 的输出里 `content` 是必有的键）。
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CallToolResult {
    /// `{ content }`，无结构化内容。
    pub fn new(content: Vec<ContentBlock>) -> Self {
        CallToolResult {
            content,
            ..CallToolResult::default()
        }
    }

    /// `content ?? []`。
    pub fn content_or_empty(&self) -> &[ContentBlock] {
        &self.content
    }

    /// `result.structuredContent !== undefined`。
    pub fn has_structured_content(&self) -> bool {
        self.structured_content.is_some()
    }
}

/// `toLlmContent` 的输出：LLM API 接受的内容形状（与 `@earendil-works/kiana-ai` 的
/// `TextContent`/`ImageContent` 一致）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LlmContent {
    /// `{ type: "text", text }`。
    Text { text: String },
    /// `{ type: "image", data, mimeType }`。
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl LlmContent {
    /// `{ type: "text", text }`。
    pub fn text(text: impl Into<String>) -> Self {
        LlmContent::Text { text: text.into() }
    }

    /// `{ type: "image", data, mimeType }`。
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        LlmContent::Image {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }

    /// `content.type`。
    pub fn type_name(&self) -> &'static str {
        match self {
            LlmContent::Text { .. } => "text",
            LlmContent::Image { .. } => "image",
        }
    }
}

/// `blockToLlmContent(block)`：单个内容块 → 模型内容。
///
/// 逐字复刻 `content.ts` 的 `switch (block.type)`：
/// - `"text"` → `{type:"text", text}`；
/// - `"image"` → `{type:"image", data, mimeType}`；
/// - `"audio"` → `{type:"text", text: "[audio {mimeType} omitted]"}`（`mimeType` 走 `String()`，
///   缺失是 `"undefined"`、`null` 是 `"null"`）；
/// - `"resource_link"` → `{type:"text", text: "{name}: {uri}"}`（同样 `String()`）；
/// - `"resource"` → `"text" in resource` 时取 `resource.text`；否则 `mimeType` 以 `image/` 开头时
///   变成图片；其余是 `"[binary resource {uri} ({mimeType ?? \"unknown type\"}) omitted]"`；
/// - 其他（含缺失 `type`）→ `"[unsupported MCP content {type}]"`（缺失读作 `"undefined"`）。
///
/// TS 会在畸形输入上抛 `TypeError`（见 [`try_block_to_llm_content`]）；本函数签名为冻结契约、
/// 不能抛，因此把异常降级成占位文本。需要 TS 的抛错语义时用 [`try_block_to_llm_content`]。
pub fn block_to_llm_content(block: &ContentBlock) -> LlmContent {
    match block {
        ContentBlock::Text(text) => LlmContent::text(text.text.clone()),
        ContentBlock::Image(image) => {
            LlmContent::image(image.data.clone(), image.mime_type.clone())
        }
        ContentBlock::Audio(audio) => {
            LlmContent::text(format!("[audio {} omitted]", audio.mime_type))
        }
        ContentBlock::ResourceLink(link) => {
            LlmContent::text(format!("{}: {}", link.name, link.uri))
        }
        ContentBlock::EmbeddedResource(embedded) => resource_to_llm_content(&embedded.resource),
        ContentBlock::Other(value) => other_block_to_llm_content(value),
    }
}

/// `blockToLlmContent(block)` 的**抛错**版本：TS 会抛 `TypeError` 的输入在这里返回
/// `Err(McpTransportError { name: "TypeError" })`。
///
/// TS 的两类抛错（都已用 Node 26 对着真实 `content.ts` 实测）：
/// - `{type:"resource"}` 的 `resource` 缺失/`null`/字符串/数字/布尔 →
///   `Cannot use 'in' operator to search for 'text' in <String(resource)>`；
/// - `resource.mimeType` 存在但不是字符串（数字/对象/数组/布尔）且没有 `text` 键 →
///   `resource.mimeType?.startsWith is not a function`；
/// - 内容块本身是 `null` → `Cannot read properties of null (reading 'type')`。
///
/// 错误信封的 `name()` 是 `"TypeError"`（[`McpTransportError::network`]），`message()` 与
/// `error.message` 逐字一致。
pub fn try_block_to_llm_content(block: &ContentBlock) -> Result<LlmContent, McpTransportError> {
    match block {
        ContentBlock::Text(text) => Ok(LlmContent::text(text.text.clone())),
        ContentBlock::Image(image) => Ok(LlmContent::image(
            image.data.clone(),
            image.mime_type.clone(),
        )),
        ContentBlock::Audio(audio) => Ok(LlmContent::text(format!(
            "[audio {} omitted]",
            audio.mime_type
        ))),
        ContentBlock::ResourceLink(link) => {
            Ok(LlmContent::text(format!("{}: {}", link.name, link.uri)))
        }
        ContentBlock::EmbeddedResource(embedded) => try_resource_to_llm_content(&embedded.resource),
        ContentBlock::Other(value) => try_other_block_to_llm_content(value),
    }
}

/// `"resource"` 分支（宽松版：TS 会抛异常的输入按占位文本降级）。
fn resource_to_llm_content(resource: &ResourceContents) -> LlmContent {
    match try_resource_to_llm_content(resource) {
        Ok(content) => content,
        Err(_) => match resource {
            ResourceContents::Other(value) => resource_value_to_llm_content(Some(value)),
            ResourceContents::Text(text) => LlmContent::text(text.text.clone()),
            ResourceContents::Blob(blob) => LlmContent::text(format!(
                "[binary resource {} ({}) omitted]",
                blob.uri,
                blob.mime_type.as_deref().unwrap_or("unknown type")
            )),
        },
    }
}

/// `"resource"` 分支（严格版：TS 抛异常时返回 `TypeError` 信封）。
fn try_resource_to_llm_content(
    resource: &ResourceContents,
) -> Result<LlmContent, McpTransportError> {
    match resource {
        ResourceContents::Text(text) => Ok(LlmContent::text(text.text.clone())),
        // 类型化 blob 的字段都是 `String`，TS 在这里不会抛异常。
        ResourceContents::Blob(blob) => Ok(blob_to_llm_content(
            &blob.uri,
            blob.mime_type.as_deref(),
            Some(&blob.blob),
        )),
        ResourceContents::Other(value) => try_resource_value_to_llm_content(Some(value)),
    }
}

/// 原始 `Value` 形态的 `"resource"` 分支（严格版，逐字复刻 `content.ts`）。
///
/// - `"text" in resource`：`resource` 不是对象/数组时 TS 抛
///   `TypeError: Cannot use 'in' operator to search for 'text' in <String(resource)>`；
/// - `resource.mimeType?.startsWith("image/")`：`mimeType` 存在且不是 `null`/字符串时 TS 抛
///   `TypeError: resource.mimeType?.startsWith is not a function`；
/// - `uri` / `blob` 缺失时模板字面量得到 `"undefined"`，显式 `null` 得到 `"null"`。
fn try_resource_value_to_llm_content(
    resource: Option<&Value>,
) -> Result<LlmContent, McpTransportError> {
    let Some(resource) = resource else {
        return Err(in_operator_error(None));
    };
    // `in` 只要求右值是对象；JS 里数组也合法（`"text" in []` 为 false），所以放行
    // `Value::Array`，随后取不到 `text` 键就继续走二进制资源分支。
    if !matches!(resource, Value::Object(_) | Value::Array(_)) {
        return Err(in_operator_error(Some(resource)));
    }
    if let Some(text) = resource.get("text") {
        return Ok(LlmContent::text(js_field_text(text)));
    }
    let mime = resource.get("mimeType");
    match mime {
        None | Some(Value::Null) => {}
        Some(Value::String(mime)) if mime.starts_with("image/") => {
            return Ok(LlmContent::image(
                kiana_json::js_string::js_string(resource.get("blob")),
                mime.clone(),
            ));
        }
        Some(Value::String(_)) => {}
        // 可选链只放行 `null`/缺失；其他类型调 `.startsWith` 抛 TypeError。
        Some(_) => {
            return Err(McpTransportError::network(
                "resource.mimeType?.startsWith is not a function",
            ));
        }
    }
    // `mimeType ?? "unknown type"`：只有 `null`/缺失是 "unknown type"（空串仍是空串）。
    let mime = match mime {
        None | Some(Value::Null) => "unknown type".to_owned(),
        Some(Value::String(mime)) => mime.clone(),
        Some(_) => unreachable!("non-string mimeType returned above"),
    };
    Ok(LlmContent::text(format!(
        "[binary resource {} ({mime}) omitted]",
        kiana_json::js_string::js_string(resource.get("uri"))
    )))
}

/// `Cannot use 'in' operator to search for 'text' in <operand>`（`None` 是 JS 的 `undefined`）。
fn in_operator_error(operand: Option<&Value>) -> McpTransportError {
    McpTransportError::network(format!(
        "Cannot use 'in' operator to search for 'text' in {}",
        kiana_json::js_string::js_string(operand)
    ))
}

/// 原始 `Value` 形态的 `"resource"` 分支（宽松版：TS 会抛异常的输入按占位文本降级）。
///
/// 宽松版的语义与严格版一致，只是把两类 `TypeError` 降级成占位文本：
/// `resource` 不是对象时按"没有 `text` 键"处理；`mimeType` 非字符串时按模板字面量的
/// `String(mimeType)` 渲染 `(…)`。
fn resource_value_to_llm_content(resource: Option<&Value>) -> LlmContent {
    match try_resource_value_to_llm_content(resource) {
        Ok(content) => content,
        Err(_) => {
            let Some(resource) = resource else {
                return LlmContent::text("[binary resource undefined (unknown type) omitted]");
            };
            let uri = kiana_json::js_string::js_string(resource.get("uri"));
            let mime = match resource.get("mimeType") {
                None | Some(Value::Null) => "unknown type".to_owned(),
                Some(value) => kiana_json::js_string::js_string(Some(value)),
            };
            LlmContent::text(format!("[binary resource {uri} ({mime}) omitted]"))
        }
    }
}

/// 二进制资源：`image/*` 变图片，其余变占位文本。
///
/// `mime ?? "unknown type"` 只对 `null`/缺失生效（空串仍是空串）。
fn blob_to_llm_content(uri: &str, mime_type: Option<&str>, blob: Option<&str>) -> LlmContent {
    if let Some(mime) = mime_type.filter(|mime| mime.starts_with("image/")) {
        return LlmContent::image(blob.unwrap_or(""), mime);
    }
    let mime = mime_type.unwrap_or("unknown type");
    LlmContent::text(format!("[binary resource {uri} ({mime}) omitted]"))
}

/// `default:` 分支（严格版）：按原始 `Value` 的 `type` 分派（覆盖 `{type:"text"}` 缺字段之类的
/// 畸形块）。`null` 块在 TS 里抛 `Cannot read properties of null (reading 'type')`。
fn try_other_block_to_llm_content(block: &Value) -> Result<LlmContent, McpTransportError> {
    if block.is_null() {
        return Err(McpTransportError::network(
            "Cannot read properties of null (reading 'type')",
        ));
    }
    match block.get("type").and_then(Value::as_str) {
        Some("text") => Ok(LlmContent::text(
            block.get("text").map(js_field_text).unwrap_or_default(),
        )),
        Some("image") => Ok(LlmContent::image(
            // TS 把 `block.data` / `block.mimeType` 原样放进结果对象（不做类型检查）；
            // [`LlmContent`] 的字段是 `String`，所以按 `String()` 语义转换（与
            // `ImageContent::from_object` 的 `required_string`、文本分支的 `js_field_text`
            // 一致）。修复前非字符串值被折成空串（评审 finding 4）。
            block.get("data").map(js_field_text).unwrap_or_default(),
            block.get("mimeType").map(js_field_text).unwrap_or_default(),
        )),
        Some("audio") => Ok(LlmContent::text(format!(
            "[audio {} omitted]",
            kiana_json::js_string::js_string(block.get("mimeType"))
        ))),
        Some("resource_link") => Ok(LlmContent::text(format!(
            "{}: {}",
            kiana_json::js_string::js_string(block.get("name")),
            kiana_json::js_string::js_string(block.get("uri"))
        ))),
        Some("resource") => try_resource_value_to_llm_content(block.get("resource")),
        _ => Ok(LlmContent::text(format!(
            "[unsupported MCP content {}]",
            kiana_json::js_string::js_string(block.get("type"))
        ))),
    }
}

/// 原始 JSON 内容块的**严格**转换（`blockToLlmContent(block)` 的对应物）。
///
/// 与 [`try_block_to_llm_content`] 的区别：这里直接读原始 `Value`，因此"键缺失"与"显式
/// `null`"仍然可区分（类型化的 [`ContentBlock`] 会把两者都折成 `None`）。
/// 判别顺序与 `switch (block.type)` 一致：`type` 不是字符串（数字块、数组块、缺 `type`）走
/// `default` 分支，`null` 块抛 `Cannot read properties of null (reading 'type')`。
pub fn try_block_value_to_llm_content(block: &Value) -> Result<LlmContent, McpTransportError> {
    // `!is_object() && is_null()` 等价于 `is_null()`；非对象非 null 的值（数字/字符串/数组）
    // 不报错，而是走下面的 `default` 分支——它们在 JS 里读 `.type` 得到 `undefined`。
    if !block.is_object() && block.is_null() {
        return Err(McpTransportError::network(
            "Cannot read properties of null (reading 'type')",
        ));
    }
    let kind = block.get("type").and_then(Value::as_str);
    match kind {
        Some("text") => Ok(LlmContent::text(
            block.get("text").map(js_field_text).unwrap_or_default(),
        )),
        Some("image") => Ok(LlmContent::image(
            // TS 把 `block.data` / `block.mimeType` 原样放进结果对象（不做类型检查）；
            // [`LlmContent`] 的字段是 `String`，所以按 `String()` 语义转换（与
            // `ImageContent::from_object` 的 `required_string`、文本分支的 `js_field_text`
            // 一致）。修复前非字符串值被折成空串（评审 finding 4）。
            block.get("data").map(js_field_text).unwrap_or_default(),
            block.get("mimeType").map(js_field_text).unwrap_or_default(),
        )),
        Some("audio") => Ok(LlmContent::text(format!(
            "[audio {} omitted]",
            kiana_json::js_string::js_string(block.get("mimeType"))
        ))),
        Some("resource_link") => Ok(LlmContent::text(format!(
            "{}: {}",
            kiana_json::js_string::js_string(block.get("name")),
            kiana_json::js_string::js_string(block.get("uri"))
        ))),
        Some("resource") => try_resource_value_to_llm_content(block.get("resource")),
        _ => Ok(LlmContent::text(format!(
            "[unsupported MCP content {}]",
            kiana_json::js_string::js_string(block.get("type"))
        ))),
    }
}

/// `default:` 分支（宽松版：见 [`try_other_block_to_llm_content`]）。
fn other_block_to_llm_content(block: &Value) -> LlmContent {
    try_other_block_to_llm_content(block).unwrap_or_else(|_| {
        LlmContent::text(format!(
            "[unsupported MCP content {}]",
            kiana_json::js_string::js_string(block.get("type"))
        ))
    })
}

/// 对象字段 → 文本：`null` / 缺失 → 空串，其余按 `String()`（与 [`string_field`] 一致）。
///
/// TS 对 `resource.text` 不校验，`{text: 0}` 的输出是数字 `0`；[`LlmContent::Text`] 的字段是
/// `String`，只能给出 `"0"`（见模块文档的偏差说明）。
fn js_field_text(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        kiana_json::js_string::js_string(Some(value))
    }
}

/// `toLlmContent(result)`：工具结果 → 模型内容。
///
/// 文本与图片原样通过；嵌入式文本资源变文本、嵌入式图片资源变图片；其余块（audio、resource
/// link、二进制资源）变短占位文本。**没有内容块但有 `structuredContent`** 时退化成它的
/// pretty JSON（2 空格缩进，`kiana_json` 的 JS 语义实现），因为服务器"应该"但常常没有把结构化结果
/// 镜像成文本。
///
/// `structuredContent` 的判定是 `!== undefined`：显式 `null` 会得到文本 `"null"`。
pub fn to_llm_content(result: &CallToolResult) -> Vec<LlmContent> {
    let mut content: Vec<LlmContent> = result.content.iter().map(block_to_llm_content).collect();
    push_structured_content(&mut content, result);
    content
}

/// `to_llm_content` 的**抛错**版本：TS 会抛 `TypeError` 的输入在这里返回错误信封
/// （见 [`try_block_to_llm_content`]）。
///
/// 回退路径（`content` 为空且 `structuredContent !== undefined`）不会抛错：它只合成文本内容，不做类型转换。
pub fn try_to_llm_content(result: &CallToolResult) -> Result<Vec<LlmContent>, McpTransportError> {
    let mut content = result
        .content
        .iter()
        .map(try_block_to_llm_content)
        .collect::<Result<Vec<_>, _>>()?;
    push_structured_content(&mut content, result);
    Ok(content)
}

/// `toLlmContent(result)` 的**原始 JSON** 版本：接受任意 JSON 值（TS 的签名也是如此，
/// `result.content ?? []` 与 `structuredContent !== undefined` 都在运行期判定）。
///
/// 这是 TS 抛错路径的完整复刻（[`try_to_llm_content`] 只覆盖已解析成 [`CallToolResult`] 的
/// 情况，而 `toLlmContent` 是包根导出、可以直接喂原始 JSON）：
/// - `result` 是 `null`/`undefined` → `TypeError: Cannot read properties of null (reading
///   'content')`；
/// - `content` 存在且不是数组（字符串/数字/对象）→ `TypeError: (result.content ?? []).map is
///   not a function`（非对象值如 `5`、`"x"`、`[1]` 读 `.content` 得到 `undefined`，不算）；
/// - 内容块自身的抛错见 [`try_block_to_llm_content`]。
pub fn try_to_llm_content_from_value(result: &Value) -> Result<Vec<LlmContent>, McpTransportError> {
    let Some(map) = result.as_object() else {
        if result.is_null() {
            return Err(McpTransportError::network(
                "Cannot read properties of null (reading 'content')",
            ));
        }
        // 原始值是数字/字符串/布尔/数组：`.content` 是 `undefined`，`?? []` 得到空数组，
        // `structuredContent` 同样是 `undefined`，因此结果为空。
        return Ok(Vec::new());
    };
    let blocks: &[Value] = match map.get("content") {
        None | Some(Value::Null) => &[],
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(McpTransportError::network(
                "(result.content ?? []).map is not a function",
            ));
        }
    };
    let mut content = blocks
        .iter()
        .map(try_block_value_to_llm_content)
        .collect::<Result<Vec<_>, _>>()?;
    if content.is_empty()
        && let Some(structured) = map.get("structuredContent")
    {
        push_structured_json(&mut content, structured);
    }
    Ok(content)
}

/// 「没有内容块但有 `structuredContent`」时的 JSON 回退（`content.length === 0 &&
/// structuredContent !== undefined`）。空列表 + 缺省 `structuredContent` 得到空列表。
fn push_structured_content(content: &mut Vec<LlmContent>, result: &CallToolResult) {
    if let Some(structured) = &result.structured_content {
        push_structured_json(content, structured);
    }
}

/// [`push_structured_content`] 的共同实现（`JSON.stringify(value, null, 2)`，整数样式键在前）。
fn push_structured_json(content: &mut Vec<LlmContent>, structured: &Value) {
    if !content.is_empty() {
        return;
    }
    // `JSON.stringify(value, null, 2)` 的键序是「整数样式键按数值升序在前，其余保持插入序」
    // （`kiana_json::js_object::sort_js_keys` 的规则）；serde_json 直接序列化不做这一步。
    let normalized = kiana_json::js_object::sort_js_keys(structured);
    content.push(LlmContent::text(kiana_json::js_object::stringify_pretty(
        &normalized,
        2,
    )));
}

// ---------------------------------------------------------------------------
// 宽松反序列化（TS 的接口只在编译期存在）
// ---------------------------------------------------------------------------
//
// TS 的这些接口只是编译期声明：运行期客户端只校验一个子集（`client.ts` 的
// `isTool`/`isResource`/…），其余字段原样传递。若 Rust 用派生的 `Deserialize` 要求字段类型
// 严格匹配，服务器发来的 `{"uri":"file:///a"}`（缺 `name`，`client.ts` 明确容忍）或
// `description: 5` 就会整体失败，而 TS 会照收不误。因此这些结构体不派生 `Deserialize`，
// 统一用下面的宽松规则：
//
// - 字符串字段：缺失/`null` → `None`，非字符串按 `String()` 转换（模板字面量语义）；
// - 对象/数组字段类型不符 → 字段归 `None`，原值留在 `extra` 里；
// - 未知键一律进 `extra`（逐字往返）。

/// 取一个对象字段；缺失与显式 `null` 同等对待（TS 的 `?? undefined`）。
fn field<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|value| !value.is_null())
}

/// `String(value)` 形式的字符串字段。
fn string_field(map: &Map<String, Value>, key: &str) -> Option<String> {
    field(map, key).map(|value| kiana_json::js_string::js_string(Some(value)))
}

/// 必填字符串：缺失时为空串（TS 运行期不校验）。
fn required_string(map: &Map<String, Value>, key: &str) -> String {
    string_field(map, key).unwrap_or_default()
}

/// **可选**字符串字段：只有真正的字符串才进类型化字段。
///
/// TS 的接口不做运行期校验，`mimeType: 5` 会原样往返；`Option<String>` 只能按 `String()` 写成
/// `"5"`（伪造了一个值）。因此非字符串值 → `None`，原值由调用方的 `restore_unconsumed`
/// 放进 `extra`，序列化时 `skip_serializing_if` 让类型化字段缺席、`extra` 写出原值。
fn optional_string(map: &Map<String, Value>, key: &str) -> Option<String> {
    field(map, key).and_then(Value::as_str).map(str::to_owned)
}

/// 布尔字段（`null`/非布尔 → `None`）。与 `optional_string` 一样只做类型判断，不负责保留
/// 原值：需要逐字往返的调用方要把 `(key, field.is_some())` 传给 `restore_unconsumed`。
fn bool_field(map: &Map<String, Value>, key: &str) -> Option<bool> {
    field(map, key).and_then(Value::as_bool)
}

/// 数字字段（`null`/非数字 → `None`）。
fn number_field(map: &Map<String, Value>, key: &str) -> Option<f64> {
    field(map, key).and_then(Value::as_f64)
}

/// 子对象字段：非对象值不算（原值留在 `extra`）。
fn typed_field<T>(
    map: &Map<String, Value>,
    key: &str,
    convert: impl Fn(&Value) -> Option<T>,
) -> Option<T> {
    field(map, key)
        .filter(|value| value.is_object())
        .and_then(convert)
}

/// `Record<string, unknown>` 字段。
fn map_field(map: &Map<String, Value>, key: &str) -> Option<Map<String, Value>> {
    field(map, key).and_then(|value| value.as_object().cloned())
}

/// 未消费的键 → `extra`（保持插入序）。
fn extra_fields(map: &Map<String, Value>, known: &[&str]) -> Map<String, Value> {
    map.iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// 内容块的 `extra`：未知键 + 没能塞进 `annotations`/`_meta` 的原值（类型不符时 TS 原样保留）。
fn extra_with_annotations_and_meta(
    map: &Map<String, Value>,
    known: &[&str],
    annotations_consumed: bool,
    meta_consumed: bool,
) -> Map<String, Value> {
    let mut extra = extra_fields(map, known);
    restore_unconsumed(
        &mut extra,
        map,
        &[
            ("annotations", annotations_consumed),
            ("_meta", meta_consumed),
        ],
    );
    extra
}

/// 类型不符、没能塞进类型化字段的**已知**键也要原样保留（TS 的接口不做运行期校验，
/// `_meta: 1` 会照原样留在对象里）。`checks` 是 `(键, 是否已消费)`。
fn restore_unconsumed(
    extra: &mut Map<String, Value>,
    map: &Map<String, Value>,
    checks: &[(&str, bool)],
) {
    for (key, consumed) in checks {
        if !consumed
            && let Some(value) = map.get(*key)
            && !value.is_null()
        {
            extra.insert((*key).to_owned(), value.clone());
        }
    }
}

/// 反序列化对象；非对象报错（这些结构体都要求对象）。
fn object_of<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Map<String, Value>, D::Error> {
    use serde::de::Error;
    match Value::deserialize(deserializer)? {
        Value::Object(map) => Ok(map),
        other => Err(D::Error::custom(format!(
            "expected an object, found {}",
            kiana_json::js_string::js_typeof(&other).as_str()
        ))),
    }
}

/// 数组字段：非数组为空；元素转换返回 `None` 时被**跳过**（例如 `tools: [1]` 得到空列表而
/// 不是错误）。`client.rs` 的校验函数（`validateListPage`、`validate_read_resource_result`
/// 等）在调用这些 `from_value` 之前已校验过元素形状；直接调用 `from_value` 的代码没有这层保护。
fn list_of<T>(
    map: &Map<String, Value>,
    key: &str,
    convert: impl Fn(&Value) -> Option<T>,
) -> Vec<T> {
    map.get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(convert).collect())
        .unwrap_or_default()
}

/// 已知键清单（用于把未知键收进 `extra`）。
///
/// 数组里的名字是**线上**（camelCase）键名，必须与结构体字段的 `rename`/`rename_all` 一致：
/// 若新增类型化字段却漏登记，该键会先被 `extra_fields` 保留，再与类型化字段一起被
/// `serde(flatten)` 写出，产生重复键。这些常量只在私有模块内使用，不属于公开 API，因此
/// 不逐个加文档。
mod keys {
    pub const IMPLEMENTATION: &[&str] = &["name", "version", "title"];
    pub const ROOT: &[&str] = &["uri", "name"];
    pub const LIST_CHANGED: &[&str] = &["listChanged"];
    pub const RESOURCES_CAPABILITY: &[&str] = &["subscribe", "listChanged"];
    pub const CLIENT_CAPABILITIES: &[&str] = &["experimental", "roots", "sampling", "elicitation"];
    pub const SERVER_CAPABILITIES: &[&str] = &[
        "experimental",
        "logging",
        "prompts",
        "resources",
        "tools",
        "completions",
    ];
    pub const INITIALIZE_PARAMS: &[&str] = &["protocolVersion", "capabilities", "clientInfo"];
    pub const INITIALIZE_RESULT: &[&str] = &[
        "protocolVersion",
        "capabilities",
        "serverInfo",
        "instructions",
    ];
    pub const PROGRESS: &[&str] = &["progressToken", "progress", "total", "message"];
    pub const CANCELLED: &[&str] = &["requestId", "reason"];
    pub const TOOL_ANNOTATIONS: &[&str] = &[
        "title",
        "readOnlyHint",
        "destructiveHint",
        "idempotentHint",
        "openWorldHint",
    ];
    pub const TOOL_EXECUTION: &[&str] = &["taskSupport"];
    pub const TOOL: &[&str] = &[
        "name",
        "title",
        "description",
        "inputSchema",
        "outputSchema",
        "annotations",
        "execution",
        "_meta",
    ];
    pub const LIST_TOOLS: &[&str] = &["tools", "nextCursor", "_meta"];
    pub const PROMPT_ARGUMENT: &[&str] = &["name", "description", "required"];
    pub const PROMPT: &[&str] = &["name", "title", "description", "arguments", "_meta"];
    pub const LIST_PROMPTS: &[&str] = &["prompts", "nextCursor", "_meta"];
    pub const PROMPT_MESSAGE: &[&str] = &["role", "content"];
    pub const GET_PROMPT: &[&str] = &["description", "messages", "_meta"];
    pub const RESOURCE: &[&str] = &[
        "uri",
        "name",
        "title",
        "description",
        "mimeType",
        "size",
        "annotations",
        "_meta",
    ];
    pub const RESOURCE_TEMPLATE: &[&str] = &[
        "uriTemplate",
        "name",
        "title",
        "description",
        "mimeType",
        "annotations",
        "_meta",
    ];
    pub const LIST_RESOURCES: &[&str] = &["resources", "nextCursor", "_meta"];
    pub const LIST_RESOURCE_TEMPLATES: &[&str] = &["resourceTemplates", "nextCursor", "_meta"];
    pub const READ_RESOURCE: &[&str] = &["contents", "_meta"];
    pub const CALL_TOOL_RESULT: &[&str] = &["content", "structuredContent", "isError", "_meta"];
    pub const CONTENT_ANNOTATIONS: &[&str] = &["audience", "priority", "lastModified"];
    pub const TEXT_CONTENT: &[&str] = &["type", "text", "annotations", "_meta"];
    pub const IMAGE_CONTENT: &[&str] = &["type", "data", "mimeType", "annotations", "_meta"];
    pub const AUDIO_CONTENT: &[&str] = &["type", "data", "mimeType", "annotations", "_meta"];
    pub const RESOURCE_LINK: &[&str] = &[
        "type",
        "uri",
        "name",
        "title",
        "description",
        "mimeType",
        "size",
        "annotations",
        "_meta",
    ];
    pub const TEXT_RESOURCE: &[&str] = &["uri", "mimeType", "text", "_meta"];
    pub const BLOB_RESOURCE: &[&str] = &["uri", "mimeType", "blob", "_meta"];
    pub const EMBEDDED_RESOURCE: &[&str] = &["type", "resource", "annotations", "_meta"];
}

impl Implementation {
    /// 宽松构造（非对象 → `None`）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let title = optional_string(map, "title");
        let mut extra = extra_fields(map, keys::IMPLEMENTATION);
        restore_unconsumed(&mut extra, map, &[("title", title.is_some())]);
        Some(Implementation {
            name: required_string(map, "name"),
            version: required_string(map, "version"),
            title,
            extra,
        })
    }
}

impl Root {
    /// 宽松构造。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let name = optional_string(map, "name");
        let mut extra = extra_fields(map, keys::ROOT);
        restore_unconsumed(&mut extra, map, &[("name", name.is_some())]);
        Some(Root {
            uri: required_string(map, "uri"),
            name,
            extra,
        })
    }
}

impl ListChangedCapability {
    fn from_object(map: &Map<String, Value>) -> Self {
        ListChangedCapability {
            list_changed: bool_field(map, "listChanged"),
            extra: extra_fields(map, keys::LIST_CHANGED),
        }
    }
}

impl ResourcesCapability {
    fn from_object(map: &Map<String, Value>) -> Self {
        ResourcesCapability {
            subscribe: bool_field(map, "subscribe"),
            list_changed: bool_field(map, "listChanged"),
            extra: extra_fields(map, keys::RESOURCES_CAPABILITY),
        }
    }
}

impl ClientCapabilities {
    fn from_object(map: &Map<String, Value>) -> Self {
        let experimental = map_field(map, "experimental");
        let roots = typed_field(map, "roots", |value| {
            value.as_object().map(ListChangedCapability::from_object)
        });
        let sampling = map_field(map, "sampling");
        let elicitation = map_field(map, "elicitation");
        let mut extra = extra_fields(map, keys::CLIENT_CAPABILITIES);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("experimental", experimental.is_some()),
                ("roots", roots.is_some()),
                ("sampling", sampling.is_some()),
                ("elicitation", elicitation.is_some()),
            ],
        );
        ClientCapabilities {
            experimental,
            roots,
            sampling,
            elicitation,
            extra,
        }
    }
}

impl ServerCapabilities {
    fn from_object(map: &Map<String, Value>) -> Self {
        let experimental = map_field(map, "experimental");
        let logging = map_field(map, "logging");
        let prompts = typed_field(map, "prompts", |value| {
            value.as_object().map(ListChangedCapability::from_object)
        });
        let resources = typed_field(map, "resources", |value| {
            value.as_object().map(ResourcesCapability::from_object)
        });
        let tools = typed_field(map, "tools", |value| {
            value.as_object().map(ListChangedCapability::from_object)
        });
        let completions = map_field(map, "completions");
        let mut extra = extra_fields(map, keys::SERVER_CAPABILITIES);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("experimental", experimental.is_some()),
                ("logging", logging.is_some()),
                ("prompts", prompts.is_some()),
                ("resources", resources.is_some()),
                ("tools", tools.is_some()),
                ("completions", completions.is_some()),
            ],
        );
        ServerCapabilities {
            experimental,
            logging,
            prompts,
            resources,
            tools,
            completions,
            extra,
        }
    }
}

impl InitializeParams {
    /// 宽松构造（非对象 → `None`）。`capabilities`/`clientInfo` 是必填字段：缺失或类型不符
    /// 时退化成默认值，原值也不会进 `extra`（否则 `serde(flatten)` 会写出重复键，见模块文档
    /// 的必填字段偏差）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let capabilities = typed_field(map, "capabilities", |value| {
            value.as_object().map(ClientCapabilities::from_object)
        });
        let client_info = typed_field(map, "clientInfo", Implementation::from_value);
        // `capabilities`/`clientInfo` 是**必填**字段（总是被序列化），不能同时进 `extra`，
        // 否则 `serde(flatten)` 会写出重复键。类型不符时退化成默认值（已知偏差）。
        let extra = extra_fields(map, keys::INITIALIZE_PARAMS);
        Some(InitializeParams {
            protocol_version: required_string(map, "protocolVersion"),
            capabilities: capabilities.unwrap_or_default(),
            client_info: client_info.unwrap_or_default(),
            extra,
        })
    }
}

impl InitializeResult {
    /// 宽松构造（非对象 → `None`）。`capabilities`/`serverInfo` 的必填字段偏差同
    /// [`InitializeParams::from_value`]；`instructions` 只在字符串时进类型化字段，**非 `null`**
    /// 的其他原值由 `restore_unconsumed` 放回 `extra`。显式 `instructions: null` 被 `field()`
    /// 当作缺失，`from_value` 路径会丢失它；`client.rs` 的 `validate_initialize_result` 在调用
    /// 本函数后用 `patch_nulls` 把它补回 `extra`。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let capabilities = typed_field(map, "capabilities", |value| {
            value.as_object().map(ServerCapabilities::from_object)
        });
        let server_info = typed_field(map, "serverInfo", Implementation::from_value);
        // `capabilities`/`serverInfo` 是**必填**字段（见 [`Tool::from_value`] 的同类说明）。
        let instructions = optional_string(map, "instructions");
        let mut extra = extra_fields(map, keys::INITIALIZE_RESULT);
        restore_unconsumed(&mut extra, map, &[("instructions", instructions.is_some())]);
        Some(InitializeResult {
            protocol_version: required_string(map, "protocolVersion"),
            capabilities: capabilities.unwrap_or_default(),
            server_info: server_info.unwrap_or_default(),
            instructions,
            extra,
        })
    }
}

impl ProgressNotification {
    /// 宽松构造；非对象、或 `progressToken` 不是合法 id 时返回 `None`（`client.rs` 的
    /// `handle_progress` 对这类通知直接忽略）。`progress` 缺失/非数字 → `NaN`；`message` 的
    /// **非 `null`** 且非字符串的原值由 `restore_unconsumed` 放回 `extra`。显式 `message: null`
    /// 被 `field()` 当作缺失，`from_value` 路径会丢失它；`client.rs` 的 `handle_progress` 不经过
    /// 本函数（它把原始 `Value` 直接交给进度回调），因此没有 `patch_nulls` 之类的补回。
    /// `progress`/`total` 的非数字原值同样不会保留（见字段文档）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let message = optional_string(map, "message");
        let mut extra = extra_fields(map, keys::PROGRESS);
        restore_unconsumed(&mut extra, map, &[("message", message.is_some())]);
        Some(ProgressNotification {
            progress_token: crate::jsonrpc::value_to_json_rpc_id(map.get("progressToken")?)?,
            progress: number_field(map, "progress").unwrap_or(f64::NAN),
            total: number_field(map, "total"),
            message,
            extra,
        })
    }
}

impl CancelledNotification {
    /// 宽松构造；`requestId` 不是合法 id 时返回 `None`。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let reason = optional_string(map, "reason");
        let mut extra = extra_fields(map, keys::CANCELLED);
        restore_unconsumed(&mut extra, map, &[("reason", reason.is_some())]);
        Some(CancelledNotification {
            request_id: crate::jsonrpc::value_to_json_rpc_id(map.get("requestId")?)?,
            reason,
            extra,
        })
    }
}

impl ToolAnnotations {
    fn from_object(map: &Map<String, Value>) -> Self {
        let title = optional_string(map, "title");
        let mut extra = extra_fields(map, keys::TOOL_ANNOTATIONS);
        restore_unconsumed(&mut extra, map, &[("title", title.is_some())]);
        ToolAnnotations {
            title,
            read_only_hint: bool_field(map, "readOnlyHint"),
            destructive_hint: bool_field(map, "destructiveHint"),
            idempotent_hint: bool_field(map, "idempotentHint"),
            open_world_hint: bool_field(map, "openWorldHint"),
            extra,
        }
    }
}

impl ToolExecution {
    fn from_object(map: &Map<String, Value>) -> Self {
        let task_support =
            optional_string(map, "taskSupport").and_then(|text| match text.as_str() {
                "forbidden" => Some(TaskSupport::Forbidden),
                "optional" => Some(TaskSupport::Optional),
                "required" => Some(TaskSupport::Required),
                _ => None,
            });
        let mut extra = extra_fields(map, keys::TOOL_EXECUTION);
        restore_unconsumed(&mut extra, map, &[("taskSupport", task_support.is_some())]);
        ToolExecution {
            task_support,
            extra,
        }
    }
}

impl Tool {
    /// 宽松构造（`name`/`inputSchema` 缺失不再整体失败；`client.ts` 的 `isTool` 才做校验）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let input_schema = map_field(map, "inputSchema");
        let output_schema = map_field(map, "outputSchema");
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ToolAnnotations::from_object)
        });
        let execution = typed_field(map, "execution", |value| {
            value.as_object().map(ToolExecution::from_object)
        });
        let meta = map_field(map, "_meta");
        let mut extra = extra_fields(map, keys::TOOL);
        // `inputSchema` 是**必填**字段（`Map<String, Value>`，总是被序列化），不能同时放进
        // `extra`——`serde(flatten)` 会写出两个同名键。非对象/缺失时退化成 `{}`（TS 会原样
        // 保留 `5`），这是必填字段面的已知偏差（见模块文档）。
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("outputSchema", output_schema.is_some()),
                ("annotations", annotations.is_some()),
                ("execution", execution.is_some()),
                ("_meta", meta.is_some()),
            ],
        );
        let title = optional_string(map, "title");
        let description = optional_string(map, "description");
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("title", title.is_some()),
                ("description", description.is_some()),
            ],
        );
        Some(Tool {
            name: required_string(map, "name"),
            title,
            description,
            input_schema: input_schema.unwrap_or_default(),
            output_schema,
            annotations,
            execution,
            meta,
            extra,
        })
    }
}

impl ListToolsResult {
    /// 宽松构造（非对象 → `None`）：`tools` 非数组 → 空列表，元素不是对象时被跳过
    /// （`list_of` 的 `filter_map`）；`nextCursor` 的**非 `null`** 且非字符串原值留在 `extra`。
    /// 显式 `nextCursor: null` 被 `field()` 当作缺失、`from_value` 路径不会保留它；`client.rs`
    /// 走的是 `validate_list_page` 而不是本函数，那里把 `null` 与空串都视为最后一页（见其文档）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let meta = map_field(map, "_meta");
        let next_cursor = optional_string(map, "nextCursor");
        let mut extra = extra_fields(map, keys::LIST_TOOLS);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("_meta", meta.is_some()),
                ("nextCursor", next_cursor.is_some()),
            ],
        );
        Some(ListToolsResult {
            tools: list_of(map, "tools", Tool::from_value),
            next_cursor,
            meta,
            extra,
        })
    }
}

impl Resource {
    /// 宽松构造。
    ///
    /// `name` 缺失/`null` 时按 `client.ts` 的 `toResource` 语义用 `uri` 回填
    /// （`item.name ?? item.uri`）：有些服务器不填 `name`，而 TS 客户端明确容忍。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let uri = required_string(map, "uri");
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let size = number_field(map, "size");
        let mut extra = extra_fields(map, keys::RESOURCE);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("annotations", annotations.is_some()),
                ("_meta", meta.is_some()),
                ("size", size.is_some()),
            ],
        );
        let title = optional_string(map, "title");
        let description = optional_string(map, "description");
        let mime_type = optional_string(map, "mimeType");
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("title", title.is_some()),
                ("description", description.is_some()),
                ("mimeType", mime_type.is_some()),
            ],
        );
        Some(Resource {
            uri: uri.clone(),
            name: string_field(map, "name").unwrap_or_else(|| uri.clone()),
            title,
            description,
            mime_type,
            size,
            annotations,
            meta,
            extra,
        })
    }
}

impl ResourceTemplate {
    /// 宽松构造（`name` 缺失/`null` 时用 `uriTemplate` 回填，同 `toResourceTemplate`）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let uri_template = required_string(map, "uriTemplate");
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let mut extra = extra_fields(map, keys::RESOURCE_TEMPLATE);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("annotations", annotations.is_some()),
                ("_meta", meta.is_some()),
            ],
        );
        let title = optional_string(map, "title");
        let description = optional_string(map, "description");
        let mime_type = optional_string(map, "mimeType");
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("title", title.is_some()),
                ("description", description.is_some()),
                ("mimeType", mime_type.is_some()),
            ],
        );
        Some(ResourceTemplate {
            uri_template: uri_template.clone(),
            name: string_field(map, "name").unwrap_or_else(|| uri_template.clone()),
            title,
            description,
            mime_type,
            annotations,
            meta,
            extra,
        })
    }
}

impl ListResourcesResult {
    /// 宽松构造（非对象 → `None`）：`resources` 非数组 → 空列表，元素不是对象时被跳过；
    /// `nextCursor` 的**非 `null`** 且非字符串原值留在 `extra`。显式 `nextCursor: null` 被
    /// `field()` 当作缺失、`from_value` 路径不会保留它；`client.rs` 走的是 `validate_list_page`
    /// 而不是本函数，那里把 `null` 与空串都视为最后一页（见其文档）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let meta = map_field(map, "_meta");
        let next_cursor = optional_string(map, "nextCursor");
        let mut extra = extra_fields(map, keys::LIST_RESOURCES);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("_meta", meta.is_some()),
                ("nextCursor", next_cursor.is_some()),
            ],
        );
        Some(ListResourcesResult {
            resources: list_of(map, "resources", Resource::from_value),
            next_cursor,
            meta,
            extra,
        })
    }
}

impl ListResourceTemplatesResult {
    /// 宽松构造（非对象 → `None`）：`resourceTemplates` 非数组 → 空列表，元素不是对象时被
    /// 跳过；`nextCursor` 的**非 `null`** 且非字符串原值留在 `extra`。显式 `nextCursor: null`
    /// 被 `field()` 当作缺失、`from_value` 路径不会保留它；`client.rs` 走的是
    /// `validate_list_page` 而不是本函数，那里把 `null` 与空串都视为最后一页（见其文档）。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let meta = map_field(map, "_meta");
        let next_cursor = optional_string(map, "nextCursor");
        let mut extra = extra_fields(map, keys::LIST_RESOURCE_TEMPLATES);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("_meta", meta.is_some()),
                ("nextCursor", next_cursor.is_some()),
            ],
        );
        Some(ListResourceTemplatesResult {
            resource_templates: list_of(map, "resourceTemplates", ResourceTemplate::from_value),
            next_cursor,
            meta,
            extra,
        })
    }
}

impl ReadResourceResult {
    /// 宽松构造（非对象 → `None`）：`contents` 非数组 → 空列表；每个元素按私有的
    /// `resource_contents_from_value` 判别，无法类型化时保留为 [`ResourceContents::Other`]。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let meta = map_field(map, "_meta");
        let mut extra = extra_fields(map, keys::READ_RESOURCE);
        restore_unconsumed(&mut extra, map, &[("_meta", meta.is_some())]);
        Some(ReadResourceResult {
            contents: list_of(map, "contents", |value| {
                Some(resource_contents_from_value(value))
            }),
            meta,
            extra,
        })
    }
}

impl CallToolResult {
    /// 宽松构造（非对象 → `None`；`content` 缺失/`null` → 空数组，§30.12）。
    ///
    /// `content` **存在但不是数组**时返回 `None`（`serde_json::from_value` 随之报错）：
    /// TS 的 `(result.content ?? []).map(...)` 对这类输入抛
    /// `TypeError: (result.content ?? []).map is not a function`，`validateCallToolResult`
    /// 也抛 `Invalid MCP tools/call result`——两种 TS 路径都**不接受**它。若这里把它读成空数组，
    /// `to_llm_content` 会给出 `structuredContent` 回退文本，即对畸形服务器输出编造一个完全不同的
    /// 结果（评审 finding #5）。需要 TS 的逐字抛错语义时用
    /// [`try_to_llm_content_from_value`]。
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        // 缺失或显式 `null` → 空数组（§30.12）；存在但不是数组返回 `None`，让 serde 层
        // 报错。TS 的 `toLlmContent` 与 `validateCallToolResult` 对这类输入都抛错，不能把
        // 它"修"成空数组（否则会给畸形服务器输出编造一个 structuredContent 回退结果）。
        let content = match map.get("content") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items.iter().map(content_block_from_value).collect(),
            Some(_) => return None,
        };
        let meta = map_field(map, "_meta");
        let is_error = bool_field(map, "isError");
        let mut extra = extra_fields(map, keys::CALL_TOOL_RESULT);
        restore_unconsumed(
            &mut extra,
            map,
            &[("_meta", meta.is_some()), ("isError", is_error.is_some())],
        );
        Some(CallToolResult {
            content,
            structured_content: map.get("structuredContent").cloned(),
            is_error,
            meta,
            extra,
        })
    }
}

impl ContentAnnotations {
    fn from_object(map: &Map<String, Value>) -> Self {
        let audience = map.get("audience").and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(|item| match item.as_str() {
                    Some("user") => Some(Audience::User),
                    Some("assistant") => Some(Audience::Assistant),
                    _ => None,
                })
                .collect()
        });
        let priority = number_field(map, "priority");
        let last_modified = optional_string(map, "lastModified");
        let mut extra = extra_fields(map, keys::CONTENT_ANNOTATIONS);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("audience", audience.is_some()),
                ("priority", priority.is_some()),
                ("lastModified", last_modified.is_some()),
            ],
        );
        ContentAnnotations {
            audience,
            priority,
            last_modified,
            extra,
        }
    }
}

impl TextContent {
    fn from_object(map: &Map<String, Value>) -> Self {
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let extra = extra_with_annotations_and_meta(
            map,
            keys::TEXT_CONTENT,
            annotations.is_some(),
            meta.is_some(),
        );
        TextContent {
            kind: TextKind::Text,
            text: required_string(map, "text"),
            annotations,
            meta,
            extra,
        }
    }
}

impl ImageContent {
    fn from_object(map: &Map<String, Value>) -> Self {
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let extra = extra_with_annotations_and_meta(
            map,
            keys::IMAGE_CONTENT,
            annotations.is_some(),
            meta.is_some(),
        );
        ImageContent {
            kind: ImageKind::Image,
            data: required_string(map, "data"),
            mime_type: required_string(map, "mimeType"),
            annotations,
            meta,
            extra,
        }
    }
}

impl AudioContent {
    fn from_object(map: &Map<String, Value>) -> Self {
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let extra = extra_with_annotations_and_meta(
            map,
            keys::AUDIO_CONTENT,
            annotations.is_some(),
            meta.is_some(),
        );
        AudioContent {
            kind: AudioKind::Audio,
            data: required_string(map, "data"),
            mime_type: required_string(map, "mimeType"),
            annotations,
            meta,
            extra,
        }
    }
}

impl ResourceLinkContent {
    fn from_object(map: &Map<String, Value>) -> Self {
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let size = number_field(map, "size");
        let title = optional_string(map, "title");
        let description = optional_string(map, "description");
        let mime_type = optional_string(map, "mimeType");
        let mut extra = extra_fields(map, keys::RESOURCE_LINK);
        restore_unconsumed(
            &mut extra,
            map,
            &[
                ("annotations", annotations.is_some()),
                ("_meta", meta.is_some()),
                ("size", size.is_some()),
                ("title", title.is_some()),
                ("description", description.is_some()),
                ("mimeType", mime_type.is_some()),
            ],
        );
        ResourceLinkContent {
            kind: ResourceLinkKind::ResourceLink,
            uri: required_string(map, "uri"),
            name: required_string(map, "name"),
            title,
            description,
            mime_type,
            size,
            annotations,
            meta,
            extra,
        }
    }
}

impl TextResourceContents {
    fn from_object(map: &Map<String, Value>) -> Self {
        let meta = map_field(map, "_meta");
        let mime_type = optional_string(map, "mimeType");
        let mut extra = extra_fields(map, keys::TEXT_RESOURCE);
        restore_unconsumed(
            &mut extra,
            map,
            &[("_meta", meta.is_some()), ("mimeType", mime_type.is_some())],
        );
        TextResourceContents {
            uri: required_string(map, "uri"),
            mime_type,
            text: required_string(map, "text"),
            meta,
            extra,
        }
    }
}

impl BlobResourceContents {
    fn from_object(map: &Map<String, Value>) -> Self {
        let meta = map_field(map, "_meta");
        let mime_type = optional_string(map, "mimeType");
        let mut extra = extra_fields(map, keys::BLOB_RESOURCE);
        restore_unconsumed(
            &mut extra,
            map,
            &[("_meta", meta.is_some()), ("mimeType", mime_type.is_some())],
        );
        BlobResourceContents {
            uri: required_string(map, "uri"),
            mime_type,
            blob: required_string(map, "blob"),
            meta,
            extra,
        }
    }
}

/// `"text" in resource` 的分支选择（§30.12：**键存在**即可，与值的类型无关）。
///
/// 这正是 TS 与"按字段类型猜测"的区别：`{uri, text: null, mimeType:"image/png", blob:"b"}`
/// 在 TS 里走文本分支（`text` 原样是 `null`），而不是图片分支。见 [`resource_to_llm_content`]。
///
/// `blob` 分支只在能**逐字**表达时才用类型化的 [`BlobResourceContents`]：`uri` 与 `blob` 都
/// 必须是字符串、`mimeType` 必须是缺失/`null`/字符串。否则（`uri` 缺失或非字符串、`mimeType` 是数字/对象…）
/// 落到 [`ResourceContents::Other`]，由 [`resource_value_to_llm_content`] 按 TS 的模板字面量与
/// 可选链语义处理：
/// - `uri` 缺失 → `"[binary resource undefined …]"`、`uri: null` → `"… null …"`
///   （类型化的 `uri: String` 只能给出空串，评审 finding #1）；
/// - `mimeType` 非字符串时 TS 抛 `TypeError: resource.mimeType?.startsWith is not a function`
///   （评审 finding #4），类型化字段的 `String()` 强制转换会伪造一个占位值。
fn resource_contents_from_value(value: &Value) -> ResourceContents {
    if let Some(map) = value.as_object() {
        // `"text" in resource` 优先**且互斥**：有 `text` 键就只能是文本资源（类型化或原样保留），
        // 绝不能因为 `text` 不是字符串而落到 `blob` 分支。
        if map.contains_key("text") {
            return if can_use_typed_text_resource(map) {
                ResourceContents::Text(TextResourceContents::from_object(map))
            } else {
                ResourceContents::Other(value.clone())
            };
        }
        if map.contains_key("blob") && can_use_typed_blob(map) {
            return ResourceContents::Blob(BlobResourceContents::from_object(map));
        }
    }
    ResourceContents::Other(value.clone())
}

/// `TextResourceContents` 能否逐字表达这个对象：`uri` 与 `text` 都必须是字符串。
///
/// TS 的接口不做运行期校验，`{uri:"u", text: 5}` 会原样往返；类型化的 `text: String` 只能按
/// `String()` 写成 `"5"`，因此这类对象落到 [`ResourceContents::Other`] 保留原值（评审
/// finding #6）。`to_llm_content` 仍然按 `"text" in resource` 走文本分支。
fn can_use_typed_text_resource(map: &Map<String, Value>) -> bool {
    map.get("uri").is_some_and(Value::is_string) && map.get("text").is_some_and(Value::is_string)
}

/// `BlobResourceContents` 能否逐字表达这个对象（见 [`resource_contents_from_value`]）。
///
/// `blob` 也必须是字符串：`required_string` 会把 `{blob: 5}` 写成 `"5"`、把 `{blob: {}}`
/// 写成 `"[object Object]"`（评审 finding #1 的同类问题）。类型化字段是 `String`，非字符串值
/// 只能落到 [`ResourceContents::Other`] 才能逐字往返；`to_llm_content` 那边 `Other` 分支仍按
/// 模板字面量给出同样的文本。
fn can_use_typed_blob(map: &Map<String, Value>) -> bool {
    map.get("uri").is_some_and(Value::is_string)
        && map.get("blob").is_some_and(Value::is_string)
        && matches!(
            map.get("mimeType"),
            None | Some(Value::Null) | Some(Value::String(_))
        )
}

impl EmbeddedResourceContent {
    fn from_object(map: &Map<String, Value>) -> Self {
        let annotations = typed_field(map, "annotations", |value| {
            value.as_object().map(ContentAnnotations::from_object)
        });
        let meta = map_field(map, "_meta");
        let extra = extra_with_annotations_and_meta(
            map,
            keys::EMBEDDED_RESOURCE,
            annotations.is_some(),
            meta.is_some(),
        );
        EmbeddedResourceContent {
            kind: EmbeddedResourceKind::Resource,
            resource: map
                .get("resource")
                .map(resource_contents_from_value)
                .unwrap_or_else(|| ResourceContents::Other(Value::Null)),
            annotations,
            meta,
            extra,
        }
    }
}

/// 按 `type` 判别内容块（`switch (block.type)` 的分支集合）。
///
/// `"audio"` 与 `"resource_link"` 需要**模板字面量插值**的字段（`mimeType`、`name`/`uri`）：
/// TS 在这些位置直接做 `` `${block.mimeType}` ``，缺失读作 `"undefined"`。类型化变体把它们存成
/// `String`，无法表达"缺失"，所以只有当这些字段确实是字符串时才落到类型化变体，否则保留
/// [`ContentBlock::Other`] 的原始 JSON，由 [`other_block_to_llm_content`] 按 `String()` 语义
/// 生成占位文本（`[audio undefined omitted]`、`undefined: undefined`）。
fn content_block_from_value(value: &Value) -> ContentBlock {
    let Some(map) = value.as_object() else {
        return ContentBlock::Other(value.clone());
    };
    match map.get("type").and_then(Value::as_str) {
        // 类型化变体的必填字符串字段只有在**确实是字符串**时才能逐字往返；否则保留原始
        // `Value`（[`ContentBlock::Other`]），由 [`other_block_to_llm_content`] 按原始
        // `type` 分派。TS 的接口不做运行期校验，`{type:"text", text: 5}` 会原样通过。
        Some("text") if map.get("text").is_some_and(Value::is_string) => {
            ContentBlock::Text(TextContent::from_object(map))
        }
        Some("image")
            if map.get("data").is_some_and(Value::is_string)
                && map.get("mimeType").is_some_and(Value::is_string) =>
        {
            ContentBlock::Image(ImageContent::from_object(map))
        }
        Some("audio")
            if map.get("data").is_some_and(Value::is_string)
                && map.get("mimeType").is_some_and(Value::is_string) =>
        {
            ContentBlock::Audio(AudioContent::from_object(map))
        }
        Some("resource_link")
            if map.get("name").is_some_and(Value::is_string)
                && map.get("uri").is_some_and(Value::is_string) =>
        {
            ContentBlock::ResourceLink(ResourceLinkContent::from_object(map))
        }
        // `resource` 键缺失时保留原始 `Value`：TS 的 `"text" in undefined` 抛的是
        // `… in undefined`，显式 `resource: null` 抛 `… in null`，两者必须可区分。
        Some("resource") if map.contains_key("resource") => {
            ContentBlock::EmbeddedResource(EmbeddedResourceContent::from_object(map))
        }
        _ => ContentBlock::Other(value.clone()),
    }
}

/// 为宽松结构体生成 `Deserialize`（统一走 `from_value`）。
///
/// 生成的实现先 `object_of` 整体读入对象，再调用各类型的 `from_value`。错误语义：输入非
/// 对象时返回 `expected an object, found <typeof>`；`from_value` 返回 `None`（如
/// [`CallToolResult`] 的 `content` 非数组、通知的 id 非法）时统一返回 `expected an object`
/// ——这是 serde 错误，不是 [`McpTransportError`]。
macro_rules! lenient_deserialize {
    ($($type:ty),* $(,)?) => {
        $(
            impl<'de> serde::Deserialize<'de> for $type {
                fn deserialize<D: serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> Result<Self, D::Error> {
                    use serde::de::Error;
                    let map = object_of(deserializer)?;
                    <$type>::from_value(&Value::Object(map))
                        .ok_or_else(|| D::Error::custom("expected an object"))
                }
            }
        )*
    };
}

lenient_deserialize!(
    Implementation,
    Root,
    InitializeParams,
    InitializeResult,
    ProgressNotification,
    CancelledNotification,
    Tool,
    ListToolsResult,
    Prompt,
    ListPromptsResult,
    GetPromptResult,
    Resource,
    ResourceTemplate,
    ListResourcesResult,
    ListResourceTemplatesResult,
    ReadResourceResult,
    CallToolResult,
);

/// 只有 `from_object` 的结构体（对象的持有者是父结构体，外部不直接反序列化）。
///
/// 这些类型没有公开的 `from_value`：父结构体先拿到对象 map，再按字段调用私有的
/// `from_object`。生成的 `Deserialize` 只做 `object_of` + `from_object`；非对象输入报
/// `expected an object, found <typeof>`。
macro_rules! lenient_deserialize_object {
    ($($type:ty),* $(,)?) => {
        $(
            impl<'de> serde::Deserialize<'de> for $type {
                fn deserialize<D: serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> Result<Self, D::Error> {
                    let map = object_of(deserializer)?;
                    Ok(<$type>::from_object(&map))
                }
            }
        )*
    };
}

lenient_deserialize_object!(
    ListChangedCapability,
    ResourcesCapability,
    ClientCapabilities,
    ServerCapabilities,
    ToolAnnotations,
    ToolExecution,
);

/// `ResourceContents` 的宽松反序列化（**键存在**即选分支，见 §30.12）。
impl<'de> serde::Deserialize<'de> for ResourceContents {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(resource_contents_from_value(&value))
    }
}

/// `EmbeddedResourceContent` 的宽松反序列化。
impl<'de> serde::Deserialize<'de> for EmbeddedResourceContent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = object_of(deserializer)?;
        Ok(EmbeddedResourceContent::from_object(&map))
    }
}

/// `ContentBlock` 的宽松反序列化（按 `type` 分派，未知块保留原值）。
impl<'de> serde::Deserialize<'de> for ContentBlock {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(content_block_from_value(&value))
    }
}

// ---------------------------------------------------------------------------
// JS 数字的 serde 桥
// ---------------------------------------------------------------------------

/// 以 ECMAScript `Number::toString` 的文本写一个 `f64`。
///
/// 先由 `kiana_json::js_num::format_js_number` 生成文本，再用
/// `serde_json::value::RawValue` 原样写进输出：若直接调用 `serialize_f64`，数字会经过
/// serde_json 自己的格式化路径，得不到 JS 的文本形式（例如 `-0` 与极大指数的写法）。
/// 非有限值按 `JSON.stringify` 的规则写成 `null`。
pub fn serialize_js_number<S: serde::Serializer>(
    value: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;
    let text = if value.is_finite() {
        kiana_json::js_num::format_js_number(*value)
    } else {
        // `JSON.stringify` 把 NaN/±Infinity 写成 null。
        "null".to_owned()
    };
    let raw = serde_json::value::RawValue::from_string(text).map_err(S::Error::custom)?;
    raw.serialize(serializer)
}

/// `Option<f64>` 版本（`None` → `null`；调用方通常配 `skip_serializing_if`）。
pub fn serialize_js_number_opt<S: serde::Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(number) => serialize_js_number(number, serializer),
        None => serializer.serialize_none(),
    }
}

/// `f64` 反序列化（JSON 数字 → f64；`null` 报错）。
pub fn deserialize_f64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(number) => number
            .as_f64()
            .ok_or_else(|| serde::de::Error::custom("expected a number")),
        _ => Err(serde::de::Error::custom("expected a number")),
    }
}

/// `Option<f64>` 版本。
pub fn deserialize_f64_opt<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_f64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected a number")),
        _ => Err(serde::de::Error::custom("expected a number")),
    }
}

/// `Record<string, unknown>` 的便捷类型（保留插入序）。
///
/// 插入序依赖工作区为 `serde_json` 打开的 `preserve_order` 特性（根 `Cargo.toml`）；该特性
/// 若被移除，`Map` 的遍历顺序会改变，本 crate 里依赖插入序的 `extra` 往返也会随之变化。
/// 需要 JS 的键序（整数样式键在前）时用 `kiana_json::js_object` 的辅助函数。
pub type JsonObject = Map<String, Value>;

/// 与 [`JsonObject`] 相同的 `BTreeMap` 视图（需要稳定排序时用；本 crate 不持久化它）。
pub type SortedJsonObject = BTreeMap<String, Value>;

// 本模块锁定三类行为：协议版本常量的稳定取值；未知/类型不符字段的宽松往返；以及
// `structuredContent` 的"缺失 vs 显式 null"区别。跨实现的差分测试在 tests/content.rs。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_versions_match_typescript() {
        assert_eq!(LATEST_PROTOCOL_VERSION, "2025-11-25");
        assert_eq!(
            SUPPORTED_PROTOCOL_VERSIONS,
            ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]
        );
        assert!(is_supported_protocol_version("2024-11-05"));
        assert!(!is_supported_protocol_version("2025-01-01"));
        assert_eq!(
            "2025-03-26".parse::<SupportedProtocolVersion>().unwrap(),
            SupportedProtocolVersion::V2025_03_26
        );
    }

    #[test]
    fn tool_round_trips_unknown_fields() {
        let raw = json!({
            "name": "search",
            "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
            "icons": [{"src": "https://x/icon.png"}],
            "_meta": {"vendor": 1}
        });
        let tool: Tool = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(tool.name, "search");
        assert_eq!(tool.input_schema["type"], json!("object"));
        assert_eq!(tool.meta.as_ref().unwrap()["vendor"], json!(1));
        assert_eq!(tool.extra["icons"][0]["src"], json!("https://x/icon.png"));
        let back = serde_json::to_value(&tool).unwrap();
        assert_eq!(back["icons"], raw["icons"]);
        assert_eq!(back["_meta"], raw["_meta"]);
        assert_eq!(back["inputSchema"], raw["inputSchema"]);
    }

    /// 回归测试（评审 finding #2，major）：TS 的这些接口只是编译期声明，运行期不做校验。
    /// 服务器发来的下方每个值在 TS 里都合法，Rust 必须同样接受。
    #[test]
    fn protocol_structs_are_as_lenient_as_the_typescript_interfaces() {
        // `client.ts` 的 `isResource` 明确容忍缺 `name`（`toResource` 用 uri 回填）。
        let resource: Resource = serde_json::from_value(json!({"uri": "file:///a"})).unwrap();
        assert_eq!(resource.uri, "file:///a");
        // `toResource` 的 `item.name ?? item.uri`。
        assert_eq!(resource.name, "file:///a");
        let resource: Resource =
            serde_json::from_value(json!({"uri": "file:///a", "name": null})).unwrap();
        assert_eq!(resource.name, "file:///a");

        // `description: 5`、`_meta: 1` 原样通过（TS 不做类型校验）：可选字段只在类型相符时
        // 进类型化字段，否则原值留在 `extra` 里（`String()` 强制转换会伪造一个 `"5"`）。
        let tool: Tool = serde_json::from_value(json!({
            "name": "t",
            "description": 5,
            "_meta": 1,
            "inputSchema": {"type": "object"},
        }))
        .unwrap();
        assert_eq!(tool.description, None);
        assert_eq!(tool.extra["description"], json!(5));
        assert!(tool.meta.is_none());
        assert_eq!(tool.extra["_meta"], json!(1));
        assert_eq!(
            serde_json::to_value(&tool).unwrap()["description"],
            json!(5),
            "mistyped optional fields round-trip verbatim"
        );

        // 真正的字符串仍然进类型化字段。
        let tool: Tool = serde_json::from_value(json!({
            "name": "t",
            "description": "d",
            "inputSchema": {"type": "object"},
        }))
        .unwrap();
        assert_eq!(tool.description.as_deref(), Some("d"));
        assert!(!tool.extra.contains_key("description"));

        // `taskSupport` 不是三个字面量之一时保留原值。
        let tool: Tool = serde_json::from_value(json!({
            "name": "t",
            "inputSchema": {},
            "execution": {"taskSupport": "weird"},
        }))
        .unwrap();
        assert!(tool.execution.as_ref().unwrap().task_support.is_none());
        assert_eq!(
            tool.execution.as_ref().unwrap().extra["taskSupport"],
            json!("weird")
        );

        // `capabilities: {tools: true}` 不是对象 → 字段为 None、原值进 extra，整体仍可用。
        // TS 的判定是 JS truthiness（`capabilities?.tools ?`），`true` 因此**算**有 tools 能力。
        let result: InitializeResult = serde_json::from_value(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": true},
            "serverInfo": {"name": "s", "version": "1"},
        }))
        .unwrap();
        assert!(result.capabilities.has_tools());
        assert_eq!(result.capabilities.extra["tools"], json!(true));

        // 假值（`false`/`0`/`""`/`null`）与缺失一样：不调用 `tools/list`。
        for falsy in [json!(false), json!(0), json!(""), Value::Null] {
            let result: InitializeResult = serde_json::from_value(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": falsy},
                "serverInfo": {"name": "s", "version": "1"},
            }))
            .unwrap();
            assert!(!result.capabilities.has_tools(), "tools: {falsy}");
        }
        // 真值但非对象（`[]`、`"yes"`）同样算有。
        for truthy in [json!([]), json!("yes"), json!(1)] {
            let result: InitializeResult = serde_json::from_value(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": truthy},
                "serverInfo": {"name": "s", "version": "1"},
            }))
            .unwrap();
            assert!(result.capabilities.has_tools(), "tools: {truthy}");
        }

        // `_meta: 1`、`isError: "yes"` 保留原值。
        let call: CallToolResult = serde_json::from_value(json!({
            "content": [],
            "_meta": 1,
            "isError": "yes",
        }))
        .unwrap();
        assert!(call.meta.is_none());
        assert_eq!(call.is_error, None);
        assert_eq!(call.extra["_meta"], json!(1));
        assert_eq!(call.extra["isError"], json!("yes"));

        // `annotations.audience: ["robot"]`：未知成员被丢弃，但整体不失败。
        let resource: Resource = serde_json::from_value(json!({
            "uri": "u",
            "name": "n",
            "annotations": {"audience": ["robot", "user"]},
        }))
        .unwrap();
        assert_eq!(
            resource.annotations.as_ref().unwrap().audience,
            Some(vec![Audience::User])
        );

        // `nextCursor: 5`（非字符串）不再整体失败：字段为 `None`、原值逐字往返。
        let page: ListToolsResult =
            serde_json::from_value(json!({"tools": [], "nextCursor": 5})).unwrap();
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.extra["nextCursor"], json!(5));
        assert_eq!(serde_json::to_value(&page).unwrap()["nextCursor"], json!(5));
    }

    #[test]
    fn initialize_result_carries_instructions_optional() {
        let value = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "s", "version": "1.0.0", "title": "Server"},
        });
        let result: InitializeResult = serde_json::from_value(value).unwrap();
        assert_eq!(result.protocol_version, "2025-06-18");
        assert!(result.capabilities.has_tools());
        assert_eq!(result.server_info.title.as_deref(), Some("Server"));
        assert!(result.instructions.is_none());
        let back = serde_json::to_value(&result).unwrap();
        assert!(back.get("instructions").is_none());
    }

    #[test]
    fn call_tool_result_distinguishes_missing_and_null_structured_content() {
        let missing: CallToolResult = serde_json::from_value(json!({"content": []})).unwrap();
        assert!(!missing.has_structured_content());
        let explicit_null: CallToolResult =
            serde_json::from_value(json!({"content": [], "structuredContent": null})).unwrap();
        assert!(explicit_null.has_structured_content());
        assert_eq!(explicit_null.structured_content, Some(Value::Null));
        let json = serde_json::to_value(&explicit_null).unwrap();
        assert_eq!(json["structuredContent"], Value::Null);
    }

    #[test]
    fn resource_contents_prefers_text_branch() {
        let both: ResourceContents =
            serde_json::from_value(json!({"uri": "u", "text": "t", "blob": "b"})).unwrap();
        assert!(matches!(both, ResourceContents::Text(_)));
        let malformed: ResourceContents = serde_json::from_value(json!({"uri": "u"})).unwrap();
        assert!(matches!(malformed, ResourceContents::Other(_)));
    }
}
