//! `extensions/mcp/config.ts`：`mcp.json` 的读取、合并、编辑与重写。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/config.ts`。
//!
//! 服务器从 agent 目录的 `mcp.json` 读取；项目被信任时再从 `<project>/.kiana/mcp.json` 读取。
//! 两者都是 MCP 客户端通用的 `mcpServers` 形状，因此已有的配置可以直接复制过来。同名的项目
//! 条目替换全局条目。
//!
//! 没有 `command`、`url`、`type` 的项目条目只覆盖同名全局服务器的 `enabled`、`exposure`、
//! `toolExposure`（例如在某个项目里关掉它：`{ "mcpServers": { "internal-tools": { "enabled": false } } }`）。
//! 全局条目的其余部分（包括项目自己无法设置的凭据）保持不变。
//!
//! # 与 TS 的对应
//!
//! - JSON 解析错误用 [`kiana_json::js_parse`]（V8 的 `JSON.parse` 文案），而不是 `serde_json`。
//! - 读文件按 Node 的 `readFileSync(path, "utf8")` 语义做有损 UTF-8 解码（非法字节 → U+FFFD）。
//! - 文件 IO 错误使用 Node 的 `error.message`（`EISDIR: illegal operation on a directory, read`、
//!   `EACCES: permission denied, open '<path>'` …），不是 `io::Error` 的 Display。
//! - 编辑函数按 JS 普通对象语义访问条目：没有自有键时还会看到 `Object.prototype` 上的属性
//!   （`toString`/`constructor` 是函数 → 报错；`__proto__` 是原型对象 → 赋值只改原型）。
//! - 重写时保留文件检测到的缩进（`/^([ \t]+)\S/m`，字符串 `space` 取前 10 个 UTF-16 码元）、
//!   未知键与键序（JS 对象的整数索引键在前、其余保持插入序），末尾恰好一个 `\n`，无 BOM。
//! - 数值按 JS 的 `JSON.stringify` 规则写出（[`kiana_json::js_object::stringify`]）。
//!
//! # 职责边界
//!
//! 本文件只做“文件 ↔ 校验后的配置”：
//!
//! - 读：[`load_mcp_config`] 读全局（以及受信任时的项目）文件，合并成 [`LoadedMcpConfig`]；
//! - 写：[`update_mcp_server_config`] / [`add_mcp_server_config`] /
//!   [`remove_mcp_server_config`] 读改写单个文件，保留其它内容、键序与缩进。
//!
//! 配置项的类型校验、`exposure` 语义与命名空间规则都不在这里，而在 `kiana_core::mcp_servers`
//! （`validate_mcp_server_config` / `mcp_namespace`）；连接与 OAuth 分别在
//! [`crate::mcp::runtime`] 与 [`crate::mcp::oauth`]。因此本文件产出的
//! `McpServerEntry::config` 一定是校验过的类型化配置。
//!
//! # 调用方
//!
//! - [`crate::mcp::cli`]：`kiana mcp add/remove` 直接调用编辑函数；`list/login/logout` 用
//!   [`load_mcp_config`] 取得合并结果，其中 `list` 把 `errors` 非空当作命令失败。
//! - [`crate::mcp::index`]：`session_start` 用 [`load_mcp_config`] 载入条目，`/mcp` 的保存
//!   动作按 `override_ ?? source` 选择文件后调用 [`update_mcp_server_config`]。
//! - `kiana-interactive`（`interactive_mode/commands/auth.rs`）：Radius 授权时用
//!   [`load_mcp_config`]/[`add_mcp_server_config`] 把服务器写进全局 `mcp.json`。
//! - `tests/mcp_config.rs`：锁定错误文案、条目顺序与重写后的字节。
//!
//! # 读取流程（[`load_mcp_config`]）
//!
//! 1. 先读 `<agent_dir>/mcp.json`（Global），项目受信任时再读 `<cwd>/.kiana/mcp.json`
//!    （Project）。顺序有意义：项目里的“覆盖条目”要合并到已经载入的全局条目上。
//! 2. 每个文件独立处理：文件不存在直接跳过（不记为错误）；读失败、JSON 解析失败、根不是
//!    对象、`mcpServers` 不是对象时，把带路径前缀的错误写入 `errors` 并停止处理**该文件**；
//!    另一个文件不受影响（先读的已完成，后读的仍会处理）。
//! 3. 逐服务器校验；项目条目若是对象且没有 `command`/`url`/`type`，按覆盖处理（只能设置
//!    `enabled`/`exposure`/`toolExposure`）。
//! 4. 普通条目校验通过后检查命名空间冲突（`-`/`_` 归一后同名），以及项目文件里的 `auth`
//!    限制。
//!
//! 函数本身不返回 `Result`：所有问题都进 `LoadedMcpConfig::errors`，由调用方决定是提示
//! 还是失败。
//!
//! # 写入流程与失败模式（编辑函数）
//!
//! 三个编辑函数都经过 `edit_mcp_servers`：
//! 读文件（不存在则视为空对象）→ JS `JSON.parse` → 校验根与 `mcpServers` 形状 → 回调修改
//! `mcpServers` → 回调要求写入时，用检测到的缩进重写整个文件并补一个 `\n`。由此：
//!
//! - 回调返回 `false`（没有实际改动）时不写文件，原字节不变；
//! - 解析/形状错误返回 [`McpConfigError`]，文件保持原样；
//! - 回调通过后的写阶段失败同样返回 [`McpConfigError`]，但代码用 `fs::write` 直接覆盖目标
//!   文件（没有临时文件 + rename 的原子替换），因此不能假定失败后磁盘上还是旧内容；
//! - 编辑是“读-改-写”，模块内没有文件锁：同一文件被并发编辑时，本模块不提供顺序或合并
//!   保证，需由调用方协调。

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use kiana_core::config::CONFIG_DIR_NAME;
use serde::Serialize;
use serde_json::{Map, Value};

pub use kiana_core::mcp_servers::{
    McpExposure, McpHttpServerConfig, McpOAuthConfig, McpServerConfig, McpStdioServerConfig,
    get_mcp_tool_exposure,
};
use kiana_core::mcp_servers::{mcp_namespace, validate_mcp_server_config};

/// `mcp.json` 里项目覆盖允许的键（顺序即错误信息里的列表）。TS: `config.ts#OVERRIDE_KEYS`。
const OVERRIDE_KEYS: [&str; 3] = ["enabled", "exposure", "toolExposure"];

/// 一个条目的来源：全局 `mcp.json`、项目的 `mcp.json`，或 `kiana.registerMcpServer()`。
///
/// TS: `config.ts#McpServerEntry.scope` 的字符串字面量联合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpServerScope {
    /// `<agentDir>/mcp.json`。
    Global,
    /// `<cwd>/.kiana/mcp.json`（仅项目被信任时读取）。
    Project,
    /// 扩展用 `kiana.registerMcpServer()` 注册；改动不写回文件。
    Extension,
}

/// 一个 MCP 服务器条目。TS: `config.ts#McpServerEntry`。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerEntry {
    /// 服务器名（`mcpServers` 的键）。
    pub name: String,
    /// 校验后的配置（保留输入键序与未知键）。
    pub config: McpServerConfig,
    /// 定义该条目的配置文件，或注册它的扩展路径。
    ///
    /// 项目覆盖条目保留被覆盖的全局条目的 `source`：这里指向全局文件，项目文件只记录在
    /// `override_` 字段。
    pub source: String,
    /// 来源（`global` / `project` / `extension`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<McpServerScope>,
    /// 覆盖这个全局服务器的 `enabled`/`exposure`/`toolExposure` 的项目 `mcp.json`。
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    pub override_: Option<String>,
}

/// [`load_mcp_config`] 的结果。TS: `config.ts#LoadedMcpConfig`。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedMcpConfig {
    /// 合并后的服务器（禁用的也在内，`enabled: false`）。
    ///
    /// 顺序基本是插入序：每个文件内部先按 JS `Object.keys` 规则（整数样式的名字升序在前），
    /// 再按“先全局、后项目”的顺序合并——覆盖保持原位置，新增的项目条目追加在后。
    pub servers: Vec<McpServerEntry>,
    /// `codemode` 服务器连接时是否激活 codemode 工具（缺省 `true`，即键不存在）。
    ///
    /// 后读的文件覆盖先读的值；某个文件里该键不是布尔值时只记一条错误并保留之前的值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_enable_codemode: Option<bool>,
    /// 读取/校验错误（逐字，含文件路径前缀）。
    pub errors: Vec<String>,
    /// 项目被信任时的项目 `mcp.json` 路径（即使文件不存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_config: Option<String>,
}

/// [`load_mcp_config`] 的参数。TS: `loadMcpConfig({ agentDir, cwd, projectTrusted })`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadMcpConfigOptions {
    /// agent 目录（`~/.kiana/agent`）。
    pub agent_dir: PathBuf,
    /// 会话工作目录。
    pub cwd: PathBuf,
    /// 项目是否被信任（决定是否读取 `<cwd>/.kiana/mcp.json`）。
    pub project_trusted: bool,
}

/// `/mcp` 可修改的设置；`enabled: true` 与 `exposure: "codemode"` 是默认值，写非覆盖条目时会删键。
///
/// TS: `config.ts#McpServerConfigPatch`。
///
/// 更新只改这两个键，条目里的其它键原样保留；字段为 `None` 表示该项不动。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServerConfigPatch {
    /// 是否启用；`None` 表示不改。
    pub enabled: Option<bool>,
    /// 服务器默认 exposure；`None` 表示不改。
    pub exposure: Option<McpExposure>,
}

/// [`update_mcp_server_config`] 的选项。TS: `{ override?: boolean }`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateMcpServerConfigOptions {
    /// 条目缺失时把它作为项目覆盖加入（而不是报错）。
    ///
    /// 只对“确实缺失”的名字生效；条目存在但不是 JSON 对象时仍然报错。按 JS 对象语义，
    /// 继承自 `Object.prototype` 的名字（如 `constructor`）不算缺失。
    pub override_: bool,
}

/// `mcp.json` 读写错误；`Display` == TS `error.message`。
///
/// 消息已经是面向用户的完整文案（可能带文件路径），调用方通常原样展示；`kiana mcp` 的
/// add/remove 与 `/mcp` 的保存动作都会再拼上 `Could not update <path>: ` 前缀。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfigError {
    /// 逐字的错误消息。
    pub message: String,
}

impl McpConfigError {
    /// 用一条消息构造。
    pub fn new(message: impl Into<String>) -> Self {
        McpConfigError {
            message: message.into(),
        }
    }
}

impl fmt::Display for McpConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for McpConfigError {}

/// 读取过程中的可变状态。TS: `config.ts#McpConfigState`。
struct McpConfigState {
    /// `Map<string, McpServerEntry>`：插入序即结果顺序，同名条目保持原位置。
    servers: IndexMap<String, McpServerEntry>,
    /// 后读文件覆盖先读值；`None` 表示没有任何文件给出合法的布尔值（键缺失或类型错误；
    /// 调用方按 `true` 处理）。
    auto_enable_codemode: Option<bool>,
    /// 累积的错误；顺序即发现顺序，调用方原样展示。
    errors: Vec<String>,
}

/// 路径的显示字符串（TS 里路径就是字符串）。
fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// JS 的 `Array.prototype` 索引判定：`0`、`1`、…、`4294967294`（无前导零）。
fn is_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    (value != u32::MAX).then_some(value)
}

/// `Object.keys(map)` 的顺序：整数索引键升序在前，其余保持插入序。
///
/// “插入序”依赖工作区把 `serde_json` 的 `preserve_order` feature 打开（根 `Cargo.toml`）：
/// 关闭后 `Map` 是 `BTreeMap`，其余键会变成字典序，重写出的 `mcp.json` 键序随之改变。
pub(crate) fn js_keys(map: &Map<String, Value>) -> Vec<String> {
    let mut indices: Vec<(u32, String)> = Vec::new();
    let mut others: Vec<String> = Vec::new();
    for key in map.keys() {
        match is_array_index(key) {
            Some(index) => indices.push((index, key.clone())),
            None => others.push(key.clone()),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    let mut keys = Vec::with_capacity(map.len());
    keys.extend(indices.into_iter().map(|(_, key)| key));
    keys.extend(others);
    keys
}

/// `typeof value === "object" && value !== null && !Array.isArray(value)`。
fn is_record(value: &Value) -> bool {
    value.is_object()
}

/// 条目是否只覆盖其它地方定义的服务器。TS: `config.ts#isOverride`。
///
/// 只看三个键是否为 `undefined`：`{"type":"stdio"}` 有 `type`，因此**不是**覆盖。
fn is_override(value: &Map<String, Value>) -> bool {
    !value.contains_key("command") && !value.contains_key("url") && !value.contains_key("type")
}

/// 读文件并按 Node 的 `readFileSync(path, "utf8")` 语义解码（非法字节替换为 U+FFFD）。
fn read_utf8(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Node 的 `fs` 错误文案里的 `{CODE}: {描述}`（libuv 的 `uv_strerror`）。
///
/// 表里只列了 Node 常见 errno；未列出的 errno 返回 `None`，调用方退回 Rust 的
/// `io::Error` Display（文案与 TS 不同，但不影响控制流）。
fn node_error_code(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    match error.raw_os_error() {
        Some(1) => Some(("EPERM", "operation not permitted")),
        Some(2) => Some(("ENOENT", "no such file or directory")),
        Some(13) => Some(("EACCES", "permission denied")),
        Some(16) => Some(("EBUSY", "resource busy or locked")),
        Some(17) => Some(("EEXIST", "file already exists")),
        Some(20) => Some(("ENOTDIR", "not a directory")),
        Some(21) => Some(("EISDIR", "illegal operation on a directory")),
        Some(22) => Some(("EINVAL", "invalid argument")),
        Some(24) => Some(("EMFILE", "too many open files")),
        Some(28) => Some(("ENOSPC", "no space left on device")),
        Some(30) => Some(("EROFS", "read-only file system")),
        Some(36) => Some(("ENAMETOOLONG", "name too long")),
        Some(40) => Some(("ELOOP", "too many symbolic links encountered")),
        _ => None,
    }
}

/// Node `fs` 错误的 `error.message`：`{CODE}: {描述}, {syscall} '{path}'`
/// （`read`/`readdir` 这类在打开之后失败的系统调用不带路径）。未知 errno 退回 `io::Error`
/// 的 Display。TS: `config.ts#readConfigFile`/`editMcpServers` 里作为 `error.message` 使用。
fn node_io_error(error: &std::io::Error, syscall: &str, path: Option<&Path>) -> String {
    let Some((code, description)) = node_error_code(error) else {
        return error.to_string();
    };
    match path {
        Some(path) => format!("{code}: {description}, {syscall} '{}'", path_string(path)),
        None => format!("{code}: {description}, {syscall}"),
    }
}

/// `readFileSync` 的错误：目录在打开后读取时失败（`EISDIR ... read`，不带路径），其余是
/// `open '{path}'`。
fn node_read_error(error: &std::io::Error, path: &Path) -> String {
    let is_directory = error.raw_os_error() == Some(21);
    node_io_error(
        error,
        if is_directory { "read" } else { "open" },
        (!is_directory).then_some(path),
    )
}

/// `Object.prototype` 的自有属性名（Node 26 `Object.getOwnPropertyNames(Object.prototype)`）。
/// JS 里 `servers?.[name]` 会看到它们；`mcp.json` 的编辑函数因此不是纯 Map 语义。
const OBJECT_PROTOTYPE_MEMBERS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// `name` 是否是继承自 `Object.prototype` 的属性。
fn is_object_prototype_member(name: &str) -> bool {
    OBJECT_PROTOTYPE_MEMBERS.contains(&name)
}

/// `servers?.[name]` 的结果：自有键、继承自 `Object.prototype`，或 `undefined`。
enum JsProperty {
    /// 自有键（`Object.hasOwn`）。
    Own,
    /// 继承自 `Object.prototype`（值为函数，`__proto__` 例外，是原型对象本身）。
    Inherited,
    /// `undefined`。
    Missing,
}

/// JS 普通对象的属性查找（`mcpServers` 是从 JSON 解析出的对象字面量，继承 `Object.prototype`）。
fn js_property(servers: Option<&Map<String, Value>>, name: &str) -> JsProperty {
    let Some(servers) = servers else {
        // `servers?.[name]`：`servers` 本身是 `undefined` 时短路为 `undefined`。
        return JsProperty::Missing;
    };
    if servers.contains_key(name) {
        JsProperty::Own
    } else if is_object_prototype_member(name) {
        JsProperty::Inherited
    } else {
        JsProperty::Missing
    }
}

/// 把服务器配置写成 JS 对象，供 `{ ...base.config, ...value }` 合并。
///
/// 也是 TS 里 `"url" in config` / `config.auth` 这类键存在性与属性读取的等价视图：
/// 校验函数返回输入对象的浅拷贝，所以 stdio 条目也可能带着 `url`（见 `mcp/runtime.rs`）。
pub(crate) fn config_to_value(config: &McpServerConfig) -> Map<String, Value> {
    match serde_json::to_value(config) {
        Ok(Value::Object(map)) => map,
        // `McpServerConfig` 的 `Serialize` 永远产出对象；这里只是防御。
        _ => Map::new(),
    }
}

/// `{ ...base, ...over }`（JS 展开：已有键保持位置，新键按 `over` 的键序追加）。
fn merge_objects(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = base.clone();
    for key in js_keys(over) {
        merged.insert(key.clone(), over[&key].clone());
    }
    merged
}

/// 读取一个 `mcp.json` 并合并进状态。TS: `config.ts#readConfigFile`。
fn read_config_file(path: &Path, scope: McpServerScope, state: &mut McpConfigState) {
    // 文件不存在是正常状态（新安装，或项目还没有 mcp.json），不产生错误。
    if !path.exists() {
        return;
    }
    let text = match read_utf8(path) {
        Ok(text) => kiana_core::text::strip_bom(&text),
        Err(error) => {
            state.errors.push(format!(
                "{}: {}",
                path_string(path),
                node_read_error(&error, path)
            ));
            return;
        }
    };
    // 解析失败只放弃这个文件；另一个文件（全局/项目）不受影响。
    let parsed = match kiana_json::js_parse::parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => {
            state
                .errors
                .push(format!("{}: {}", path_string(path), error.message));
            return;
        }
    };
    if !is_record(&parsed) {
        state.errors.push(format!(
            "{}: expected an object with an \"mcpServers\" object",
            path_string(path)
        ));
        return;
    }
    let parsed = parsed.as_object().expect("checked is_record above");
    if let Some(mcp_servers) = parsed.get("mcpServers")
        && !is_record(mcp_servers)
    {
        state.errors.push(format!(
            "{}: expected an object with an \"mcpServers\" object",
            path_string(path)
        ));
        return;
    }

    // 文件级开关，不进入服务器条目；类型错误只记一条，文件里的服务器仍会处理。
    match parsed.get("autoEnableCodemode") {
        Some(Value::Bool(value)) => state.auto_enable_codemode = Some(*value),
        Some(_) => state.errors.push(format!(
            "{}: autoEnableCodemode must be a boolean",
            path_string(path)
        )),
        None => {}
    }

    let servers_map = parsed
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for name in js_keys(&servers_map) {
        let value = &servers_map[&name];
        // 项目条目的“覆盖”分支：必须已有同名全局条目，且只能出现 OVERRIDE_KEYS 里的键。
        if scope == McpServerScope::Project
            && value.is_object()
            && is_override(value.as_object().expect("checked is_object above"))
        {
            let base = state.servers.get(&name).cloned();
            let value_object = value.as_object().expect("checked is_object above");
            let extra: Vec<String> = js_keys(value_object)
                .into_iter()
                .filter(|key| !OVERRIDE_KEYS.contains(&key.as_str()))
                .collect();
            let Some(base) = base else {
                state.errors.push(format!(
                    "{}: server \"{name}\" needs \"command\" or \"url\", or a global server to override",
                    path_string(path)
                ));
                continue;
            };
            if !extra.is_empty() {
                state.errors.push(format!(
                    "{}: server \"{name}\": an override can only set {}",
                    path_string(path),
                    OVERRIDE_KEYS.join(", ")
                ));
                continue;
            }
            // 以全局条目（校验后的 JS 对象）为底，项目值覆盖同名键；其余键保持不变。
            let merged = merge_objects(&config_to_value(&base.config), value_object);
            match validate_mcp_server_config(&name, &Value::Object(merged)) {
                Ok(config) => {
                    let entry = McpServerEntry {
                        name: base.name,
                        config,
                        source: base.source,
                        scope: base.scope,
                        override_: Some(path_string(path)),
                    };
                    state.servers.insert(entry.name.clone(), entry);
                }
                Err(message) => state
                    .errors
                    .push(format!("{}: {message}", path_string(path))),
            }
            continue;
        }
        match validate_mcp_server_config(&name, value) {
            Err(message) => {
                state
                    .errors
                    .push(format!("{}: {message}", path_string(path)));
                continue;
            }
            Ok(config) => {
                // 只在 `-`/`_` 上不同的名字会共享同一个命名空间。
                // 保留先出现的条目，后来的记为冲突（`tests/mcp_config.rs` 锁定
                // `work-files` / `work_files`）。
                let namespace = mcp_namespace(&name);
                let clash = state
                    .servers
                    .keys()
                    .find(|other| *other != &name && mcp_namespace(other) == namespace)
                    .cloned();
                if let Some(clash) = clash {
                    state.errors.push(format!(
                        "{}: server \"{name}\" conflicts with \"{clash}\"",
                        path_string(path)
                    ));
                    continue;
                }
                // TS 检查的是校验后对象上的键（`"url" in config && config.auth`），而不是
                // transport 变体：stdio 条目带 `url`（哪怕不是字符串）时同样会被拒绝。
                // `auth` 按 JS 真值判断：0/false/空字符串/null 都不触发。
                let config_value = config_to_value(&config);
                if scope == McpServerScope::Project
                    && config_value.contains_key("url")
                    && kiana_json::js_string::is_truthy(config_value.get("auth"))
                {
                    state.errors.push(format!(
                        "{}: server \"{name}\": auth is only allowed in the global mcp.json",
                        path_string(path)
                    ));
                    continue;
                }
                let entry = McpServerEntry {
                    name: name.clone(),
                    config,
                    source: path_string(path),
                    scope: Some(scope),
                    override_: None,
                };
                state.servers.insert(entry.name.clone(), entry);
            }
        }
    }
}

/// 读取全局（以及项目被信任时的项目）MCP 配置。禁用的服务器也在结果里（`enabled: false`），
/// 这样 `/mcp` 能重新启用它们。
/// TS: `config.ts#loadMcpConfig`。
///
/// # 参数与返回
///
/// 全局文件固定为 `<agent_dir>/mcp.json`；`project_trusted` 为 `false` 时完全不读项目文件，
/// `LoadedMcpConfig::project_config` 也是 `None`。项目文件路径即使不存在也会返回
/// （`/mcp` 把它作为保存项目覆盖的目标路径，并用 `is_some()` 决定是否提供 “in this
/// project” 操作；路径本身不展示）。
///
/// # 失败模式
///
/// 本函数不返回 `Result`：文件不可读、JSON 非法、条目校验失败等都只追加到
/// `LoadedMcpConfig::errors`。本函数始终返回结构体：文件不存在时 `errors` 为空，内容非法时
/// 是非空错误列表，调用方必须自己检查 `errors`。
pub fn load_mcp_config(options: LoadMcpConfigOptions) -> LoadedMcpConfig {
    let mut state = McpConfigState {
        servers: IndexMap::new(),
        auto_enable_codemode: None,
        errors: Vec::new(),
    };
    // 先全局后项目：项目的覆盖分支依赖 state 里已经存在的同名全局条目。
    read_config_file(
        &options.agent_dir.join("mcp.json"),
        McpServerScope::Global,
        &mut state,
    );
    // 未受信任时不读项目文件；路径本身仍要返回给调用方（文件不存在也一样）。
    let project_config = options
        .project_trusted
        .then(|| options.cwd.join(CONFIG_DIR_NAME).join("mcp.json"));
    if let Some(project_config) = &project_config {
        read_config_file(project_config, McpServerScope::Project, &mut state);
    }
    LoadedMcpConfig {
        servers: state.servers.into_values().collect(),
        auto_enable_codemode: state.auto_enable_codemode,
        errors: state.errors,
        project_config: project_config.map(|path| path_string(&path)),
    }
}

/// 修改定义/覆盖某个服务器的 `mcp.json` 里该服务器的设置。带 `override_` 时，缺失的条目会作为
/// 覆盖加入。覆盖条目保留默认值（它们要替换全局服务器的值）。其它内容保持不变；文件用其缩进
/// 重写。TS: `config.ts#updateMcpServerConfig`。
///
/// # 错误
///
/// - 文件不是 JSON 对象或 `mcpServers` 不是对象：返回 [`McpConfigError`]（JSON 解析错误
///   原样返回，形状错误带路径）；
/// - 条目缺失且没有要求 `override_`，或同名条目不是 JSON 对象：
///   `<path> does not define MCP server "<name>"`；
/// - 创建父目录或写文件失败：Node 风格的 IO 文案。
///
/// 调用方是 `/mcp` 的保存动作（`index.rs` 的 `default_update_config`）；`save_config` 会
/// 把错误再拼成 `Could not update <target>: <message>` 后展示。
pub fn update_mcp_server_config(
    path: &Path,
    name: &str,
    patch: &McpServerConfigPatch,
    options: UpdateMcpServerConfigOptions,
) -> Result<(), McpConfigError> {
    edit_mcp_servers(path, |servers, _parsed| {
        // `let server = servers?.[name]`：缺失才走 override 创建（继承的属性不算缺失）。
        if matches!(js_property(servers.as_ref(), name), JsProperty::Missing) && options.override_ {
            servers
                .get_or_insert_with(Map::new)
                .insert(name.to_string(), Value::Object(Map::new()));
        }
        if matches!(js_property(servers.as_ref(), name), JsProperty::Own) {
            let Some(Value::Object(server)) = servers.as_mut().and_then(|map| map.get_mut(name))
            else {
                return Err(McpConfigError::new(format!(
                    "{} does not define MCP server \"{name}\"",
                    path_string(path)
                )));
            };
            // 覆盖条目必须显式写出值才能替换全局值，所以即使是默认值也不能删键。
            let keep_defaults = is_override(server);
            if let Some(enabled) = patch.enabled {
                if enabled && !keep_defaults {
                    server.shift_remove("enabled");
                } else {
                    server.insert("enabled".to_string(), Value::Bool(enabled));
                }
            }
            if let Some(exposure) = patch.exposure {
                if exposure == McpExposure::Codemode && !keep_defaults {
                    server.shift_remove("exposure");
                } else {
                    server.insert(
                        "exposure".to_string(),
                        Value::String(exposure.as_str().to_string()),
                    );
                }
            }
            return Ok(true);
        }
        // `servers["__proto__"]`（没有自有键时）是 `Object.prototype`：它是记录，赋值改的是
        // 原型而不是文件内容。其余继承属性是函数，`isRecord` 为假 → 报错。
        if name == "__proto__"
            && matches!(js_property(servers.as_ref(), name), JsProperty::Inherited)
        {
            return Ok(true);
        }
        Err(McpConfigError::new(format!(
            "{} does not define MCP server \"{name}\"",
            path_string(path)
        )))
    })
}

/// 往 `mcp.json` 添加一个服务器（文件缺失时创建）。同名的已有条目会被替换；返回是否替换了条目。
/// TS: `config.ts#addMcpServerConfig`。
///
/// # 错误
///
/// 文件解析/形状错误、创建父目录失败或写入失败都返回 [`McpConfigError`]；这些情况下文件
/// 可能未被修改（解析阶段）或只被部分覆盖（写入阶段，见模块文档）。
///
/// # 注意
///
/// `name` 按 JS 普通对象语义查找：命中 `Object.prototype` 的继承属性（如 `constructor`）
/// 也算“已替换”。`__proto__` 例外——赋值不产生自有键，条目不会写进文件，但返回值仍是
/// `true`（`tests/mcp_config.rs` 锁定）。
pub fn add_mcp_server_config(
    path: &Path,
    name: &str,
    config: &McpServerConfig,
) -> Result<bool, McpConfigError> {
    let value =
        serde_json::to_value(config).map_err(|error| McpConfigError::new(format!("{error}")))?;
    let mut replaced = false;
    edit_mcp_servers(path, |servers, _parsed| {
        // `target = servers ?? {}`；`target[name] !== undefined` 也看 Object.prototype。
        // 文件没有 `mcpServers` 时 `target` 是一个新的 `{}`，继承属性依然存在。
        let own = servers.as_ref().is_some_and(|map| map.contains_key(name));
        let empty = Map::new();
        let target_lookup = js_property(Some(servers.as_ref().unwrap_or(&empty)), name);
        replaced = !matches!(target_lookup, JsProperty::Missing);
        let target = servers.get_or_insert_with(Map::new);
        // 没有自有 `__proto__` 时，`target["__proto__"] = config` 走继承的 setter：改的是原型，
        // 条目不会写进文件（TS 里它就此丢失）。
        if !(name == "__proto__" && !own) {
            target.insert(name.to_string(), value.clone());
        }
        Ok(true)
    })?;
    Ok(replaced)
}

/// 从 `mcp.json` 删除一个服务器。文件不存在或未定义它时返回 `false`（不创建/不改文件）。
/// TS: `config.ts#removeMcpServerConfig`。
///
/// 名字命中 `Object.prototype` 的继承属性（如 `toString`）时按 TS 语义返回 `true` 并重写
/// 文件；实际删除是空操作，因此条目集合不变（若原文件已是本模块的规范排版，字节也不变；
/// `tests/mcp_config.rs` 锁定该场景）。
pub fn remove_mcp_server_config(path: &Path, name: &str) -> Result<bool, McpConfigError> {
    if !path.exists() {
        return Ok(false);
    }
    let mut removed = false;
    edit_mcp_servers(path, |servers, _parsed| {
        match js_property(servers.as_ref(), name) {
            JsProperty::Missing => Ok(false),
            // `delete servers[name]` 对继承属性是空操作，但 TS 仍报告删除成功并重写文件。
            JsProperty::Inherited => {
                removed = true;
                Ok(true)
            }
            JsProperty::Own => {
                servers
                    .as_mut()
                    .expect("an own property implies a servers object")
                    .shift_remove(name);
                removed = true;
                Ok(true)
            }
        }
    })?;
    Ok(removed)
}

/// 读一个 `mcp.json`（缺失时视为空配置），让 `edit` 修改它的 `mcpServers`，返回 `true` 时按原
/// 缩进写回。其它内容保持不变。TS: `config.ts#editMcpServers`（模块私有，不导出）。
fn edit_mcp_servers<F>(path: &Path, edit: F) -> Result<(), McpConfigError>
where
    F: FnOnce(
        &mut Option<Map<String, Value>>,
        &mut Map<String, Value>,
    ) -> Result<bool, McpConfigError>,
{
    // 文件不存在时按空配置处理；存在但读失败/解析失败则直接返回错误，不写文件。
    let text = if path.exists() {
        Some(kiana_core::text::strip_bom(&read_utf8(path).map_err(
            |error| McpConfigError::new(node_read_error(&error, path)),
        )?))
    } else {
        None
    };
    // 解析错误用 JS 的 JSON.parse 文案；非对象根与非法 `mcpServers` 都按形状错误处理。
    let mut parsed: Map<String, Value> = match &text {
        None => Map::new(),
        Some(text) => match kiana_json::js_parse::parse(text) {
            Ok(Value::Object(map)) => map,
            Ok(_) => {
                return Err(McpConfigError::new(format!(
                    "{}: expected an object with an \"mcpServers\" object",
                    path_string(path)
                )));
            }
            Err(error) => return Err(McpConfigError::new(error.message)),
        },
    };
    if let Some(mcp_servers) = parsed.get("mcpServers")
        && !is_record(mcp_servers)
    {
        return Err(McpConfigError::new(format!(
            "{}: expected an object with an \"mcpServers\" object",
            path_string(path)
        )));
    }
    // 回调修改的是 `mcpServers` 的副本；`parsed` 仍持有原对象，用于写回其它键。
    let mut servers = parsed.get("mcpServers").and_then(Value::as_object).cloned();
    // 回调返回 `false` 表示没有实际改动：不重写文件，原字节保持不变。
    if !edit(&mut servers, &mut parsed)? {
        return Ok(());
    }
    if let Some(servers) = servers {
        parsed.insert("mcpServers".to_string(), Value::Object(servers));
    }
    // 保留原文件第一处行首缩进；新文件或检测不到时用两个空格。
    let indent = text
        .as_deref()
        .and_then(detect_indent)
        .unwrap_or_else(|| "  ".to_string());
    // add 可能写一个还不存在的目录（agent 目录尚未创建）。
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| McpConfigError::new(node_io_error(&error, "mkdir", Some(parent))))?;
    }
    let body = stringify_with_gap(&Value::Object(parsed), &indent);
    // 结尾恰好一个换行；读取时已去掉 BOM，因此写回无 BOM。
    fs::write(path, format!("{body}\n"))
        .map_err(|error| McpConfigError::new(node_io_error(&error, "open", Some(path))))
}

/// JS 的空白集合（`WhiteSpace` + `LineTerminator`，含 U+FEFF，不含 U+0085）。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `/^([ \t]+)\S/m`：第一行“行首至少一个空格/制表符 + 非空白字符”的缩进串。
///
/// 不能用 Rust 正则的 `(?m)`：JS 的多行 `^` 还匹配 `\r`、U+2028、U+2029 之后的位置，
/// 所以这里手动按行终止符扫描。
fn detect_indent(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut start = 0usize;
    loop {
        let mut index = start;
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b'\t') {
            index += 1;
        }
        if index > start
            && index < bytes.len()
            && let Some(ch) = text[index..].chars().next()
            && !is_js_whitespace(ch)
        {
            return Some(text[start..index].to_string());
        }
        // 下一个行起点：`\n`、`\r`、U+2028、U+2029 之后（JS 的 multiline `^`）。
        start = text[start..].char_indices().find_map(|(offset, ch)| {
            matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
                .then_some(start + offset + ch.len_utf8())
        })?;
    }
}

/// `JSON.stringify(value, null, gap)`：`gap` 是缩进字符串（JS 的字符串 `space` 取前 10 个
/// UTF-16 码元），键序与数字格式按 JS 规则。
fn stringify_with_gap(value: &Value, gap: &str) -> String {
    // gap 只来自 `detect_indent`（空格/制表符），因此按 char 取 10 个与 JS 按 UTF-16
    // 码元取 10 个等价。
    let gap: String = gap.chars().take(10).collect();
    let sorted = kiana_json::js_object::sort_js_keys(value);
    let mut out = String::new();
    write_gap_value(&sorted, &gap, 0, &mut out);
    out
}

fn push_gap(out: &mut String, gap: &str, level: usize) {
    for _ in 0..level {
        out.push_str(gap);
    }
}

/// `JSON.stringify` 缩进排版的递归实现：空数组/空对象写成 `[]`/`{}`（无换行），非空容器
/// 每个元素独占一行。键序已由 [`stringify_with_gap`] 里的 `sort_js_keys` 归一。
fn write_gap_value(value: &Value, gap: &str, level: usize, out: &mut String) {
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                push_gap(out, gap, level + 1);
                write_gap_value(item, gap, level + 1, out);
            }
            out.push('\n');
            push_gap(out, gap, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                push_gap(out, gap, level + 1);
                out.push_str(&kiana_json::js_string::js_quote(key));
                out.push_str(": ");
                write_gap_value(item, gap, level + 1, out);
            }
            out.push('\n');
            push_gap(out, gap, level);
            out.push('}');
        }
        scalar => out.push_str(&kiana_json::js_object::stringify(scalar)),
    }
}
