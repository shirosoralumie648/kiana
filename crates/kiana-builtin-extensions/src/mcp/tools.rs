//! `extensions/mcp/tools.ts`：把 MCP 工具适配成 kiana 的工具定义。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/tools.ts`。
//!
//! 结果映射到 kiana 的模型可见内容（文本与图片）。超过 20KB 的文本保留首尾、切掉中间（与 Codex
//! 一致），全文写到模型能读的临时文件。图片以外的二进制资源也存到临时文件，资源链接会指出
//! `read_mcp_resource` 工具。codemode 脚本拿到完整的 `CallToolResult`（去掉 `_meta`），永不截断。
//!
//! 与 TS 的已知差异：
//!
//! 1. `createHash("sha256")` 用 [`kiana_ai::model_catalog::sha256_hex`]（同一输入字节、同一小写十六
//!    进制），因此本 crate 不依赖 `sha2`。
//! 2. `string | Uint8Array` 的保存回调统一为 `Vec<u8>`（写文件时两者都是字节）。
//! 3. 渲染器每帧新建组件而不是复用 `lastComponent`；宿主的 `last_component` 没有向下转型，无法复用具体组件，输出逐字一致（复用只是性能优化）。
//! 4. TS 的 `toModelContent`/`convertMcpResult` 是可能拒绝的 Promise（畸形服务器输出上的
//!    TypeError）；Rust 侧返回 `Result<_, AgentToolError>`，`error.message` 是当前稳定契约（改动需同步测试）。
//! 5. `CallToolResult`/内容块是 kiana-mcp 的冻结类型化结构体：`structuredContent` 的键序是结构体
//!    声明序（服务端的原始键序无法还原），显式 `null` 的可选字段（若解析时没进 `extra`）也会
//!    丢失——这是 kiana-mcp 记录在案的契约限制，本单元无法在输出边界修复。进度通知的
//!    `progress.message` 在 TS 里可以是非字符串并被原样放进文本块；`TextContent.text` 是
//!    `String`，这里只能给 `String(value)`。
//! 6. 默认保存器失败时输出 Node 的 `error.message` 文本；`io::Error` 不携带
//!    `writeOutputFile` 生成的随机路径，因此消息里的 `kiana-mcp-<16 hex><ext>` 是同形的等价路径
//!    （随机部分本来就不可观测），`os.tmpdir()` 的 `TMP`/`TEMP` 回退也未复刻。
//! 7. `createMcpToolDefinition` 在 `tool.description` 是非字符串时于 TS 抛
//!    `tool.description?.trim is not a function`；本函数的冻结签名返回 `ToolDefinition`（不可
//!    失败），这类畸形输入退化为“没有 description”。
//!
//! # 为什么需要这个模块
//!
//! MCP 服务器给的是协议层数据：`tools/list` 的原始 JSON 与 `tools/call` 的 `CallToolResult`。
//! 模型侧要的是 kiana 的 `ToolDefinition`——合法工具名、对象型参数 schema、声明式输出
//! schema，以及模型能直接读的文本/图片。本模块负责这段适配：`mcp/index.rs`
//! 的 `register_tools` 对每个服务器工具调用 [`create_mcp_tool_definition`] 并注册；每次
//! 调用的执行与结果转换由本文件的 `McpToolExecutable` 与 [`convert_mcp_result`] 完成。
//! `mcp/resources.rs` 复用同一套内容转换（[`to_model_content`] + [`limit_mcp_content`]），
//! 因此 `read_mcp_resource` 与普通 MCP 工具在模型眼里是同一种形状。
//!
//! # 一次调用的执行顺序
//!
//! 1. `McpToolExecutable::execute` 先调用注册时注入的取客户端回调（类型
//!    [`McpGetClientFn`]）。连接可能还在建立、服务器可能已被禁用、工具可能已下架，这些
//!    情况在 `mcp/index.rs` 的回调里表现为 `McpTransportError`，这里转成 `AgentToolError`。
//! 2. 构造 `McpRequestOptions`：取消令牌透传、单次请求超时来自
//!    [`McpToolDefinitionOptions::timeout_ms`]、进度通知经 `progress_text` 变成一条文本
//!    更新推给 `on_update`。
//! 3. `call_tool` 返回后，[`convert_mcp_result`] 逐块转换内容（[`to_model_content`]），
//!    再按 [`MCP_OUTPUT_MAX_BYTES`] 做中间截断（[`limit_mcp_content`]）；保存成功时全文
//!    写到临时文件，失败则文本里给出提示。
//! 4. `structured_content` 放的是服务器结果的序列化 JSON（去掉 `_meta`，键序见上文第 5 条
//!    差异）；它给 codemode 脚本用，永不截断。模型只看到第 3 步的文本/图片。
//!
//! # 边界与失败模式
//!
//! - 内容块转换是“全有或全无”：任何一块触发 TS 等价的 `TypeError`（`Buffer.from`、`in`
//!   运算符、可选链的文案见各 `*_error` 辅助函数），[`to_model_content`] 返回 `Err`，
//!   本次执行不会把部分内容交给模型。
//! - 截断只作用于文本：图片块原样保留，截断时全部排到文本块之后；图片块不参与这一计量。
//! - 保存失败不会让调用失败：全文截断路径退化为 `[Could not save the full output: …]`，
//!   二进制资源退化为 `[Binary resource … could not be saved: …]`，`fullOutputPath` 缺失。
//! - `isError` 结果若没有任何文本块，会补一条
//!   `MCP tool <server>/<tool> returned an error`，保证模型至少看到一句话。
//!
//! # 阅读顺序
//!
//! 1. 工具定义：[`create_mcp_tool_name`] → [`create_mcp_result_schema`] →
//!    [`create_mcp_tool_definition`]（其中 `relocate_schema` 负责 `$ref` 重定位）。
//! 2. 结果转换：`block_to_content` 的分派 → [`to_model_content`] → [`limit_mcp_content`]
//!    → [`convert_mcp_result`]。
//! 3. 渲染：[`create_mcp_tool_renderers`]。
//! 4. 端到端断言见 `crates/kiana-builtin-extensions/tests/mcp_tools.rs`。

use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::{AgentToolError, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{
    ImageContent, ImageKind, JsonValue, TextContent, TextKind, UserContentBlock,
};
use kiana_ext_api::tool_types::{
    RenderCallFn, RenderResultFn, ToolAnnotations, ToolDefinition, ToolDefinitionExecute,
    ToolExposure, ToolNamespace, ToolRenderContext, ToolRenderers,
};
use kiana_ext_api::{BoxFuture, ExtensionToolContextApi};
use kiana_mcp::auth_provider::McpTransportError;
use kiana_mcp::protocol_types::{CallToolResult, ContentBlock, LlmContent};
use kiana_mcp::{McpRequestOptions, Tool as McpTool};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container};
use kiana_ui_kit::visual_truncate::{Keep, VisualLinePreview, VisualLinePreviewOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// 资源工具名（`read_mcp_resource`）。`block_to_content` 用它拼 `resource_link` 的提示文本；
/// 定义本身由 `mcp/resources.rs` 的 `create_mcp_resource_tool_definitions` 构造。
pub use kiana_core::mcp_servers::READ_MCP_RESOURCE_TOOL;

use crate::mcp::config::McpExposure;

/// MCP exposure → kiana 的 `ToolExposure`：`codemode` 与 `deferred` 都不列进 codemode 描述，
/// 区别只在 MCP 扩展激活哪个工具去触达它们。
/// TS: `tools.ts#toToolExposure`。
///
/// 两个枚举的 `Codemode` 变体含义不同，不要按名字直接对应：[`McpExposure::Codemode`] 表示
/// “可从 codemode 脚本调用，但不声明给模型、也不列进 codemode 描述”，在 kiana 的工具面上
/// 等价于 `deferred`，而不是 `ToolExposure::Codemode`。这条映射来自 TS 的
/// `exposure === "codemode" ? "deferred" : exposure`。
pub fn to_tool_exposure(exposure: McpExposure) -> ToolExposure {
    match exposure {
        McpExposure::Codemode => ToolExposure::Deferred,
        McpExposure::Deferred => ToolExposure::Deferred,
        McpExposure::Direct => ToolExposure::Direct,
        McpExposure::Hidden => ToolExposure::Hidden,
    }
}

/// provider 工具名上限（`[A-Za-z0-9_-]` 的 64 个字符）。TS: `tools.ts#MAX_TOOL_NAME_LENGTH`。
const MAX_TOOL_NAME_LENGTH: usize = 64;
/// 模型可见的 MCP 结果文本超过这个字节数就被从中间截断。
/// TS: `tools.ts#MCP_OUTPUT_MAX_BYTES`。
///
/// 计量对象是 [`limit_mcp_content`] 把所有文本块用 `\n` 拼起来后的 UTF-8 字节数
/// （`truncate_middle` 的 `total_bytes`），不是字符数；图片不参与计量。
pub const MCP_OUTPUT_MAX_BYTES: usize = 20 * 1024;
/// 展开前显示的可视（换行后）行数。TS: `tools.ts#OUTPUT_PREVIEW_LINES`。
const OUTPUT_PREVIEW_LINES: usize = 5;

/// MCP 工具结果的 details。TS: `tools.ts#McpToolDetails`。
///
/// 这是 `AgentToolResult::details` 的类型化对应物；实际写出的 JSON 由本文件的
/// `details_json` 手工构造（键序 `server`、`tool`、`fullOutputPath`，与这里的 camelCase
/// 序列化一致）。渲染器在折叠视图里读 `fullOutputPath` 追加全文路径。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDetails {
    /// 服务器名。
    pub server: String,
    /// 服务器上的工具名。
    pub tool: String,
    /// 模型可见文本被截断时，保存全文的临时文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

/// 保存截断后的全文或二进制资源，返回文件路径。`extension` 含点（例如 `.txt`）。
/// TS: `tools.ts#McpOutputSaver`（`string | Uint8Array` → `Vec<u8>`）。
///
/// 回调是异步的，并通过 `Arc` 共享：`ConvertMcpResultOptions::save_output` 可能被
/// [`to_model_content`] 的并发块转换同时调用，实现必须是 `Send + Sync` 且自行保证并发
/// 安全。只有真正需要落盘时（文本被截断，或遇到非文本的二进制资源）才会调用。
pub type McpOutputSaver = Arc<
    dyn Fn(Vec<u8>, String) -> BoxFuture<'static, Result<String, std::io::Error>> + Send + Sync,
>;

/// 把数据写到一个新的临时输出文件并返回路径。TS: `tools.ts#saveToTempFile`。
///
/// 失败时把 `io::Error` 换成 Node `error.message` 的形状（`ENOENT: no such file or directory,
/// open '<path>'`）：`write_output_file` 的错误不携带它尝试的路径，因此这里按 Node 的临时文件
/// 命名规则补一个同形的 `kiana-mcp-<16 hex><extension>`（随机部分本来就不可观测）。
///
/// 底层 `kiana_core::output_files::write_output_file` 以 `create_new` 语义创建文件（不覆盖
/// 已存在的同名文件，也不跟随预先放置的符号链接；Unix 上权限 0600），且不创建目录；目录
/// 不存在或不可写时错误原样冒泡到这里。成功时返回的是真实路径，只有失败文案才用占位路径。
pub async fn save_to_temp_file(data: Vec<u8>, extension: &str) -> Result<String, std::io::Error> {
    kiana_core::output_files::write_output_file("kiana-mcp", extension, &data)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| {
            std::io::Error::new(error.kind(), node_output_file_error(&error, extension))
        })
}

/// libuv 对常见文件系统 errno 的命名（Linux 数值；其它平台先按 `ErrorKind` 兜底）。
///
/// 这张表只用于拼错误文案，不参与任何控制流；走查顺序是 `raw_os_error()` → `ErrorKind`
/// → 放弃。文案是当前稳定契约（与 Node 的 `uv_strerror` 对齐），测试
/// `formats_failed_output_file_writes_like_node` 锁定了 ENOENT/EACCES/ENAMETOOLONG 三条。
#[cfg(unix)]
const UNIX_ERRNO: [(i32, &str, &str); 11] = [
    (2, "ENOENT", "no such file or directory"),
    (13, "EACCES", "permission denied"),
    (17, "EEXIST", "file already exists"),
    (20, "ENOTDIR", "not a directory"),
    (21, "EISDIR", "illegal operation on a directory"),
    (24, "EMFILE", "too many open files"),
    (28, "ENOSPC", "no space left on device"),
    (30, "EROFS", "read-only file system"),
    (36, "ENAMETOOLONG", "name too long"),
    (40, "ELOOP", "too many symbolic links encountered"),
    (122, "EDQUOT", "disk quota exceeded"),
];

/// `(code, description)`：Node/libuv 的 errno 文本。
///
/// 先查原始 errno（仅 `#[cfg(unix)]` 分支），再按 [`std::io::ErrorKind`] 兜底到最常见的
/// 几种文案；两者都没有时返回 `None`，调用方退回 `error.to_string()`。
fn node_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error()
        && let Some(entry) = UNIX_ERRNO.iter().find(|(code, _, _)| *code == raw)
    {
        return Some((entry.1, entry.2));
    }
    match error.kind() {
        std::io::ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        std::io::ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        std::io::ErrorKind::AlreadyExists => Some(("EEXIST", "file already exists")),
        std::io::ErrorKind::InvalidInput => Some(("EINVAL", "invalid argument")),
        _ => None,
    }
}

/// Node `fs.writeFile` 打开临时文件失败时的 `error.message`。
///
/// 无法映射到 errno 的错误（例如测试里的 `io::Error::other("boom")`）原样返回
/// `error.to_string()`，不伪造路径。
fn node_output_file_error(error: &std::io::Error, extension: &str) -> String {
    let Some((code, description)) = node_errno(error) else {
        return error.to_string();
    };
    format!(
        "{code}: {description}, open '{}'",
        temp_output_file_path(extension)
    )
}

/// Node 的 `path.join(os.tmpdir(), \`kiana-mcp-${randomBytes(8).toString("hex")}${extension}\`)`。
///
/// 这里生成的是**错误文案里的占位路径**，不是真实文件名：随机段用 `RandomState` 加纳秒
/// 时间与进程号拼出 16 位十六进制（不是加密随机数、也不重试），目录来自
/// `std::env::temp_dir()`（Unix 上只按 `TMPDIR` → `/tmp` 解析），Node `os.tmpdir()`
/// 额外的 `TMP`/`TEMP` 回退未复刻（见模块文档第 6 条差异）。成功保存走的是
/// `write_output_file` 返回的真实路径。
fn temp_output_file_path(extension: &str) -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    let hex = format!("{:016x}", hasher.finish());
    let mut directory = std::env::temp_dir().to_string_lossy().into_owned();
    // Node 在长度 > 1 时去掉一个结尾分隔符（Windows 的 `C:\` 除外）。
    #[cfg(unix)]
    if directory.len() > 1 && directory.ends_with('/') {
        directory.pop();
    }
    #[cfg(windows)]
    if directory.len() > 1
        && directory.ends_with('\\')
        && directory.as_bytes().get(directory.len() - 2) != Some(&b':')
    {
        directory.pop();
    }
    let separator = std::path::MAIN_SEPARATOR;
    if directory.ends_with(separator) {
        format!("{directory}kiana-mcp-{hex}{extension}")
    } else {
        format!("{directory}{separator}kiana-mcp-{hex}{extension}")
    }
}

/// 默认的保存实现：临时输出文件。
fn default_output_saver() -> McpOutputSaver {
    Arc::new(|data, extension| Box::pin(async move { save_to_temp_file(data, &extension).await }))
}

/// 取工具调用入口的回调（连接可能还在建立）。TS: `createMcpToolDefinition` 的 `getClient`。
///
/// 每次执行调用一次，且在参数归一化之前；返回 `Err` 时本次工具调用以该错误的 `Display`
/// 文本失败（服务器被禁用、仍在连接、工具已下架都由 `mcp/index.rs` 的闭包产生）。回调必须
/// `Send + Sync`，因为执行器会跨 `await` 持有它。
pub type McpGetClientFn = Arc<
    dyn Fn() -> BoxFuture<'static, Result<Arc<dyn McpToolCaller>, McpTransportError>> + Send + Sync,
>;

/// `read_mcp_resource` 能否读取服务器资源的谓词。TS: `readableResources?: () => boolean`。
///
/// 在工具调用返回之后、转换之前取值一次，作为 [`ConvertMcpResultOptions::readable_resources`]
/// 的快照；它只影响 `resource_link` 文本里是否提示 `read_mcp_resource`，不改变结果结构。
pub type ReadableResourcesFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// MCP 工具调用入口（`McpClient`/连接的适配层）。TS: `tools.ts#McpToolCaller`。
///
/// 本 crate 中由 `McpServerConnection`（`mcp/runtime.rs`）实现；`mcp/index.rs` 的
/// `get_client` 闭包在检查服务器状态与工具可用性之后，把它转成
/// `Arc<dyn McpToolCaller>` 交给执行器。
// `async-trait` 给方法生成 `#[must_use]`，clippy 的 `double_must_use` 看不到宏内属性
// （与工作区其它 `async_trait` trait 相同的局部例外）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait McpToolCaller: Send + Sync {
    /// 调用服务器上的一个工具。参数是工具调用原文（`null` 已换成 `{}`）。
    ///
    /// 返回服务器的原始结果，不做内容转换（转换由 [`convert_mcp_result`] 负责）；超时、
    /// 取消与协议错误以 `McpTransportError` 返回，由调用方转成 `AgentToolError`。
    async fn call_tool(
        &self,
        name: &str,
        args: Value,
        options: &McpRequestOptions,
    ) -> Result<CallToolResult, McpTransportError>;
}

/// `mcp__<server>__<tool>`：清洗并在过长时追加哈希后缀。与 Codex 一致，`[A-Za-z0-9_]` 之外的
/// 每个 UTF-16 码元都变成 `_`，因此名字也能当 codemode 标识符。`is_taken` 报告已被别的 MCP 工具
/// 占用的名字：清洗可能把两个工具映射到一个名字（`a-b` 与 `a_b`），它们都会拿到哈希后缀。
/// TS: `tools.ts#createMcpToolName`。
///
/// 步骤：
/// 1. 拼 `mcp__{server}__{tool}`，按 UTF-16 码元清洗成 `[A-Za-z0-9_]`；非 ASCII 与孤立
///    代理项都变成 `_`（一个 astral 字符占两个码元，因此得到两个下划线）；
/// 2. 长度不超过 `MAX_TOOL_NAME_LENGTH`（64）且 `is_taken` 为假时原样返回；
/// 3. 否则取 `sha256("{server}\0{tool}")` 的前 8 个十六进制字符，把名字截到 55 个字符，
///    再拼 `_{suffix}`（名字原本够长时总长恰好 64）。
///
/// 同一对 server/tool 的结果稳定，不同对靠哈希区分；`is_taken` 只在名字不超长时被调用
/// （短路），调用方不能依赖它对超长名字产生副作用。清洗后的名字只含 ASCII，所以按字节
/// 截断不会切坏字符。
pub fn create_mcp_tool_name(server: &str, tool: &str, is_taken: impl Fn(&str) -> bool) -> String {
    let raw = format!("mcp__{server}__{tool}");
    let mut name = String::with_capacity(raw.len());
    for unit in raw.encode_utf16() {
        let byte = u8::try_from(unit).unwrap_or(0);
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            name.push(char::from(byte));
        } else {
            name.push('_');
        }
    }
    if name.len() <= MAX_TOOL_NAME_LENGTH && !is_taken(&name) {
        return name;
    }
    let hash = kiana_ai::model_catalog::sha256_hex(&format!("{server}\0{tool}"));
    let suffix = &hash[..8];
    let keep = MAX_TOOL_NAME_LENGTH - suffix.len() - 1;
    format!("{}_{suffix}", &name[..keep.min(name.len())])
}

/// `textOf(content)`：只拼文本块，用 `\n` 连接。
///
/// 图片块被丢弃。返回的字符串既是 [`limit_mcp_content`] 的截断计量对象，也用于判断一个
/// 错误结果是否还有可读文本（[`convert_mcp_result`]）。
fn text_of(content: &[UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 一个文本内容块。
///
/// 固定 `kind_text: TextKind::Text` 且不带 `text_signature`；本模块的进度更新与
/// `mcp/resources.rs` 的结果都通过它构造模型可见文本。
pub(crate) fn text_block(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

/// `toLlmContent` 的结果转成模型的用户内容块。
///
/// 用于两条路径：`block_to_content` 的逐块兜底（`fallback_content`），以及 `content`
/// 为空时整体回退到 `kiana_mcp` 的 `to_llm_content` 的结果。
/// `LlmContent` 已经是模型 API 形状的文本/图片，这里只补上 kiana 的 kind 字段
/// （文本不带 `text_signature`）。
fn llm_to_user(blocks: Vec<LlmContent>) -> Vec<UserContentBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            LlmContent::Text { text } => UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text,
                text_signature: None,
            }),
            LlmContent::Image { data, mime_type } => UserContentBlock::Image(ImageContent {
                kind_image: ImageKind::Image,
                data,
                mime_type,
            }),
        })
        .collect()
}

/// `CallToolResult` 的 details JSON（键序 `server`、`tool`、`fullOutputPath`）。
///
/// 这里直接拼 JSON，而不是序列化 [`McpToolDetails`]；两者内容相同（`McpToolDetails` 的
/// camelCase 形态给出同样的三个键）。`mcp/resources.rs` 的 JSON 结果也复用它。
pub(crate) fn details_json(server: &str, tool: &str, full_output_path: Option<&str>) -> Value {
    let mut details = Map::new();
    details.insert("server".to_string(), Value::String(server.to_string()));
    details.insert("tool".to_string(), Value::String(tool.to_string()));
    if let Some(path) = full_output_path {
        details.insert(
            "fullOutputPath".to_string(),
            Value::String(path.to_string()),
        );
    }
    Value::Object(details)
}

/// 用一条消息构造 `AgentToolError`。
///
/// 只填 `message`（`Display` 就是它）；`name`/`stack` 留空，因此 TS 的 `TypeError.name`
/// 不会继续上传，只有消息文本与 TS 逐字对齐。
pub(crate) fn agent_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: None,
        stack: None,
    }
}

/// MCP 内容块的紧凑类型说明（脚本最常读的 text/image 字段）。渲染进每个 MCP 工具的声明。
/// TS: `tools.ts#CONTENT_BLOCK_SCHEMA`（逐字）。
///
/// `type` 是唯一必填字段；`additionalProperties: true` 保留服务器可能加入的未知字段。
/// 这份 JSON 随后会进入 [`create_mcp_result_schema`] 的整棵树并被排成 JS 键序；这些键
/// 都不是整数样式键，`sort_js_keys` 会保持插入序，因此这里的声明顺序就是最终键序
/// （改动顺序会改变输出）。
fn content_block_schema() -> Value {
    let mut properties = Map::new();
    properties.insert(
        "type".to_string(),
        object([(
            "enum",
            Value::Array(
                ["text", "image", "audio", "resource_link", "resource"]
                    .into_iter()
                    .map(|item| Value::String(item.to_string()))
                    .collect(),
            ),
        )]),
    );
    properties.insert(
        "text".to_string(),
        object([("type", Value::String("string".into()))]),
    );
    properties.insert(
        "data".to_string(),
        object([("type", Value::String("string".into()))]),
    );
    properties.insert(
        "mimeType".to_string(),
        object([("type", Value::String("string".into()))]),
    );
    object([
        ("type", Value::String("object".into())),
        ("properties", Value::Object(properties)),
        ("required", Value::Array(vec![Value::String("type".into())])),
        ("additionalProperties", Value::Bool(true)),
    ])
}

/// 按给定顺序构造一个 JSON 对象。
fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// 值处在“数据位置”的关键字：其中的 `$ref` 是数据，不是引用。
///
/// 递归到这些键时必须原样克隆：例如某个 schema 的 `default` 值恰好是 `{"$ref": "#/x"}`，
/// 它是用户数据，重定位会破坏它。`tests/mcp_tools.rs` 对 `$defs.Hit/v1.default` 的断言
/// 覆盖了这一点。
const SCHEMA_DATA_KEYWORDS: [&str; 4] = ["const", "enum", "default", "examples"];
/// 值是“名字 → schema”映射的关键字：键是名字，值才是 schema。
///
/// 这里的名字是用户定义的（属性名、定义名），不是 schema 关键字，所以不能对名字做
/// “数据关键字”判断；只对每个值递归。其余关键字（`items`、`allOf`、
/// `additionalProperties` 等）的值要么是单个 schema、要么是 schema 数组，走通用递归。
const SCHEMA_MAP_KEYWORDS: [&str; 6] = [
    "properties",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "$defs",
    "definitions",
];

/// 把 `schema` 当成另一个 schema 的 JSON Pointer `pointer` 处的子 schema：它的本地引用
/// （`#`、`#/$defs/Item`）重写到 `pointer` 之下，仍解析到原来的子 schema。
/// TS: `tools.ts#relocateSchema`（按 schema 位置遍历，不按值）。
///
/// 递归规则按“这个键的值在 JSON Schema 里处于什么位置”决定，而不是按值本身：
///
/// - 键是 `$ref` 且值是字符串 `#` 或 `#/…`：重写为 `pointer` + 去掉开头 `#` 的部分；
///   其它字符串（`https://…`、`#anchor`）与非字符串原样保留，落到通用分支；
/// - 键属于 `SCHEMA_DATA_KEYWORDS`：值是数据，整块克隆，不进入递归；
/// - 键属于 `SCHEMA_MAP_KEYWORDS` 且值是对象：每个成员的值都是一个子 schema，逐个重定位
///   （成员名本身不参与关键字判断）；
/// - 其余键（`items`、`allOf`、`additionalProperties`、未知关键字）：对值做通用递归，
///   对象与数组都会遍历。
///
/// 只做文本重写：不解析引用、不检查目标是否存在，也不修改入参（返回一棵新树）。
fn relocate_schema(schema: &Value, pointer: &str) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| relocate_schema(item, pointer))
                .collect(),
        ),
        Value::Object(map) => {
            let mut relocated = Map::new();
            for (key, value) in map {
                // 只有指向本 schema 内部的字符串才算引用；其它值的 `$ref`（远程字符串、
                // 非字符串）落到通用分支，会像 TS 一样继续递归进去。
                let local_reference = match value {
                    Value::String(reference)
                        if key == "$ref" && (reference == "#" || reference.starts_with("#/")) =>
                    {
                        Some(reference)
                    }
                    _ => None,
                };
                if let Some(reference) = local_reference {
                    relocated.insert(
                        key.clone(),
                        Value::String(format!("{pointer}{}", &reference[1..])),
                    );
                } else if SCHEMA_DATA_KEYWORDS.contains(&key.as_str()) {
                    relocated.insert(key.clone(), value.clone());
                } else if SCHEMA_MAP_KEYWORDS.contains(&key.as_str())
                    && let Some(entries) = value.as_object()
                {
                    let mut mapped = Map::new();
                    for (name, subschema) in entries {
                        mapped.insert(name.clone(), relocate_schema(subschema, pointer));
                    }
                    relocated.insert(key.clone(), Value::Object(mapped));
                } else {
                    relocated.insert(key.clone(), relocate_schema(value, pointer));
                }
            }
            Value::Object(relocated)
        }
        other => other.clone(),
    }
}

/// 每个 MCP 工具的输出 schema：脚本收到的 `CallToolResult`（不含 `_meta`，见
/// [`convert_mcp_result`]），其中 `structuredContent` 是工具自己的输出 schema。
/// TS: `tools.ts#createMcpResultSchema`。
///
/// `structured_content_schema` 是服务器 `outputSchema` 的对象形态：它的本地 `$ref` 会按
/// `#/properties/structuredContent` 重定位，使脚本读取嵌套结果时仍能解析引用；`None`
/// 生成空 schema `{}`（表示任意 JSON）。外层固定声明 `content`/`structuredContent`/
/// `isError`，只要求 `content`，并允许额外字段。
///
/// 注意这里声明的是**整个 `CallToolResult`**，不只是 `structuredContent`：
/// [`create_mcp_tool_definition`] 把它整体放进 `ToolDefinition::output_schema`，codemode
/// 的 `render_tool_output_type` 也按这个形状生成脚本类型。
pub fn create_mcp_result_schema(structured_content_schema: Option<&Map<String, Value>>) -> Value {
    create_mcp_result_schema_from_raw(
        structured_content_schema.map(|schema| Value::Object(schema.clone())),
    )
}

/// [`create_mcp_result_schema`] 的原始值版本：TS 的 `outputSchema` 是任意 JSON 值（只有对象
/// 才是合法 schema，但运行期不做校验），`structuredContentSchema ? ... : {}` 按 JS 真值判定。
///
/// 因此 `0`/`false`/`""`/`null` 都退化成 `{}`，而数组、字符串、对象会被原样重定位
/// （`relocate_schema` 对数组遍历元素、对标量直接克隆）。最后 `sort_js_keys` 把整棵树排成
/// JS 对象键序：整数样式的键升序在前，其余按插入序。
fn create_mcp_result_schema_from_raw(structured_content_schema: Option<Value>) -> Value {
    let structured_content = match structured_content_schema {
        Some(schema) if kiana_json::js_string::is_truthy(Some(&schema)) => {
            relocate_schema(&schema, "#/properties/structuredContent")
        }
        _ => Value::Object(Map::new()),
    };
    // JS 对象自身的键序：整数样式键升序在前（服务器 schema 里可能有 "2"/"10" 这类属性名）。
    kiana_json::js_object::sort_js_keys(&object([
        ("type", Value::String("object".into())),
        (
            "properties",
            object([
                (
                    "content",
                    object([
                        ("type", Value::String("array".into())),
                        ("items", content_block_schema()),
                    ]),
                ),
                ("structuredContent", structured_content),
                (
                    "isError",
                    object([("type", Value::String("boolean".into()))]),
                ),
            ]),
        ),
        (
            "required",
            Value::Array(vec![Value::String("content".into())]),
        ),
        ("additionalProperties", Value::Bool(true)),
    ]))
}

/// 把模型可见文本限制在 [`MCP_OUTPUT_MAX_BYTES`] 内。更长的文本变成一个文本块（Codex 的截断
/// 格式），后面跟着全文文件路径；图片跟在文本之后。返回 `(content, fullOutputPath)`。
/// TS: `tools.ts#limitMcpContent`。
///
/// 判定与输出：
/// 1. 用 `text_of` 把文本块用 `\n` 拼起来，交给 `truncate_middle` 按 UTF-8 字节裁剪；
/// 2. 未超限：原样返回输入 `content`（块数、顺序、图片位置都不变），路径为 `None`；
/// 3. 超限：把**拼接后的全文**交给 saver（扩展名固定 `.txt`），返回单个文本块——告警头、
///    保留的首尾、全文路径或失败提示——再接上原 `content` 里的所有图片块。也就是说多个
///    文本块被合并成一条，图片之间的相对顺序保留，但不再穿插在文本之间；
/// 4. `full_output_path` 只在保存成功时是 `Some`，失败为 `None`（调用方据此决定 details
///    是否带 `fullOutputPath`）。
///
/// 告警头里的 `original token count` 是 `ceil(total_bytes / 4)` 的估算，不是真实分词；
/// `total_lines` 是拼接文本在截断前的行数。
pub async fn limit_mcp_content(
    content: Vec<UserContentBlock>,
    save_output: Option<McpOutputSaver>,
) -> (Vec<UserContentBlock>, Option<String>) {
    let combined = text_of(&content);
    let truncation = kiana_tools::truncate::truncate_middle(&combined, MCP_OUTPUT_MAX_BYTES);
    if !truncation.truncated {
        return (content, None);
    }
    let saver = save_output.unwrap_or_else(default_output_saver);
    let (full_output_path, where_) =
        match saver(combined.clone().into_bytes(), ".txt".to_string()).await {
            Ok(path) => {
                let where_ = format!("[Full output: {path} (read it with offset/limit)]");
                (Some(path), where_)
            }
            Err(error) => (None, format!("[Could not save the full output: {error}]")),
        };
    let tokens = truncation.total_bytes.div_ceil(4);
    let total_lines = truncation.total_lines;
    let text = format!(
        "Warning: truncated output (original token count: {tokens})\nTotal output lines: {total_lines}\n\n{}\n\n{where_}",
        truncation.content
    );
    let mut limited = vec![text_block(text)];
    limited.extend(
        content
            .into_iter()
            .filter(|block| matches!(block, UserContentBlock::Image(_))),
    );
    (limited, full_output_path)
}

/// [`convert_mcp_result`] 的选项。TS: `tools.ts#ConvertMcpResultOptions`。
///
/// `Default` 即空配置：按需创建临时文件，且不提示 `read_mcp_resource`。
#[derive(Clone, Default)]
pub struct ConvertMcpResultOptions {
    /// 保存截断文本与二进制资源；缺省写临时文件。
    ///
    /// 回调在真正需要落盘时才被调用，并被克隆后跨 `await` 使用；`mcp/resources.rs` 与
    /// 工具执行器都传 `None`。
    pub save_output: Option<McpOutputSaver>,
    /// 服务器的资源能否用 `read_mcp_resource` 读取（资源链接因此会指出该工具）。
    ///
    /// 这是调用方在转换前取好的快照（`mcp/index.rs` 用一个实时查询资源列表的闭包求值），
    /// 转换过程中不再变化。
    pub readable_resources: bool,
}

/// 保存二进制资源用的文件扩展名：URI 末尾的扩展名，否则 `.bin`。
/// TS: `tools.ts#extensionOf`。
///
/// 先按 URL 解析取 `path()`（去掉 query/fragment），解析失败时把整个 URI 当路径；取最后一个
/// `.` 之后的部分，只有 1..=8 个 ASCII 字母数字才采用。它只影响临时文件名，猜错不会报错。
fn extension_of(uri: &str) -> String {
    let path = match url::Url::parse(uri) {
        Ok(url) => url.path().to_string(),
        Err(_) => uri.to_string(),
    };
    let Some(extension) = path.rsplit_once('.').map(|(_, extension)| extension) else {
        return ".bin".to_string();
    };
    if (1..=8).contains(&extension.len())
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        format!(".{extension}")
    } else {
        ".bin".to_string()
    }
}

/// 这些 MIME 类型的 blob 按文本显示。TS: `tools.ts#isTextMimeType`。
///
/// 只比较 `;` 之前的部分（忽略 `charset` 等参数），去首尾 JS 空白并转小写后匹配
/// `text/`、`application/json`、`+json`、`+xml`。`None`（服务器没给类型）按二进制处理。
fn is_text_mime_type(mime_type: Option<&str>) -> bool {
    let Some(mime_type) = mime_type else {
        return false;
    };
    let base =
        kiana_json::js_string::js_trim(mime_type.split(';').next().unwrap_or("")).to_lowercase();
    base.starts_with("text/")
        || base == "application/json"
        || base.ends_with("+json")
        || base.ends_with("+xml")
}

/// JS 的 `Number(value)`（`formatSize` 用它做数值比较）。数组按 `join(",")` 后再解析。
///
/// 服务器可以把 `size` 写成任意 JSON：字符串按 JS 数字语法解析，数组先按
/// `Array.prototype.join(",")` 拼成字符串（`[5]` → 5，`[1,2]` → `"1,2"` → NaN），
/// 对象得到 `"[object Object]"` → NaN。`null` 是 0、布尔是 0/1。
fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::js_num::parse_js_number(text),
        Value::Array(items) => {
            let joined = items
                .iter()
                .map(|item| kiana_json::js_string::js_string(Some(item)))
                .collect::<Vec<_>>()
                .join(",");
            kiana_json::js_num::parse_js_number(&joined)
        }
        // 普通对象的 ToPrimitive 是 `"[object Object]"` → NaN。
        Value::Object(_) => f64::NAN,
    }
}

/// `formatSize(bytes)`，参数是 JS 的任意值：`bytes < 1024` 时按模板字面量（`String(bytes)`）
/// 写出，否则按 `(bytes / 1024).toFixed(1)`。TS: `core/tools/truncate.ts#formatSize`。
///
/// 三档：`< 1024` 用原始值的 `String()` 形态（`null` → `nullB`，字符串 `"2048"`
/// 因为数值比较已走下一档），`< 1MiB` 用 KB，其余用 MB。`NaN`（例如对象）落到 MB 分支，
/// `js_to_fixed` 输出 `"NaN"`，与 JS 的 `toFixed` 相同。
fn format_size_js(bytes: &Value) -> String {
    let number = js_to_number(bytes);
    if number < 1024.0 {
        format!("{}B", kiana_json::js_string::js_string(Some(bytes)))
    } else if number < 1024.0 * 1024.0 {
        format!(
            "{}KB",
            kiana_ai::model_catalog::js_to_fixed(number / 1024.0, 1)
        )
    } else {
        format!(
            "{}MB",
            kiana_ai::model_catalog::js_to_fixed(number / (1024.0 * 1024.0), 1)
        )
    }
}

/// `"blob" in resource` 对非对象值抛出的 TypeError 文案。
///
/// `in` 运算符只接受对象；`undefined`（代码里的 `None`）、`null`、字符串、数字、布尔都
/// 会抛这条错误。操作数按 `String()` 形态写进消息（`undefined`、`null`、原字符串等）；
/// 数组与对象不会走到这里——它们是合法对象，只是没有 `blob` 属性。
fn js_in_operator_error(key: &str, operand: Option<&Value>) -> AgentToolError {
    agent_error(format!(
        "Cannot use 'in' operator to search for '{key}' in {}",
        kiana_json::js_string::js_string(operand)
    ))
}

/// `Buffer.from(data, "base64")` 失败时的 Node TypeError 文案。
///
/// 文案是当前稳定契约（与 Node 的参数校验对齐），`received` 部分按实际类型描述；用于服务器发来非法
/// `blob` 字段时把错误原样暴露给模型。
fn buffer_from_error(blob: Option<&Value>) -> AgentToolError {
    let received = match blob {
        None => "Received undefined".to_string(),
        Some(Value::Null) => "Received null".to_string(),
        Some(Value::Bool(value)) => format!("Received type boolean ({value})"),
        Some(Value::Number(_)) => format!(
            "Received type number ({})",
            kiana_json::js_string::js_string(blob)
        ),
        Some(Value::String(_)) => "Received type string".to_string(),
        Some(Value::Array(_)) => "Received an instance of Array".to_string(),
        Some(Value::Object(_)) => "Received an instance of Object".to_string(),
    };
    agent_error(format!(
        "The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object. {received}"
    ))
}

/// JS 的 `Buffer.from(data, "base64")`：字符串按 V8 的宽松 base64 解码；数字数组按
/// `ToNumber(value) & 255` 转成字节；其它类型抛 TypeError。
///
/// 三条分支对应 Node 的行为：字符串走 `kiana_tui::terminal_image::base64_decode`（非法
/// 字符被跳过、`-`/`_` 当 `+`/`/`，因此字符串解码本身不会失败）；数组逐项 `ToNumber`
/// 后截断取低 8 位（NaN 记 0）；其它类型返回上面的 TypeError。所以这里唯一的失败路径是
/// 类型错误。
fn buffer_from_base64(blob: &Value) -> Result<Vec<u8>, AgentToolError> {
    match blob {
        Value::String(text) => Ok(kiana_tui::terminal_image::base64_decode(text)),
        Value::Array(items) => {
            let mut bytes = Vec::with_capacity(items.len());
            for item in items {
                let number = js_to_number(item);
                let byte = if number.is_nan() {
                    0
                } else {
                    (number.trunc() as i64 & 0xff) as u8
                };
                bytes.push(byte);
            }
            Ok(bytes)
        }
        other => Err(buffer_from_error(Some(other))),
    }
}

/// `block.resource.mimeType?.startsWith("image/")`：`null`/缺失是 false，其它非字符串抛
/// TypeError（可选链只放行 `null`/`undefined`）。
///
/// 调用点据此决定带 `blob` 的资源是否按图片处理：图片资源不在这里解码，而是交给兜底转换
/// 变成图片块。
fn mime_starts_with_image(mime_type: Option<&Value>) -> Result<bool, AgentToolError> {
    match mime_type {
        None | Some(Value::Null) => Ok(false),
        Some(Value::String(mime_type)) => Ok(mime_type.starts_with("image/")),
        Some(_) => Err(agent_error(
            "block.resource.mimeType?.startsWith is not a function",
        )),
    }
}

/// `toLlmContent({ content: [block] })`，含 TS 在该路径上的 TypeError。
///
/// 这是 `block_to_content` 的兜底：交给 `kiana_mcp` 的严格转换（`try_block_to_llm_content`）
/// 再包装成单块结果。文本/图片/音频块、没有 `blob` 的资源、MIME 为图片的资源以及未知
/// `type` 的块都走这里；畸形输入返回的错误消息与 TS 的 `error.message` 一致。
fn fallback_content(block: &ContentBlock) -> Result<Vec<UserContentBlock>, AgentToolError> {
    match kiana_mcp::protocol_types::try_block_to_llm_content(block) {
        Ok(content) => Ok(llm_to_user(vec![content])),
        Err(error) => Err(agent_error(error.message())),
    }
}

/// 服务器结果里一个内容块的模型可见内容。TS: `tools.ts#blockToContent`。
///
/// 按 `block.as_value()` 的原始 JSON 视图分派：TS 只看 `type` 与原始字段（`resource_link`
/// 甚至不要求 `name`），类型化结构体在字段缺失/类型不符时会落到错误的兜底分支。
///
/// 分派规则：
/// - `resource_link`：只生成一行文本描述，不读取资源；
/// - `resource`（嵌入式资源）：值是对象且含 `blob` 时——文本 MIME 显示解码后的 UTF-8，
///   其余保存到临时文件并给出路径；MIME 为图片的留给兜底转换变成图片块；没有 `blob`
///   的资源（例如内联文本）也走兜底；
/// - 其它 `type`（text/image/audio/未知）以及原始值不是对象的情况走 `fallback_content`。
///
/// 每个可能抛错的分支都复刻 TS 的 TypeError 文案，因为模型看到的就是这条消息。
async fn block_to_content(
    server: &str,
    block: &ContentBlock,
    options: &ConvertMcpResultOptions,
) -> Result<Vec<UserContentBlock>, AgentToolError> {
    let value = block.as_value();
    let Some(map) = value.as_object() else {
        return fallback_content(block);
    };
    match map.get("type").and_then(Value::as_str) {
        Some("resource_link") => {
            let mut details: Vec<String> = Vec::new();
            // `[block.mimeType, ...].filter(Boolean)`：空串/0/null 都被过滤掉。
            if kiana_json::js_string::is_truthy(map.get("mimeType")) {
                details.push(kiana_json::js_string::js_string(map.get("mimeType")));
            }
            // `block.size === undefined ? undefined : formatSize(block.size)`：显式 null 也格式化。
            if let Some(size) = map.get("size") {
                details.push(format_size_js(size));
            }
            let read = if options.readable_resources {
                format!(". Read it with {READ_MCP_RESOURCE_TOOL} (server \"{server}\")")
            } else {
                String::new()
            };
            let description = if kiana_json::js_string::is_truthy(map.get("description")) {
                format!(
                    ": {}",
                    kiana_json::js_string::js_string(map.get("description"))
                )
            } else {
                String::new()
            };
            // `block.title ?? block.name`：只对 null/缺失回退。
            let title = match map.get("title") {
                None | Some(Value::Null) => map.get("name"),
                Some(title) => Some(title),
            };
            let details = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            // 最终形如 `[Resource <uri> "<title>" (mime, size): description. Read it with
            // read_mcp_resource (server "<server>")]`，可选段按存在与否拼接。
            return Ok(vec![text_block(format!(
                "[Resource {} \"{}\"{details}{description}{read}]",
                kiana_json::js_string::js_string(map.get("uri")),
                kiana_json::js_string::js_string(title)
            ))]);
        }
        Some("resource") => {
            let resource = match map.get("resource") {
                Some(Value::Object(resource)) => Some(resource),
                // 数组也是对象：`"blob" in [...]` 为 false（没有自有的 blob 属性）。
                Some(Value::Array(_)) => None,
                other => return Err(js_in_operator_error("blob", other)),
            };
            let has_blob = resource.is_some_and(|resource| resource.contains_key("blob"));
            // 只有“有 blob 且 MIME 不是图片”才在这里消费；图片资源留给 fallback 转成
            // ImageContent，没有 blob 的资源（例如内联文本）也走 fallback。
            if let Some(resource) = resource
                && has_blob
                && !mime_starts_with_image(resource.get("mimeType"))?
            {
                let blob = resource.get("blob").expect("contains_key checked above");
                // `blob` 是服务器的原始值：字符串按宽松 base64，数组逐项取低 8 位，其它类型抛 TypeError。
                let data = buffer_from_base64(blob)?;
                let mime_type = match resource.get("mimeType") {
                    Some(Value::String(mime_type)) => Some(mime_type.as_str()),
                    _ => None,
                };
                if is_text_mime_type(mime_type) {
                    // 文本 MIME：按 UTF-8 宽松解码，非法字节替换成 U+FFFD。
                    return Ok(vec![text_block(
                        String::from_utf8_lossy(&data).into_owned(),
                    )]);
                }
                // 展示用大小：已解码的字节数，走 u64 版本的 format_size（不是上面对任意 JSON 值的版本）。
                let kind = format!(
                    "{}, {}",
                    mime_type.unwrap_or("unknown type"),
                    kiana_tools::truncate::format_size(data.len() as u64)
                );
                let uri = kiana_json::js_string::js_string(resource.get("uri"));
                // 保存失败只降级成文本提示，本次工具调用仍然成功（与 limit_mcp_content 的全文保存一致）。
                let saver = options
                    .save_output
                    .clone()
                    .unwrap_or_else(default_output_saver);
                return Ok(match saver(data, extension_of(&uri)).await {
                    Ok(path) => vec![text_block(format!(
                        "[Binary resource {uri} ({kind}) saved to {path}]"
                    ))],
                    Err(error) => vec![text_block(format!(
                        "[Binary resource {uri} ({kind}) could not be saved: {error}]"
                    ))],
                });
            }
        }
        _ => {}
    }
    fallback_content(block)
}

/// 服务器内容块的模型可见内容（截断之前）。TS: `tools.ts#toModelContent`。
///
/// 与 TS 一样并行转换（`Promise.all`）；任一块抛错时整个转换失败（`Promise.all` 的拒绝）。
///
/// 并发语义：所有块的 future 用 `join_all` 同时推进，输出顺序与输入顺序一致；保存器等
/// 副作用可能交错执行。`join_all` 不会因为一个块失败而取消其它块，因此错误按**块顺序**
/// 返回第一个失败（后面的错误可能被前面掩盖），且失败时不会返回部分内容；失败前其它块
/// 可能已经写出临时文件，本函数不做清理。
pub async fn to_model_content(
    server: &str,
    blocks: &[ContentBlock],
    options: &ConvertMcpResultOptions,
) -> Result<Vec<UserContentBlock>, AgentToolError> {
    let converted = futures::future::join_all(
        blocks
            .iter()
            .map(|block| block_to_content(server, block, options)),
    )
    .await;
    let mut content = Vec::new();
    for block in converted {
        content.extend(block?);
    }
    Ok(content)
}

/// 转换一个 MCP 结果。`isError` 结果变成保留结构化结果的错误结果。
/// TS: `tools.ts#convertMcpResult`（TS 的 Promise 拒绝 → 这里的 `Err`）。
///
/// 步骤：
/// 1. `content` 非空时逐块转换；为空时回退到 `kiana_mcp` 的 `to_llm_content`（把
///    `structuredContent` 渲染成两空格缩进的 JSON），避免服务器只给结构化结果时模型
///    什么都看不到；
/// 2. 判定 `isError`：类型化字段优先，否则按 `extra["isError"]` 的 JS 真值（`1`、`"false"`
///    都算错误，原始值留在 `extra`）；
/// 3. 出错且没有任何文本块时补一条 `MCP tool {server}/{tool} returned an error`；
/// 4. 截断（[`limit_mcp_content`]），得到模型内容与可选的 `fullOutputPath`；
/// 5. `structured_content`：把结果序列化后删掉 `_meta`，再按 JS 键序排序；它总是 `Some`。
///
/// 错误：只有内容块转换会返回 `Err`（保存失败已降级为文本）；传输/超时错误发生在更早的
/// `call_tool` 处。`details` 的键序固定为 `server`、`tool`、`fullOutputPath`。
pub async fn convert_mcp_result(
    server: &str,
    tool: &str,
    result: CallToolResult,
    options: ConvertMcpResultOptions,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    // `content` 为空时用 `to_llm_content` 把 `structuredContent` 渲染成两空格缩进 JSON
    // 兜底；否则模型看不到仅含结构化结果的服务器输出。
    let mut converted = if result.content.is_empty() {
        let fallback = kiana_mcp::protocol_types::to_llm_content(&result);
        llm_to_user(fallback)
    } else {
        to_model_content(server, &result.content, &options).await?
    };
    let is_error = match result.is_error {
        Some(is_error) => is_error,
        // `kiana-mcp` 只把布尔值读进 `is_error`；TS 的 `result.isError` 是任意真值
        // （`isError: 1`、`"false"` 都算错误），原始值留在 `extra`。
        None => kiana_json::js_string::is_truthy(result.extra.get("isError")),
    };
    if is_error && text_of(&converted).is_empty() {
        converted.push(text_block(format!(
            "MCP tool {server}/{tool} returned an error"
        )));
    }
    let (content, full_output_path) = limit_mcp_content(converted, options.save_output).await;
    // 脚本拿到的是服务器结果的再序列化：去掉 `_meta`（TS 的 `delete result._meta`），再排成
    // JS 对象键序。键序来自结构体声明序，不是服务器的原始键序（模块文档第 5 条差异）。
    let mut script_result = serde_json::to_value(&result).unwrap_or(Value::Null);
    if let Value::Object(script_result) = &mut script_result {
        script_result.shift_remove("_meta");
    }
    Ok(AgentToolResult {
        content,
        details: details_json(server, tool, full_output_path.as_deref()),
        structured_content: Some(kiana_json::js_object::sort_js_keys(&script_result)),
        usage: None,
        is_error: is_error.then_some(true),
        terminate: None,
    })
}

/// 工具输入 schema 必须是对象。MCP 服务器可能省略 `type`（或给它 `null`），有些 provider
/// 也拒绝没有 `properties` 的对象 schema。TS: `tools.ts#toParameters`
/// （`type: schema.type ?? "object"`）。
///
/// 只补两个字段：`type` 缺失或为 `null` 时写 `"object"`（`??` 语义，`false` 等其它值保留），
/// `properties` 键完全缺失时补空对象（显式 `null` 保留，因为判定是 `=== undefined`）。
/// 其余关键字原样保留，最后按 `{...schema, type, properties}` 的 JS 键序排序。
fn to_parameters(schema: &Map<String, Value>) -> Value {
    let mut parameters = schema.clone();
    if parameters.get("type").is_none_or(Value::is_null) {
        parameters.insert("type".to_string(), Value::String("object".to_string()));
    }
    if !parameters.contains_key("properties") {
        parameters.insert("properties".to_string(), Value::Object(Map::new()));
    }
    // `{...schema, type, properties}` 的对象展开按 JS 自身键序（整数样式键升序在前）。
    kiana_json::js_object::sort_js_keys(&Value::Object(parameters))
}

/// MCP 工具 annotations 里的布尔提示；没有任何提示时返回 `None`。
/// TS: `tools.ts#toToolAnnotations`。
///
/// 只搬运四个标准化 hint，`title` 等其它注解忽略（title 在
/// [`create_mcp_tool_definition`] 里单独处理）。四个 hint 全缺失（或服务器根本没给
/// annotations）时返回 `None`，不会产出一个全空的注解对象。
fn to_tool_annotations(tool: &McpTool) -> Option<ToolAnnotations> {
    let source = tool.annotations.as_ref()?;
    let annotations = ToolAnnotations {
        read_only_hint: source.read_only_hint,
        destructive_hint: source.destructive_hint,
        idempotent_hint: source.idempotent_hint,
        open_world_hint: source.open_world_hint,
    };
    if annotations.read_only_hint.is_none()
        && annotations.destructive_hint.is_none()
        && annotations.idempotent_hint.is_none()
        && annotations.open_world_hint.is_none()
    {
        return None;
    }
    Some(annotations)
}

/// 取工具调用进度通知的显示文本：`progress.message ?? \`Progress ${progress}${"/"+total}\``。
///
/// `progress` 是服务器发来的原始参数，不做类型校验：`total` 缺失时不加后缀，存在则按
/// `String()` 拼成 `/total`（`null` → `/null`）；`message` 为 `null`/缺失时用
/// `Progress <progress>[/<total>]` 兜底，非字符串也按 `String()` 显示。
fn progress_text(progress: &Value) -> String {
    let map = progress.as_object();
    let total = match map.and_then(|map| map.get("total")) {
        None => String::new(),
        Some(total) => format!("/{}", kiana_json::js_string::js_string(Some(total))),
    };
    let fallback = format!(
        "Progress {}{total}",
        kiana_json::js_string::js_string(map.and_then(|map| map.get("progress")))
    );
    match map.and_then(|map| map.get("message")) {
        None | Some(Value::Null) => fallback,
        Some(message) => kiana_json::js_string::js_string(Some(message)),
    }
}

/// [`create_mcp_tool_definition`] 的参数。TS: `tools.ts#createMcpToolDefinition` 的 options。
///
/// 所有字段都由 `mcp/index.rs` 在注册时填好：`tool` 来自连接的工具列表，`name` 由
/// [`create_mcp_tool_name`] 生成；本结构不做校验或去重。`get_client` 每次执行都会重新
/// 调用，取到的是执行时的连接，而不是注册时的快照。
pub struct McpToolDefinitionOptions {
    /// 服务器名（工具名与 label 的一部分）。
    pub server: String,
    /// 服务器上的工具。
    pub tool: McpTool,
    /// 注册给模型的名字（[`create_mcp_tool_name`] 的结果）。
    pub name: String,
    /// 服务器默认 exposure。
    pub exposure: McpExposure,
    /// 工具所属命名空间。
    pub namespace: ToolNamespace,
    /// 每次请求的超时（毫秒）。
    pub timeout_ms: f64,
    /// 取工具调用入口（连接可能还在建立）。
    pub get_client: McpGetClientFn,
    /// `read_mcp_resource` 能否读取该服务器的资源。
    pub readable_resources: Option<ReadableResourcesFn>,
}

/// 执行一个 MCP 工具定义。
///
/// `ToolDefinition::execute` 是 `Arc<dyn ToolDefinitionExecute>`，需要一个自持有的结构体
/// 保存全部选项；每次执行都通过 [`McpGetClientFn`] 重新取连接，拿到的是当前连接而不是
/// 注册时的快照。
struct McpToolExecutable {
    options: McpToolDefinitionOptions,
}

impl ToolDefinitionExecute for McpToolExecutable {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
        _ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        Box::pin(async move {
            // 阶段 1：取当前连接。服务器被禁用、仍在启动、工具已下架都在这里失败
            // （错误由 mcp/index.rs 的 get_client 闭包构造），消息原样交给上层。
            let options = &self.options;
            let client = (options.get_client)()
                .await
                .map_err(|error| agent_error(error.to_string()))?;
            // 阶段 2：参数归一化。TS 的 `params ?? {}`：模型没给参数时发送空对象而不是 null。
            let args = if params.is_null() {
                Value::Object(Map::new())
            } else {
                params
            };
            // 阶段 3：把服务器的进度通知转成一条文本更新。没有 on_update 时什么都不做，
            // 但仍返回 Ok——“没人接收进度”不是传输错误。
            let progress_server = options.server.clone();
            let progress_tool = options.tool.name.clone();
            let progress_update = on_update.clone();
            let on_progress: kiana_mcp::client::ProgressCallback =
                Arc::new(move |progress: &Value| {
                    if let Some(on_update) = &progress_update {
                        on_update(AgentToolResult {
                            content: vec![text_block(progress_text(progress))],
                            details: details_json(&progress_server, &progress_tool, None),
                            structured_content: None,
                            usage: None,
                            is_error: None,
                            terminate: None,
                        });
                    }
                    Ok(())
                });
            // 取消令牌是共享句柄（Clone 不复制状态）；signal_reason 保持 None，对应裸 abort。
            // timeout_ms 是单次请求的截止时间，收到进度通知会把它重置为“现在 + 同一个值”。
            let request_options = McpRequestOptions {
                signal: signal.cloned(),
                signal_reason: None,
                timeout_ms: Some(options.timeout_ms),
                on_progress: Some(on_progress),
            };
            let result = client
                .call_tool(&options.tool.name, args, &request_options)
                .await
                .map_err(|error| agent_error(error.to_string()))?;
            // 阶段 4：转换前取一次“资源可读”快照（只影响 resource_link 的提示文本）。
            let readable_resources = options
                .readable_resources
                .as_ref()
                .map(|readable| readable())
                .unwrap_or(false);
            // 阶段 5：转换。save_output 为 None → 需要落盘时用默认临时文件。
            convert_mcp_result(
                &options.server,
                &options.tool.name,
                result,
                ConvertMcpResultOptions {
                    save_output: None,
                    readable_resources,
                },
            )
            .await
        })
    }
}

/// 构造一个 MCP 工具定义。TS: `tools.ts#createMcpToolDefinition`。
///
/// 输入是服务器在 `tools/list` 里声明的 `Tool`，输出是可直接注册的 `ToolDefinition`。
/// 关键映射：
/// - `name` 由调用方先经 [`create_mcp_tool_name`] 生成，本函数只负责带上；
/// - `label` 为 `server/tool`：渲染器拿它当调用标题，`ToolDefinition::label` 同时供 UI 显示；
/// - 描述优先级：JS trim 后非空的 `description` → 非空 `title` → 生成的
///   `MCP tool <tool> from server <server>`；
/// - 参数 schema 经 `to_parameters` 补齐 `type`（缺失或 `null` 时为 `object`）与
///   `properties`；
/// - 输出 schema 经 `create_mcp_result_schema_from_raw` 包装成整个 `CallToolResult` 的
///   声明（服务器 `outputSchema` 被重定位到 `structuredContent` 下）；
/// - `exposure` 经 [`to_tool_exposure`] 映射，`readable_resources` 传给执行器。
///
/// `title` 取 `tool.title`，缺失时回退到 `annotations.title`；它不参与 trim，只用于
/// 描述兜底。
pub fn create_mcp_tool_definition(options: McpToolDefinitionOptions) -> ToolDefinition {
    let server = options.server.clone();
    let tool = options.tool.clone();
    let title = tool.title.clone().or_else(|| {
        tool.annotations
            .as_ref()
            .and_then(|annotations| annotations.title.clone())
    });
    let annotations = to_tool_annotations(&tool);
    let label = format!("{}/{}", options.server, tool.name);
    let trimmed_description = tool
        .description
        .as_deref()
        .map(kiana_json::js_string::js_trim)
        .unwrap_or("");
    let description = if !trimmed_description.is_empty() {
        trimmed_description.to_string()
    } else if let Some(title) = title.filter(|title| !title.is_empty()) {
        title
    } else {
        format!("MCP tool {} from server {}", tool.name, server)
    };
    let renderers = create_mcp_tool_renderers(&label);
    // `tool.outputSchema` 在 TS 里是任意 JSON 值；`kiana-mcp` 只把对象放进类型化字段，非对象
    // （数组/字符串/数字/布尔）原样留在 `extra`。两者合并回 TS 看到的原始值。
    let raw_output_schema = tool.output_schema.clone().map_or_else(
        || tool.extra.get("outputSchema").cloned(),
        |schema| Some(Value::Object(schema)),
    );
    ToolDefinition {
        name: options.name.clone(),
        label,
        description,
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: to_parameters(&tool.input_schema),
        constrained_sampling: None,
        render_shell: renderers.render_shell,
        prepare_arguments: None,
        output_schema: Some(create_mcp_result_schema_from_raw(raw_output_schema)),
        exposure: Some(to_tool_exposure(options.exposure)),
        namespace: Some(options.namespace.clone()),
        reaches_tools: None,
        annotations,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(McpToolExecutable { options }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// 全局主题着色（渲染器在 TUI 里运行时主题一定已初始化；与 `keyHint` 的前提一致）。
///
/// 未初始化时 `expect` 直接 panic，而不是静默返回原文：渲染只发生在 TUI 启动之后，
/// 这对应 TS 在这里抛异常的行为。
fn theme_fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 折叠结果里“还有 N 行”的提示行：`... (N more lines,` + 键提示 + `)`。
///
/// 三段分别着色：muted 的前缀、`app.tools.expand` 的键位提示、muted 的右括号。它作为
/// `VisualLinePreview` 的 `format_hint`，在折叠输出末尾渲染隐藏行数。
fn more_lines_hint(hidden: usize) -> String {
    format!(
        "{} {}{}",
        theme_fg(ThemeColor::Muted, &format!("... ({hidden} more lines,")),
        kiana_ui_kit::keybinding_hints::key_hint("app.tools.expand", "to expand"),
        theme_fg(ThemeColor::Muted, ")")
    )
}

/// MCP 工具调用的渲染器，label 形如 `server/tool`（工具注册前也可用）。
/// TS: `tools.ts#createMcpToolRenderers`。
///
/// 返回的 `ToolRenderers` 只填 `render_call` 与 `render_result`，`render_shell` 为 `None`
/// （使用默认外壳）。两个闭包都不持有可变状态：每次渲染重新构造组件，因此同一个定义
/// 可以被多处渲染。
///
/// `render_result` 的折叠视图：
/// - `get_text_output` 拼接文本块；未显示图片时追加图片占位行；
/// - JS trim 后为空 → 返回空容器（不画空行）；
/// - 每行按 `is_error` 着色（`ThemeColor::Error` / `ThemeColor::ToolOutput`），制表符
///   替换成三个空格；
/// - 展开时整段显示；折叠时只显示前 `OUTPUT_PREVIEW_LINES`（5）个**可视**行并附
///   “还有 N 行”提示，`details.fullOutputPath` 存在时再补一行全文路径。
///
/// `details` 是未校验的 JSON：`fullOutputPath` 缺失、为 `null` 或非字符串时静默跳过。
pub fn create_mcp_tool_renderers(label: &str) -> ToolRenderers {
    let call_label = label.to_string();
    let render_call: RenderCallFn = Arc::new(
        move |args: &JsonValue, theme, context: &ToolRenderContext| {
            let text = kiana_tools::render_utils::format_tool_call_with_args(
                &call_label,
                Some(args),
                theme,
                context.expanded,
            );
            Box::new(Text::with_padding(text, 0, 0)) as Box<dyn Component>
        },
    );
    let render_result: RenderResultFn = Arc::new(
        |result: &AgentToolResult<JsonValue>, options, theme, context: &ToolRenderContext| {
            let mut component = Container::new();
            let raw_output =
                kiana_tools::render_utils::get_text_output(Some(result), context.show_images);
            let output = kiana_json::js_string::js_trim(&raw_output);
            if output.is_empty() {
                return Box::new(component) as Box<dyn Component>;
            }
            let color = if context.is_error {
                ThemeColor::Error
            } else {
                ThemeColor::ToolOutput
            };
            let styled = kiana_tools::render_utils::replace_tabs(output)
                .split('\n')
                .map(|line| theme.fg(color, line))
                .collect::<Vec<_>>()
                .join("\n");
            component.add_child(Box::new(Spacer::new()));
            if options.expanded {
                component.add_child(Box::new(Text::with_padding(styled, 0, 0)));
            } else {
                // 限制可视行而不是逻辑行：MCP 结果常常是一整行长 JSON。
                component.add_child(Box::new(VisualLinePreview::new(VisualLinePreviewOptions {
                    text: styled,
                    max_visual_lines: OUTPUT_PREVIEW_LINES,
                    keep: Keep::Start,
                    format_hint: Box::new(more_lines_hint),
                })));
                if let Some(full_output_path) =
                    result.details.get("fullOutputPath").and_then(Value::as_str)
                {
                    component.add_child(Box::new(Text::with_padding(
                        theme.fg(
                            ThemeColor::Muted,
                            &format!("Full output: {full_output_path}"),
                        ),
                        0,
                        0,
                    )));
                }
            }
            Box::new(component) as Box<dyn Component>
        },
    );
    ToolRenderers {
        render_shell: None,
        render_call: Some(render_call),
        render_result: Some(render_result),
    }
}

// 单元测试只覆盖本文件内可见的纯逻辑（JS 键序、`$ref` 重定位、isError 真值、Node 错误
// 文案）；端到端的内容转换与 TS 黄金文件在 `tests/mcp_tools.rs`。
#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use serde_json::json;

    /// 构造一个只用于读取声明的工具定义（调用入口不会被触发）。
    fn definition_for(tool: McpTool) -> ToolDefinition {
        let get_client: McpGetClientFn = Arc::new(|| {
            Box::pin(async { Err(McpTransportError::new("unused")) })
                as BoxFuture<'static, Result<Arc<dyn McpToolCaller>, McpTransportError>>
        });
        create_mcp_tool_definition(McpToolDefinitionOptions {
            server: "s".to_string(),
            tool,
            name: "n".to_string(),
            exposure: McpExposure::Direct,
            namespace: ToolNamespace {
                name: "mcp".to_string(),
                aliases: None,
                description: None,
                instructions: None,
            },
            timeout_ms: 1000.0,
            get_client,
            readable_resources: None,
        })
    }

    fn object_keys(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// 评审回归：JS 对象自身的键序（整数样式键升序在前），`relocateSchema`/`toParameters` 都按它。
    #[test]
    fn emits_schema_objects_in_javascript_key_order() {
        let schema = create_mcp_result_schema(
            json!({
                "properties": { "10": { "type": "string" }, "2": { "type": "string" }, "b": { "type": "string" } },
                "$defs": { "10": {}, "2": {}, "a": {} }
            })
            .as_object(),
        );
        let structured = &schema["properties"]["structuredContent"];
        assert_eq!(object_keys(structured), vec!["properties", "$defs"]);
        assert_eq!(object_keys(&structured["properties"]), vec!["2", "10", "b"]);
        assert_eq!(object_keys(&structured["$defs"]), vec!["2", "10", "a"]);

        let tool = McpTool {
            name: "t".to_string(),
            input_schema: json!({
                "10": { "type": "string" },
                "2": { "type": "string" },
                "b": { "type": "string" }
            })
            .as_object()
            .expect("schema")
            .clone(),
            ..McpTool::default()
        };
        let parameters = definition_for(tool).parameters;
        assert_eq!(
            object_keys(&parameters),
            vec!["2", "10", "b", "type", "properties"]
        );
        assert_eq!(
            kiana_json::js_object::stringify(&parameters),
            r#"{"2":{"type":"string"},"10":{"type":"string"},"b":{"type":"string"},"type":"object","properties":{}}"#
        );
    }

    /// 评审回归：非对象的 `outputSchema` 原样进入 `structuredContent`（TS 的真值判定）。
    #[test]
    fn keeps_raw_non_object_output_schemas() {
        let schema_for = |output_schema: Value| {
            let tool = McpTool::from_value(&json!({
                "name": "t",
                "inputSchema": {},
                "outputSchema": output_schema
            }))
            .expect("tool");
            assert_eq!(tool.output_schema, None, "the value lives in extra");
            definition_for(tool).output_schema.expect("schema")
        };
        assert_eq!(
            schema_for(json!([]))["properties"]["structuredContent"],
            json!([])
        );
        assert_eq!(
            schema_for(json!("x"))["properties"]["structuredContent"],
            json!("x")
        );
        assert_eq!(
            schema_for(json!(0))["properties"]["structuredContent"],
            json!({})
        );
        assert_eq!(
            schema_for(json!(false))["properties"]["structuredContent"],
            json!({})
        );
    }

    /// 评审回归：`isError` 是任意真值，不只是布尔 `true`。
    #[tokio::test]
    async fn treats_truthy_non_boolean_is_error_as_an_error() {
        let result = CallToolResult::from_value(&json!({ "content": [], "isError": 1 }))
            .expect("call tool result");
        assert_eq!(result.is_error, None, "1 lands in extra");
        let converted = convert_mcp_result("s", "t", result, ConvertMcpResultOptions::default())
            .await
            .expect("convert");
        assert_eq!(converted.is_error, Some(true));
        assert_eq!(
            converted.content,
            vec![text_block("MCP tool s/t returned an error")]
        );
        let structured = converted.structured_content.expect("structured content");
        assert_eq!(structured["isError"], json!(1));
        assert_eq!(object_keys(&structured), vec!["content", "isError"]);

        // Falsy values stay successes and keep the raw value.
        for value in [json!(0), json!(null)] {
            let result = CallToolResult::from_value(&json!({ "content": [], "isError": value }))
                .expect("call tool result");
            let converted =
                convert_mcp_result("s", "t", result, ConvertMcpResultOptions::default())
                    .await
                    .expect("convert");
            assert_eq!(converted.is_error, None);
            assert_eq!(
                converted.structured_content.expect("structured content")["isError"],
                value
            );
        }
    }

    /// 评审回归：默认保存器失败时，文本是 Node 的 `error.message` 形状而不是 Rust 的
    /// `No such file or directory (os error 2)`。
    #[cfg(unix)]
    #[test]
    fn formats_failed_output_file_writes_like_node() {
        let message = node_output_file_error(&std::io::Error::from_raw_os_error(2), ".txt");
        assert!(
            Regex::new(
                r"^ENOENT: no such file or directory, open '.*kiana-mcp-[0-9a-f]{16}\.txt'$"
            )
            .expect("regex")
            .is_match(&message),
            "{message}"
        );
        let message = node_output_file_error(&std::io::Error::from_raw_os_error(13), ".bin");
        assert!(
            message.starts_with("EACCES: permission denied, open '"),
            "{message}"
        );
        // libuv 的 `uv_strerror(ENAMETOOLONG)` 是 "name too long"（Node 26 实测）。
        let message = node_output_file_error(&std::io::Error::from_raw_os_error(36), ".txt");
        assert!(
            message.starts_with("ENAMETOOLONG: name too long, open '"),
            "{message}"
        );
        // Non-errno errors keep their own message (custom savers in tests use `io::Error::other`).
        assert_eq!(
            node_output_file_error(&std::io::Error::other("boom"), ".txt"),
            "boom"
        );
    }
}
