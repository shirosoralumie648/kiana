//! `core/mcp-servers.ts`：MCP server 配置校验、工具暴露（exposure）与注册表。
//!
//! 本文件只做**配置校验与注册表**；连接、OAuth、工具暴露协议由 `telemetry-mcp` 单元实现。
//!
//! `McpServerConfig` 等类型必须在本 crate 本地定义：`kiana-mcp` 不是 `kiana-core` 的允许依赖
//! （`ARCHITECTURE.md` §3.5 / spec §2.9.8 的 Rust 设计）。
//!
//! TS: `packages/coding-agent/src/core/mcp-servers.ts`。
//!
//! ## 已知差异（都是类型化带来的，行为已尽量保留）
//!
//! 1. TS 的 `validateMcpServerConfig` 返回**原始对象的浅拷贝**，因此未识别的键会被保留、键序是
//!    输入顺序。Rust 的类型化配置把这类键收进各变体的 `extra`，并额外记录输入键序
//!    （`input_key_order`，`#[doc(hidden)]`，不参与相等比较）：`Serialize` 按记录的顺序输出已知
//!    字段与未识别键（整数索引键按 JS 规则升序在前）；写出的 JSON 文本是当前稳定落盘契约，改动需同步测试。
//!    程序化构造（`input_key_order` 为空）时按字段声明序输出、`extra` 追加在后——TS 没有对应的
//!    “程序化构造再写回”路径，`mcp add` 的输入对象仍经 `validateMcpServerConfig` 记录键序。
//!    `Serialize` 用 `kiana_json::js_object::stringify` 生成 JSON 文本（[`serde_json::value::RawValue`]）
//!    嵌入，因此数字也按 JS 格式化；代价是 `serde_json::to_string_pretty` 不会展开缩进——下游写
//!    `mcp.json` 应取 `serde_json::to_value` 后走 `kiana_json::js_object::stringify_pretty`。
//! 2. `type` 字段保存为 `Option<String>` 并原样保留（`"stdio"`、`"http"`、`"streamable-http"`），
//!    因为 TS 在 `streamable-http` 分支把原对象原样返回；枚举判别用 [`McpServerConfig`] 的变体，
//!    而不是序列化标签。
//! 3. JS 对象的原型链会让 `toolExposure["constructor"]` / `["toString"]` 之类返回
//!    `Object.prototype` 上的函数（`overrides[toolName] !== undefined` 成立），而类型化
//!    `IndexMap` 查不到；TS 于是把一个函数当成 exposure 返回（违反它自己的返回类型），
//!    Rust 的 `McpExposure` 枚举无法表达这种值，退回模式列表/服务器 exposure。只有工具名
//!    恰好是这些原型成员时才有差异。
//! 4. JSON `null` 与 TS 的 `undefined` 不同：字段显式为 `null` 时 TS 会走“值非法”分支并报错
//!    （`null !== undefined`），本实现保留该行为（`Option` 只表示键缺失）。
//! 5. `McpServerRegistry::get` 返回配置的克隆（TS 返回 `Map` 内的同一对象引用，可被调用方就地
//!    修改）；`list` 的深拷贝语义与 `structuredClone` 一致。
//!
//! `timeout` 以及 `extra` 里的所有数字都按 JS `number` 写出（[`kiana_json::js_num::format_js_number`]：
//! `60` 而不是 serde 的 `60.0`，`1e20` 写成 `100000000000000000000`，`JSON.parse("1.0")` 写回 `1`）。
//!
//! # 职责边界
//!
//! 本文件只接受**已经解析成 `serde_json::Value` 的配置**，输出类型化后的配置或错误字符串。
//! 它不读文件、不访问网络、不展开 `${NAME}`/`!cmd` 占位符、不建立 MCP 连接；`mcp.json`
//! 的读取/合并/重写在 `kiana-builtin-extensions` 的 `mcp` 模块，连接与 OAuth 由该模块与
//! `kiana-mcp`（TS 的 `packages/mcp`）实现。
//!
//! 下游导入方（按调用点）：
//! - `kiana-extensions`：用 [`validate_mcp_server_config`] + [`McpServerRegistry`] 实现扩展的
//!   `registerMcpServer`（校验 → 所有权/命名空间检查 → 注册）。
//! - `kiana-builtin-extensions` 的 `mcp` 模块：用 [`validate_mcp_server_config`] 读取并合并
//!   `mcp.json`、用 [`get_mcp_tool_exposure`] 计算每个工具的 exposure。
//! - `kiana-session`：用 [`create_tool_name_matcher`] 与 [`is_mcp_tool_name`] 过滤工具名。
//! - `kiana-ext-api`：`ExtensionRuntimeState` 内嵌 [`McpServerRegistry`]
//!   （`crates/kiana-ext-api/src/types.rs`）；`McpServersChangeEvent` 反序列化时调用
//!   [`validate_mcp_server_config`]（`crates/kiana-ext-api/src/events.rs`）。
//! - `kiana-interactive`：`/login` 的 auth 命令直接构造 `McpAuthConfig`/`McpHttpServerConfig`
//!   （`crates/kiana-interactive/src/interactive_mode/commands/auth.rs`）。
//!
//! # 一次校验的执行流程
//!
//! [`validate_mcp_server_config`] 的检查顺序是外部可观察的（它决定多条错误同时存在时先报
//! 哪一条）。下面的顺序按代码实际执行列出；函数文档将该顺序与错误文案定为当前稳定契约，
//! `tests/mcp_servers.rs` 用 TS 生成的 fixture 对拍错误串：
//!
//! 1. 校验 server 名（`^[A-Za-z0-9_-]+$`）。这一步的错误和“必须是对象”的错误都**不带**
//!    `server "<name>": ` 前缀；其余错误带。
//! 2. 要求输入是 JSON 对象。
//! 3. 把 `exposure` 与 `toolExposure` 里的旧名 `codemode-deferred` 就地替换为 `codemode`。
//!    换名发生在校验之前，所以别名能通过校验，且返回的配置里存的是现名。
//! 4. 依次检查公共字段 `exposure` → `toolExposure` → `enabled` → `description` → `timeout`。
//! 5. 按 `type` 与 `url`/`command` 选择 transport：`url` 是字符串且 `type` 缺省或为
//!    `http`/`streamable-http` 时走 HTTP 分支（先判断）；否则 `command` 是字符串且 `type`
//!    缺省或为 `stdio` 时走 stdio 分支；都不满足则返回最后那条 “needs either ...”。
//!    分支选择之前，`type` 是 `"sse"` 会先单独报错。
//! 6. HTTP 分支校验 `url`（只接受 http/https）、`headers`、`oauth`、`auth`；stdio 分支校验
//!    `args`、`env`、`cwd`。**不属于当前 transport 的键不做类型校验**，它们进入 `extra`
//!    并在写回时保留（例如 `{type:"stdio", command:"x", url:"https://…"}` 的 `url`）。
//!
//! # 关键不变量
//!
//! - 四个需要写回的配置结构体（[`McpOAuthConfig`]、[`McpAuthConfig`]、
//!   [`McpStdioServerConfig`]、[`McpHttpServerConfig`]）都保存 `input_key_order`
//!   （输入对象的键序）与 `extra`（未识别的键），保证写回 `mcp.json` 时的
//!   JSON 文本逐字节稳定（见文件头“已知差异”第 1 条）。`PartialEq`
//!   忽略键序：两次校验只要配置内容相同就相等，与输入键序无关。
//! - `build_base`/`build_oauth`/`build_auth`/`build_stdio`/`build_http` 只在类型检查全部
//!   通过后调用，因此它们用 `and_then(Value::as_*)` 宽松取值、取不到就留 `None`/空串；
//!   这些函数不返回错误。直接构造结构体（绕过校验）时字段可以是任意值，序列化不会兜底。
//! - 校验失败是普通返回值 `Err(String)`，不是 panic，也没有专门的错误枚举；错误文案是 TS
//!   的逐字映射，调用方原样拼上文件路径等前缀后展示。
//!
//! # 注册表与并发
//!
//! [`McpServerRegistry`] 内部是 `Arc<Mutex<...>>`，`Clone` 得到的是同一张表的别名而非拷贝。
//! 变更回调在所有锁释放之后、由调用 `register`/`unregister` 的线程**同步**执行，所以回调
//! 里可以再次调用注册表方法（现有 runner 的 `mcp_servers_change` 监听器就在回调里调用
//! `list()`）。反过来，回调不能假设自己看到的正是触发它的那次变更：并发修改时两次回调
//! 之间可能夹着另一次变更。详见 [`McpServerRegistry`] 的文档。
//!
//! # 阅读顺序
//!
//! 先读 [`McpServerConfig`] 及各配置结构体弄清数据形状，再读
//! [`validate_mcp_server_config`] 的分支，最后读 [`McpServerRegistry`]。工具名匹配
//! （[`create_tool_name_matcher`]、[`get_mcp_tool_exposure`]）与配置校验相互独立，可最后读。

use std::collections::HashSet;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use url::{Host, Url};

use crate::paths::js_trim;

/// `list_mcp_resources` 工具名。TS: `mcp-servers.ts#LIST_MCP_RESOURCES_TOOL`。
pub const LIST_MCP_RESOURCES_TOOL: &str = "list_mcp_resources";
/// `list_mcp_resource_templates` 工具名。TS: `mcp-servers.ts#LIST_MCP_RESOURCE_TEMPLATES_TOOL`。
pub const LIST_MCP_RESOURCE_TEMPLATES_TOOL: &str = "list_mcp_resource_templates";
/// `read_mcp_resource` 工具名。TS: `mcp-servers.ts#READ_MCP_RESOURCE_TOOL`。
pub const READ_MCP_RESOURCE_TOOL: &str = "read_mcp_resource";

/// 三个 MCP 资源工具（每个有资源的 server 都能调用）。
const MCP_RESOURCE_TOOLS: [&str; 3] = [
    LIST_MCP_RESOURCES_TOOL,
    LIST_MCP_RESOURCE_TEMPLATES_TOOL,
    READ_MCP_RESOURCE_TOOL,
];

/// 工具暴露方式。TS: `mcp-servers.ts#McpExposure`。
///
/// `Serialize`/`Deserialize` 的字符串形态就是 [`Self::as_str`] 的四个字面量
/// （`#[serde(rename_all = "lowercase")]`）。旧名 `codemode-deferred` **不在**枚举里：
/// 它只在配置校验阶段被替换成 `codemode`（见 `resolve_exposure_aliases`），
/// [`Self::parse`] 对它是 `None`。
///
/// 本文件只解析并保存“怎么暴露”；据此声明/过滤工具由消费方执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpExposure {
    /// `codemode`：可从 codemode 脚本调用，但不声明给模型、也不列进 codemode 描述。
    /// `codemode-deferred` 是它的旧别名。
    Codemode,
    /// `deferred`：在 `tool_search` 加载前不声明给模型。
    Deferred,
    /// `direct`：像普通工具一样声明给模型（也可从 codemode 调用）。
    Direct,
    /// `hidden`：注册但不可达。
    Hidden,
}

impl McpExposure {
    /// TS 里的字符串字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            McpExposure::Codemode => "codemode",
            McpExposure::Deferred => "deferred",
            McpExposure::Direct => "direct",
            McpExposure::Hidden => "hidden",
        }
    }

    /// 严格解析（大小写敏感，与 TS 的 `MCP_EXPOSURES.includes` 一致）；别名不在这里处理。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codemode" => Some(McpExposure::Codemode),
            "deferred" => Some(McpExposure::Deferred),
            "direct" => Some(McpExposure::Direct),
            "hidden" => Some(McpExposure::Hidden),
            _ => None,
        }
    }
}

impl fmt::Display for McpExposure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `MCP_EXPOSURES.map(v => \`"${v}"\`).join(", ")` 的逐字结果，用于错误消息。
const MCP_EXPOSURE_LIST: &str = "\"codemode\", \"deferred\", \"direct\", \"hidden\"";

/// OAuth client 的注册方式。TS: `McpOAuthConfig.clientRegistration`（`"dcr" | "cimd"`）。
///
/// `Serialize`/`Deserialize` 用 `"dcr"`/`"cimd"` 两个小写字面量。配置里只允许这两个值：
/// 其它字符串（包括大小写变体）由 `validate_oauth` 拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpClientRegistration {
    /// `dcr`：动态 client 注册（默认）。
    Dcr,
    /// `cimd`：kiana 在 `KIANA_API_BASE_URL/oauth` 上的 Client ID Metadata Document。
    Cimd,
}

impl McpClientRegistration {
    /// TS 里的字符串字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            McpClientRegistration::Dcr => "dcr",
            McpClientRegistration::Cimd => "cimd",
        }
    }
}

/// OAuth client 设置（用于不支持动态注册的 server）。
/// TS: `mcp-servers.ts#McpOAuthConfig`。
///
/// `Serialize` 按输入键序输出（见模块文档第 1 条），因此手写实现；`PartialEq` 忽略
/// [`Self::input_key_order`]（键序只影响写出，不影响配置相等）。
///
/// 本文件只校验值的类型与 URL 形状（`validate_oauth`），不读取、不展开 `clientSecret` 等
/// 字段里的 `${NAME}`/`!cmd` 引用；那是配置消费方的职责。
#[derive(Debug, Clone, Default)]
pub struct McpOAuthConfig {
    /// 预注册的 client id。
    pub client_id: Option<String>,
    /// client secret（可引用环境变量 `${NAME}` 或命令 `!cmd`）。
    pub client_secret: Option<String>,
    /// 回环回调服务器的端口（1..=65535）。
    pub callback_port: Option<u16>,
    /// 已注册的 redirect URI（必须是 loopback 上的 http URI）。
    pub callback_url: Option<String>,
    /// 请求的 scope（空格分隔）。
    pub scope: Option<String>,
    /// 动态注册时发送的 `client_name`（默认 `kiana`）。
    pub client_name: Option<String>,
    /// 没有 `clientId` 时 kiana 如何标识自己。
    pub client_registration: Option<McpClientRegistration>,
    /// 代替 discover 使用的授权服务器元数据文档 URL。
    pub auth_server_metadata_url: Option<String>,
    /// TS 保留输入对象的全部键，因此未识别的 oauth 字段也原样带回。
    pub extra: IndexMap<String, Value>,
    /// 校验时输入对象的键序；`Serialize` 用它还原 TS 的 JS 对象键序（模块文档第 1 条）。
    #[doc(hidden)]
    pub input_key_order: Vec<String>,
}

/// `auth: { provider }`：用 kiana provider 的 token 代替 OAuth。
/// TS: `McpHttpServerConfig.auth`。
///
/// `Deserialize` 用 `#[serde(flatten)]` 把未识别键收进 `extra`；`input_key_order` 标了
/// `#[serde(skip)]`，所以经 `Deserialize` 得到的值该字段恒为空，序列化时先写入 `provider`、
/// 再追加 `extra`；最终文本仍由 `kiana_json::js_object::stringify` 把整数索引键排到最前
/// （例如 `extra` 含 `"0"` 时 `0` 出现在 `provider` 之前）。本文件的校验路径不走
/// `Deserialize`：`build_auth` 手工记录输入键序。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAuthConfig {
    /// `/login <provider>` 的 provider id。
    pub provider: String,
    /// TS 保留输入对象的全部键，因此未识别的 auth 字段也原样带回。
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
    /// 校验时输入对象的键序；`Serialize` 用它还原 TS 的 JS 对象键序（模块文档第 1 条）。
    #[serde(skip)]
    #[doc(hidden)]
    pub input_key_order: Vec<String>,
}

impl McpOAuthConfig {
    /// 已知字段（按声明序），供 [`build_js_object`] 按输入键序重排。
    fn js_fields(&self) -> Vec<(&'static str, Value)> {
        let mut fields = Vec::new();
        if let Some(client_id) = &self.client_id {
            fields.push(("clientId", Value::String(client_id.clone())));
        }
        if let Some(client_secret) = &self.client_secret {
            fields.push(("clientSecret", Value::String(client_secret.clone())));
        }
        if let Some(callback_port) = self.callback_port {
            fields.push(("callbackPort", Value::Number(callback_port.into())));
        }
        if let Some(callback_url) = &self.callback_url {
            fields.push(("callbackUrl", Value::String(callback_url.clone())));
        }
        if let Some(scope) = &self.scope {
            fields.push(("scope", Value::String(scope.clone())));
        }
        if let Some(client_name) = &self.client_name {
            fields.push(("clientName", Value::String(client_name.clone())));
        }
        if let Some(client_registration) = self.client_registration {
            fields.push((
                "clientRegistration",
                Value::String(client_registration.as_str().to_string()),
            ));
        }
        if let Some(auth_server_metadata_url) = &self.auth_server_metadata_url {
            fields.push((
                "authServerMetadataUrl",
                Value::String(auth_server_metadata_url.clone()),
            ));
        }
        fields
    }

    fn to_js_object(&self) -> Value {
        build_js_object(&self.input_key_order, self.js_fields(), &self.extra)
    }
}

impl Serialize for McpOAuthConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_js_json(&self.to_js_object(), serializer)
    }
}

impl PartialEq for McpOAuthConfig {
    fn eq(&self, other: &Self) -> bool {
        self.client_id == other.client_id
            && self.client_secret == other.client_secret
            && self.callback_port == other.callback_port
            && self.callback_url == other.callback_url
            && self.scope == other.scope
            && self.client_name == other.client_name
            && self.client_registration == other.client_registration
            && self.auth_server_metadata_url == other.auth_server_metadata_url
            && self.extra == other.extra
    }
}

impl McpAuthConfig {
    /// 已知字段，供 [`build_js_object`] 按输入键序重排。
    fn js_fields(&self) -> Vec<(&'static str, Value)> {
        vec![("provider", Value::String(self.provider.clone()))]
    }

    fn to_js_object(&self) -> Value {
        build_js_object(&self.input_key_order, self.js_fields(), &self.extra)
    }
}

impl Serialize for McpAuthConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_js_json(&self.to_js_object(), serializer)
    }
}

impl PartialEq for McpAuthConfig {
    fn eq(&self, other: &Self) -> bool {
        self.provider == other.provider && self.extra == other.extra
    }
}

/// 按 JS `JSON.stringify` 的规则写一个可选数字（`60` 而不是 `60.0`、`1e+21` 而不是 `1e21`）。
///
/// serde 的 `f64` 序列化走 ryu（整数值会带 `.0`），与 TS 的 `JSON.stringify` 不同
/// （`ARCHITECTURE.md` §4.3 第 4 条）。这里用 [`serde_json::value::RawValue`] 原样写出
/// [`kiana_json::js_num::format_js_number`] 的文本。
///
/// [`McpServerConfigBase`] 自身仍派生 `Serialize`（独立序列化时按声明序）；两个 transport
/// 结构体的 `Serialize` 是手写的，走 [`kiana_json::js_object::stringify`]，数字规则相同。
///
/// 非有限的 `f64`（`NaN`/`±Infinity`）由 `format_js_number` 写成 `null`。JSON 输入里不会
/// 出现这类值（JSON 没有 NaN/Infinity），只有程序化构造的 `Option<f64>` 可能触发。
fn serialize_js_number_opt<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::Error;
    let Some(number) = value else {
        return serializer.serialize_none();
    };
    let text = kiana_json::js_num::format_js_number(*number);
    let raw = serde_json::value::RawValue::from_string(text).map_err(S::Error::custom)?;
    raw.serialize(serializer)
}

/// 用 `JSON.stringify` 的等价文本写出一个 [`Value`]（键序与数字格式都按 JS）。
///
/// 配置结构体的 `Serialize` 用它：TS 的 `validateMcpServerConfig` 返回输入对象的浅拷贝，
/// 写回 `mcp.json` 时键序就是调用方的输入顺序（spec `ca-builtin-extensions.md` §2.2.7）。
/// `RawValue` 在 `to_string` 与 `to_value` 两条路径上都保留 JSON 结构。
fn serialize_js_json<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::Error;
    let text = kiana_json::js_object::stringify(value);
    serde_json::value::RawValue::from_string(text)
        .map_err(S::Error::custom)?
        .serialize(serializer)
}

/// 组装要写出的 JS 对象：已知字段与未识别键都按 `recorded`（校验时的输入键序）就位；
/// 未记录的已知字段（程序化构造或校验后新增）按声明序追加，其余 `extra` 追加在后。
///
/// `recorded` 是输入对象 `Map` 的键序（`serde_json` 开了 `preserve_order`），因此对经
/// `validate_mcp_server_config` 构造的配置，每个输入键都会在第一轮被放到原位。JS 的
/// “整数索引键升序在前”不在这里处理：最终文本由 `kiana_json::js_object::stringify`
/// 统一重排，本函数只负责相对顺序。
///
/// 参数：
/// - `recorded`：期望的键序；其中既不在 `known` 也不在 `extra` 的名字会被静默跳过
///   （正常路径不会发生，因为 `known` + `extra` 覆盖输入的全部键）。
/// - `known`：已知字段的值，按声明序给出；未被 `recorded` 提到的按此顺序追加。
/// - `extra`：未识别的键，保持输入顺序；已经在第一轮插入过的键不会重复追加。
fn build_js_object(
    recorded: &[String],
    known: Vec<(&'static str, Value)>,
    extra: &IndexMap<String, Value>,
) -> Value {
    let mut map = Map::with_capacity(known.len() + extra.len());
    let mut used = vec![false; known.len()];
    // 第一轮：严格按调用方的输入键序放置已知字段与未识别键。
    for key in recorded {
        if let Some(index) = known.iter().position(|(name, _)| *name == key.as_str()) {
            used[index] = true;
            map.insert(key.clone(), known[index].1.clone());
        } else if let Some(value) = extra.get(key.as_str()) {
            map.insert(key.clone(), value.clone());
        }
    }
    // 第二轮：输入里没出现过的已知字段（程序化构造，或校验后新增）按声明序补齐。
    for (index, (name, value)) in known.into_iter().enumerate() {
        if !used[index] {
            map.insert(name.to_string(), value);
        }
    }
    // 第三轮：剩余的未识别键按输入顺序追加；已插入的键跳过，避免覆盖第一轮的位置。
    for (key, value) in extra {
        if !map.contains_key(key) {
            map.insert(key.clone(), value.clone());
        }
    }
    Value::Object(map)
}

/// `f64` → JSON 值。`NaN`/`±Infinity` 在 `JSON.stringify` 里写成 `null`。
///
/// `serde_json::Number::from_f64` 对非有限值返回 `None`，这里据此回退到 [`Value::Null`]。
/// 配置校验路径用 `Value::as_f64` 取值，JSON 本身没有 NaN/Infinity，只有程序化构造才会
/// 走到这个回退分支。
fn js_number_value(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// 两种 server 共有的配置字段。TS: `mcp-servers.ts#McpServerConfigBase`。
///
/// 字段顺序是当前稳定契约：`exposure`、`description`、`toolExposure`、`enabled`、`timeout`（改动需同步测试）。
/// `None` 表示 TS 的 `undefined`（缺省），取值语义在调用方：
/// `exposure` 缺省 `codemode`、`enabled` 缺省 `true`、`timeout` 缺省 60 秒。
///
/// 这些缺省值不在本文件实现：本文件的 [`get_mcp_tool_exposure`] 只负责 `exposure` 的
/// `codemode` 缺省；`enabled` 由下游按 `!= Some(false)` 判断，`timeout` 由下游按
/// `unwrap_or(60.0)` 取秒数（`kiana-builtin-extensions` 的 `mcp` 模块）。
///
/// 派生的 `Serialize` 只在**单独序列化** `McpServerConfigBase` 时使用；作为两个 transport
/// 变体的一部分写出时走各变体手写的 `js_fields`，两者字段顺序一致（声明序）。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfigBase {
    /// 默认暴露方式（缺省 `codemode`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<McpExposure>,
    /// 一句话描述 server 提供的能力。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 单个工具的暴露方式覆盖（精确名优先，其次按插入顺序匹配含 `*` 的模式）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_exposure: Option<IndexMap<String, McpExposure>>,
    /// `false` 表示保留条目但不连接（缺省 `true`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 每次请求的超时秒数（缺省 60）。按 JS `JSON.stringify` 的规则写出。
    #[serde(
        serialize_with = "serialize_js_number_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout: Option<f64>,
}

impl McpServerConfigBase {
    /// 已知公共字段（按声明序），供 [`build_js_object`] 按输入键序重排。
    ///
    /// 返回顺序同时是“程序化构造（没有 `input_key_order`）时”的写出顺序。
    /// `tool_exposure` 内部保持 `IndexMap` 的插入序，整数索引键由最终 stringify 重排；
    /// `timeout` 经 [`js_number_value`] 转成 JS 数字形态（非有限值 → `null`）。
    fn js_fields(&self) -> Vec<(&'static str, Value)> {
        let mut fields = Vec::new();
        if let Some(exposure) = self.exposure {
            fields.push(("exposure", Value::String(exposure.as_str().to_string())));
        }
        if let Some(description) = &self.description {
            fields.push(("description", Value::String(description.clone())));
        }
        if let Some(tool_exposure) = &self.tool_exposure {
            let mut map = Map::with_capacity(tool_exposure.len());
            for (tool, exposure) in tool_exposure {
                map.insert(tool.clone(), Value::String(exposure.as_str().to_string()));
            }
            fields.push(("toolExposure", Value::Object(map)));
        }
        if let Some(enabled) = self.enabled {
            fields.push(("enabled", Value::Bool(enabled)));
        }
        if let Some(timeout) = self.timeout {
            fields.push(("timeout", js_number_value(timeout)));
        }
        fields
    }
}

/// stdio transport 的 MCP server 配置。TS: `mcp-servers.ts#McpStdioServerConfig`。
///
/// `Serialize` 按校验输入的键序输出（模块文档第 1 条），因此手写实现；`PartialEq` 忽略
/// [`Self::input_key_order`]。
#[derive(Debug, Clone, Default)]
pub struct McpStdioServerConfig {
    /// 公共字段。
    pub base: McpServerConfigBase,
    /// transport 标签（缺省即 stdio；显式写了就原样保留）。
    pub type_: Option<String>,
    /// 启动命令。
    pub command: String,
    /// 命令参数。
    pub args: Option<Vec<String>>,
    /// 环境变量（值可引用环境变量 `${NAME}` 或命令 `!cmd`）。
    pub env: Option<IndexMap<String, String>>,
    /// 工作目录（相对路径按会话工作目录解析）。
    pub cwd: Option<String>,
    /// TS 返回的是输入对象的浅拷贝，因此保留未识别的键（含与本 transport 无关的键）。
    pub extra: IndexMap<String, Value>,
    /// 校验时输入对象的键序；`Serialize` 用它还原 TS 的 JS 对象键序（模块文档第 1 条）。
    #[doc(hidden)]
    pub input_key_order: Vec<String>,
}

impl McpStdioServerConfig {
    /// 已知字段：公共字段（声明序）之后依次是 `type`（仅在输入里有时）、`command`（必写）、
    /// `args`、`env`、`cwd`。`extra` 不在这里，由 [`build_js_object`] 的第三轮追加。
    fn js_fields(&self) -> Vec<(&'static str, Value)> {
        let mut fields = self.base.js_fields();
        if let Some(type_) = &self.type_ {
            fields.push(("type", Value::String(type_.clone())));
        }
        fields.push(("command", Value::String(self.command.clone())));
        if let Some(args) = &self.args {
            fields.push((
                "args",
                Value::Array(args.iter().cloned().map(Value::String).collect()),
            ));
        }
        if let Some(env) = &self.env {
            fields.push(("env", string_map_value(env)));
        }
        if let Some(cwd) = &self.cwd {
            fields.push(("cwd", Value::String(cwd.clone())));
        }
        fields
    }

    fn to_js_object(&self) -> Value {
        build_js_object(&self.input_key_order, self.js_fields(), &self.extra)
    }
}

impl Serialize for McpStdioServerConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_js_json(&self.to_js_object(), serializer)
    }
}

impl PartialEq for McpStdioServerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && self.type_ == other.type_
            && self.command == other.command
            && self.args == other.args
            && self.env == other.env
            && self.cwd == other.cwd
            && self.extra == other.extra
    }
}

/// streamable HTTP transport 的 MCP server 配置。TS: `mcp-servers.ts#McpHttpServerConfig`。
///
/// `Serialize` 按校验输入的键序输出（模块文档第 1 条），因此手写实现；`PartialEq` 忽略
/// [`Self::input_key_order`]。
#[derive(Debug, Clone, Default)]
pub struct McpHttpServerConfig {
    /// 公共字段。
    pub base: McpServerConfigBase,
    /// transport 标签（`"http"` 或原样保留的 `"streamable-http"`）。
    pub type_: Option<String>,
    /// server URL（http/https）。
    pub url: String,
    /// 额外请求头（值可引用环境变量 `${NAME}` 或命令 `!cmd`）。
    pub headers: Option<IndexMap<String, String>>,
    /// OAuth client 设置。
    pub oauth: Option<McpOAuthConfig>,
    /// 用 kiana provider 的 token 代替 OAuth。
    pub auth: Option<McpAuthConfig>,
    /// TS 返回的是输入对象的浅拷贝，因此保留未识别的键（含与本 transport 无关的键）。
    pub extra: IndexMap<String, Value>,
    /// 校验时输入对象的键序；`Serialize` 用它还原 TS 的 JS 对象键序（模块文档第 1 条）。
    #[doc(hidden)]
    pub input_key_order: Vec<String>,
}

impl McpHttpServerConfig {
    /// 已知字段：公共字段（声明序）之后依次是 `type`（仅在输入里有时）、`url`（必写）、
    /// `headers`、`oauth`、`auth`。嵌套的 `oauth`/`auth` 各自按自己的 `input_key_order`
    /// 递归重排。`extra` 不在这里，由 [`build_js_object`] 的第三轮追加。
    fn js_fields(&self) -> Vec<(&'static str, Value)> {
        let mut fields = self.base.js_fields();
        if let Some(type_) = &self.type_ {
            fields.push(("type", Value::String(type_.clone())));
        }
        fields.push(("url", Value::String(self.url.clone())));
        if let Some(headers) = &self.headers {
            fields.push(("headers", string_map_value(headers)));
        }
        if let Some(oauth) = &self.oauth {
            fields.push(("oauth", oauth.to_js_object()));
        }
        if let Some(auth) = &self.auth {
            fields.push(("auth", auth.to_js_object()));
        }
        fields
    }

    fn to_js_object(&self) -> Value {
        build_js_object(&self.input_key_order, self.js_fields(), &self.extra)
    }
}

impl Serialize for McpHttpServerConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serialize_js_json(&self.to_js_object(), serializer)
    }
}

impl PartialEq for McpHttpServerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && self.type_ == other.type_
            && self.url == other.url
            && self.headers == other.headers
            && self.oauth == other.oauth
            && self.auth == other.auth
            && self.extra == other.extra
    }
}

/// `IndexMap<String, String>` → JSON 对象（保持插入序，`stringify` 再按 JS 规则排整数键）。
fn string_map_value(map: &IndexMap<String, String>) -> Value {
    let mut object = Map::with_capacity(map.len());
    for (key, value) in map {
        object.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(object)
}

/// 一个 MCP server 的配置（stdio 或 streamable HTTP）。
/// TS: `mcp-servers.ts#McpServerConfig`。
///
/// `Serialize` 是 `untagged`：输出就是对应变体的对象本身（`type` 字段只在输入有时出现）。
///
/// 变体判别发生在 [`validate_mcp_server_config`] 的分支选择里，不是从 `type` 字符串直接
/// 映射：`url` 是字符串且 `type` 缺省或为 `http`/`streamable-http` 时为 [`Self::Http`]，
/// `command` 是字符串且 `type` 缺省或为 `stdio` 时为 [`Self::Stdio`]。因此没有 `type` 而
/// 同时带 `url` 与 `command` 的输入会得到 `Http`，`command` 收进 `extra`；写了
/// `type: "stdio"` 才会压过 `url`。
///
/// 本类型没有 `Deserialize`：从 JSON 还原必须走 [`validate_mcp_server_config`]，否则会绕过
/// 类型检查与键序记录。`PartialEq` 由变体与各自的字段比较（忽略 `input_key_order`）。
///
/// 变体尺寸差异（HTTP 632B vs stdio 360B）来自 TS 的扁平配置形状；为保持下游按
/// `McpServerConfig::Stdio(cfg)` / `Http(cfg)` 直接取字段的 API（规划者冻结的形状），
/// 不把变体装箱，局部放宽 clippy 的 `large_enum_variant`（§4.10 允许带理由的局部例外）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    /// stdio transport。
    Stdio(McpStdioServerConfig),
    /// streamable HTTP transport。
    Http(McpHttpServerConfig),
}

impl McpServerConfig {
    /// 公共字段（两种变体都有）。
    pub fn base(&self) -> &McpServerConfigBase {
        match self {
            McpServerConfig::Stdio(config) => &config.base,
            McpServerConfig::Http(config) => &config.base,
        }
    }

    /// 公共字段的可变引用。
    pub fn base_mut(&mut self) -> &mut McpServerConfigBase {
        match self {
            McpServerConfig::Stdio(config) => &mut config.base,
            McpServerConfig::Http(config) => &mut config.base,
        }
    }
}

/// `*` 通配的工具名模式转成正则（`^...$`）。转义集合逐字：`[.+?^${}()|[\]\\]`。
///
/// 注意不含 `*`（它是通配符），也不含 `-`、`/`、`,`：这些字符按字面量处理。
///
/// `*` 展开成 JS 正则的 `.*`，而 JS 的 `.`（无 `s` 标志）不匹配行终止符 `\n`、`\r`、U+2028、
/// U+2029；Rust `regex` 的 `.` 只排除 `\n`。因此这里写显式字符类而不是 `.*`，保持匹配集合
/// 与非 `s` 标志正则的匹配集合一致（例如 `"a\rb"` 不匹配 `*`）。
///
/// 实现：按 `*` 切分，段内逐字符转义，相邻两段之间插入 `JS_DOT` 后跟 `*`。首尾的 `*`
/// 产生空段，因此 `*` 可以匹配空串、`a*` 可以匹配 `a`；`*` 之间的字面量必须按序出现。
fn tool_pattern_source(pattern: &str) -> String {
    /// JS 正则 `.` 的等价字符类。
    const JS_DOT: &str = "[^\\n\\r\\u{2028}\\u{2029}]";
    let mut source = String::with_capacity(pattern.len() + 8);
    for (index, part) in pattern.split('*').enumerate() {
        if index > 0 {
            source.push_str(JS_DOT);
            source.push('*');
        }
        for ch in part.chars() {
            if matches!(
                ch,
                '.' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
            ) {
                source.push('\\');
            }
            source.push(ch);
        }
    }
    source
}

/// 把模式编译成锚定的正则。输入只会由转义后的字面量与 JS `.` 的字符类（后跟 `*`）组成，必然合法。
fn tool_pattern_regex(pattern: &str) -> Regex {
    Regex::new(&format!("^{}$", tool_pattern_source(pattern)))
        .expect("escaped tool pattern is always a valid regex")
}

/// 构造工具名匹配器：条目是精确名或含 `*` 的模式，`*` 匹配任意字符；只有 `*` 是特殊字符。
/// TS: `mcp-servers.ts#createToolNameMatcher`。
///
/// # 参数与返回
///
/// 闭包复制/编译了 `entries` 的数据（精确名进 `HashSet`，含 `*` 的条目编译成正则并缓存），
/// 可反复调用；但返回类型仍捕获 `entries` 的生命周期——matcher 存活期间 `entries` 不能被
/// 释放或移动（可先结束其作用域或显式 drop matcher，见 `kiana-session/src/sdk.rs`）。
/// 匹配是大小写敏感的。
///
/// # 匹配规则
///
/// - 不含 `*` 的条目按**完整等值**匹配（空字符串条目只匹配空工具名）。
/// - 含 `*` 的条目按模式匹配，`*` 可匹配空串，其余字符按字面量处理（`.`、`?`、`[` 等
///   虽是正则元字符，但会被转义成字面量；转义集合见 `tool_pattern_source`）。
/// - 与 [`get_mcp_tool_exposure`] 的一处差别：本函数对含 `*` 的条目**从不**做精确名匹配；
///   那里会先精确查 `toolExposure` 的键，所以恰好名为 `a*b` 的工具名能命中配置里的
///   `a*b` 键，而本函数把该键当作模式。
pub fn create_tool_name_matcher<S: AsRef<str>>(entries: &[S]) -> impl Fn(&str) -> bool {
    let mut names = HashSet::new();
    let mut patterns = Vec::new();
    // 分类：含 `*` 的条目是模式，其余是精确名。模式在构造时编译一次，匹配时不再做正则解析。
    for entry in entries {
        let entry = entry.as_ref();
        if entry.contains('*') {
            patterns.push(tool_pattern_regex(entry));
        } else {
            names.insert(entry.to_string());
        }
    }
    // 每次调用先查精确名集合，再顺次试模式；任一命中即 `true`。
    move |name: &str| names.contains(name) || patterns.iter().any(|pattern| pattern.is_match(name))
}

/// 工具是否来自 MCP：`mcp__<server>__<tool>` 形式，或三个资源工具之一。
/// TS: `mcp-servers.ts#isMcpToolName`。
///
/// 只做字符串前缀/等值判断：不解析出 server 名，也不要求 `mcp__` 后面有内容
/// （单独的 `mcp__` 也返回 `true`，测试 `is_mcp_tool_name_and_constants` 锁定了这一点）。
/// 资源工具名必须完全相等，`list_mcp_resources_extra` 之类不算。
pub fn is_mcp_tool_name(name: &str) -> bool {
    name.starts_with("mcp__") || MCP_RESOURCE_TOOLS.contains(&name)
}

/// server 的命名空间：`mcp__<server>`，`-` 换成 `_`（与工具名一致）。
/// TS: `mcp-servers.ts#mcpNamespace`。
///
/// 该函数不做校验也不去重；它只做字符串替换。因此 `work-files` 与 `work_files` 会映射到
/// 同一个命名空间（`mcp__work_files`），注册时的冲突检查依赖这个等式（回归 #10239，
/// 见 `kiana-extensions` 的 `prepare_mcp_server`）。server 名本身由
/// [`validate_mcp_server_config`] 限制为 `[A-Za-z0-9_-]`，所以替换不会产生其它歧义。
pub fn mcp_namespace(server: &str) -> String {
    format!("mcp__{}", server.replace('-', "_"))
}

/// JSON 值是否是普通对象；是则返回内部 `Map` 的借用，否则 `None`。
///
/// 这里用 `Value::as_object`：`null`、数组、字符串、数字、布尔都返回 `None`。配置字段
/// 只要普通对象，数组不能当作 record 使用。
fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// 是否是“所有值都是字符串”的对象，用于 `headers`、`env`。
///
/// 空对象满足条件（`Iterator::all` 对空迭代返回 `true`），因此 `{}` 合法；非对象或任一
/// 值为 `null`/数字等则返回 `false`。
fn is_string_record(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.values().all(Value::is_string))
}

/// 值是否是**现名**的 exposure 字面量（`codemode`/`deferred`/`direct`/`hidden`）。
///
/// 旧名 `codemode-deferred` 在这里是 `false`；它只在 `resolve_exposure_aliases` 阶段被
/// 换掉，因此校验时看到的已经是现名。
fn is_exposure(value: &Value) -> bool {
    value.as_str().and_then(McpExposure::parse).is_some()
}

/// 旧 exposure 名的替换；其它值原样返回。
///
/// 只识别字符串 `codemode-deferred`。`null`、数字等原样返回，交由后续 `is_exposure` 报
/// “值非法”，而不是在这里提前报错。
fn resolve_exposure_alias_value(value: &Value) -> Value {
    match value.as_str() {
        Some("codemode-deferred") => Value::String("codemode".to_string()),
        _ => value.clone(),
    }
}

/// 拷贝输入对象，并把 `exposure` 与 `toolExposure` 的每个值中的旧名换成现名。
///
/// 只处理键 `exposure` 与 `toolExposure`：
/// - `exposure` 存在时总是重新插入（即使值没变），键位置不变；
/// - `toolExposure` 只有是对象时才重建（重建后仍是同一对象位置）；不是对象则原样保留，
///   随后由 `validate_mcp_server_config` 报 “must map tool names to exposures”；
/// - 其它键与嵌套值不被检查。
///
/// 与 TS 的差异：TS 这里是 `{...input}` 浅拷贝（嵌套对象与调用方共享），Rust 的
/// `Map::clone` 会递归复制 `Value`。本函数只用于生成本次校验的独立配置，输入对象在调用
/// 期间不被修改，因此这一差异不可观察。
fn resolve_exposure_aliases(value: &Map<String, Value>) -> Map<String, Value> {
    let mut resolved = value.clone();
    if let Some(exposure) = resolved.get("exposure") {
        let exposure = resolve_exposure_alias_value(exposure);
        resolved.insert("exposure".to_string(), exposure);
    }
    let tool_exposure = resolved.get("toolExposure").cloned();
    if let Some(Value::Object(entries)) = tool_exposure {
        let mut mapped = Map::new();
        for (tool, entry) in &entries {
            mapped.insert(tool.clone(), resolve_exposure_alias_value(entry));
        }
        resolved.insert("toolExposure".to_string(), Value::Object(mapped));
    }
    resolved
}

/// 一个工具的实际暴露方式：`toolExposure` 精确名优先，其次按插入顺序的第一个匹配模式，
/// 否则用 server 的 `exposure`（缺省 `codemode`）。
/// TS: `mcp-servers.ts#getMcpToolExposure`。
///
/// 优先级（第一个命中即返回）：
/// 1. `toolExposure[tool_name]` 的精确键查找。注意这一步对**含 `*` 的键也生效**：工具名
///    恰好叫 `a*b` 时会命中同名键，而不会被当作模式。
/// 2. 按 `IndexMap` 的插入顺序遍历含 `*` 的键，第一个匹配的模式生效。多个模式重叠时
///    先写的赢（测试 `resolves_per_tool_exposure_from_exact_names_then_patterns_in_order`
///    锁定了这一点）。
/// 3. server 级 `exposure`，缺省 [`McpExposure::Codemode`]。
///
/// 模式匹配使用本文件的 `tool_pattern_source` 规则（只有 `*` 特殊，`.` 不匹配行终止符）。
/// 本函数每次调用都重新编译模式正则，没有缓存；配置校验不检查模式文本，任意键都会按
/// 转义规则编译成正则。
pub fn get_mcp_tool_exposure(config: &McpServerConfig, tool_name: &str) -> McpExposure {
    let base = config.base();
    if let Some(overrides) = &base.tool_exposure {
        // 第一优先级：精确名（含形如 `a*b` 的键，它们在这里按字面量命中）。
        if let Some(exposure) = overrides.get(tool_name) {
            return *exposure;
        }
        // 第二优先级：按插入顺序找第一个匹配的 `*` 模式。
        for (pattern, exposure) in overrides {
            if pattern.contains('*') && tool_pattern_regex(pattern).is_match(tool_name) {
                return *exposure;
            }
        }
    }
    // 兜底：server 级 exposure；TS 的 `?? "codemode"`。
    base.exposure.unwrap_or(McpExposure::Codemode)
}

/// 本机回环 host：`localhost`、`127.0.0.1`、`::1`。
///
/// 只按 URL 里的字面 host 判断，不做 DNS 解析：`localhost.example.com` 之类不是回环，
/// `[0:0:0:0:0:0:0:1]` 会在 `url` 解析后归一为 `::1` 从而命中。domain 比较忽略大小写
/// （`LOCALHOST` 也算）。
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip == Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(ip)) => ip == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

/// redirect URI 能否由 kiana 的回环回调服务器提供：`http:`、host 是 `localhost`/`127.0.0.1`/`[::1]`、
/// 且查询串与片段为空。
/// TS: `mcp-servers.ts#isLoopbackRedirectUri`。
///
/// WHATWG 会把单独的 `?`/`#` 规范化为**空**查询/片段（`url.search === ""`），因此这里把
/// `Some("")` 也视为空（`url` crate 对 `http://x/?` 返回 `query() == Some("")`）。
pub fn is_loopback_redirect_uri(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "http"
        && is_loopback_host(&url)
        && url.query().is_none_or(str::is_empty)
        && url.fragment().is_none_or(str::is_empty)
}

/// `callbackPort` 是否是一个端口号（`Number.isInteger` 且 1..=65535）。
///
/// JSON 的 `8080` 与 `8080.0` 都接受（`as_f64` 后 `fract() == 0.0`），`8080.5`、`0`、
/// `65536`、`null`、字符串都不接受。返回 `u16` 供 [`McpOAuthConfig::callback_port`] 使用。
fn callback_port(value: &Value) -> Option<u16> {
    let number = value.as_f64()?;
    if number.fract() == 0.0 && (1.0..=65535.0).contains(&number) {
        Some(number as u16)
    } else {
        None
    }
}

/// 校验 `oauth` 字段；错误串**不带** `server "<name>": ` 前缀（调用方加）。
/// TS: `mcp-servers.ts#validateOAuth`。
///
/// 只做类型与 URL 形状检查：不访问服务器、不读取/展开 `clientSecret` 里的占位符。
/// 检查顺序即报错优先级（任一步失败立即返回）：对象 → `clientId`/`clientSecret` 类型 →
/// `callbackPort` 范围 → `callbackUrl` 形状与端口一致性 → `scope`/`clientName` →
/// `clientRegistration` 的组合约束 → `authServerMetadataUrl` 的 scheme。
///
/// 两条容易误读的边界：
/// - `callbackUrl` 与 `callbackPort` 的端口只在**两者都显式带端口**时比较。URL 里没写
///   端口（例如 `http://[::1]/cb`）就不与 `callbackPort` 冲突，测试
///   `validates_the_oauth_callback_url_scope_and_client_name` 覆盖了该组合。
/// - `cimd` 在**提供了 `callbackUrl`** 时额外要求：host 不是 `[::1]` 且 path 恰为
///   `/callback`。因此 `[::1]` 能通过回环检查，却会在 `cimd` 组合检查里被拒（错误文案
///   只说 “on localhost or 127.0.0.1 with path /callback”）。
fn validate_oauth(value: Option<&Value>) -> Result<(), String> {
    // `oauth` 键缺失（`None`）是合法的（`Option` 只表示键缺失）；显式 `null` 不是
    // `None` 而是 `Some(Null)`，会走到下面的“必须是对象”错误。
    let Some(value) = value else {
        return Ok(());
    };
    let Some(map) = is_record(value) else {
        return Err("oauth must be an object".to_string());
    };
    // 以下按字段顺序逐个检查；先失败的先报，后面的字段不再看。
    if let Some(client_id) = map.get("clientId")
        && !client_id.is_string()
    {
        return Err("oauth.clientId must be a string".to_string());
    }
    if let Some(client_secret) = map.get("clientSecret")
        && !client_secret.is_string()
    {
        return Err("oauth.clientSecret must be a string".to_string());
    }
    if let Some(port) = map.get("callbackPort")
        && callback_port(port).is_none()
    {
        return Err("oauth.callbackPort must be a port number".to_string());
    }
    // callbackUrl：必须先是字符串，再通过回环 http URI 检查（查询串/片段必须为空）。
    if let Some(callback_url) = map.get("callbackUrl") {
        let Some(callback_url) = callback_url.as_str() else {
            return Err(
                "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                    .to_string(),
            );
        };
        if !is_loopback_redirect_uri(callback_url) {
            return Err(
                "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
                    .to_string(),
            );
        }
        // 能通过 `is_loopback_redirect_uri` 说明 `Url::parse` 已经成功，这里不会失败。
        let parsed = Url::parse(callback_url).expect("a loopback redirect URI parses");
        // 只有 `parsed.port()` 为 `Some` 且 `callbackPort` 有效时才比较；URL 省略端口或写
        // 默认端口（如 http 的 `:80`，`url` crate/WHATWG 会规范化掉）时都不比较。
        let url_port = parsed.port();
        let port = map.get("callbackPort").and_then(callback_port);
        if let (Some(url_port), Some(port)) = (url_port, port)
            && url_port != port
        {
            return Err(
                "oauth.callbackUrl and oauth.callbackPort name different ports".to_string(),
            );
        }
    }
    if let Some(scope) = map.get("scope")
        && !scope.is_string()
    {
        return Err("oauth.scope must be a string".to_string());
    }
    // clientName：必须是字符串，且按 JS `String.prototype.trim` 去掉首尾空白后非空
    // （因此 `" "` 被拒；`null` 也会落到 `_` 分支被拒）。
    if let Some(client_name) = map.get("clientName") {
        match client_name.as_str() {
            Some(name) if !js_trim(name).is_empty() => {}
            _ => return Err("oauth.clientName must be a non-empty string".to_string()),
        }
    }
    // clientRegistration：`dcr` 没有额外约束；`cimd` 与 `clientId`/`clientName` 互斥，
    // 且若提供了 callbackUrl，还要求路径为 `/callback`、host 不是 `[::1]`；其它值
    // （含大小写变体）直接报错。
    if let Some(registration) = map.get("clientRegistration") {
        let registration = registration.as_str();
        if registration != Some("dcr") {
            if registration != Some("cimd") {
                return Err("oauth.clientRegistration must be \"dcr\" or \"cimd\"".to_string());
            }
            // 用 `contains_key` 表示“字段存在即冲突”；能走到这里时两个字段的值已经通过
            // 前面的类型检查（缺失则视为不存在）。
            if map.contains_key("clientId") || map.contains_key("clientName") {
                return Err(
                    "oauth.clientRegistration \"cimd\" cannot be combined with oauth.clientId or oauth.clientName"
                        .to_string(),
                );
            }
            // 若 callbackUrl 存在，它已经通过上面的回环检查（必可解析）；这里的 `if let Ok`
            // 只是防御性写法，同时也跳过 callbackUrl 缺失的情况。
            if let Some(callback) = map.get("callbackUrl").and_then(Value::as_str)
                && let Ok(callback) = Url::parse(callback)
            {
                let is_ipv6_loopback =
                    matches!(callback.host(), Some(Host::Ipv6(ip)) if ip == Ipv6Addr::LOCALHOST);
                if is_ipv6_loopback || callback.path() != "/callback" {
                    return Err(
                            "oauth.clientRegistration \"cimd\" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback"
                                .to_string(),
                        );
                }
            }
        }
    }
    // authServerMetadataUrl：必须是 https，或 http + 回环 host；非字符串、不可解析、
    // 其它 scheme 都统一报同一条错误。
    if let Some(metadata_url) = map.get("authServerMetadataUrl") {
        let parsed = metadata_url
            .as_str()
            .and_then(|value| Url::parse(value).ok());
        let valid = parsed.is_some_and(|url| {
            url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(&url))
        });
        if !valid {
            return Err(
                "oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]"
                    .to_string(),
            );
        }
    }
    Ok(())
}

/// 把“字符串 → 字符串”的 JSON 对象转成 `IndexMap`，保持输入键序。
///
/// 非对象、或任一值不是字符串时返回 `None`（任一项失败即整体失败）。校验路径只会在
/// `is_string_record` 通过后调用它，因此实际不会失败；返回 `Option` 是给调用方一个
/// 统一的“形状不符”表示。
fn string_map(value: &Value) -> Option<IndexMap<String, String>> {
    let map = value.as_object()?;
    let mut out = IndexMap::with_capacity(map.len());
    for (key, entry) in map {
        out.insert(key.clone(), entry.as_str()?.to_string());
    }
    Some(out)
}

/// 两种 transport 共同识别的键（加上各自 transport 的键，其余进 `extra`）。
const BASE_KEYS: [&str; 5] = [
    "exposure",
    "description",
    "toolExposure",
    "enabled",
    "timeout",
];

/// 键是否属于本 transport 的已知字段：公共键或该 transport 自己的键。
///
/// 注意 `type` 在两种 transport 的 `transport_keys` 里都出现，因此它不是 `extra`；
/// 另一 transport 特有的键（如 stdio 配置里的 `url`）则落入 `extra`。
fn is_known_key(key: &str, transport_keys: &[&str]) -> bool {
    BASE_KEYS.contains(&key) || transport_keys.contains(&key)
}

/// 输入对象的键序（`Map` 启用了 `preserve_order`，即 TS 的插入序）。
fn key_order(value: &Map<String, Value>) -> Vec<String> {
    value.keys().cloned().collect()
}

/// 收集不属于本变体的键，保持输入顺序；值原样复制（不做类型校验）。
///
/// 这是“TS 返回输入对象浅拷贝”的落点：`url`、`command` 等其它 transport 的键，以及任意
/// 自定义键都会留在这里，并在 `Serialize` 时回到原来的相对位置。
fn extra_fields(value: &Map<String, Value>, transport_keys: &[&str]) -> IndexMap<String, Value> {
    value
        .iter()
        .filter(|(key, _)| !is_known_key(key, transport_keys))
        .map(|(key, entry)| (key.clone(), entry.clone()))
        .collect()
}

/// 从**已通过校验**的对象提取公共字段。
///
/// 取值统一用 `and_then(Value::as_*)`：校验阶段已经保证类型，或在类型不符时提前返回，
/// 所以这里取不到只可能意味着键缺失，不会掩盖用户错误。`tool_exposure` 再过滤一次
/// “能否解析成 [`McpExposure`]”，同样由校验保证；`timeout` 接受 JSON 整数与浮点
/// （`as_f64`）。
fn build_base(value: &Map<String, Value>) -> McpServerConfigBase {
    McpServerConfigBase {
        exposure: value
            .get("exposure")
            .and_then(Value::as_str)
            .and_then(McpExposure::parse),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        tool_exposure: value
            .get("toolExposure")
            .and_then(Value::as_object)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(tool, exposure)| {
                        Some((tool.clone(), McpExposure::parse(exposure.as_str()?)?))
                    })
                    .collect()
            }),
        enabled: value.get("enabled").and_then(Value::as_bool),
        timeout: value.get("timeout").and_then(Value::as_f64),
    }
}

/// 从**已通过 `validate_oauth`** 的对象构建 [`McpOAuthConfig`]，并记录输入键序。
///
/// `callback_port` 复用 [`callback_port`] 的范围检查；`client_registration` 只识别
/// `"dcr"`/`"cimd"` 两个字面量（其它值已被校验拒绝，这里映射为 `None` 只是兜底）。
/// `extra` 收集未识别的 oauth 键，写回时保持原位。
fn build_oauth(value: &Map<String, Value>) -> McpOAuthConfig {
    McpOAuthConfig {
        client_id: value
            .get("clientId")
            .and_then(Value::as_str)
            .map(str::to_string),
        client_secret: value
            .get("clientSecret")
            .and_then(Value::as_str)
            .map(str::to_string),
        callback_port: value.get("callbackPort").and_then(callback_port),
        callback_url: value
            .get("callbackUrl")
            .and_then(Value::as_str)
            .map(str::to_string),
        scope: value
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string),
        client_name: value
            .get("clientName")
            .and_then(Value::as_str)
            .map(str::to_string),
        client_registration: match value.get("clientRegistration").and_then(Value::as_str) {
            Some("dcr") => Some(McpClientRegistration::Dcr),
            Some("cimd") => Some(McpClientRegistration::Cimd),
            _ => None,
        },
        auth_server_metadata_url: value
            .get("authServerMetadataUrl")
            .and_then(Value::as_str)
            .map(str::to_string),
        extra: extra_fields(
            value,
            &[
                "clientId",
                "clientSecret",
                "callbackPort",
                "callbackUrl",
                "scope",
                "clientName",
                "clientRegistration",
                "authServerMetadataUrl",
            ],
        ),
        input_key_order: key_order(value),
    }
}

/// 从**已通过校验**的 `auth` 对象构建 [`McpAuthConfig`]。
///
/// “`provider` 必须是非空字符串”由 `validate_mcp_server_config` 的 HTTP 分支保证；
/// 这里缺失时用空串兜底（防御性，正常路径不会发生）。`extra` 收集 provider 之外的键。
fn build_auth(value: &Map<String, Value>) -> McpAuthConfig {
    McpAuthConfig {
        provider: value
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        extra: extra_fields(value, &["provider"]),
        input_key_order: key_order(value),
    }
}

/// 从**已通过校验**的对象构建 stdio 变体。
///
/// 调用前提：`command` 是字符串（分支条件），且 `args`/`env`/`cwd` 已通过类型检查。
/// `args` 用 `filter_map(Value::as_str)` 只保留字符串元素（校验已保证全为字符串）；`env`
/// 复用 [`string_map`]。`extra` 收集公共键与 `type`/`command`/`args`/`env`/`cwd` 之外的键，
/// 因此 stdio 条目可以带着 `url` 之类的其它 transport 键。
fn build_stdio(value: &Map<String, Value>) -> McpStdioServerConfig {
    McpStdioServerConfig {
        base: build_base(value),
        type_: value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string),
        command: value
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        args: value.get("args").and_then(Value::as_array).map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        }),
        env: value.get("env").and_then(string_map),
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_string),
        extra: extra_fields(value, &["type", "command", "args", "env", "cwd"]),
        input_key_order: key_order(value),
    }
}

/// 从**已通过校验**的对象构建 HTTP 变体。
///
/// 调用前提：`url` 是字符串（分支条件），`headers`/`oauth`/`auth` 已通过各自检查。
/// `oauth`/`auth` 只在值是对象时构建（校验已保证类型）；`extra` 收集公共键与
/// `type`/`url`/`headers`/`oauth`/`auth` 之外的键。
fn build_http(value: &Map<String, Value>) -> McpHttpServerConfig {
    McpHttpServerConfig {
        base: build_base(value),
        type_: value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string),
        url: value
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        headers: value.get("headers").and_then(string_map),
        oauth: value
            .get("oauth")
            .and_then(Value::as_object)
            .map(build_oauth),
        auth: value.get("auth").and_then(Value::as_object).map(build_auth),
        extra: extra_fields(value, &["type", "url", "headers", "oauth", "auth"]),
        input_key_order: key_order(value),
    }
}

/// 校验 `mcpServers` 形状里的一项。成功时返回别名已替换的配置副本；失败时返回错误消息
/// （**错误是值**，不是异常：调用方决定是否上报）。
/// TS: `mcp-servers.ts#validateMcpServerConfig`。
///
/// 校验顺序与错误文案是当前稳定契约（改动需同步测试）；显式的 JSON `null` 视为“值非法”
/// （`Option` 只表示键缺失）。
///
/// # 参数与返回
///
/// - `name`：`mcpServers` 对象的键。它先被 `^[A-Za-z0-9_-]+$` 校验；不合法时错误串
///   **没有** `server "<name>": ` 前缀（名字本身不合法，引号里就是它）。
/// - `raw`：这一项的 JSON 值。必须是对象；显式 `null` 也报 “must be an object”。
/// - `Ok`：从**别名替换后**的对象重新构建的配置（`input_key_order` 记录的是 `raw` 的键序，
///   键没有增删，只有 `exposure`/`toolExposure` 的值可能变化）。
/// - `Err`：可直接展示的英文文案，除名字/对象错误外都带 `server "<name>": ` 前缀。
///   函数不 panic、不做 IO。
///
/// # 检查顺序
///
/// 1. server 名 → 2. 输入是对象 → 3. 别名替换 → 4. 公共字段（`exposure`、`toolExposure`
/// 逐项、`enabled`、`description`、`timeout`）→ 5. `type: "sse"` 直接拒绝 →
/// 6. transport 分派（HTTP 先）→ 7. 该 transport 的字段检查 → 8. 都不匹配时的兜底错误。
///
/// 第 4 步的顺序决定多条字段同时非法时先报哪一条；第 6-7 步只检查被选中 transport 的键，
/// 另一个 transport 的键不做类型检查、原样进入 `extra`。
pub fn validate_mcp_server_config(name: &str, raw: &Value) -> Result<McpServerConfig, String> {
    // 第 1 步：名字。错误串不带 `server "<name>": ` 前缀。
    if !server_name_regex().is_match(name) {
        return Err(format!(
            "invalid server name \"{name}\" (use letters, digits, \"_\" and \"-\")"
        ));
    }
    // 第 2 步：必须是 JSON 对象（数组、标量、`null` 都拒绝）。
    let Some(raw_map) = raw.as_object() else {
        return Err(format!("server \"{name}\" must be an object"));
    };
    // 第 3 步：先替换 exposure 旧名，后续校验与构建都基于这份副本；这样
    // `codemode-deferred` 能通过第 4 步，且返回的配置里存的是 `codemode`。
    let value = resolve_exposure_aliases(raw_map);

    // 第 4 步：公共字段。按固定顺序检查，先失败的先返回。
    if let Some(exposure) = value.get("exposure")
        && !is_exposure(exposure)
    {
        return Err(format!(
            "server \"{name}\": exposure must be one of {MCP_EXPOSURE_LIST}"
        ));
    }
    if let Some(tool_exposure) = value.get("toolExposure") {
        let Some(entries) = tool_exposure.as_object() else {
            return Err(format!(
                "server \"{name}\": toolExposure must map tool names to exposures"
            ));
        };
        for (tool, exposure) in entries {
            if !is_exposure(exposure) {
                return Err(format!(
                    "server \"{name}\": toolExposure \"{tool}\" must be one of {MCP_EXPOSURE_LIST}"
                ));
            }
        }
    }
    if let Some(enabled) = value.get("enabled")
        && !enabled.is_boolean()
    {
        return Err(format!("server \"{name}\": enabled must be a boolean"));
    }
    if let Some(description) = value.get("description")
        && !description.is_string()
    {
        return Err(format!("server \"{name}\": description must be a string"));
    }
    if let Some(timeout) = value.get("timeout") {
        let valid = timeout.as_f64().is_some_and(|seconds| seconds > 0.0);
        if !valid {
            return Err(format!(
                "server \"{name}\": timeout must be a positive number of seconds"
            ));
        }
    }

    // 第 5 步：`type` 的解析与 sse 拒绝。`"sse"` 不论有没有 url/command 都先报错。
    let type_value = value.get("type");
    let type_str = type_value.and_then(Value::as_str);
    if type_str == Some("sse") {
        return Err(format!(
            "server \"{name}\": legacy SSE transport is not supported; use the streamable HTTP URL"
        ));
    }
    // TS compares `type === undefined` (key absent), not `typeof type !== "string"`: an explicit
    // non-string `type` (including JSON null) matches neither branch and falls through to the
    // final error.
    let type_absent = type_value.is_none();
    let type_matches_http =
        type_absent || matches!(type_str, Some("http") | Some("streamable-http"));
    let type_matches_stdio = type_absent || type_str == Some("stdio");

    // 第 6 步：HTTP 分支先于 stdio 判断。`url` 必须是字符串；`type` 缺省或为
    // http/streamable-http。同时有 url 与 command 且没有 type 时走这里，command 进 extra。
    let http_branch = value.get("url").is_some_and(Value::is_string) && type_matches_http;
    if http_branch {
        // url 必须是可解析、scheme 为 http/https 的 URL；其它 scheme、相对路径、
        // `file:` 等都以同一条文案拒绝。
        let url_text = value.get("url").and_then(Value::as_str).unwrap_or_default();
        let parsed = Url::parse(url_text)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"));
        let Some(url) = parsed else {
            return Err(format!(
                "server \"{name}\": url must be an http or https URL"
            ));
        };
        if let Some(headers) = value.get("headers")
            && !is_string_record(headers)
        {
            return Err(format!(
                "server \"{name}\": headers must map names to strings"
            ));
        }
        // oauth 的错误串由 validate_oauth 返回时不带前缀，这里统一补上。
        if let Some(oauth) = value.get("oauth") {
            validate_oauth(Some(oauth)).map_err(|error| format!("server \"{name}\": {error}"))?;
        }
        // auth.provider 必须是非空字符串；并且带 auth 的 server 只允许 https，
        // 或 http + 回环 host（`is_loopback_host` 的三个 host）。
        if let Some(auth) = value.get("auth") {
            let provider = auth
                .as_object()
                .and_then(|auth| auth.get("provider"))
                .and_then(Value::as_str);
            if provider.is_none_or(str::is_empty) {
                return Err(format!(
                    "server \"{name}\": auth.provider must be a provider name"
                ));
            }
            if url.scheme() != "https" && !is_loopback_host(&url) {
                return Err(format!(
                    "server \"{name}\": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]"
                ));
            }
        }
        return Ok(McpServerConfig::Http(build_http(&value)));
    }

    // 第 7 步：stdio 分支。`command` 必须是字符串；`type` 缺省或为 `stdio`。
    // 只有 `type: "stdio"` 能在同时有 url 时压过上面的 HTTP 分支（fixture
    // `both-transports-stdio`）。
    let stdio_branch = value.get("command").is_some_and(Value::is_string) && type_matches_stdio;
    if stdio_branch {
        // args：必须是字符串数组（空数组合法）；null、混合类型都拒绝。
        if let Some(args) = value.get("args") {
            let valid = args
                .as_array()
                .is_some_and(|args| args.iter().all(Value::is_string));
            if !valid {
                return Err(format!(
                    "server \"{name}\": args must be an array of strings"
                ));
            }
        }
        if let Some(env) = value.get("env")
            && !is_string_record(env)
        {
            return Err(format!("server \"{name}\": env must map names to strings"));
        }
        if let Some(cwd) = value.get("cwd")
            && !cwd.is_string()
        {
            return Err(format!("server \"{name}\": cwd must be a string"));
        }
        return Ok(McpServerConfig::Stdio(build_stdio(&value)));
    }

    // 第 8 步：两个分支都不匹配。典型情况：type 与可用字段不匹配（`type:"stdio"` + 只有
    // url、`type:"http"` + 只有 command、type 是非字符串），或既没有 url 也没有 command。
    // 这条兜底文案不区分具体原因（所有不匹配的情况共用同一条错误）。
    Err(format!(
        "server \"{name}\" needs either \"command\" (stdio) or \"url\" (streamable HTTP)"
    ))
}

/// server 名的白名单：一个或多个 ASCII 字母、数字、`_`、`-`，整串锚定。
///
/// 用 `LazyLock` 只编译一次；正则字面量固定，`expect` 不会触发。名字含空格、点、非 ASCII
/// 或为空都会被 [`validate_mcp_server_config`] 拒绝。
fn server_name_regex() -> &'static Regex {
    static SERVER_NAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("^[A-Za-z0-9_-]+$").expect("server name regex is valid"));
    &SERVER_NAME
}

/// 扩展注册的 MCP server。TS: `mcp-servers.ts#RegisteredMcpServer`。
///
/// 字段顺序与 TS 接口一致：`name`、`config`、`extensionPath`（`Serialize` 用
/// `#[serde(rename_all = "camelCase")]` 写出后者；`config` 用自己的手写 `Serialize`，
/// 因此内层键序仍按 `input_key_order`）。
///
/// 这是注册表的存储项：`register` 按 `name` 去重，`unregister` 按 `extension_path` 判所有
/// 权。`Clone` 是深拷贝（`config` 内的 `String`/`IndexMap`/`Value` 都会复制）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredMcpServer {
    /// server 名（`mcpServers` 的键）。
    pub name: String,
    /// 校验后的配置。
    pub config: McpServerConfig,
    /// 注册它的扩展路径。
    pub extension_path: String,
}

/// 注册表变更回调。TS: `McpServerRegistry.setChangeListener` 的 `listener`。
///
/// 回调没有参数：监听方要自己调用 `list()`/`get()` 读取当前状态，因此拿不到“哪一项变了”。
/// 它在调用 `register`/`unregister` 的线程上**同步**执行（锁已释放），所以回调里可以安全地
/// 再次访问注册表；但也意味着并发修改时它看到的状态可能已经包含别的线程的更新，不能把
/// 回调次数或顺序当作变更日志。
///
/// 回调 `panic` 会从触发它的 `register`/`unregister` 向外传播（本文件没有 `catch_unwind`）。
pub type McpServerChangeListener = Arc<dyn Fn() + Send + Sync>;

/// 一个运行时的扩展 MCP server 注册表。TS: `mcp-servers.ts#McpServerRegistry`。
///
/// - `register` 同名替换但**保留插入位置**（`IndexMap::insert` 对已存在的键不改顺序），并**总是**
///   触发 change listener（即使值没变）。
/// - `unregister` 仅在现有条目的 `extensionPath` 相同时删除；删除成功才触发 listener，否则静默。
/// - `list` 按注册顺序返回深拷贝（`structuredClone` 语义）。
/// - `set_change_listener(None)` 可清空回调。
///
/// # 并发模型
///
/// 状态是 `Arc<Mutex<IndexMap<...>>>`，另有一个独立的 `Arc<Mutex<Option<listener>>>`。
/// 所有取锁都经过 `lock()`（毒化后取回内层数据，不 panic）。由此得到的行为：
///
/// - `Clone` 共享同一张表；这是刻意的——扩展运行时的多个快照必须看到同一份注册表。
/// - `register`/`unregister` 先拿 `servers` 锁、再拿 `change_listener` 锁取回调快照，
///   然后释放两把锁，最后在调用线程上同步执行回调。锁的获取顺序只有
///   `servers → change_listener` 这一种，`set_change_listener` 只拿后者；不要新增反向顺序。
/// - 回调执行时没有任何锁被持有，所以回调可以调用 `list()`/`get()`（现有 runner 的
///   `mcp_servers_change` 监听器正是这样做的，重入不会死锁）。
/// - `list`/`get` 在持锁期间完成克隆后返回；调用方拿到的是独立副本，改它不影响注册表。
///
/// # 不负责的事
///
/// 本类型不做所有权检查（同名 server 是否属于另一个扩展）、不做命名空间冲突检查；这些
/// 由 `kiana-extensions` 的 `prepare_mcp_server` 在调用 `register` 之前完成。
// `ExtensionRuntime` 的多个快照（`loader-facade` 的 ``result.runtime``）必须看到同一份注册表：
// TS 里 `runtime.mcpServers` 是一个共享对象。内部持有 `Arc` 让 `Clone` 成为别名而非空副本。
#[derive(Clone)]
pub struct McpServerRegistry {
    servers: std::sync::Arc<Mutex<IndexMap<String, RegisteredMcpServer>>>,
    change_listener: std::sync::Arc<Mutex<Option<McpServerChangeListener>>>,
}

impl Default for McpServerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for McpServerRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let servers = lock(&self.servers);
        f.debug_struct("McpServerRegistry")
            .field("servers", &servers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// 取标准库 `Mutex` 的守卫，遇到毒化时恢复内层数据而不是 panic。
///
/// 本文件所有 `Mutex` 访问都经过它。加锁顺序固定为 `servers → change_listener`
/// （`register`/`unregister`），`set_change_listener` 只拿后者；回调在锁外执行，
/// 因此回调重入注册表不会死锁。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 锁内不运行用户代码；毒化只可能来自 OOM 级别的 panic，恢复内层数据。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl McpServerRegistry {
    /// 建立一个空注册表（无回调）。等价于 [`Default::default`]。
    pub fn new() -> Self {
        Self {
            servers: std::sync::Arc::new(Mutex::new(IndexMap::new())),
            change_listener: std::sync::Arc::new(Mutex::new(None)),
        }
    }

    /// 注册或替换一个 server（调用方负责所有权检查）。
    /// TS: `McpServerRegistry.register`。
    ///
    /// 同名项被替换但保留原来的位置；无论是否有实际变化，都在锁外调用一次当前回调
    /// （回调为 `None` 时什么也不发生）。回调收到的信息只有“发生了变更”，具体变化由回调
    /// 自己重新 `list()` 得知。
    pub fn register(&self, server: RegisteredMcpServer) {
        // 在同一个临界区内完成“插入 + 取回调快照”；出块即释放两把锁，之后才调用回调，
        // 避免回调重入注册表时死锁（`list()`/`get()` 会再拿 `servers` 锁）。
        let listener = {
            let mut servers = lock(&self.servers);
            servers.insert(server.name.clone(), server);
            lock(&self.change_listener).clone()
        };
        if let Some(listener) = listener {
            listener();
        }
    }

    /// 删除 `extensionPath` 注册的 server；别的扩展注册的同名项保持不动。
    /// TS: `McpServerRegistry.unregister`。
    ///
    /// 名字不存在、或现有条目的 `extension_path` 不同时**静默返回**：不删除、不触发回调。
    /// 删除成功则调用一次回调（同样在锁外）。删除用 `shift_remove`，其余项的相对顺序不变。
    pub fn unregister(&self, name: &str, extension_path: &str) {
        // 与 `register` 相同：插入/删除与回调快照在同一临界区，回调在两把锁都释放后执行。
        let listener = {
            let mut servers = lock(&self.servers);
            match servers.get(name) {
                Some(server) if server.extension_path == extension_path => {
                    servers.shift_remove(name);
                }
                // 名字不存在或所有者为别人：直接返回，锁随作用域释放，也不通知回调。
                _ => return,
            }
            lock(&self.change_listener).clone()
        };
        if let Some(listener) = listener {
            listener();
        }
    }

    /// 取一个 server 的克隆（TS 返回注册表内的对象引用；见模块文档第 5 条）。
    /// TS: `McpServerRegistry.get`。
    ///
    /// 返回 `None` 表示名字未注册。克隆在持锁期间完成，调用方修改返回值不会影响注册表。
    pub fn get(&self, name: &str) -> Option<RegisteredMcpServer> {
        lock(&self.servers).get(name).cloned()
    }

    /// 按注册顺序返回全部 server 的深拷贝（`config` 也是克隆）。
    /// TS: `McpServerRegistry.list`。
    ///
    /// 顺序是插入顺序（同名的替换不改变位置）；空注册表返回空 `Vec`。克隆在持锁期间完成，
    /// 因此返回值是一份一致快照，但随后注册表仍可能被其它线程修改。
    pub fn list(&self) -> Vec<RegisteredMcpServer> {
        lock(&self.servers).values().cloned().collect()
    }

    /// 设置每次变更后的回调；`None` 清空。
    /// TS: `McpServerRegistry.setChangeListener`。
    ///
    /// 只替换回调本身，不立即调用，也不补发历史变更。替换后由下一次
    /// `register`/`unregister` 使用；回调在调用线程同步执行（见
    /// [`McpServerChangeListener`] 的约定）。
    pub fn set_change_listener(&self, listener: Option<McpServerChangeListener>) {
        *lock(&self.change_listener) = listener;
    }
}
