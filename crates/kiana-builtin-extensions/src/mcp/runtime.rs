//! `extensions/mcp/runtime.ts`：连接、传输与 OAuth 登录。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/runtime.ts`（`runtime.lazy.ts` 的
//! `loadMcpRuntime` 是动态 import 包装，Rust 侧由 [`load_mcp_runtime`] 直接给出本模块）。
//!
//! 一个 [`McpServerConnection`] 对应 `mcp.json` 里的一个服务器：懒连接、掉线后下次调用重连、
//! 瞬时 HTTP 错误重试、会话过期重建会话、401 后走 OAuth 刷新或要求登录。
//!
//! # 与 TS 的对应
//!
//! - 状态字段（`state`/`error`/`tools`/`has_resources`/`instructions`/`challenge`）是共享的
//!   [`ConnectionFields`]（`RwLock`/`AtomicBool`）：[`McpServerConnection`] 是廉价的可克隆
//!   句柄，`Deref` 到这些字段（`connection.state.read()`），另有同名快照方法
//!   （`connection.state()`）。TS 的普通字段在 Rust 的 `&self` 方法下需要内部可变性。
//! - 回调参数是 `&McpServerConnection`（TS 的 `connection`）；句柄克隆共享同一状态。
//! - `AbortController` → [`CancellationToken`]；`Promise` 记忆化 → [`futures::future::Shared`]。
//! - 错误 `Display` 逐字等于 TS 的 `error.message`。
//! - `"url" in config` 一律按校验后的 JS 对象（`config_to_value`）判断：校验返回输入对象的
//!   浅拷贝，stdio 条目也可能带 `url` 键（`{command, url: null}`、`type: "stdio"` + 字符串
//!   url），TS 的 transport/OAuth/重试/描述都按这个键分支。
//!
//! # 这个文件解决什么问题
//!
//! `mcp.json` 的每个条目在运行时都是一个 [`McpServerConnection`]：它把 `kiana_mcp::McpClient`、
//! 传输（stdio / Streamable HTTP）、OAuth 与状态字段绑成一个可克隆句柄。没有它，调用方
//! （工具、资源列表、`/mcp` 命令）就得各自处理“现在有没有连接、断了要不要重连、会话过期
//! 怎么办、401 要不要登录”。
//!
//! 它不解析配置（`config.rs`）、不实现 OAuth 细节（`oauth.rs`）、不渲染工具/资源
//! （`tools.rs`/`resources.rs`）；这些模块通过本文件暴露的句柄与 trait 实现协作。
//!
//! # 在系统中的位置
//!
//! - 构造：`mcp/index.rs` 的 `create_connection`（扩展发现流程）与 `mcp/cli.rs` 的
//!   `create_connection`（`kiana mcp` 子命令）用 [`McpServerConnectionOptions`] 建句柄；
//!   默认传输工厂是 [`create_default_transport`]。
//! - 调用：同 crate 的 `index.rs`/`cli.rs` 调用 [`McpServerConnection::get_client`]、
//!   [`McpServerConnection::reconnect`]、[`McpServerConnection::close`] 等方法；工具层通过
//!   `McpToolCaller`、资源层通过 `McpResourceServer`（两个 trait 都在本文件实现）下发请求。
//! - 集成测试：`tests/mcp_connections.rs` 用内存传输驱动本文件的行为。
//!
//! # 一次调用的执行流程
//!
//! 1. [`McpServerConnection::get_client`] 先看有没有处于 `connected` 的客户端；有就直接返回。
//! 2. 否则单飞执行 `open()`：状态先置 `connecting`，对带 `url` 键的服务器最多再做两次重试
//!    （间隔见 `CONNECT_RETRY_DELAYS_MS`，仅瞬时错误）。
//! 3. `connect_once` 为每次尝试新建客户端，登记 `roots` 与关闭监听（配置了日志时再挂日志
//!    通知），让传输工厂给出的传输完成 `initialize` 握手（服务器声明 `tools` capability
//!    时再 `tools/list`）；成功后把客户端、工具、
//!    resources capability 与 instructions 写入 [`ConnectionFields`]，状态置 `connected`。
//! 4. `call_tool`/资源请求经 `with_client` 取客户端执行；只读请求在首次瞬时 HTTP 错误后重试
//!    一次，会话过期（HTTP 404）对只读请求和工具调用都重连重跑一次，需要登录的 401/授权
//!    错误转成登录指引（没有 auth provider 的 401 原样返回）。
//!
//! # 状态机
//!
//! 状态集合与转换见 [`ServerState`] 的文档；写入点只有 `open`、`setup_connected_client`、
//! `connect_failed`、`handle_client_close`、`mark_needs_auth` 与 `close`。
//!
//! # 并发与生命周期
//!
//! - 句柄克隆共享 `Arc<ConnectionInner>`/`Arc<ConnectionFields>`；字段用 `std::sync::RwLock`
//!   或 `AtomicBool`，读写都是短临界区（复制/替换后立即释放），因此同步 getter 是安全的。
//! - 建连用 `tokio::sync::Mutex<Option<Shared<...>>>` 做单飞：并发调用者 await 同一个
//!   future，`open()` 只跑一次；有 tokio 运行时的时候另起任务驱动这个 future，避免等待者
//!   取消后 future 无人 poll。
//! - [`McpServerConnection::close`] 取消 `shutdown` token：重试的 sleep、进行中的 `open` 与
//!   已建立客户端的关闭桥都会看到它。
//!
//! # 失败模式
//!
//! - 连接失败：`error` 字段保存失败原因（stdio 服务器连接失败时追加 stderr 尾部，最多
//!   `STDERR_TAIL_CHARS` 个 UTF-16 码元），返回的错误带
//!   `MCP server "<name>" failed to connect: ` 前缀；需要登录时改置 `needs-auth` 并返回登录指引。
//! - 请求失败：除上述重试/登录转换外，`McpTransportError` 原样返回给调用方。
//! - 关闭后调用：`get_client` 返回 `MCP server "<name>" is shut down`，不会创建新连接。
//!
//! # 阅读顺序
//!
//! [`ServerState`] → [`ConnectionFields`] → [`McpServerConnection`]（`new`、`get_client`、
//! `with_client`、`open`、`close`）→ `McpToolCaller`/`McpResourceServer` 两个实现。

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use indexmap::IndexMap;
use kiana_config::resolve_config_value::resolve_config_value_or_throw;
use kiana_core::config::VERSION;
use kiana_core::mcp_servers::{McpClientRegistration, McpServerConfig};
use kiana_json::js_string::{is_truthy, js_string};
use kiana_mcp::auth_provider::{AuthProvider, McpTransportError};
use kiana_mcp::client::{ConnectionState, Roots};
use kiana_mcp::oauth::OAuthChallenge;
use kiana_mcp::transports::stdio::{
    StdioEnvPolicy, StdioStderrMode, StdioTransport, StdioTransportOptions,
};
use kiana_mcp::transports::{McpTransport, Subscription, TransportEvents};
use kiana_mcp::{
    CallToolResult, GetPromptResult, JSON_RPC_ERROR_CODES, ListResourceTemplatesResult,
    ListResourcesResult, McpClient, McpClientOptions, McpError, McpRequestOptions, Prompt,
    ReadResourceResult, Resource, ResourceTemplate, Root, StreamableHttpTransport,
    StreamableHttpTransportOptions, Tool,
};
use kiana_sandbox::SandboxManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::mcp::config::{McpServerEntry, config_to_value, js_keys};
use crate::mcp::resources::McpResourceServer;
use crate::mcp::tools::McpToolCaller;

pub use crate::mcp::log::McpServerLog;
use crate::mcp::oauth::{
    McpAuthProvider, McpAuthProviderOptions, McpOAuthSettings, create_mcp_auth_provider,
};
pub use crate::mcp::oauth::{McpOAuthCredentialStore, McpSignInCancelledError, sign_in_mcp_server};

const DEFAULT_TIMEOUT_SECONDS: f64 = 60.0;
const STDERR_TAIL_CHARS: usize = 2_000;
/// HTTP 服务器瞬时错误后两次重连的间隔。
const CONNECT_RETRY_DELAYS_MS: [u64; 2] = [250, 1_000];

/// `disconnected`：连接断开了（例如 stdio 服务器退出）；下次调用重连。
///
/// TS: `runtime.ts#ServerState`（不导出）。
///
/// # 状态机
///
/// | 状态 | 含义 | 写入点 |
/// |---|---|---|
/// | `connecting` | 正在建立连接；整个重试期间保持此状态，成功/失败后立即转出 | 每次 `open()` 开始 |
/// | `connected` | 握手完成（声明 `tools` capability 时还包括 `tools/list`），且客户端仍是 `ConnectionState::Connected` | `setup_connected_client` 末尾 |
/// | `disconnected` | 传输上报关闭；当前客户端被解除关联，下次调用重连 | `handle_client_close` |
/// | `needs-auth` | 需要用户登录（401/OAuth 授权要求，或凭据被删除） | `mark_needs_auth`（经 `connect_failed`/`with_client`/`sign_out`） |
/// | `failed` | 连接失败且不需要登录；`error` 保存原因 | `connect_failed` |
/// | `closed` | `close()` 已调用（或关闭期间连接失败） | `close`/`connect_failed` |
///
/// 只有 `connected` 会走“复用当前客户端”的快速路径；`disconnected`/`failed`/`needs-auth`
/// 都会在下一次调用时重新 `open()`。`closed` 是终态：`shutdown` token 一旦取消就不会再建立
/// 新连接（`closed()` 检查）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerState {
    /// `connecting`。
    Connecting,
    /// `connected`。
    Connected,
    /// `disconnected`。
    Disconnected,
    /// `needs-auth`。
    NeedsAuth,
    /// `failed`。
    Failed,
    /// `closed`。
    Closed,
}

impl ServerState {
    /// TS 里的字符串字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ServerState::Connecting => "connecting",
            ServerState::Connected => "connected",
            ServerState::Disconnected => "disconnected",
            ServerState::NeedsAuth => "needs-auth",
            ServerState::Failed => "failed",
            ServerState::Closed => "closed",
        }
    }
}

impl fmt::Display for ServerState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 诊断面使用的服务器状态（MCP manager 的稳定契约）。
///
/// 与内部 [`ServerState`] 的差别：`disconnected` 会懒重连，归到 [`Self::Connecting`]；
/// `closed` 没有对外意义，归到 [`Self::Disabled`]。这五个值是 `/mcp` 与结构化 API
/// 暴露的唯一状态集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatusState {
    /// 正在建立连接（含断线后的下一次懒重连）。
    Connecting,
    /// 握手与列表加载完成。
    Connected,
    /// 需要用户登录。
    NeedsAuth,
    /// 启动失败（`last_error` 有原因）。
    Failed,
    /// 配置禁用或已关闭。
    Disabled,
}

impl McpServerStatusState {
    /// 稳定字符串（`snake_case`）。
    pub fn as_str(self) -> &'static str {
        match self {
            McpServerStatusState::Connecting => "connecting",
            McpServerStatusState::Connected => "connected",
            McpServerStatusState::NeedsAuth => "needs_auth",
            McpServerStatusState::Failed => "failed",
            McpServerStatusState::Disabled => "disabled",
        }
    }
}

/// `McpServerStatus`：单个服务器的诊断快照。
///
/// 一个服务器失败不会影响其它服务器的状态；调用方按 `Vec` 顺序逐个展示即可。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    /// 状态。
    pub state: McpServerStatusState,
    /// 最近一次失败原因（`failed`/`needs-auth` 时通常有值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// `tools/list` 的工具数量（禁用/未连接时为 0）。
    pub tool_count: usize,
    /// `prompts/list` 的 prompt 数量。
    pub prompt_count: usize,
}

/// `(entry, cwd, authProvider) => McpTransport`。
///
/// 每次连接尝试（`open()` 的每一轮重试、每次重连）都调用一次，参数是创建连接时的条目快照、
/// 会话 cwd 与当前认证 provider。工厂签名不能返回 `Result`：构造期失败（URL 非法、header
/// 解析失败）由 [`create_default_transport`] 包成“在 `start()` 时才报错”的传输，而不是在这里
/// 丢失。自定义工厂（测试、SDK 调用方）返回的传输不经过 stdio 注册表，因此这类连接拿不到
/// stderr 尾部。
pub type McpTransportFactory = Arc<
    dyn Fn(&McpServerEntry, &Path, Option<Arc<dyn AuthProvider>>) -> Arc<dyn McpTransport>
        + Send
        + Sync,
>;

/// `providerToken(provider) => Promise<string | undefined>`。
///
/// 仅在 OAuth 分支未生效（`oauth_url_value` 返回 `None`）而 `"url" in config` 且
/// `auth.provider` 为真时使用：每次 `AuthProvider::token()` 都重新调用，因此 kiana provider
/// 的 key 轮换会在下一次请求生效。
pub type ProviderTokenFn = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// 被丢弃的 `Error` 消息。
fn error_message(error: &McpTransportError) -> String {
    error.to_string()
}

/// 网络失败与过载/重启中的服务器，值得再试一次。
///
/// 判定：有 HTTP status 时只看 408、429 与 500–599（501 除外）；没有 status 时只要
/// `is_type_error()` 为真就算瞬时错误——包括 fetch 层网络错误、`Invalid URL`、
/// 配置/协议结果校验抛出的 TypeError 等。404（会话过期）与 401 不在此列，由调用方另行处理。
fn is_transient_error(error: &McpTransportError) -> bool {
    match error.status() {
        Some(status) => status == 408 || status == 429 || (status >= 500 && status != 501),
        None => error.is_type_error(),
    }
}

/// 需要登录时返回给用户看的文案：
/// `MCP server "<name>" requires sign-in. Run /login <provider> to sign in.`；没有 provider
/// 时提示 `/mcp`。provider 的读取与 [`uses_oauth`] 只在 `"url" in config` 这个前置判断上相同
/// （stdio 条目带 `url` 也可能命中）；[`uses_oauth`] 另外检查整个 `auth` 是否为真、
/// 以及 headers 是否已带 `authorization`。
fn sign_in_required_message(entry: &McpServerEntry) -> String {
    // TS: `"url" in entry.config ? entry.config.auth?.provider : undefined`。
    // 校验返回的是输入对象的浅拷贝，stdio 条目也可能带 `url`（见 [`config_has_url`]）。
    let config = config_to_value(&entry.config);
    let provider = config
        .contains_key("url")
        .then(|| js_property(config.get("auth"), "provider"))
        .flatten()
        .filter(|provider| is_truthy(Some(provider)))
        .map(|provider| js_string(Some(provider)));
    format!(
        "MCP server \"{}\" requires sign-in. Run {} to sign in.",
        entry.name,
        provider
            .map(|provider| format!("/login {provider}"))
            .unwrap_or_else(|| "/mcp".to_string())
    )
}

/// `"url" in config`：TS 依据校验后对象上的键，而不是 transport 变体。
///
/// `validateMcpServerConfig` 返回输入对象的浅拷贝，因此 stdio 条目也可能带着 `url` 键：
/// `{command, url: null}` 与 `{type: "stdio", command, url: "https://…"}` 都通过校验，
/// 但运行时的一切（transport、OAuth、重试、`/mcp` 描述）都按 `"url" in config` 分支。
fn config_has_url(config: &McpServerConfig) -> bool {
    match config {
        McpServerConfig::Http(_) => true,
        McpServerConfig::Stdio(config) => config.extra.contains_key("url"),
    }
}

/// JS 的命名属性读取：非对象上的属性都是 `undefined`。
fn js_property<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value
        .and_then(Value::as_object)
        .and_then(|object| object.get(key))
}

/// `Object.keys(headers ?? {}).some(header => header.toLowerCase() === "authorization")`。
///
/// 数组/字符串的 `Object.keys` 是索引，数字/布尔是空数组；只有对象可能命中。
fn headers_contain_authorization(headers: Option<&Value>) -> bool {
    match headers {
        Some(Value::Object(map)) => map
            .keys()
            .any(|header| header.to_lowercase() == "authorization"),
        _ => false,
    }
}

/// HTTP 服务器在配置没有 `Authorization` 头且没有 `auth` 时用 OAuth。
///
/// 三个条件同时成立才为真：`"url" in config`（stdio 条目带 `url` 也算）、`config.auth` 按 JS
/// truthiness 为假、`headers` 的键里没有大小写不敏感的 `authorization`。为真且
/// `oauth_url_value` 返回 `Some`（url 按 JS truthiness 有值）时 [`McpServerConnection::new`]
/// 才创建 OAuth provider；否则走 `auth.provider` 的 token 路径或没有认证。
fn uses_oauth(entry: &McpServerEntry) -> bool {
    let config = config_to_value(&entry.config);
    if !config.contains_key("url") || is_truthy(config.get("auth")) {
        return false;
    }
    !headers_contain_authorization(config.get("headers"))
}

/// `~` / `~/…`（Windows 上还有 `~\…`）表示 home 目录，和 shell 一样。
fn expand_home(value: &str) -> String {
    let home = || {
        dirs::home_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if value == "~" {
        return home();
    }
    let is_home_prefix = value.starts_with("~/") || (cfg!(windows) && value.starts_with("~\\"));
    if is_home_prefix {
        // TS 用 `join(homedir(), value.slice(2))`，Node 的 `path.join` 会做词法规范化：
        // `~//x` 是 home/x（不是 /x）、`~/a/../b` 是 home/b、`~/` 是 home。
        return join_home(&home(), &value[2..]);
    }
    value.to_string()
}

/// `path.join(home, rest)`：词法规范化，忽略 rest 的根前缀（Node 的 join 不是 resolve），
/// 并在 rest 以分隔符结尾时保留尾部分隔符（`join("/h", "a/")` → `"/h/a/"`）。
fn join_home(home: &str, rest: &str) -> String {
    let mut joined = PathBuf::from(home);
    // Node 的 join 把 rest 当相对片段拼接：前导 `/` 与 `C:` 都不重置基准
    // （`win32.join("C:\\u", "D:\\x")` 是 `C:\\u\\D:\\x`），所以按分隔符逐段处理，
    // 而不是交给 `Path::components` 解释根/盘符。
    #[cfg(windows)]
    let parts = rest.split(['/', '\\']);
    #[cfg(not(windows))]
    let parts = rest.split('/');
    for part in parts {
        match part {
            "" | "." => {}
            ".." => {
                joined.pop();
            }
            part => joined.push(part),
        }
    }
    let mut result = joined.to_string_lossy().into_owned();
    let trailing = rest.ends_with('/') || (cfg!(windows) && rest.ends_with('\\'));
    if trailing && !result.ends_with(std::path::MAIN_SEPARATOR) {
        result.push(std::path::MAIN_SEPARATOR);
    }
    result
}

/// `path.resolve(base, value)` 的等价物（绝对化 + 词法规范化）。
fn resolve_from(base: &Path, value: &str) -> PathBuf {
    // Node 的 `path.resolve` 总是给出绝对路径：相对 base 先按进程工作目录绝对化。
    let base = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(base)
    };
    let value_path = Path::new(value);
    let joined = if value_path.is_absolute() {
        value_path.to_path_buf()
    } else {
        base.join(value_path)
    };
    normalize_lexically(&joined)
}

/// 词法规范化：去掉 `.` 与可消的 `..`（不访问文件系统）。
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() && !result.has_root() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    if result.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        result
    }
}

/// `pathToFileURL(path).href`。
///
/// Node 的实现是 `path.resolve(path)` + WHATWG URL；输入以路径分隔符结尾时保留尾部斜杠
/// （根路径除外），因此 `"/tmp/x/"` → `file:///tmp/x/`、`"."` → `file:///tmp`。
fn file_url(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let resolved = normalize_lexically(&joined);
    let mut href = Url::from_file_path(&resolved)
        .map(|url| url.to_string())
        .unwrap_or_default();
    let trailing_separator = raw.ends_with('/') || (cfg!(windows) && raw.ends_with('\\'));
    if trailing_separator && !href.ends_with('/') {
        href.push('/');
    }
    // url crate 的 file 路径集合比 Node 的 `pathToFileURL` 小：Node 还编码 `[ ] ^ | ~`。
    encode_file_url_extras(&href)
}

/// Node `pathToFileURL` 额外百分号编码的字符（`encodePathChars` + URL 序列化之外的部分）。
///
/// `%XX` 序列里只有十六进制数字，因此对已编码的 href 做字符替换不会破坏现有转义。
fn encode_file_url_extras(href: &str) -> String {
    let mut encoded = String::with_capacity(href.len());
    for ch in href.chars() {
        match ch {
            '[' => encoded.push_str("%5B"),
            ']' => encoded.push_str("%5D"),
            '^' => encoded.push_str("%5E"),
            '|' => encoded.push_str("%7C"),
            '~' => encoded.push_str("%7E"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// `basename(path)`（Node 的 `path.basename` 语义，不是 [`Path::file_name`]：`"."` → `"."`，
/// `"//"` → `""`，输入里的尾部斜杠被忽略而不改变最后一个分量）。
fn basename(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    let separators: &[char] = &['/', '\\'];
    #[cfg(not(windows))]
    let separators: &[char] = &['/'];
    let trimmed = raw.trim_end_matches(|character: char| separators.contains(&character));
    if trimmed.is_empty() {
        return String::new();
    }
    let name = match trimmed.rfind(|character: char| separators.contains(&character)) {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    };
    // `path.win32.basename("C:")` 是 `""`；盘符本身不是文件名。
    if cfg!(windows) && name.ends_with(':') {
        return String::new();
    }
    name.to_string()
}

/// `string.trim().slice(-STDERR_TAIL_CHARS) || undefined`（按 UTF-16 码元切）。
fn stderr_tail(text: &str) -> Option<String> {
    let trimmed = kiana_json::js_string::js_trim(text);
    if trimmed.is_empty() {
        return None;
    }
    let units: Vec<u16> = trimmed.encode_utf16().collect();
    let start = units.len().saturating_sub(STDERR_TAIL_CHARS);
    Some(String::from_utf16_lossy(&units[start..]))
}

/// 已创建、尚未被连接取走的 stdio 传输（`instanceof StdioTransport` 的等价物）。
///
/// 键是 trait object 的数据指针（`Arc::as_ptr`）。[`create_default_transport`] 创建
/// `StdioTransport` 时登记，`setup_client` 用工厂返回的同一个 `Arc` 取走并删除条目，因此表里
/// 只有“已创建、还未开始连接”的传输。自定义工厂创建的传输不会登记，那些连接失败时拿不到
/// stderr 尾部。锁内只做插入/删除，不跨 `await`。
fn stdio_registry() -> &'static Mutex<HashMap<usize, Arc<StdioTransport>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<StdioTransport>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn transport_key(transport: &Arc<dyn McpTransport>) -> usize {
    Arc::as_ptr(transport) as *const () as usize
}

fn take_registered_stdio(transport: &Arc<dyn McpTransport>) -> Option<Arc<StdioTransport>> {
    stdio_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&transport_key(transport))
}

/// start 时立刻失败的传输：`createDefaultTransport` 在 TS 里同步抛错，Rust 的工厂签名不能
/// 返回 `Result`，因此把错误推迟到 `start()`（两条路径最终都经 `connectFailed` 得到同一文案）。
struct FailingTransport {
    events: TransportEvents,
    error: McpTransportError,
}

impl FailingTransport {
    fn new(error: McpTransportError) -> Self {
        FailingTransport {
            events: TransportEvents::new(),
            error,
        }
    }
}

#[async_trait]
impl McpTransport for FailingTransport {
    async fn start(&self) -> Result<(), McpTransportError> {
        Err(self.error.clone())
    }

    async fn send(
        &self,
        _message: Value,
        _options: kiana_mcp::transports::McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        Err(self.error.clone())
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        Ok(())
    }

    fn on_message(
        &self,
        listener: kiana_mcp::transports::McpTransportMessageListener,
    ) -> Subscription {
        self.events.on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: kiana_mcp::transports::McpTransportErrorListener) -> Subscription {
        self.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: kiana_mcp::transports::McpTransportCloseListener) -> Subscription {
        self.events.on_close(move || listener())
    }
}

/// `Object.entries(value)`（JSON 值）：对象按键序（整数键在前）、数组按索引、字符串按
/// UTF-16 码元；其余值没有可枚举的自有属性。
fn js_object_entries(value: &Value) -> Vec<(String, Value)> {
    match value {
        Value::Object(map) => js_keys(map)
            .into_iter()
            .map(|key| {
                let value = map.get(&key).cloned().unwrap_or(Value::Null);
                (key, value)
            })
            .collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), value.clone()))
            .collect(),
        Value::String(text) => text
            .encode_utf16()
            .enumerate()
            .map(|(index, unit)| {
                (
                    index.to_string(),
                    Value::String(String::from_utf16_lossy(&[unit])),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `resolveHeadersOrThrow(headers, description)`，其中 `headers` 可能来自未校验的输入
/// （stdio 条目带 `url` 时，TS 照样把 `config.headers` 交给它）。
///
/// 每个值都先经 `resolve_config_value_or_throw`（`!` 引用、`${ENV}` 等）解析；非字符串值会
/// 复刻 TS 在 `config.startsWith` 处的 `TypeError`，以便被当作瞬时错误重试。解析后的映射为空
/// 时返回 `None`。错误由 [`create_default_transport`] 转成“`start()` 时报错”的传输。
fn resolve_headers_value(
    headers: Option<&Value>,
    description: &str,
) -> Result<Option<IndexMap<String, String>>, McpTransportError> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    // TS: `if (!headers) return undefined`。
    if !is_truthy(Some(headers)) {
        return Ok(None);
    }
    let mut resolved: IndexMap<String, String> = IndexMap::new();
    for (key, value) in js_object_entries(headers) {
        let Some(value) = value.as_str() else {
            // `resolveConfigValueOrThrow(nonString)` 在 `parseConfigValueReference` 里读
            // `config.startsWith`，V8 抛 TypeError（瞬时错误，连接会重试）。
            return Err(McpTransportError::network(
                "config.startsWith is not a function",
            ));
        };
        let resolved_value =
            resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""), None)
                .map_err(|error| McpTransportError::new(error.to_string()))?;
        resolved.insert(key, resolved_value);
    }
    if resolved.is_empty() {
        Ok(None)
    } else {
        Ok(Some(resolved))
    }
}

/// `new StreamableHttpTransport({ url, headers, authProvider })`。
///
/// 构造函数返回 `Err`（例如 URL 非法）时同样包成 `FailingTransport`，错误推迟到 `start()`；
/// 这样两条创建路径（HTTP 与 stdio）对上层都表现为“连接失败”，文案一致。
fn http_transport(
    url: String,
    headers: Option<IndexMap<String, String>>,
    auth_provider: Option<Arc<dyn AuthProvider>>,
) -> Arc<dyn McpTransport> {
    let options = StreamableHttpTransportOptions {
        url,
        headers: headers.map(|headers| headers.into_iter().collect()),
        auth_provider,
        ..StreamableHttpTransportOptions::default()
    };
    match StreamableHttpTransport::new(options) {
        Ok(transport) => Arc::new(transport),
        Err(error) => Arc::new(FailingTransport::new(error)),
    }
}

// ---------------------------------------------------------------------------
// 进程级默认 MCP 沙箱
// ---------------------------------------------------------------------------

/// 进程级默认 MCP 沙箱：宿主在启动阶段安装，供未显式注入 manager 的 MCP 扩展使用。
///
/// `builtin_extensions()` 用 `McpExtensionOptions::default()` 构造 MCP 扩展，拿不到会话
/// 运行时的 `SandboxManager`；`kiana-coding-agent` 在创建会话运行时之前按有效权限模式
/// 安装本默认值，stdio 服务器因此不再只依赖 `KIANA_SANDBOX_POLICY`。
///
/// 只写一次：进程内第一次安装生效，之后的调用被忽略。同一进程多次启动（SDK/测试）因此
/// 沿用第一次的 manager。
static DEFAULT_MCP_SANDBOX: OnceLock<Arc<SandboxManager>> = OnceLock::new();

/// 安装进程级默认 MCP 沙箱；只有第一次调用生效。
pub fn install_default_mcp_sandbox(manager: Arc<SandboxManager>) {
    let _ = DEFAULT_MCP_SANDBOX.set(manager);
}

/// 读取进程级默认 MCP 沙箱。
pub fn default_mcp_sandbox() -> Option<Arc<SandboxManager>> {
    DEFAULT_MCP_SANDBOX.get().cloned()
}

/// 解析默认传输使用的沙箱：显式 `options.sandbox` > 进程级默认 > `KIANA_SANDBOX_POLICY`。
///
/// `from_env` 只在两个显式来源都缺失时才调用：它可能探测 bubblewrap，且未设置
/// `KIANA_SANDBOX_POLICY` 时返回 `None`（保持无沙箱）。
fn resolve_default_mcp_sandbox(
    explicit: Option<Arc<SandboxManager>>,
    process_default: Option<Arc<SandboxManager>>,
    from_env: impl FnOnce() -> Option<Arc<SandboxManager>>,
) -> Option<Arc<SandboxManager>> {
    explicit.or(process_default).or_else(from_env)
}

/// `createDefaultTransport(entry, cwd, authProvider)`。
///
/// # 分支与失败模式
///
/// - `"url" in config`（含 stdio 条目上的 `url` 键，见 `config_has_url`）：按 HTTP 处理。
///   `headers` 先做配置值解析，失败时返回一条在 `start()` 立刻报错的传输；URL 本身非法同样由
///   `StreamableHttpTransport::new` 的 `Err` 转成这种传输。错误不会在这里丢失，只是推迟到
///   `start()`，以便统一走 `connect_failed`。
/// - 否则按 stdio：`env` 逐项解析（失败同上）；`command`、`args`、`cwd` 做 `~` 展开，cwd 相对
///   会话 cwd 做 `path.resolve` 语义的词法规范化；stderr 走 pipe，并以 `Arc` 指针为键登记进
///   本模块的 stdio 注册表，供连接失败时取 stderr 尾部。
pub fn create_default_transport(
    entry: &McpServerEntry,
    cwd: &Path,
    auth_provider: Option<Arc<dyn AuthProvider>>,
) -> Arc<dyn McpTransport> {
    // 默认工厂的沙箱由 create_default_transport_with_sandbox 解析：
    // 进程级默认（宿主按权限模式安装）优先，其次才是 `KIANA_SANDBOX_POLICY`。
    create_default_transport_with_sandbox(entry, cwd, auth_provider, None)
}

/// 带沙箱的 [`create_default_transport`]。
///
/// `sandbox` 只作用于 stdio 条目；HTTP 传输没有子进程，保持原样。`None` 时与
/// [`create_default_transport`] 完全一致：依次回退到进程级默认与
/// `KIANA_SANDBOX_POLICY`。
pub fn create_default_transport_with_sandbox(
    entry: &McpServerEntry,
    cwd: &Path,
    auth_provider: Option<Arc<dyn AuthProvider>>,
    sandbox: Option<Arc<SandboxManager>>,
) -> Arc<dyn McpTransport> {
    // TS: `if ("url" in config)`——stdio 条目带 `url` 时也走 HTTP（`new URL(config.url)`
    // 对 null/非字符串抛 "Invalid URL"，服务器因此 failed，而不是启动 command）。
    if config_has_url(&entry.config) {
        let config = config_to_value(&entry.config);
        let description = format!("MCP server \"{}\"", entry.name);
        let headers = match resolve_headers_value(config.get("headers"), &description) {
            Ok(headers) => headers,
            Err(error) => return Arc::new(FailingTransport::new(error)),
        };
        return http_transport(js_string(config.get("url")), headers, auth_provider);
    }
    let McpServerConfig::Stdio(config) = &entry.config else {
        // `Http` 的序列化结果一定有 `url` 键；这里不可达。
        return Arc::new(FailingTransport::new(McpTransportError::network(
            "Invalid URL",
        )));
    };
    let mut env: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for (key, value) in config.env.iter().flatten() {
        match resolve_config_value_or_throw(
            value,
            &format!("MCP server \"{}\" env \"{}\"", entry.name, key),
            None,
        ) {
            Ok(value) => {
                env.insert(key.clone(), value);
            }
            Err(error) => {
                return Arc::new(FailingTransport::new(McpTransportError::new(
                    error.to_string(),
                )));
            }
        }
    }
    let command = expand_home(&config.command);
    let args = config
        .args
        .as_ref()
        .map(|args| args.iter().map(|arg| expand_home(arg)).collect());
    let config_cwd = config.cwd.as_deref().unwrap_or(".");
    let resolved_cwd = resolve_from(cwd, &expand_home(config_cwd));
    // 优先级：显式参数 > 进程级默认（宿主按权限模式安装）> `KIANA_SANDBOX_POLICY`。
    // 后端不可用时 `SandboxManager::spawn` 返回 `Unavailable`，传输启动失败，绝不回退到
    // 无沙箱 spawn（见 `StdioTransportOptions::sandbox` 的文档）。
    let sandbox = resolve_default_mcp_sandbox(sandbox, default_mcp_sandbox(), || {
        SandboxManager::from_env(cwd).map(Arc::new)
    });
    let options = StdioTransportOptions {
        command,
        args,
        cwd: Some(resolved_cwd),
        env: if env.is_empty() { None } else { Some(env) },
        // 默认白名单：PATH/HOME/SHELL/LANG/LC_*/TMPDIR/代理 + 配置里的 env；剥离
        // *_KEY/*_TOKEN/*_SECRET/*_PASSWORD/*_CREDENTIAL*。
        env_policy: Some(StdioEnvPolicy::default()),
        stderr: Some(StdioStderrMode::Pipe),
        sandbox,
        ..StdioTransportOptions::new("")
    };
    let transport = Arc::new(StdioTransport::new(options));
    // TS 用 `transport instanceof StdioTransport` 取 stderr；Rust 的 trait object 不能
    // downcast，因此在创建时登记，连接取走传输后立刻查表。
    let key = Arc::as_ptr(&transport) as *const () as usize;
    stdio_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, Arc::clone(&transport));
    transport
}

/// 服务器没有实现 `resources/templates/list` 时没有模板。
///
/// 只把 JSON-RPC `-32601 METHOD_NOT_FOUND` 折叠成 `empty`；其它错误（包括传输层错误）原样
/// 返回，交由 `with_client` 的重试/登录逻辑处理。分页与合并两条路径都用它。
async fn without_templates<T, F, Fut>(list: F, empty: T) -> Result<T, McpTransportError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, McpTransportError>>,
{
    match list().await {
        Ok(value) => Ok(value),
        Err(error) => {
            let method_not_found = matches!(
                error.as_mcp(),
                Some(McpError::JsonRpc { code, .. })
                    if *code == JSON_RPC_ERROR_CODES::METHOD_NOT_FOUND
            );
            if method_not_found {
                Ok(empty)
            } else {
                Err(error)
            }
        }
    }
}

fn transport_ptr_eq(left: &Arc<McpClient>, right: &Arc<McpClient>) -> bool {
    Arc::ptr_eq(left, right)
}

/// 同一连接的全部可变状态（`Deref` 目标）。
///
/// 字段名与 TS 的公共字段一一对应；`&self` 方法需要内部可变性，因此用 `RwLock`/`AtomicBool`。
///
/// 锁只做短临界区：读方法在锁内复制/clone 后立即释放，写方法只替换值。所有锁在 poison
/// （持锁线程 panic）后都用 `into_inner()` 恢复，后续读写不会因此失败。`has_resources` 的
/// 所有读写都是 `SeqCst`。
pub struct ConnectionFields {
    /// TS: `state`。
    pub state: RwLock<ServerState>,
    /// TS: `error`。
    pub error: RwLock<Option<String>>,
    /// TS: `tools`。
    pub tools: RwLock<Vec<Tool>>,
    /// `prompts/list` 的结果（服务器没声明 prompts capability 时为空）。
    pub prompts: RwLock<Vec<Prompt>>,
    /// TS: `hasResources`。
    pub has_resources: AtomicBool,
    /// TS: `instructions`。
    pub instructions: RwLock<Option<String>>,
    /// TS: `challenge`。
    pub challenge: RwLock<Option<OAuthChallenge>>,
}

impl Default for ConnectionFields {
    fn default() -> Self {
        ConnectionFields {
            state: RwLock::new(ServerState::Connecting),
            error: RwLock::new(None),
            tools: RwLock::new(Vec::new()),
            prompts: RwLock::new(Vec::new()),
            has_resources: AtomicBool::new(false),
            instructions: RwLock::new(None),
            challenge: RwLock::new(None),
        }
    }
}

/// 读锁；被 poison（持锁线程 panic）时取回内部值继续使用，不向上传播 panic。
fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 写锁；被 poison 时同样继续使用。
fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 记忆化的连接 future（TS 的 `opening` promise）。
type ConnectFuture = Shared<BoxFuture<'static, Result<Arc<McpClient>, McpTransportError>>>;
/// 记忆化的关闭 future（TS 的 `closing` promise）。
type CloseFuture = Shared<BoxFuture<'static, ()>>;

/// 连接状态变化的回调（参数是连接句柄）。
///
/// `on_tools` 在连接成功（即使工具列表为空）与 `notifications/tools/list_changed` 刷新成功后
/// 调用；`on_change` 在 `open` 开始、连接成功、失败、传输断开、工具刷新与 `close()` 时调用
/// （`refresh_tools` 出错时也会调用）。回调在触发状态变化的那段异步代码里同步执行，可能来自
/// tokio 任务，不是专门的 UI 线程；句柄克隆共享同一份状态。
pub type McpConnectionCallback = Arc<dyn Fn(&McpServerConnection) + Send + Sync>;

/// 一个已连接（或正在连接）的 MCP 客户端。
///
/// 所有句柄克隆共享这一份数据：
///
/// - `client`：当前客户端；`std::sync::Mutex` 只做短拷贝/替换。
/// - `opening`：单飞连接 future；用 `tokio::sync::Mutex` 是因为要在 `await` 前后“查/放”。
/// - `shutdown`：`close()` 的取消信号；连接重试的 `select!` 与连接后的 abort 桥都监听它。
/// - `stderr_tail`：最近一次失败的 stdio stderr 尾部，供错误文案使用。
/// - `oauth_provider`：仅 OAuth 分支存在；`close()` 结束时等它的后台刷新落盘。
/// - `auth_provider`：交给传输的认证实现（OAuth 或 provider token 二选一）。
struct ConnectionInner {
    cwd: PathBuf,
    create_transport: McpTransportFactory,
    on_tools: McpConnectionCallback,
    on_change: Option<McpConnectionCallback>,
    log: Option<Arc<McpServerLog>>,
    client: Mutex<Option<Arc<McpClient>>>,
    opening: tokio::sync::Mutex<Option<ConnectFuture>>,
    shutdown: CancellationToken,
    stderr_tail: Mutex<Option<String>>,
    oauth_provider: Option<Arc<McpAuthProvider>>,
    auth_provider: Option<Arc<dyn AuthProvider>>,
}

/// 一个配置好的服务器。调用发现连接已断开时懒重连。
///
/// TS: `runtime.ts#McpServerConnection`。
///
/// 句柄只含 `entry` 快照与两个 `Arc`，克隆廉价且共享同一份状态。`entry` 是创建连接时的
/// 快照：配置文件之后的修改不会反映到这里，调用方（如 `index.rs`）要另行保存最新配置。
#[derive(Clone)]
pub struct McpServerConnection {
    /// 配置条目。
    pub entry: McpServerEntry,
    inner: Arc<ConnectionInner>,
    fields: Arc<ConnectionFields>,
}

impl Deref for McpServerConnection {
    type Target = ConnectionFields;

    fn deref(&self) -> &ConnectionFields {
        &self.fields
    }
}

/// [`McpServerConnection`] 的构造参数。TS: 构造函数的 options。
pub struct McpServerConnectionOptions {
    /// 配置条目。
    pub entry: McpServerEntry,
    /// 会话工作目录。
    pub cwd: PathBuf,
    /// 传输工厂。
    pub create_transport: McpTransportFactory,
    /// OAuth 凭据存储。
    pub credentials: Arc<McpOAuthCredentialStore>,
    /// 有 `auth.provider` 时读取 kiana provider 的当前 token。
    pub provider_token: Option<ProviderTokenFn>,
    /// 工具列表变化时调用（连接成功后至少一次，`tools/list_changed` 刷新成功后再调用）。
    pub on_tools: McpConnectionCallback,
    /// `state`/`error`/`tools` 变化时调用；`None` 表示不通知。
    pub on_change: Option<McpConnectionCallback>,
    /// 接收服务器的日志消息（`notifications/message`）。
    pub log: Option<McpServerLog>,
}

/// 只有 token 的 auth provider（`auth.provider`）：每次请求重读 provider 的 token。
///
/// 没有注册 [`ProviderTokenFn`]（`None`）时 `token()` 恒为 `None`，请求不带认证头；服务器若
/// 因此回 401，`needs_sign_in` 仍会把它转成登录指引（`auth_provider.is_some()`）。
struct ProviderTokenAuth {
    provider_token: Option<ProviderTokenFn>,
    provider: String,
}

#[async_trait]
impl AuthProvider for ProviderTokenAuth {
    async fn token(&self) -> Option<String> {
        match &self.provider_token {
            Some(provider_token) => provider_token(self.provider.clone()).await,
            None => None,
        }
    }
}

/// 服务器的 OAuth 设置（clientSecret 每次调用重新解析）。
///
/// TS: `"url" in config ? config.oauth : undefined`——stdio 条目带 `url` 时，`oauth` 也来自
/// 未校验的输入对象。
fn oauth_settings_for(entry: &McpServerEntry) -> Result<McpOAuthSettings, String> {
    let config = config_to_value(&entry.config);
    if !config.contains_key("url") {
        return Ok(McpOAuthSettings::default());
    }
    oauth_settings_from_value(&entry.name, config.get("oauth"))
}

/// `oauthSettings()` 的字段读取（`oauth` 已按 JS truthiness/属性语义处理）。
fn oauth_settings_from_value(
    name: &str,
    oauth: Option<&Value>,
) -> Result<McpOAuthSettings, String> {
    let Some(oauth) = oauth else {
        return Ok(McpOAuthSettings::default());
    };
    // TS: `if (!oauth) return {}`；truthy 的非对象值上所有字段都是 `undefined`。
    if !is_truthy(Some(oauth)) {
        return Ok(McpOAuthSettings::default());
    }
    let Some(oauth) = oauth.as_object() else {
        return Ok(McpOAuthSettings::default());
    };
    let client_secret = match oauth.get("clientSecret") {
        None => None,
        // TS 只跳过 `undefined`；`null`/非字符串会进 `resolveConfigValueOrThrow`，在
        // `config.startsWith` 抛 TypeError。
        Some(Value::String(secret)) => Some(
            resolve_config_value_or_throw(
                secret,
                &format!("MCP server \"{name}\" oauth.clientSecret"),
                None,
            )
            .map_err(|error| error.to_string())?,
        ),
        Some(_) => return Err("config.startsWith is not a function".to_string()),
    };
    // TS: `authServerMetadataUrl ? new URL(...) : undefined`。
    let auth_server_metadata_url = match oauth.get("authServerMetadataUrl") {
        Some(url) if is_truthy(Some(url)) => {
            Some(Url::parse(&js_string(Some(url))).map_err(|_| "Invalid URL".to_string())?)
        }
        _ => None,
    };
    // 其余字段只有类型匹配才生效：字符串字段用 `as_str`；`callbackPort` 用 `as_u64` 且必须
    // 能放进 `u16`；`clientRegistration` 只识别 "dcr"/"cimd"，其它值等价于未设置。
    Ok(McpOAuthSettings {
        client_id: oauth
            .get("clientId")
            .and_then(Value::as_str)
            .map(str::to_string),
        client_secret,
        callback_port: oauth
            .get("callbackPort")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok()),
        callback_url: oauth
            .get("callbackUrl")
            .and_then(Value::as_str)
            .map(str::to_string),
        scope: oauth
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string),
        client_name: oauth
            .get("clientName")
            .and_then(Value::as_str)
            .map(str::to_string),
        client_registration: match oauth.get("clientRegistration").and_then(Value::as_str) {
            Some("dcr") => Some(McpClientRegistration::Dcr),
            Some("cimd") => Some(McpClientRegistration::Cimd),
            _ => None,
        },
        auth_server_metadata_url,
    })
}

/// 服务器 URL（仅当它用 OAuth 时，按 JS truthiness 取值）。
///
/// 用于 [`McpServerConnection::new`] 决定是否创建 OAuth provider：stdio 条目要求 `url` 按 JS
/// truthiness 为真，因此 `{command, url: null}` 不会建 provider。公开的
/// [`McpServerConnection::oauth_url`] 用的是更严格的 `oauth_url_str`（只接受非空字符串）。
fn oauth_url_value(entry: &McpServerEntry) -> Option<String> {
    if !uses_oauth(entry) {
        return None;
    }
    match &entry.config {
        McpServerConfig::Http(config) => Some(config.url.clone()),
        McpServerConfig::Stdio(config) => config
            .extra
            .get("url")
            .filter(|url| is_truthy(Some(url)))
            .map(|url| js_string(Some(url))),
    }
}

/// `"url" in config ? config.auth?.provider : undefined`，按 JS truthiness 取字符串。
fn config_provider(entry: &McpServerEntry) -> Option<String> {
    let config = config_to_value(&entry.config);
    if !config.contains_key("url") {
        return None;
    }
    js_property(config.get("auth"), "provider")
        .filter(|provider| is_truthy(Some(provider)))
        .map(|provider| js_string(Some(provider)))
}

/// 服务器 URL 的 `&str` 视图；TS 的 `oauthUrl` 是原始值（可能是 `null`/数字），
/// 调用方只关心 truthiness 时非字符串值与 `None` 等价。
///
/// HTTP 条目直接返回配置里的 URL；stdio 条目只接受非空字符串。`index.rs` 的凭据快照
/// （`stored_tokens`）用它作为存储键的一部分。
fn oauth_url_str(entry: &McpServerEntry) -> Option<&str> {
    if !uses_oauth(entry) {
        return None;
    }
    match &entry.config {
        McpServerConfig::Http(config) => Some(&config.url),
        McpServerConfig::Stdio(config) => config
            .extra
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty()),
    }
}

impl McpServerConnection {
    /// `new McpServerConnection(options)`。
    ///
    /// 只构造对象，不做网络/进程 IO：状态初始为 [`ServerState::Connecting`]，真正连接推迟到
    /// 第一次 `get_client()`/`call_tool()`。认证方式在构造时确定：
    ///
    /// - OAuth 分支：`uses_oauth` 成立（带 `url`、`auth` 不为真、headers 无 `Authorization`）
    ///   且 `url` 按 JS truthiness 有值 → 创建 OAuth provider（凭据按 `name` + `url` 定位），
    ///   收到的 OAuth 挑战写进 `challenge`；
    /// - 否则若带 `url` 键且 `auth.provider` 为真 → 使用 [`ProviderTokenFn`]（每次请求取 token）；
    /// - 其余（含普通 stdio 条目）没有 auth provider。
    pub fn new(options: McpServerConnectionOptions) -> Self {
        let fields = Arc::new(ConnectionFields::default());
        let url = oauth_url_value(&options.entry);
        let provider = config_provider(&options.entry);
        let (oauth_provider, auth_provider) = match (&url, provider) {
            (Some(url), _) => {
                let challenge_fields = Arc::clone(&fields);
                let settings_entry = options.entry.clone();
                let settings: Arc<dyn Fn() -> Result<McpOAuthSettings, String> + Send + Sync> =
                    Arc::new(move || oauth_settings_for(&settings_entry));
                let auth = Arc::new(create_mcp_auth_provider(McpAuthProviderOptions {
                    server_url: url.clone(),
                    store: options.credentials.for_server(&options.entry.name, url),
                    settings,
                    on_challenge: Arc::new(move |challenge| {
                        *write_lock(&challenge_fields.challenge) = Some(challenge);
                    }),
                }));
                let transport_auth: Arc<dyn AuthProvider> =
                    Arc::clone(&auth) as Arc<dyn AuthProvider>;
                (Some(auth), Some(transport_auth))
            }
            (None, Some(provider)) => {
                let auth: Arc<dyn AuthProvider> = Arc::new(ProviderTokenAuth {
                    provider_token: options.provider_token.clone(),
                    provider,
                });
                (None, Some(auth))
            }
            (None, None) => (None, None),
        };
        McpServerConnection {
            entry: options.entry,
            inner: Arc::new(ConnectionInner {
                cwd: options.cwd,
                create_transport: options.create_transport,
                on_tools: options.on_tools,
                on_change: options.on_change,
                log: options.log.map(Arc::new),
                client: Mutex::new(None),
                opening: tokio::sync::Mutex::new(None),
                shutdown: CancellationToken::new(),
                stderr_tail: Mutex::new(None),
                oauth_provider,
                auth_provider,
            }),
            fields,
        }
    }

    /// `entry.name`。
    pub fn name(&self) -> &str {
        &self.entry.name
    }

    /// 当前状态（快照）。
    pub fn state(&self) -> ServerState {
        *read_lock(&self.fields.state)
    }

    /// 当前错误（快照）。
    pub fn error(&self) -> Option<String> {
        read_lock(&self.fields.error).clone()
    }

    /// 当前工具列表（快照）。
    pub fn tools(&self) -> Vec<Tool> {
        read_lock(&self.fields.tools).clone()
    }

    /// `prompts/list` 的结果快照。
    pub fn prompts(&self) -> Vec<Prompt> {
        read_lock(&self.fields.prompts).clone()
    }

    /// 当前服务器状态的结构化诊断快照。
    ///
    /// `enabled` 来自配置（服务器被禁用时即使有连接也显示 `disabled`）。`disconnected`
    /// 会在下次调用时自动重连，诊断面把它归到 `connecting`；`closed` 没有独立状态，
    /// 归到 `disabled`。
    pub fn status(&self, enabled: bool) -> McpServerStatus {
        let state = if !enabled {
            McpServerStatusState::Disabled
        } else {
            match self.state() {
                ServerState::Connecting | ServerState::Disconnected => {
                    McpServerStatusState::Connecting
                }
                ServerState::Connected => McpServerStatusState::Connected,
                ServerState::NeedsAuth => McpServerStatusState::NeedsAuth,
                ServerState::Failed => McpServerStatusState::Failed,
                ServerState::Closed => McpServerStatusState::Disabled,
            }
        };
        McpServerStatus {
            state,
            last_error: self.error(),
            tool_count: self.tools().len(),
            prompt_count: self.prompts().len(),
        }
    }

    /// `prompts/get`：参数原样下发；只读请求复用重试/登录处理。
    pub async fn get_prompt(
        &self,
        name: &str,
        args: Option<serde_json::Map<String, Value>>,
    ) -> Result<GetPromptResult, McpTransportError> {
        let name = name.to_string();
        self.with_client(
            move |client| {
                let name = name.clone();
                let args = args.clone();
                async move {
                    client
                        .get_prompt(&name, args, &McpRequestOptions::new())
                        .await
                }
            },
            true,
        )
        .await
    }

    /// 是否提供资源（`hasResources` 的读法）。
    pub fn has_resources(&self) -> bool {
        self.fields.has_resources.load(Ordering::SeqCst)
    }

    /// 服务器 instructions（`initialize` 返回的，已 trim）。
    pub fn instructions(&self) -> Option<String> {
        read_lock(&self.fields.instructions).clone()
    }

    /// 最近一次 OAuth 挑战（快照）。
    pub fn challenge(&self) -> Option<OAuthChallenge> {
        read_lock(&self.fields.challenge).clone()
    }

    fn closed(&self) -> bool {
        self.inner.shutdown.is_cancelled()
    }

    /// 每次请求的超时（毫秒）。
    ///
    /// 配置里的 `timeout` 单位是秒（缺省 60，见 `McpServerConfigBase::timeout`），这里换算后
    /// 传给 `McpClientOptions::request_timeout_ms`。
    pub fn timeout_ms(&self) -> f64 {
        self.entry
            .config
            .base()
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
            * 1000.0
    }

    /// 服务器用 OAuth 时的 URL。
    ///
    /// 非 OAuth 服务器返回 `None`；stdio 条目的 `url` 必须是字符串且非空（HTTP 条目直接取
    /// 配置里的 URL）。`index.rs` 的凭据快照（`stored_tokens`）把它作为查询令牌时的服务器 URL。
    pub fn oauth_url(&self) -> Option<&str> {
        oauth_url_str(&self.entry)
    }

    /// OAuth 设置（`clientSecret` 惰性解析，失败只影响刷新/登录）。
    ///
    /// 每次调用都从 `entry` 的配置值重新解析（`!` 引用/环境变量会重新执行），错误是 TS 的
    /// `error.message` 文本（例如 `config.startsWith is not a function`）。
    pub fn oauth_settings(&self) -> Result<McpOAuthSettings, String> {
        oauth_settings_for(&self.entry)
    }

    fn set_state(&self, state: ServerState) {
        *write_lock(&self.fields.state) = state;
    }

    fn set_error(&self, error: Option<String>) {
        *write_lock(&self.fields.error) = error;
    }

    fn set_tools(&self, tools: Vec<Tool>) {
        *write_lock(&self.fields.tools) = tools;
    }

    fn set_prompts(&self, prompts: Vec<Prompt>) {
        *write_lock(&self.fields.prompts) = prompts;
    }

    fn set_instructions(&self, instructions: Option<String>) {
        *write_lock(&self.fields.instructions) = instructions;
    }

    fn set_stderr_tail(&self, tail: Option<String>) {
        *self
            .inner
            .stderr_tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = tail;
    }

    fn stderr_tail(&self) -> Option<String> {
        self.inner
            .stderr_tail
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn current_client(&self) -> Option<Arc<McpClient>> {
        self.inner
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_current_client(&self, client: Option<Arc<McpClient>>) {
        *self
            .inner
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = client;
    }

    fn is_current_client(&self, client: &Arc<McpClient>) -> bool {
        self.current_client()
            .is_some_and(|current| transport_ptr_eq(&current, client))
    }

    /// 取得一个已连接的客户端；必要时建立连接。
    ///
    /// 快速路径：当前客户端存在且 `McpClient` 自身状态为 `Connected` 时直接返回克隆（克隆共享
    /// 同一个内部句柄）。否则走单飞建连，并发的调用者等待同一个 future（见 `get_client_arc`）。
    /// 关闭后调用立即返回 `MCP server "<name>" is shut down`；连接失败的错误已经
    /// `connect_failed` 包装成 `MCP server "<name>" failed to connect: ...`，可直接展示给用户。
    pub async fn get_client(&self) -> Result<McpClient, McpTransportError> {
        self.get_client_arc().await.map(|client| (*client).clone())
    }

    /// 与 [`Self::get_client`] 相同，但返回共享句柄（内部用来比较对象身份）。
    ///
    /// 这是本文件的并发核心：`opening` 槽保存一个 `Shared` future，保证同一时刻只有一次
    /// `open()`。拿到结果后槽被清空，因此下一次调用（例如连接已断开）会重新建连。
    async fn get_client_arc(&self) -> Result<Arc<McpClient>, McpTransportError> {
        if self.closed() {
            return Err(McpTransportError::new(format!(
                "MCP server \"{}\" is shut down",
                self.entry.name
            )));
        }
        // 快路径：连接仍然可用时直接复用，不碰 opening 锁。
        if let Some(client) = self.current_client()
            && client.connection_state() == ConnectionState::Connected
        {
            return Ok(client);
        }
        // 单飞：已有进行中的连接时等待同一个 future，而不是再开一次。
        let mut guard = self.inner.opening.lock().await;
        if let Some(flight) = guard.clone() {
            drop(guard);
            return flight.await;
        }
        // 新建 flight；future 自己负责在结束（成功或失败）时清空槽，让后续调用可以重试。
        let this = self.clone();
        let slot = Arc::clone(&self.inner);
        let flight: ConnectFuture = async move {
            let result = this.open().await;
            *slot.opening.lock().await = None;
            result
        }
        .boxed()
        .shared();
        *guard = Some(flight.clone());
        drop(guard);
        // TS 的 promise 创建后立即开始执行；Rust 的 future 惰性，因此由后台任务驱动。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let driver = flight.clone();
            handle.spawn(async move {
                let _ = driver.await;
            });
        }
        flight.await
    }

    /// 用新凭据重新连接（例如登录之后）。
    ///
    /// 先等待进行中的 `open()` 结束（忽略其结果），再关闭当前客户端（若存在），然后重新连接；
    /// 返回的错误与 [`Self::get_client`] 相同。登录成功后由 `index.rs`/`cli.rs` 调用。
    pub async fn reconnect(&self) -> Result<(), McpTransportError> {
        let opening = self.inner.opening.lock().await.clone();
        if let Some(opening) = opening {
            let _ = opening.await;
        }
        if let Some(client) = self.current_client() {
            self.drop_client(&client).await;
        }
        self.get_client().await.map(|_| ())
    }

    /// 存储的凭据被删除后断开连接。
    ///
    /// 等待进行中的连接、关闭当前客户端，然后（若连接尚未关闭）置为 `needs-auth` 并清空
    /// `error`。它不删除凭据本身，存储的清理由调用方完成。
    pub async fn sign_out(&self) {
        let opening = self.inner.opening.lock().await.clone();
        if let Some(opening) = opening {
            let _ = opening.await;
        }
        if let Some(client) = self.current_client() {
            self.drop_client(&client).await;
        }
        if !self.closed() {
            self.mark_needs_auth();
        }
    }

    /// OAuth 服务器在刷新后仍拒绝请求时需要用户重新登录。
    fn needs_sign_in(&self, error: &McpTransportError) -> bool {
        error.is_oauth_authorization_required()
            || (self.inner.auth_provider.is_some() && error.is_auth_required())
    }

    fn mark_needs_auth(&self) {
        self.set_state(ServerState::NeedsAuth);
        self.set_error(None);
        self.changed();
    }

    fn changed(&self) {
        if let Some(on_change) = &self.inner.on_change {
            on_change(self);
        }
    }

    async fn drop_client(&self, client: &Arc<McpClient>) {
        if self.is_current_client(client) {
            self.set_current_client(None);
        }
        let _ = client.close().await;
    }

    /// 运行一个请求，必要时重连。只读请求在瞬时 HTTP 错误后重试一次；工具调用不重试
    /// （它们可能已经执行）。
    ///
    /// 两类例外对工具调用同样生效：会话过期（404，请求未执行）会重连重跑一次；需要登录的错误
    /// 会置 `needs-auth` 并返回登录指引。重试时 `run` 用下一轮 `get_client_arc` 取到的客户端执行。
    async fn with_client<T, F, Fut>(&self, run: F, read_only: bool) -> Result<T, McpTransportError>
    where
        F: Fn(McpClient) -> Fut,
        Fut: Future<Output = Result<T, McpTransportError>>,
    {
        let mut attempt = 1;
        loop {
            // 每一轮都重新取客户端：上一轮可能已经解除关联或重连。
            let client = self.get_client_arc().await?;
            match run((*client).clone()).await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    // 只读请求（resources/*）遇到瞬时 HTTP 错误重试一次；工具调用不重试——
                    // 请求可能已经在服务器上执行。这里不关闭连接：网络抖动不等于会话失效，
                    // 下一轮 get_client 通常会复用同一个客户端。
                    if read_only
                        && attempt == 1
                        && error.status().is_some()
                        && is_transient_error(&error)
                    {
                        tokio::time::sleep(Duration::from_millis(CONNECT_RETRY_DELAYS_MS[0])).await;
                        attempt += 1;
                        continue;
                    }
                    if error.is_session_expired() && attempt == 1 {
                        // 服务器不再认识这个会话（重启、部署），请求没有执行。换新会话重试一次。
                        // 旧客户端只解除关联、不关闭：关闭会让其它在途调用失败，它们同样会重试。
                        if self.is_current_client(&client) {
                            self.set_current_client(None);
                        }
                        attempt += 1;
                        continue;
                    }
                    // 其余 401/授权错误：OAuth 服务器或配置了 auth 的服务器转为 needs-auth，
                    // 返回的文案指向 /login <provider> 或 /mcp；没有 auth provider 的 401 原样返回。
                    if !self.needs_sign_in(&error) {
                        return Err(error);
                    }
                    self.drop_client(&client).await;
                    self.mark_needs_auth();
                    return Err(McpTransportError::new(sign_in_required_message(
                        &self.entry,
                    )));
                }
            }
        }
    }

    /// 建立连接（单飞 future 的主体）。按配置决定连接级重试次数，失败统一走 `connect_failed`。
    async fn open(&self) -> Result<Arc<McpClient>, McpTransportError> {
        // 每轮打开（含重连）都先回 connecting，让 on_change/UI 立刻看到状态变化。
        self.set_state(ServerState::Connecting);
        self.changed();
        // TS: `"url" in config ? CONNECT_RETRY_DELAYS_MS : []`（stdio 条目带 `url` 时也重试）。
        let retries: &[u64] = if config_has_url(&self.entry.config) {
            &CONNECT_RETRY_DELAYS_MS
        } else {
            &[]
        };
        let mut attempt = 0usize;
        loop {
            // 清掉上一轮的 stderr 尾部，避免失败文案混入旧输出。
            self.set_stderr_tail(None);
            match self.connect_once().await {
                Ok(client) => return Ok(client),
                Err(error) => {
                    let delay = retries.get(attempt).copied();
                    let Some(delay) = delay else {
                        return Err(self.connect_failed(error));
                    };
                    // 重试只对瞬时错误；close() 后立即停止。
                    if self.closed() || !is_transient_error(&error) {
                        return Err(self.connect_failed(error));
                    }
                    // 等待期间响应 shutdown：close() 不必等满重试间隔。
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(delay)) => {}
                        () = self.inner.shutdown.cancelled() => {}
                    }
                    if self.closed() {
                        return Err(self.connect_failed(error));
                    }
                }
            }
            attempt += 1;
        }
    }

    /// 一次连接尝试：新建客户端 → 挂监听 → `setup_client`；失败时关闭客户端。
    async fn connect_once(&self) -> Result<Arc<McpClient>, McpTransportError> {
        // TS 直接用构造函数收到的 `this.cwd`：`pathToFileURL(this.cwd).href` 与
        // `basename(this.cwd)`，不做词法规范化（`"."` 的名字仍是 `"."`）。
        let roots = Roots::List(vec![Root {
            uri: file_url(&self.inner.cwd),
            name: Some(basename(&self.inner.cwd)),
            extra: serde_json::Map::new(),
        }]);
        // 每次尝试都新建客户端（重连不复用旧客户端）：McpClient 从 Idle 才能 connect。
        let client = Arc::new(McpClient::new(McpClientOptions {
            name: "kiana".to_string(),
            version: VERSION.to_string(),
            request_timeout_ms: Some(self.timeout_ms()),
            roots: Some(roots),
            ..McpClientOptions::default()
        }));
        // 服务器日志通知直接追加到 mcp.log；没有 log 时忽略。
        if let Some(log) = &self.inner.log {
            let log = Arc::clone(log);
            let name = self.entry.name.clone();
            client.on_notification(
                "notifications/message",
                Arc::new(move |params: Option<&Value>| {
                    log.write(&name, params);
                    Ok(())
                }),
            );
        }
        // 关闭桥：连接创建后立刻挂上 shutdown。`close()` 可能在 initialize 期间到达，而本次
        // 尝试的客户端尚未登记为 current_client，只能靠这条任务关闭它。
        let closing: Arc<tokio::sync::Mutex<Option<CloseFuture>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let done = CancellationToken::new();
        let abort_task = {
            let client = Arc::clone(&client);
            let closing = Arc::clone(&closing);
            let shutdown = self.inner.shutdown.clone();
            let done = done.clone();
            tokio::spawn(async move {
                // `AbortSignal` 的语义：在已经 abort 的信号上添加的 listener 不会被调用。
                if shutdown.is_cancelled() {
                    return;
                }
                tokio::select! {
                    () = shutdown.cancelled() => close_client_once(&client, &closing).await,
                    () = done.cancelled() => {}
                }
            })
        };
        let result = self.setup_client(&client).await;
        // setup 结束后取消 done，成功路径上不会因为后续 close() 再关一次（close 只关
        // current_client）；失败路径则需要显式关闭本次尝试的客户端。
        done.cancel();
        let _ = abort_task.await;
        match result {
            Ok(()) => Ok(client),
            Err(error) => {
                close_client_once(&client, &closing).await;
                Err(error)
            }
        }
    }

    /// `connectOnce` 里 `try { ... }` 的部分：调用传输工厂并在失败时抓取 stderr 尾部。
    async fn setup_client(&self, client: &Arc<McpClient>) -> Result<(), McpTransportError> {
        // 工厂每轮尝试调用一次；stdio 传输若来自本模块的注册表，在此 take 出来。
        let transport = (self.inner.create_transport)(
            &self.entry,
            &self.inner.cwd,
            self.inner.auth_provider.clone(),
        );
        let stdio = take_registered_stdio(&transport);
        let result = self
            .setup_connected_client(client, &transport, stdio.as_ref())
            .await;
        if result.is_err()
            && let Some(stdio) = &stdio
        {
            // TS 在 catch 里对任何失败（initialize、tools/list、setup 检查）都抓 stderr 尾部。
            self.set_stderr_tail(stderr_tail(&stdio.stderr()));
        }
        result
    }

    /// 连接之后的初始化与状态记录。
    ///
    /// 顺序：`connect`（启动传输 + initialize 握手）→ 注册工具变更/关闭监听 → 按 capabilities
    /// 决定是否 `tools/list` → 再次确认连接仍有效 → 一次性发布 tools/has_resources/
    /// instructions/state → 通知回调。
    async fn setup_connected_client(
        &self,
        client: &Arc<McpClient>,
        transport: &Arc<dyn McpTransport>,
        stdio: Option<&Arc<StdioTransport>>,
    ) -> Result<(), McpTransportError> {
        // 1) 启动传输并完成 initialize 握手；失败（传输、协议、超时）由调用方关闭 client。
        client.connect(Arc::clone(transport)).await?;
        // 2) 工具/prompt 列表变化通知：异步刷新，不阻塞通知分发。
        let connection = self.clone();
        let tools_client = Arc::clone(client);
        client.on_notification(
            "notifications/tools/list_changed",
            Arc::new(move |_params: Option<&Value>| {
                let connection = connection.clone();
                let client = Arc::clone(&tools_client);
                tokio::spawn(async move {
                    connection.refresh_tools(client).await;
                });
                Ok(())
            }),
        );
        let connection = self.clone();
        let prompts_client = Arc::clone(client);
        client.on_notification(
            "notifications/prompts/list_changed",
            Arc::new(move |_params: Option<&Value>| {
                let connection = connection.clone();
                let client = Arc::clone(&prompts_client);
                tokio::spawn(async move {
                    connection.refresh_prompts(client).await;
                });
                Ok(())
            }),
        );
        // 3) 传输关闭：只处理“当前客户端且未关闭”的情况，见 handle_client_close。
        let connection = self.clone();
        let close_client = Arc::clone(client);
        let close_stdio = stdio.cloned();
        client.on_close(Arc::new(move || {
            connection.handle_client_close(&close_client, close_stdio.clone());
            Ok(())
        }));
        // 没有 tools capability 的服务器（只有 prompts 或 resources）不回答 tools/list。
        let capabilities = client.server_capabilities();
        let has_tools = capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.has_tools());
        let tools = if has_tools {
            client.list_tools(&McpRequestOptions::new()).await?
        } else {
            Vec::new()
        };
        // prompts capability 按类型化字段判断：`prompts: null`（旧服务器/测试夹具常见）
        // 不触发 `prompts/list`，避免对未声明该能力的服务器发请求。
        let has_prompts = capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.prompts.is_some());
        let prompts = if has_prompts {
            client.list_prompts(&McpRequestOptions::new()).await?
        } else {
            Vec::new()
        };
        // tools/list / prompts/list 期间连接可能被 close() 或服务器断开：写状态前再确认一次，避免把已失效的
        // 连接标成 connected。
        if self.closed() {
            return Err(McpTransportError::new("shut down while connecting"));
        }
        if client.connection_state() != ConnectionState::Connected {
            return Err(McpTransportError::new("connection closed during setup"));
        }
        // 4) 先登记 current_client（后续请求才能复用），再发布其余字段。
        self.set_current_client(Some(Arc::clone(client)));
        self.set_tools(tools);
        self.set_prompts(prompts);
        // TS: `client.serverCapabilities?.resources !== undefined` —— **存在性**，不是真值：
        // `resources: null`/`false`/`[]` 也算有 resources。宽松解析把非对象值（含显式 `null`，
        // 由 `patch_nulls` 补回）留在 `extra` 里，所以两处都要看。
        let has_resources = capabilities.as_ref().is_some_and(|capabilities| {
            capabilities.resources.is_some() || capabilities.extra.contains_key("resources")
        });
        self.fields
            .has_resources
            .store(has_resources, Ordering::SeqCst);
        self.set_instructions(
            client
                .instructions()
                .map(|instructions| kiana_json::js_string::js_trim(&instructions).to_string())
                .filter(|instructions| !instructions.is_empty()),
        );
        self.set_state(ServerState::Connected);
        self.set_error(None);
        (self.inner.on_tools)(self);
        self.changed();
        Ok(())
    }

    /// 把连接错误写成状态与文案，并返回包装后的错误。
    ///
    /// 两种分支：需要登录时置 `needs-auth` 并返回登录指引（返回的文案不含 stderr 尾部）；
    /// 否则关闭期间记 `closed`、其余记 `failed`，`error` 保存 `error.message` 加 stderr 尾部。
    fn connect_failed(&self, error: McpTransportError) -> McpTransportError {
        if self.needs_sign_in(&error) && !self.closed() {
            self.mark_needs_auth();
            return McpTransportError::new(sign_in_required_message(&self.entry));
        }
        self.set_state(if self.closed() {
            ServerState::Closed
        } else {
            ServerState::Failed
        });
        let message = match self.stderr_tail() {
            Some(tail) => format!("{}\n{}", error_message(&error), tail),
            None => error_message(&error),
        };
        self.set_error(Some(message.clone()));
        self.changed();
        McpTransportError::new(format!(
            "MCP server \"{}\" failed to connect: {message}",
            self.entry.name
        ))
    }

    /// 传输断开。下次调用会重连；在那之前状态显示原因。
    ///
    /// 只处理当前客户端且连接未关闭的事件：旧客户端或 `close()` 触发的关闭事件不能覆盖新状态。
    /// 错误文案固定为 `Connection closed`，stdio 服务器再附加 stderr 尾部。
    fn handle_client_close(&self, client: &Arc<McpClient>, stdio: Option<Arc<StdioTransport>>) {
        if !self.is_current_client(client) || self.closed() {
            return;
        }
        self.set_current_client(None);
        self.set_state(ServerState::Disconnected);
        let stderr = stdio.and_then(|stdio| stderr_tail(&stdio.stderr()));
        self.set_error(Some(match stderr {
            Some(stderr) => format!("Connection closed\n{stderr}"),
            None => "Connection closed".to_string(),
        }));
        self.changed();
    }

    /// `notifications/tools/list_changed` 的处理：重新 `tools/list`。
    ///
    /// 成功时先校验客户端仍是当前客户端且连接未关闭，再替换工具列表并触发 `on_tools`；
    /// 失败只写入 `error`（`Failed to refresh tools: ...`），不改变 state，也不关闭连接。
    /// 请求结束后（除非成功结果因连接已切换而提前返回）调用一次 `changed()`，让 UI 有机会展示。
    async fn refresh_tools(&self, client: Arc<McpClient>) {
        match client.list_tools(&McpRequestOptions::new()).await {
            Ok(tools) => {
                // 请求期间可能已重连/关闭：旧连接的列表不能覆盖新状态。
                if !self.is_current_client(&client) || self.closed() {
                    return;
                }
                self.set_tools(tools);
                (self.inner.on_tools)(self);
            }
            Err(error) => {
                self.set_error(Some(format!("Failed to refresh tools: {error}")));
            }
        }
        self.changed();
    }

    /// `notifications/prompts/list_changed` 的处理：重新 `prompts/list`。
    ///
    /// 与 [`Self::refresh_tools`] 相同：失败只写 `error`（`Failed to refresh prompts: ...`）
    /// 不改 state；成功后复用 `on_tools` 回调，让扩展重新注册 prompt 命令（命令按名字覆盖，
    /// 已删除的 prompt 由命令处理器在调用时判定“不再可用”）。
    async fn refresh_prompts(&self, client: Arc<McpClient>) {
        match client.list_prompts(&McpRequestOptions::new()).await {
            Ok(prompts) => {
                if !self.is_current_client(&client) || self.closed() {
                    return;
                }
                self.set_prompts(prompts);
                (self.inner.on_tools)(self);
            }
            Err(error) => {
                self.set_error(Some(format!("Failed to refresh prompts: {error}")));
            }
        }
        self.changed();
    }

    /// 等这个服务器的所有传输都关闭（包括仍在连接的那个）。
    ///
    /// 步骤：取消 `shutdown`（正在 sleep 的重试与打开中的传输都会看见）→ 立即置 `closed` 并
    /// 通知 → 取出当前客户端并发起 close，同时等待进行中的 `open()`（两者并发，不让关闭被
    /// 建连拖住）→ 最后等 OAuth provider 的后台刷新落盘。可重复调用；关闭后 `get_client`
    /// 一律失败。
    pub async fn close(&self) {
        self.inner.shutdown.cancel();
        self.set_state(ServerState::Closed);
        self.changed();
        let client = self.current_client();
        self.set_current_client(None);
        let opening = self.inner.opening.lock().await.clone();
        let client_close = async {
            if let Some(client) = client {
                let _ = client.close().await;
            }
        };
        let opening_wait = async {
            if let Some(opening) = opening {
                let _ = opening.await;
            }
        };
        futures::join!(client_close, opening_wait);
        // 服务器已经答复的刷新可能轮换了 refresh token；不等保存完就退出会丢 grant。
        if let Some(provider) = &self.inner.oauth_provider {
            provider.settled().await;
        }
    }
}

/// 记忆化的 `client.close()`：同一连接尝试里 setup 失败与 shutdown 桥都可能调用它，`Shared`
/// future 保证只真正执行一次 close，后到的调用等待同一个 future 完成（不是提前返回）。
///
/// close 的错误被忽略（`let _ =`），因此关闭失败不会阻止连接收尾。
async fn close_client_once(
    client: &Arc<McpClient>,
    slot: &tokio::sync::Mutex<Option<CloseFuture>>,
) {
    let flight = {
        let mut guard = slot.lock().await;
        match guard.clone() {
            Some(flight) => flight,
            None => {
                let client = Arc::clone(client);
                let flight: CloseFuture = async move {
                    let _ = client.close().await;
                }
                .boxed()
                .shared();
                *guard = Some(flight.clone());
                flight
            }
        }
    };
    flight.await;
}

// `tools/call` 走 read_only = false：瞬时 HTTP 错误不重试（请求可能已经执行），但会话过期
// 仍会重连重跑一次（HTTP 404 表示服务器已不认识该会话）。
#[async_trait]
impl McpToolCaller for McpServerConnection {
    async fn call_tool(
        &self,
        name: &str,
        args: Value,
        options: &McpRequestOptions,
    ) -> Result<CallToolResult, McpTransportError> {
        // 只把 JSON 对象作为 arguments 传给服务器；非对象值等价于省略 arguments。
        let arguments = args.as_object().cloned();
        let name = name.to_string();
        let options = options.clone();
        self.with_client(
            move |client| {
                let name = name.clone();
                let arguments = arguments.clone();
                let options = options.clone();
                async move { client.call_tool(&name, arguments, &options).await }
            },
            false,
        )
        .await
    }
}

// `resources/*` 都是只读请求：with_client(..., true) 允许首次瞬时 HTTP 错误后重试一次；
// `resources/templates/list` 额外把服务器的 METHOD_NOT_FOUND 折叠成空结果（见 `without_templates`）。
#[async_trait]
impl McpResourceServer for McpServerConnection {
    fn name(&self) -> &str {
        McpServerConnection::name(self)
    }

    fn timeout_ms(&self) -> f64 {
        McpServerConnection::timeout_ms(self)
    }

    async fn resources_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> Result<ListResourcesResult, McpTransportError> {
        let cursor = cursor.map(str::to_string);
        let options = options.clone();
        self.with_client(
            move |client| {
                let cursor = cursor.clone();
                let options = options.clone();
                async move {
                    client
                        .list_resources_page(cursor.as_deref(), &options)
                        .await
                }
            },
            true,
        )
        .await
    }

    async fn resource_templates_page(
        &self,
        cursor: Option<&str>,
        options: &McpRequestOptions,
    ) -> Result<ListResourceTemplatesResult, McpTransportError> {
        let cursor = cursor.map(str::to_string);
        let options = options.clone();
        self.with_client(
            move |client| {
                let cursor = cursor.clone();
                let options = options.clone();
                async move {
                    without_templates(
                        || client.list_resource_templates_page(cursor.as_deref(), &options),
                        ListResourceTemplatesResult::default(),
                    )
                    .await
                }
            },
            true,
        )
        .await
    }

    async fn all_resources(
        &self,
        options: &McpRequestOptions,
    ) -> Result<Vec<Resource>, McpTransportError> {
        let options = options.clone();
        self.with_client(
            move |client| {
                let options = options.clone();
                async move { client.list_resources(&options).await }
            },
            true,
        )
        .await
    }

    async fn all_resource_templates(
        &self,
        options: &McpRequestOptions,
    ) -> Result<Vec<ResourceTemplate>, McpTransportError> {
        let options = options.clone();
        self.with_client(
            move |client| {
                let options = options.clone();
                async move {
                    without_templates(|| client.list_resource_templates(&options), Vec::new()).await
                }
            },
            true,
        )
        .await
    }

    async fn read_resource(
        &self,
        uri: &str,
        options: &McpRequestOptions,
    ) -> Result<ReadResourceResult, McpTransportError> {
        let uri = uri.to_string();
        let options = options.clone();
        self.with_client(
            move |client| {
                let uri = uri.clone();
                let options = options.clone();
                async move { client.read_resource(&uri, &options).await }
            },
            true,
        )
        .await
    }
}

/// `runtime.lazy.ts#loadMcpRuntime` 的等价物：本模块已经静态链接，直接返回。
///
/// 与 TS 的 lazy 模块入口同名，但没有副作用（不注册监听、不初始化全局状态），调用方可以
/// 忽略它。目前没有任何调用方；保留它是为了保留 TS 的 lazy 入口形状（可直接返回，无副作用）。
pub fn load_mcp_runtime() {}

#[cfg(test)]
mod tests {
    //! 本文件内联测试与 `tests/mcp_connections.rs` 的分工：
    //!
    //! - 这里锁定纯函数与配置分支的语义：`"url" in config` 对 OAuth/登录/传输的影响、
    //!   `pathToFileURL`/`path.join`/`basename` 的 Node 等价行为；
    //! - 连接生命周期（重连、连接级重试、会话过期、登录、roots 载荷）在集成测试里用内存传输
    //!   覆盖；stderr tail 仅由 `create_default_transport` 路径间接构造，尚未有集成测试。
    //!
    //! fixture：`entry()` 用 `validate_mcp_server_config` 造合法条目；文件 URL 与 `~` 展开的
    //! 期望值来自 Node 实测（`pathToFileURL(path).href`、`path.join(homedir(), ...)`）。
    use super::*;
    use serde_json::json;

    fn entry(name: &str, config: Value) -> McpServerEntry {
        McpServerEntry {
            name: name.to_string(),
            config: kiana_core::mcp_servers::validate_mcp_server_config(name, &config)
                .expect("config is valid"),
            source: "test".to_string(),
            scope: None,
            override_: None,
        }
    }

    /// 测试用 manager：`DangerFullAccess` 不触发后端探测，构造是纯内存操作。
    fn test_manager(label: &str) -> Arc<SandboxManager> {
        Arc::new(SandboxManager::new(
            kiana_sandbox::SandboxPolicy::danger_full_access(),
            std::env::temp_dir().join(format!("kiana-mcp-sandbox-{label}")),
        ))
    }

    /// 显式 `options.sandbox` 压过进程级默认与 `KIANA_SANDBOX_POLICY`。
    #[test]
    fn explicit_sandbox_beats_process_default_and_env() {
        let explicit = test_manager("explicit");
        let process_default = test_manager("process-default");
        let env = test_manager("env");
        let resolved = resolve_default_mcp_sandbox(
            Some(Arc::clone(&explicit)),
            Some(Arc::clone(&process_default)),
            || Some(Arc::clone(&env)),
        );
        assert!(Arc::ptr_eq(
            &resolved.expect("explicit sandbox is used"),
            &explicit
        ));
    }

    /// 进程级默认压过 `KIANA_SANDBOX_POLICY`，且不再构造 env manager（避免多余的探测）。
    #[test]
    fn process_default_beats_env_and_skips_from_env() {
        let process_default = test_manager("process-default");
        let env = test_manager("env");
        let mut from_env_called = false;
        let resolved =
            resolve_default_mcp_sandbox(None, Some(Arc::clone(&process_default)), || {
                from_env_called = true;
                Some(Arc::clone(&env))
            });
        assert!(Arc::ptr_eq(
            &resolved.expect("process default is used"),
            &process_default
        ));
        assert!(
            !from_env_called,
            "from_env must not be consulted when a process default exists"
        );
    }

    /// 两个显式来源都缺失时才回退到 `from_env`；`from_env` 返回 `None` 时保持无沙箱。
    #[test]
    fn env_is_the_last_fallback() {
        let env = test_manager("env");
        let resolved = resolve_default_mcp_sandbox(None, None, || Some(Arc::clone(&env)));
        assert!(Arc::ptr_eq(&resolved.expect("env fallback is used"), &env));
        assert!(
            resolve_default_mcp_sandbox(None, None, || None).is_none(),
            "no explicit, no process default, no env -> unsandboxed"
        );
    }

    /// 显式 manager 会真正进入 stdio 传输的选项（可观察的接线行为）。
    #[test]
    fn explicit_sandbox_is_wired_into_the_stdio_transport() {
        let entry = entry(
            "wired",
            json!({"command": "definitely-missing-command-kiana"}),
        );
        let explicit = test_manager("wired");
        let transport = create_default_transport_with_sandbox(
            &entry,
            Path::new("."),
            None,
            Some(Arc::clone(&explicit)),
        );
        let stdio = take_registered_stdio(&transport).expect("stdio transport is registered");
        let sandbox = stdio.options().sandbox.clone().expect("sandbox is set");
        assert!(Arc::ptr_eq(&sandbox, &explicit));
    }

    /// 评审回归：TS 的运行时分支全部按 `"url" in config`，而不是校验出的 transport 变体。
    /// `validateMcpServerConfig` 返回输入对象的浅拷贝，因此这两个 stdio 条目都带 `url` 键。
    #[test]
    fn url_key_presence_controls_oauth_and_sign_in() {
        let null_url = entry("nullurl", json!({"command": "npx", "url": null}));
        assert!(config_has_url(&null_url.config));
        assert!(uses_oauth(&null_url));
        // `config.url` 是 null：truthiness 为假，不创建 OAuth provider。
        assert_eq!(oauth_url_value(&null_url), None);
        assert_eq!(
            sign_in_required_message(&null_url),
            "MCP server \"nullurl\" requires sign-in. Run /mcp to sign in."
        );

        let stray = entry(
            "stray",
            json!({
                "type": "stdio",
                "command": "x",
                "url": "https://example.com/mcp",
                "auth": {"provider": "radius"},
            }),
        );
        assert!(config_has_url(&stray.config));
        // `config.auth` 为真 → 不用 OAuth，但 sign-in 文案仍读它。
        assert!(!uses_oauth(&stray));
        assert_eq!(config_provider(&stray).as_deref(), Some("radius"));
        assert_eq!(
            sign_in_required_message(&stray),
            "MCP server \"stray\" requires sign-in. Run /login radius to sign in."
        );

        let header = entry(
            "header",
            json!({
                "type": "stdio",
                "command": "x",
                "url": "https://example.com/mcp",
                "headers": {"Authorization": "Bearer t"},
            }),
        );
        assert!(config_has_url(&header.config));
        assert!(!uses_oauth(&header));

        // 没有 url 键的 stdio 条目完全不参与这些分支。
        let plain = entry(
            "plain",
            json!({"command": "x", "auth": {"provider": "radius"}}),
        );
        assert!(!config_has_url(&plain.config));
        assert!(!uses_oauth(&plain));
        assert_eq!(config_provider(&plain), None);
    }

    /// `{command, url: null}`：TS 建 `StreamableHttpTransport`，`new URL(null)` 抛
    /// `TypeError("Invalid URL")`（瞬时错误，open() 会重试），不会启动 command。
    #[tokio::test]
    async fn null_url_uses_the_http_transport_and_fails_with_invalid_url() {
        let entry = entry(
            "nullurl",
            json!({"command": "definitely-missing-command-kiana", "url": null}),
        );
        let transport = create_default_transport(&entry, Path::new("."), None);
        let error = transport
            .start()
            .await
            .expect_err("null URL must fail like `new URL(null)`");
        assert_eq!(error.to_string(), "Invalid URL");
        assert!(
            is_transient_error(&error),
            "TypeError is transient, so the HTTP retry delays apply"
        );
    }

    /// `{type: "stdio", command, url: "https://…"}`：TS 用 URL 建 HTTP transport，command
    /// 从不启动（这里用不存在的 command 区分：stdio transport 会在 start 时报 ENOENT）。
    #[tokio::test]
    async fn stdio_config_with_a_string_url_uses_the_http_transport() {
        let entry = entry(
            "stray",
            json!({
                "type": "stdio",
                "command": "definitely-missing-command-kiana",
                "url": "https://example.com/mcp",
            }),
        );
        assert!(uses_oauth(&entry));
        assert_eq!(
            oauth_url_value(&entry).as_deref(),
            Some("https://example.com/mcp")
        );
        let transport = create_default_transport(&entry, Path::new("."), None);
        transport
            .start()
            .await
            .expect("HTTP transport starts without spawning the command");
        transport.close().await.expect("close");
    }

    /// 期望值来自 Node `pathToFileURL(path).href`（node 26）。
    #[cfg(unix)]
    #[test]
    fn file_url_matches_node_path_to_file_url() {
        for (path, href) in [
            ("/tmp/a[b]~c^|", "file:///tmp/a%5Bb%5D%7Ec%5E%7C"),
            ("/tmp/a b", "file:///tmp/a%20b"),
            ("/tmp/a%b", "file:///tmp/a%25b"),
            ("/tmp/a?b#c", "file:///tmp/a%3Fb%23c"),
            ("/tmp/a{b}`c", "file:///tmp/a%7Bb%7D%60c"),
            ("/tmp/a\"b", "file:///tmp/a%22b"),
            ("/tmp/a\\b", "file:///tmp/a%5Cb"),
            ("/tmp/ä", "file:///tmp/%C3%A4"),
            ("/tmp/x/", "file:///tmp/x/"),
            ("/tmp/x/.", "file:///tmp/x"),
            ("/tmp/a/../b", "file:///tmp/b"),
        ] {
            assert_eq!(file_url(Path::new(path)), href, "{path}");
        }
    }

    /// 期望值来自 Node `path.join(homedir(), value.slice(2))`。
    #[test]
    fn expand_home_matches_node_path_join() {
        let home = dirs::home_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let separator = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/"), home);
        assert_eq!(expand_home("~//x"), format!("{home}{separator}x"));
        assert_eq!(expand_home("~/a/../b"), format!("{home}{separator}b"));
        assert_eq!(
            expand_home("~/a/"),
            format!("{home}{separator}a{separator}")
        );
        #[cfg(unix)]
        assert_eq!(expand_home("~/../../.."), "/");
        assert_eq!(expand_home("/absolute/x"), "/absolute/x");
        assert_eq!(expand_home("relative/x"), "relative/x");
    }

    #[test]
    fn basename_matches_node_path_basename() {
        assert_eq!(basename(Path::new(".")), ".");
        assert_eq!(basename(Path::new("//")), "");
        assert_eq!(basename(Path::new("/a/b/")), "b");
        assert_eq!(basename(Path::new("/a/b")), "b");
    }
}
