//! `extensions/mcp/cli.ts`：`kiana mcp` 子命令。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/cli.ts`（`cli.lazy.ts` 的
//! `loadMcpCommand` 是动态 import 包装，Rust 侧由 [`load_mcp_command`] 直接给出）。
//!
//! 会话之外添加/删除/检查 MCP 服务器并登录。Agent 通过 bash 调它来配置服务器、验证写出的
//! `mcp.json`、启动 OAuth 登录；用户只需在浏览器里批准。运行中的会话在下一轮读取新凭据。
//!
//! # 设计原因与模块边界
//! 读取配置、建立连接、跑 OAuth 分别由 `config`/`runtime`/`oauth` 提供；本文件只把它们串成
//! 命令行语义：参数解析、帮助与错误文案、退出码、文件写入顺序。这样调用方
//! （`kiana-coding-agent` 的 `app.rs`）只需传入 `args[1..]` 与输出回调，不必了解 MCP 内部；
//! 测试也能注入 `log`/`error`/`open_url` 与凭据存储，避免真实终端、浏览器与用户凭据。
//! 注意 `list`/`login` 仍会真实连接服务器，测试用本地进程或 mock HTTP 服务器提供它们。
//!
//! # 执行流程
//! 1. 先判断帮助：无参数、`help`、或任意位置出现 `--help`/`-h` 都打印帮助并返回 0；
//! 2. `add`/`remove` 直接编辑 `mcp.json`（`-l`/`--local` 选项目文件），加载流程不预先读
//!    `trust.json`（`add -l` 在写完之后才检查，见注意事项）；
//! 3. 其它命令先读 `trust.json`：读取失败即打印错误并返回 1；只有 `Some(true)` 算受信任，
//!    `None` 与 `Some(false)` 都按未受信任处理（决定可继承自父目录，见 `ProjectTrustStore::get`）；
//! 4. 用 `load_mcp_config` 合并全局与（受信任时的）项目配置，再分派 `list`/`login`/`logout`。
//!
//! # 输出与失败模式
//! 人类可读消息走 `log`（默认 stdout），诊断走 `error`（默认 stderr）；只有 `list --json`
//! 输出 JSON。退出码只有 0/1：参数错误与 trust 读取失败对任何命令都返回 1；配置读取错误
//! 与任一启用的服务器未连接只计入 `list` 的失败判定（`login`/`logout` 不读 `loaded.errors`，
//! 即使全局 `mcp.json` 损坏，只要受信任项目里存在目标服务器仍可能返回 0）；登录失败返回 1。
//! 颜色仅在 stdout 是终端（或被 `FORCE_COLOR` 强制）
//! 时启用；管道因此看到纯文本，但测试注入回调不影响颜色判断，在终端下运行的测试仍可能
//! 拿到带 ANSI 的文本（单元测试用 `plain()` 归一）。
//!
//! # 注意事项
//! - `add`/`remove` 通过 `config` 模块重写整个目标文件（保留其它内容与缩进），写失败返回 1；
//! - `add -l` 先写文件、再检查信任状态：`trust.json` 读失败时条目已经落盘，命令仍返回 1
//!   （与 TS 的顺序一致，见 `tests/mcp_command.rs`）；
//! - `list`/`login` 会真实启动 stdio 子进程或发 HTTP 请求，两条路径都用 `close()` 释放连接；
//! - `login` 在 stdin 不是终端时不会提示粘贴回调 URL，只能等浏览器回调或超时。

use std::collections::HashMap;
use std::io::{BufRead as _, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_config::trust::manager::ProjectTrustStore;
use kiana_core::config::{APP_NAME, CONFIG_DIR_NAME};
use kiana_core::mcp_servers::{McpServerConfig, get_mcp_tool_exposure};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::mcp::config::{
    LoadMcpConfigOptions, LoadedMcpConfig, McpServerEntry, McpServerScope, add_mcp_server_config,
    config_to_value, load_mcp_config, remove_mcp_server_config,
};
use crate::mcp::oauth::{
    McpOAuthCredentialStore, McpSignInError, McpSignInOptions, McpSignInPrompt, sign_in_mcp_server,
};
use crate::mcp::runtime::{
    McpServerConnection, McpServerConnectionOptions, McpServerLog, create_default_transport,
};

/// `--timeout` 的默认值（秒）。帮助文本末尾的 `default: 300` 是本常量的手工副本，
/// 改这里要同步改 [`help_text`]。
const DEFAULT_LOGIN_TIMEOUT_SECONDS: f64 = 300.0;

/// `chalk.bold` / `chalk.dim` 是否生效（非 TTY 时是纯文本）。
///
/// 判定顺序：`FORCE_COLOR`（值不是 `0`/`false`）优先开启；其次只要 `NO_COLOR` 存在就关闭；
/// 最后看 stdout 是否为终端。只检查 stdout：帮助与列表都写到 `log`（默认 stdout）；但
/// `help_hint()` 经 [`dim`] 包装后会被拼接进错误文本（用法错误、未知子命令等），尽管这些
/// 消息输出到 stderr，其着色与否仍跟随 stdout 的 TTY 状况。
fn color_enabled() -> bool {
    if let Ok(force) = std::env::var("FORCE_COLOR")
        && force != "0"
        && !force.eq_ignore_ascii_case("false")
    {
        return true;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stdout().is_terminal()
}

fn bold(text: &str) -> String {
    if color_enabled() {
        format!("\u{1b}[1m{text}\u{1b}[22m")
    } else {
        text.to_string()
    }
}

fn dim(text: &str) -> String {
    if color_enabled() {
        format!("\u{1b}[2m{text}\u{1b}[22m")
    } else {
        text.to_string()
    }
}

/// `HELP`（逐字；chalk 着色只作用于 `Usage:`）。
///
/// 文本是当前稳定契约：本文件的单元测试 `the_help_banner_matches_the_ts_text`
/// 断言了首行、说明段、`-l` 行、bearer/OAuth 两行与末尾的 `--timeout` 行，改动换行或对齐
/// 会破坏输出契约。
/// `APP_NAME`/`CONFIG_DIR_NAME` 在运行时插值，因此帮助文本随应用名与配置目录名变化。
fn help_text() -> String {
    format!(
        "{usage}
  {app} mcp add <server> [options] -- <command> [args...]
  {app} mcp add <server> [options] --url <url>
  {app} mcp remove <server> [-l]
  {app} mcp list [--json]
  {app} mcp login <server> [--timeout <seconds>]
  {app} mcp logout <server>

Configure and check MCP servers and sign in to OAuth servers without starting a session.
Reads ~/{dir}/agent/mcp.json and, in trusted projects, {dir}/mcp.json.

Commands:
  add <server>            Add or replace a server in mcp.json
  remove <server>         Remove a server from mcp.json
  list                    Show state, tools, and errors (exits 1 on failure)
  login <server>          Sign in through the browser
  logout <server>         Delete the stored OAuth credentials

Options for add and remove:
  -l, --local             Use {dir}/mcp.json in the current project instead of the global file

Options for add:
  --url <url>             Streamable HTTP server URL (instead of a command)
  --env <KEY=VALUE>       Environment variable for a stdio server (repeatable)
  --cwd <dir>             Working directory for a stdio server
  --header <KEY=VALUE>    HTTP header (repeatable)
  --bearer-token-env-var <NAME>
                          Send \"Authorization: Bearer ${{NAME}}\"
  --oauth-client-id <id>  Pre-registered OAuth client id
  --oauth-client-secret <secret>
                          OAuth client secret (may be ${{NAME}} or !command)
  --oauth-callback-port <port>
                          Fixed OAuth callback port
  --oauth-client-name <name>
                          Client name sent when registering with the OAuth server
  --exposure <mode>       codemode (default), deferred, direct, or hidden
  --description <text>    What the server offers, shown in the system prompt

Other options:
  --json                  Print the list as JSON
  --timeout <seconds>     Time limit for the whole sign-in (default: 300)",
        usage = bold("Usage:"),
        app = APP_NAME,
        dir = CONFIG_DIR_NAME,
    )
}

/// `HELP_HINT`。
fn help_hint() -> String {
    dim(&format!("Use \"{APP_NAME} mcp --help\" for usage."))
}

/// 命令行回调（`log`/`error`/`openUrl`）。
///
/// 参数是一整条消息（可能含换行），由实现方决定如何落地；回调在调用点同步执行，因此实现
/// 不应长时间阻塞，否则会拖慢命令推进。三个字段共用这个类型：`log` 与 `error` 的区别只在
/// 默认输出流（stdout/stderr），`open_url` 收到的是一个 URL 字符串。
pub type McpCommandCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// `kiana mcp` 的可注入副作用。
///
/// TS: `cli.ts#McpCommandOptions`。
///
/// 除 `cwd`/`agent_dir` 外的字段都是可选的注入点：缺省时分别使用 stdout、stderr、平台浏览器
/// 与默认凭据存储（`mcp-auth.json`）。`kiana-coding-agent` 的 `app.rs` 用它们注入 `log`/`error`
/// 捕获输出；测试还注入 `credentials` 以隔离凭据文件；目前没有调用方注入 `open_url`。
/// `Clone` 是浅拷贝（回调与凭据存储都用 `Arc` 共享）。
#[derive(Clone)]
pub struct McpCommandOptions {
    /// 会话工作目录。
    pub cwd: PathBuf,
    /// agent 目录（`~/.kiana/agent`）。
    pub agent_dir: PathBuf,
    /// 凭据存储；默认 `mcp-auth.json`。
    ///
    /// `None` 时构造 `McpOAuthCredentialStore::new(None, None)`，它按 `get_agent_dir()`（环境
    /// 变量或 `~/.kiana/agent`）定位 `mcp-auth.json`，**不是**本结构的 `agent_dir` 字段；
    /// 自定义 agent 目录的调用方应显式注入凭据存储。
    pub credentials: Option<Arc<McpOAuthCredentialStore>>,
    /// 默认是平台浏览器。
    pub open_url: Option<McpCommandCallback>,
    /// 默认输出到 stdout。
    pub log: Option<McpCommandCallback>,
    /// 默认输出到 stderr。
    pub error: Option<McpCommandCallback>,
}

impl McpCommandOptions {
    /// 最小参数：cwd 与 agent 目录。
    ///
    /// 其余字段为 `None`，即输出到进程 stdout/stderr、使用平台浏览器与默认凭据存储。
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        McpCommandOptions {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            credentials: None,
            open_url: None,
            log: None,
            error: None,
        }
    }
}

/// `ServerReport`（键顺序即 JSON 输出顺序）。
///
/// 同一份结构同时驱动 `--json` 与人类可读输出，两个通道因此不会出现状态差异。可选字段在
/// `--json` 里通过 `skip_serializing_if` 省略：`override` 只在项目覆盖全局条目时出现，
/// `toolExposure` 只列出与服务器默认 `exposure` 不同的工具，`resources` 只在服务器提供资源时
/// 为 `true`，`error` 只在启用的服务器未连接且记录过错误时出现。
#[derive(Debug, Clone, Serialize)]
struct ServerReport {
    name: String,
    scope: String,
    source: String,
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    override_: Option<String>,
    enabled: bool,
    exposure: String,
    /// `describeTransport(entry)` 的原始返回值（string、或 stdio 条目带 `url` 时的任意值）。
    transport: Value,
    state: String,
    tools: Vec<String>,
    #[serde(rename = "toolExposure", skip_serializing_if = "Option::is_none")]
    tool_exposure: Option<IndexMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// `entry.scope ?? "global"`。
///
/// `None`（条目未设置 scope）与 `Some(Global)` 都输出 `global`：TS 的 `?? "global"` 对
/// 两者一视同仁，报告里也始终要有一个确定的作用域字符串。
fn scope_string(entry: &McpServerEntry) -> String {
    match entry.scope {
        Some(McpServerScope::Project) => "project".to_string(),
        Some(McpServerScope::Extension) => "extension".to_string(),
        Some(McpServerScope::Global) | None => "global".to_string(),
    }
}

/// `"url" in config ? config.url : [command, ...args].join(" ")`。
///
/// TS 的返回值是原始 `config.url`（stdio 条目带 `url` 时可能是 `null`/数字），JSON 输出
/// 因此保留原值；人类可读输出用 [`js_string`] 做 `String(v)`。
fn describe_transport(entry: &McpServerEntry) -> Value {
    let config = config_to_value(&entry.config);
    if let Some(url) = config.get("url") {
        return url.clone();
    }
    let McpServerConfig::Stdio(config) = &entry.config else {
        // 与 `config_to_value` 的 `"url"` 键一致：`Http` 不可能走到这里。
        return Value::Null;
    };
    let mut parts = vec![config.command.clone()];
    parts.extend(config.args.iter().flatten().cloned());
    Value::String(parts.join(" "))
}

/// 为一个条目构造一次性连接（`list`/`login`/`logout` 使用；`list`/`login` 建立连接，
/// `logout` 只读取 `oauth_url`）。
///
/// 使用默认传输工厂与真实凭据存储，服务器日志写到 `<agent_dir>/mcp.log`；`on_tools` 是空
/// 操作，因为命令执行期间没有会话需要接收工具更新。`provider_token` 为 `None`：命令行登录
/// 只处理 OAuth（`oauth_url()` 有值）的服务器，不使用 `auth.provider` 的 token。
/// 调用方负责 `close().await` 释放传输：`list` 在生成报告后关闭，`login` 由
/// `run_mcp_command` 在登录返回后关闭。`logout` 只用它读取 `oauth_url()`，不建立连接。
fn create_connection(
    entry: &McpServerEntry,
    options: &McpCommandOptions,
    credentials: &Arc<McpOAuthCredentialStore>,
) -> McpServerConnection {
    McpServerConnection::new(McpServerConnectionOptions {
        entry: entry.clone(),
        cwd: options.cwd.clone(),
        create_transport: Arc::new(create_default_transport),
        credentials: Arc::clone(credentials),
        provider_token: None,
        on_tools: Arc::new(|_| {}),
        on_change: None,
        log: Some(McpServerLog::new(options.agent_dir.join("mcp.log"))),
    })
}

/// 选项别名：`-l` 等价于 `--local`。TS: `cli.ts#OPTION_ALIASES`。
///
/// 别名在“是否以 `--` 开头”的判断**之前**归一，因此单横线的 `-l` 也能被识别为选项；其余
/// 单横线参数（如 `-x`）仍按位置参数处理。
fn option_alias(arg: &str) -> &str {
    match arg {
        "-l" => "--local",
        _ => arg,
    }
}

/// 一个已知选项的取值方式。
///
/// `Flag` 不带值；`Value` 只保留最后一次出现的值（同名后写覆盖先写）；`List` 按出现顺序
/// 累积全部值（`--env A=1 --env B=2`）。未知选项名不会走到这里：调用方先按名字查表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionKind {
    Flag,
    Value,
    List,
}

/// `parse_options` 里保存的一个已解析值；Flag 与带值选项共用一个映射。
#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredValue {
    Flag,
    Value(String),
}

/// `parseOptions` 的结果。
///
/// 单次从左到右扫描：位置参数保持出现顺序；`values` 保存 Flag 与 `Value`（同名后写覆盖，
/// 但 `IndexMap` 保留首次插入的位置）；`lists` 按出现顺序累积。`--` 之后的全部内容都进入
/// `positional`（`--` 本身被丢弃）。
#[derive(Debug, Default)]
struct ParsedOptions {
    positional: Vec<String>,
    values: IndexMap<String, StoredValue>,
    lists: IndexMap<String, Vec<String>>,
}

impl ParsedOptions {
    fn value(&self, option: &str) -> Option<&str> {
        match self.values.get(option) {
            Some(StoredValue::Value(value)) => Some(value),
            _ => None,
        }
    }

    fn has(&self, option: &str) -> bool {
        self.values.contains_key(option)
    }
}

/// 解析 `--name value` 选项；未知选项报错并返回 `None`。
///
/// `max_positionals` 到达后其余全部是位置参数（`add <server> <command> --flag` 因此把
/// `--flag` 交给子命令）。
///
/// # 规则
/// - 只有以 `--` 开头的参数会被当作选项；`-l` 先经 [`option_alias`] 归一，其它单横线参数
///   （如 `-x`）不匹配任何选项，会原样成为位置参数；
/// - 单独的 `--` 结束选项解析，其后的参数即使以 `--` 开头也都进入 `positional`；
/// - 未知选项、或带值选项后面没有参数时，调用 `error` 回调并返回 `None`（调用方据此返回
///   退出码 1），已解析的部分被丢弃；
/// - `max_positionals` 为 `Some(n)` 时，一旦已有 n 个位置参数，剩余参数原样成为位置参数
///   （包括看起来像选项的参数），这正是 `add` 把服务器自身参数透传下去的方式。
fn parse_options(
    args: &[String],
    known: &[(&str, OptionKind)],
    error: &dyn Fn(&str),
    max_positionals: Option<usize>,
) -> Option<ParsedOptions> {
    let mut parsed = ParsedOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let raw = &args[index];
        let arg = option_alias(raw);
        let at_limit = max_positionals.is_some_and(|max| parsed.positional.len() >= max);
        if arg == "--" || at_limit {
            let start = if arg == "--" { index + 1 } else { index };
            parsed.positional.extend(args[start..].iter().cloned());
            break;
        }
        if !arg.starts_with("--") {
            parsed.positional.push(arg.to_string());
            index += 1;
            continue;
        }
        let name = &arg[2..];
        let kind = known
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, kind)| *kind);
        let Some(kind) = kind else {
            error(&format!("Unknown option {arg}.\n{}", help_hint()));
            return None;
        };
        if kind == OptionKind::Flag {
            parsed.values.insert(name.to_string(), StoredValue::Flag);
            index += 1;
            continue;
        }
        index += 1;
        let Some(value) = args.get(index) else {
            error(&format!("{arg} needs a value."));
            return None;
        };
        if kind == OptionKind::List {
            parsed
                .lists
                .entry(name.to_string())
                .or_default()
                .push(value.clone());
        } else {
            parsed
                .values
                .insert(name.to_string(), StoredValue::Value(value.clone()));
        }
        index += 1;
    }
    Some(parsed)
}

/// JS 的 `Number(string)`。
///
/// 供 `--timeout` 与 `--oauth-callback-port` 的字符串参数使用：先按 JS `trim` 去掉空白，
/// 空串为 `0`；支持 `0x`/`0b`/`0o` 前缀（带符号时按 JS 语法判为 `NaN`）、`Infinity` 与
/// 十进制/指数写法；其余无法完整解析的输入返回 `NaN`。`NaN` 不会被静默忽略：`--timeout`
/// 会因此被拒绝，而 `--oauth-callback-port` 经 [`number_value`] 写成 JSON `null` 后由
/// `validate_mcp_server_config` 拒绝。
fn js_number(value: &str) -> f64 {
    let text = kiana_json::js_string::js_trim(value);
    if text.is_empty() {
        return 0.0;
    }
    let (unsigned, had_sign) = match text.as_bytes().first() {
        Some(b'+') => (&text[1..], true),
        Some(b'-') => (&text[1..], true),
        _ => (text, false),
    };
    // 十六/二/八进制字符串（不允许符号）。
    let radix = if unsigned.starts_with("0x") || unsigned.starts_with("0X") {
        Some(16)
    } else if unsigned.starts_with("0b") || unsigned.starts_with("0B") {
        Some(2)
    } else if unsigned.starts_with("0o") || unsigned.starts_with("0O") {
        Some(8)
    } else {
        None
    };
    if let Some(radix) = radix {
        if had_sign || unsigned.len() <= 2 {
            return f64::NAN;
        }
        let digits = &unsigned[2..];
        let mut result = 0.0f64;
        for ch in digits.chars() {
            let Some(digit) = ch.to_digit(radix) else {
                return f64::NAN;
            };
            result = result * f64::from(radix) + f64::from(digit);
        }
        return result;
    }
    if text == "Infinity" || text == "+Infinity" {
        return f64::INFINITY;
    }
    if text == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("inf") || lower.contains("nan") {
        return f64::NAN;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// `JSON.stringify(number)` 的值形态。
///
/// `NaN`/`±Infinity` 写成 `null`，`-0` 写成 `0`，安全整数范围内的整数值写成整数，其余用
/// 浮点数。`add` 用它把 `--oauth-callback-port` 的解析结果写进 `callbackPort`：用户输入
/// 非法时得到 `null`，随后在校验阶段报 "oauth.callbackPort must be a port number"。
fn number_value(value: f64) -> Value {
    if value.is_nan() || value.is_infinite() {
        return Value::Null;
    }
    if value == 0.0 {
        return Value::Number(serde_json::Number::from(0));
    }
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
        return Value::Number(serde_json::Number::from(value as i64));
    }
    Value::Number(
        serde_json::Number::from_f64(value).unwrap_or_else(|| serde_json::Number::from(0)),
    )
}

/// JS 对象的整数索引键在前（升序），其余保持插入序。
///
/// “整数索引键”与 JS 数组索引的判定一致：非空、无前导零（`"0"` 例外）、长度不超过 10、
/// 数值不是 `2^32-1`。该顺序既影响 `--json` 的字节输出，也决定 `toolExposure` 的展示顺序；
/// 这些映射都建立在保留插入序的 `Map`/`IndexMap` 上。
fn js_key_order(keys: impl Iterator<Item = String>) -> Vec<String> {
    let mut indices: Vec<(u32, String)> = Vec::new();
    let mut others: Vec<String> = Vec::new();
    for key in keys {
        let parsed = if !key.is_empty()
            && key.len() <= 10
            && !(key.len() > 1 && key.starts_with('0'))
            && key.bytes().all(|byte| byte.is_ascii_digit())
        {
            key.parse::<u32>().ok().filter(|value| *value != u32::MAX)
        } else {
            None
        };
        match parsed {
            Some(index) => indices.push((index, key)),
            None => others.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    let mut ordered: Vec<String> = indices.into_iter().map(|(_, key)| key).collect();
    ordered.extend(others);
    ordered
}

/// `{[key]: value, ...}` 的 JS 键序。
///
/// `headers`/`env` 写回 `mcp.json` 前经过这里；`HashMap` 只用于查找，最终顺序完全由
/// [`js_key_order`] 决定，重复键保留最后一次的值、首次出现的位置（与 JS 对象赋值一致）。
fn object_with_js_order(pairs: &[(String, String)]) -> Map<String, Value> {
    let lookup: HashMap<&str, &str> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut map = Map::new();
    for key in js_key_order(pairs.iter().map(|(key, _)| key.clone())) {
        if let Some(value) = lookup.get(key.as_str()) {
            map.insert(key, Value::String((*value).to_string()));
        }
    }
    map
}

/// `Object.fromEntries(pairs)` 的键序（整数索引键按数值升序在前，其余保持插入序）。
///
/// `toolExposure` 用它在 `--json` 与人类可读输出里复现 TS 的条目顺序；`IndexMap` 的插入
/// 语义是“已有的键保留原位置、更新值”，与 JS 对象重复键的行为一致。
fn indexmap_with_js_order(pairs: &[(String, String)]) -> IndexMap<String, String> {
    let lookup: HashMap<&str, &str> = pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut map = IndexMap::new();
    for key in js_key_order(pairs.iter().map(|(key, _)| key.clone())) {
        if let Some(value) = lookup.get(key.as_str()) {
            map.insert(key, (*value).to_string());
        }
    }
    map
}

/// `parsePairs(option, pairs, error)`。
///
/// 把重复的 `KEY=VALUE` 选项折叠成对象。只在**第一个** `=` 处分割，因此值里可以再含 `=`；
/// 缺少 `=` 或键为空（`=v`）都报错并返回 `None`；值可以为空（`K=`）。重复的键保留最后
/// 一次的值与首次出现的位置。没有该项时返回空映射（不是 `None`）。
fn parse_pairs(
    option: &str,
    pairs: Option<&Vec<String>>,
    error: &dyn Fn(&str),
) -> Option<IndexMap<String, String>> {
    let Some(pairs) = pairs else {
        return Some(IndexMap::new());
    };
    let mut record: IndexMap<String, String> = IndexMap::new();
    for pair in pairs {
        let Some(separator) = pair.find('=') else {
            error(&format!("--{option} expects KEY=VALUE, got \"{pair}\"."));
            return None;
        };
        if separator == 0 {
            error(&format!("--{option} expects KEY=VALUE, got \"{pair}\"."));
            return None;
        }
        record.insert(
            pair[..separator].to_string(),
            pair[separator + 1..].to_string(),
        );
    }
    Some(record)
}

/// `runMcpCommand` 的实现：执行一次 `kiana mcp …` 调用并返回进程退出码。
///
/// `args` 是去掉可执行文件与 `mcp` 之后的参数（例如 `["add", "-l", "docs", "--", "npx", "srv"]`），
/// `options` 提供工作目录、agent 目录与可注入的副作用。
///
/// # 分派顺序
/// 1. 无参数：打印帮助，返回 0；
/// 2. `command == "help"`，或**任意位置**出现 `--help`/`-h`：打印帮助，返回 0。帮助检查在
///    子命令解析之前，所以 `add x -- cmd --help` 也只打印帮助、不写文件；
/// 3. `add`/`remove`：直接编辑 `mcp.json`；`add` 仅在 `-l` 写完之后才检查信任状态，`remove`
///    仅在目标文件未命中条目时重新加载配置、定位另一作用域；二者都不经过后续统一的信任
///    检查与 `load_mcp_config` 合并流程；
/// 4. 其余命令（包括未知子命令）：先读 `trust.json`，失败即把消息交给 `error` 并返回 1；
///    再 `load_mcp_config` 合并全局与（受信任时的）项目配置，最后分派。
///
/// # 退出码与输出
/// 只有 0（成功，或帮助）与 1（任何失败）。人类可读消息走 `options.log`（默认 stdout），
/// 诊断走 `options.error`（默认 stderr）；`list --json` 的 JSON 也经 `log` 输出。
///
/// # 副作用
/// `add`/`remove` 创建或重写 `mcp.json`（保留其它内容）；`login` 把凭据写入凭据存储；
/// `list`/`login` 会真实连接服务器。`login` 结束后（无论成败）都会 `close()` 连接。
pub async fn run_mcp_command(args: Vec<String>, options: McpCommandOptions) -> i32 {
    let log: McpCommandCallback = match &options.log {
        Some(log) => Arc::clone(log),
        None => Arc::new(|line: &str| println!("{line}")),
    };
    let error: McpCommandCallback = match &options.error {
        Some(error) => Arc::clone(error),
        None => Arc::new(|line: &str| eprintln!("{line}")),
    };
    let Some((command, rest)) = args.split_first() else {
        log(&help_text());
        return 0;
    };
    if command == "help" || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        log(&help_text());
        return 0;
    }

    let project_config = options.cwd.join(CONFIG_DIR_NAME).join("mcp.json");
    if command == "add" || command == "remove" {
        return if command == "add" {
            add(rest, &project_config, &options, &log, &error)
        } else {
            remove(rest, &project_config, &options, &log, &error)
        };
    }

    // TS 的 `ProjectTrustStore.get` 对损坏/无法读取的 trust.json 会抛错：整个命令失败，
    // 不会静默当成“项目未受信任”。
    // `get` 返回最近的有效决定（可继承自父目录）：`None`（没有任何决定）与 `Some(false)`
    // 都按未受信任处理，因此只有 `Some(true)` 才算受信任。
    let project_trusted = match ProjectTrustStore::new(&options.agent_dir.to_string_lossy())
        .get(&options.cwd.to_string_lossy())
    {
        Ok(decision) => decision == Some(true),
        Err(trust_error) => {
            error(&trust_error.message);
            return 1;
        }
    };
    let loaded = load_mcp_config(LoadMcpConfigOptions {
        agent_dir: options.agent_dir.clone(),
        cwd: options.cwd.clone(),
        project_trusted,
    });
    let untrusted_note = (!project_trusted && project_config.exists()).then(|| {
        // 只在项目文件确实存在时才提示“被忽略”；文件不存在时没有可忽略的内容。
        format!(
            "{} is ignored because the project is not trusted. Start {APP_NAME} in the project to trust it.",
            project_config.to_string_lossy()
        )
    });
    let credentials = match &options.credentials {
        Some(credentials) => Arc::clone(credentials),
        None => Arc::new(McpOAuthCredentialStore::new(None, None)),
    };

    match command.as_str() {
        "list" => {
            let Some(parsed) =
                parse_options(rest, &[("json", OptionKind::Flag)], error.as_ref(), None)
            else {
                return 1;
            };
            if !parsed.positional.is_empty() {
                error(&format!(
                    "Usage: {APP_NAME} mcp list [--json]\n{}",
                    help_hint()
                ));
                return 1;
            }
            list(
                &loaded,
                parsed.has("json"),
                untrusted_note.as_deref(),
                &options,
                &credentials,
                &log,
            )
            .await
        }
        "login" | "logout" => {
            // `login` 只接受 `--timeout`；`logout` 的已知选项表为空（除全局提前处理的
            // `--help`/`-h` 外，任何 `--xxx` 都会由 `parse_options` 报 Unknown option）。
            // 两者都要求恰好一个服务器名位置参数。
            const LOGIN_OPTIONS: [(&str, OptionKind); 1] = [("timeout", OptionKind::Value)];
            let known: &[(&str, OptionKind)] = if command == "login" {
                &LOGIN_OPTIONS
            } else {
                &[]
            };
            let Some(parsed) = parse_options(rest, known, error.as_ref(), None) else {
                return 1;
            };
            let mut positional = parsed.positional.iter();
            let name = positional.next().cloned();
            let extra: Vec<&String> = positional.collect();
            let Some(name) = name.filter(|name| !name.is_empty()) else {
                error(&format!(
                    "Usage: {APP_NAME} mcp {command} <server>\n{}",
                    help_hint()
                ));
                return 1;
            };
            if !extra.is_empty() {
                error(&format!(
                    "Usage: {APP_NAME} mcp {command} <server>\n{}",
                    help_hint()
                ));
                return 1;
            }
            let entry = loaded.servers.iter().find(|server| server.name == name);
            let Some(entry) = entry else {
                error(&format!(
                    "No MCP server named \"{name}\".{} Configured: {}.",
                    untrusted_note
                        .as_deref()
                        .map(|note| format!(" {note}"))
                        .unwrap_or_default(),
                    if loaded.servers.is_empty() {
                        "none".to_string()
                    } else {
                        loaded
                            .servers
                            .iter()
                            .map(|server| server.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
                return 1;
            };
            let entry = entry.clone();
            let connection = create_connection(&entry, &options, &credentials);
            let Some(url) = connection.oauth_url().map(str::to_string) else {
                error(&format!(
                    "MCP server \"{name}\" does not use OAuth. Only HTTP servers without an Authorization header do."
                ));
                return 1;
            };
            if command == "logout" {
                // TS 里 `withLock`（以及 JSON 解析/写回）的错误直接抛出，命令不会静默成功。
                match credentials.remove(&name, &url) {
                    Ok(true) => log(&format!("Signed out of MCP server \"{name}\".")),
                    Ok(false) => log(&format!("No stored credentials for MCP server \"{name}\".")),
                    Err(remove_error) => {
                        error(&remove_error.message());
                        return 1;
                    }
                }
                return 0;
            }
            // 秒 → 毫秒。这里只拒绝非有限值与非正数；整数性与 Node 的 2^32 上限在 `login`
            // 里由 `abort_signal_timeout_delay` 按 `AbortSignal.timeout` 的规则校验。
            let timeout = parsed
                .value("timeout")
                .map(js_number)
                .unwrap_or(DEFAULT_LOGIN_TIMEOUT_SECONDS);
            if !timeout.is_finite() || timeout <= 0.0 {
                error("--timeout must be a positive number of seconds.");
                return 1;
            }
            let result = login(
                &entry,
                &connection,
                &url,
                timeout * 1000.0,
                &options,
                &credentials,
                &log,
                &error,
            )
            .await;
            // 不论登录成功、失败还是被取消都关闭连接：`close()` 会取消 shutdown token、
            // 关闭已有传输并等待进行中的连接结束。（OAuth 回调服务器由
            // `sign_in_mcp_server` 在自己的 finally 等价路径里关闭。）
            connection.close().await;
            result
        }
        _ => {
            error(&format!(
                "Unknown mcp command \"{command}\".\n{}",
                help_hint()
            ));
            1
        }
    }
}

/// `kiana mcp add`：构造配置、校验，然后写入目标 `mcp.json`。
///
/// 参数形状是 `add <server> [options] (--url <url> | -- <command> [args...])`。第一个位置
/// 参数是服务器名；`--url` 与 stdio 命令必须二选一。HTTP 专属选项（header 与 OAuth 系列）
/// 和 stdio 专属选项（env/cwd）不能混用。
///
/// 参数与校验失败都在写文件之前返回：参数错误、选项混用、名字/URL/oauth 校验失败都不会创建
/// 或修改文件（`tests/mcp_command.rs::rejects_invalid_add_invocations_without_writing`）。
/// 写入成功后先 log `Added`/`Replaced`；只有 `--local` 才再检查信任状态（见函数内注释）。
fn add(
    args: &[String],
    project_config: &Path,
    options: &McpCommandOptions,
    log: &McpCommandCallback,
    error: &McpCommandCallback,
) -> i32 {
    let usage = format!(
        "Usage: {APP_NAME} mcp add <server> [options] (--url <url> | -- <command> [args...])\n{}",
        help_hint()
    );
    let known: &[(&str, OptionKind)] = &[
        ("local", OptionKind::Flag),
        ("url", OptionKind::Value),
        ("env", OptionKind::List),
        ("cwd", OptionKind::Value),
        ("header", OptionKind::List),
        ("bearer-token-env-var", OptionKind::Value),
        ("oauth-client-id", OptionKind::Value),
        ("oauth-client-secret", OptionKind::Value),
        ("oauth-callback-port", OptionKind::Value),
        ("oauth-client-name", OptionKind::Value),
        ("exposure", OptionKind::Value),
        ("description", OptionKind::Value),
    ];
    // 位置参数达到 2 个后停止识别选项：后续 `--root` 之类的参数原样进入位置参数列表，
    // 交给 stdio 子命令（见 `parse_options` 的 `max_positionals`）。
    let Some(parsed) = parse_options(args, known, error.as_ref(), Some(2)) else {
        return 1;
    };
    let mut positional = parsed.positional.iter();
    let name = positional.next().cloned();
    let command: Vec<String> = positional.cloned().collect();
    let url = parsed.value("url").map(str::to_string);
    // 必须恰好提供一种传输：有 url 时不能带命令，没有 url 时命令不能为空。
    if !name.as_deref().is_some_and(|name| !name.is_empty())
        || (url.is_none()) == command.is_empty()
    {
        error(&usage);
        return 1;
    }
    let name = name.unwrap_or_default();
    let http_only = [
        "header",
        "bearer-token-env-var",
        "oauth-client-id",
        "oauth-client-secret",
        "oauth-callback-port",
        "oauth-client-name",
    ];
    let stdio_only = ["env", "cwd"];
    // 变量名叫 `allowed`，但这里列出的是当前传输方式**不允许**的选项：没有 url 即 stdio，
    // HTTP 专属选项非法；有 url 则 stdio 专属选项非法。
    let allowed = if url.is_none() {
        &http_only[..]
    } else {
        &stdio_only[..]
    };
    let misplaced = allowed
        .iter()
        .find(|option| parsed.has(option) || parsed.lists.contains_key(**option));
    if let Some(misplaced) = misplaced {
        error(&format!(
            "--{misplaced} only applies to {}.",
            if url.is_none() {
                "HTTP servers (--url)"
            } else {
                "stdio servers"
            }
        ));
        return 1;
    }

    // 键的插入顺序就是写回 `mcp.json` 的键序（JS 对象的属性顺序）：先传输字段，再公共的
    // exposure/description。`headers`/`env` 另经 `object_with_js_order` 还原 JS 键序。
    let mut config = Map::new();
    if let Some(url) = &url {
        let Some(headers) = parse_pairs("header", parsed.lists.get("header"), error.as_ref())
        else {
            return 1;
        };
        let mut header_pairs: Vec<(String, String)> = headers.into_iter().collect();
        let bearer = parsed.value("bearer-token-env-var");
        if let Some(bearer) = bearer {
            header_pairs.push(("Authorization".to_string(), format!("Bearer ${{{bearer}}}")));
        }
        let mut oauth = Map::new();
        if let Some(client_id) = parsed.value("oauth-client-id") {
            oauth.insert("clientId".to_string(), Value::String(client_id.to_string()));
        }
        if let Some(client_secret) = parsed.value("oauth-client-secret") {
            oauth.insert(
                "clientSecret".to_string(),
                Value::String(client_secret.to_string()),
            );
        }
        if let Some(port) = parsed.value("oauth-callback-port") {
            oauth.insert("callbackPort".to_string(), number_value(js_number(port)));
        }
        if let Some(client_name) = parsed.value("oauth-client-name") {
            oauth.insert(
                "clientName".to_string(),
                Value::String(client_name.to_string()),
            );
        }
        config.insert("url".to_string(), Value::String(url.clone()));
        if !header_pairs.is_empty() {
            config.insert(
                "headers".to_string(),
                Value::Object(object_with_js_order(&header_pairs)),
            );
        }
        if !oauth.is_empty() {
            config.insert("oauth".to_string(), Value::Object(oauth));
        }
    } else {
        let Some(env) = parse_pairs("env", parsed.lists.get("env"), error.as_ref()) else {
            return 1;
        };
        let mut command = command.into_iter();
        let executable = command.next().unwrap_or_default();
        let command_args: Vec<String> = command.collect();
        config.insert("command".to_string(), Value::String(executable));
        if !command_args.is_empty() {
            config.insert(
                "args".to_string(),
                Value::Array(command_args.into_iter().map(Value::String).collect()),
            );
        }
        if !env.is_empty() {
            let env_pairs: Vec<(String, String)> = env.into_iter().collect();
            config.insert(
                "env".to_string(),
                Value::Object(object_with_js_order(&env_pairs)),
            );
        }
        if let Some(cwd) = parsed.value("cwd") {
            config.insert("cwd".to_string(), Value::String(cwd.to_string()));
        }
    }
    if let Some(exposure) = parsed.value("exposure") {
        config.insert("exposure".to_string(), Value::String(exposure.to_string()));
    }
    if let Some(description) = parsed.value("description") {
        config.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    // 先校验再落盘：`validate_mcp_server_config` 会检查名字、URL scheme、headers/oauth 形状
    // 与 exposure，所以非法输入不会留下半个条目。
    let validated =
        match kiana_core::mcp_servers::validate_mcp_server_config(&name, &Value::Object(config)) {
            Ok(validated) => validated,
            Err(message) => {
                error(&message);
                return 1;
            }
        };

    let project = parsed.has("local");
    let path = if project {
        project_config.to_path_buf()
    } else {
        options.agent_dir.join("mcp.json")
    };
    let scope = if project { "project" } else { "global" };
    let replaced = match add_mcp_server_config(&path, &name, &validated) {
        Ok(replaced) => replaced,
        Err(update_error) => {
            error(&format!(
                "Could not update {}: {update_error}",
                path.to_string_lossy()
            ));
            return 1;
        }
    };
    log(&format!(
        "{} {scope} MCP server \"{name}\" in {}.",
        if replaced { "Replaced" } else { "Added" },
        path.to_string_lossy()
    ));
    if project {
        // TS 在写出并 log "Added/Replaced" 之后才读 trust 文件：读失败时文件已更新，
        // 命令以错误结束（不再输出 not-trusted 提示与 "Check it with"）。
        match ProjectTrustStore::new(&options.agent_dir.to_string_lossy())
            .get(&options.cwd.to_string_lossy())
        {
            Ok(Some(true)) => {}
            Ok(_) => log(&format!(
                "The project is not trusted, so {} is ignored until you start {APP_NAME} in the project and trust it.",
                path.to_string_lossy()
            )),
            Err(trust_error) => {
                error(&trust_error.message);
                return 1;
            }
        }
    }
    // 没有 Authorization 头的 HTTP 服务器可能用 OAuth。判断只看校验后配置里是否存在名为
    // `authorization`（大小写不敏感）的 header，不看它的值，也不看 `auth` 字段；因此
    // `--bearer-token-env-var` 加出的 Authorization 头会关掉这条登录提示（测试锁定）。
    let may_need_sign_in = match serde_json::to_value(&validated) {
        Ok(Value::Object(map)) => {
            map.contains_key("url")
                && !map
                    .get("headers")
                    .and_then(Value::as_object)
                    .is_some_and(|headers| {
                        headers
                            .keys()
                            .any(|header| header.to_lowercase() == "authorization")
                    })
        }
        _ => false,
    };
    log(&format!(
        "Check it with: {APP_NAME} mcp list{}",
        if may_need_sign_in {
            format!(". If it requires sign-in: {APP_NAME} mcp login {name}")
        } else {
            String::new()
        }
    ));
    0
}

/// `kiana mcp remove`：从选定的 `mcp.json` 删除一个条目。
///
/// `-l`/`--local` 选择项目文件，否则是全局文件。文件缺失或条目不存在时 `remove_mcp_server_config`
/// 返回 `false`；此时会重新加载配置（强制包含项目文件）以判断该名字是否定义在另一个作用域，
/// 在错误消息里提示应该加还是去掉 `--local`。
fn remove(
    args: &[String],
    project_config: &Path,
    options: &McpCommandOptions,
    log: &McpCommandCallback,
    error: &McpCommandCallback,
) -> i32 {
    let Some(parsed) = parse_options(args, &[("local", OptionKind::Flag)], error.as_ref(), None)
    else {
        return 1;
    };
    let mut positional = parsed.positional.iter();
    let name = positional.next().cloned();
    let extra: Vec<&String> = positional.collect();
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        error(&format!(
            "Usage: {APP_NAME} mcp remove <server> [-l]\n{}",
            help_hint()
        ));
        return 1;
    };
    if !extra.is_empty() {
        error(&format!(
            "Usage: {APP_NAME} mcp remove <server> [-l]\n{}",
            help_hint()
        ));
        return 1;
    }
    let project = parsed.has("local");
    let global_config = options.agent_dir.join("mcp.json");
    let path = if project {
        project_config.to_path_buf()
    } else {
        global_config
    };
    let scope = if project { "project" } else { "global" };
    let removed = match remove_mcp_server_config(&path, &name) {
        Ok(removed) => removed,
        Err(remove_error) => {
            error(&format!(
                "Could not update {}: {remove_error}",
                path.to_string_lossy()
            ));
            return 1;
        }
    };
    if removed {
        log(&format!(
            "Removed {scope} MCP server \"{name}\" from {}.",
            path.to_string_lossy()
        ));
        return 0;
    }
    let removed_scope = if project {
        McpServerScope::Project
    } else {
        McpServerScope::Global
    };
    // 这里用 `project_trusted: true` 只是为了在错误消息里指出条目实际所在文件：即使项目
    // 未受信任，也能说明“它定义在项目配置里；用 --local”。此调用不写任何文件。
    let other = load_mcp_config(LoadMcpConfigOptions {
        agent_dir: options.agent_dir.clone(),
        cwd: options.cwd.clone(),
        project_trusted: true,
    })
    .servers
    .into_iter()
    .find(|server| server.name == name && server.scope != Some(removed_scope));
    let extra = match other {
        Some(other) => format!(
            " It is defined in {}{}.",
            other.source,
            if other.scope == Some(McpServerScope::Project) {
                "; use --local"
            } else {
                "; omit --local"
            }
        ),
        None => String::new(),
    };
    error(&format!(
        "No {scope} MCP server named \"{name}\" in {}.{extra}",
        path.to_string_lossy()
    ));
    1
}

/// `kiana mcp list`：为每个条目生成报告并输出（人类可读或 `--json`）。
///
/// 每个**启用**的条目都会建立一次真实连接；禁用的条目直接报告 `disabled`，不连接。所有条目
/// 的 future 由 `join_all` 同时推进（在同一任务内交替执行，不额外限制并发数），全部结束后
/// 才输出，因此报告顺序始终等于 `loaded.servers` 的顺序。
///
/// 失败判定：`loaded.errors` 非空，或任一启用条目的最终状态不是 `connected`（`needs-auth`、
/// `failed`、`disconnected` 都算失败）→ 返回 1；否则返回 0。即使返回 1，报告仍完整输出，
/// 方便脚本据此诊断。
async fn list(
    loaded: &LoadedMcpConfig,
    json: bool,
    untrusted_note: Option<&str>,
    options: &McpCommandOptions,
    credentials: &Arc<McpOAuthCredentialStore>,
    log: &McpCommandCallback,
) -> i32 {
    // 每个条目一个独立连接；future 之间不共享状态，结束后按原顺序汇总。连接失败不会中断
    // 其它条目：错误由连接自身的 state/error 字段记录，下面照常采集。
    let reports = futures::future::join_all(loaded.servers.iter().cloned().map(|entry| {
        let options = options.clone();
        let credentials = Arc::clone(credentials);
        async move {
            let mut report = ServerReport {
                name: entry.name.clone(),
                scope: scope_string(&entry),
                source: entry.source.clone(),
                override_: entry.override_.clone(),
                enabled: entry.config.base().enabled != Some(false),
                exposure: entry
                    .config
                    .base()
                    .exposure
                    .unwrap_or(kiana_core::mcp_servers::McpExposure::Codemode)
                    .as_str()
                    .to_string(),
                transport: describe_transport(&entry),
                state: "disabled".to_string(),
                tools: Vec::new(),
                tool_exposure: None,
                resources: None,
                error: None,
            };
            if !report.enabled {
                return report;
            }
            let connection = create_connection(&entry, &options, &credentials);
            // 忽略 `get_client` 的错误是有意的：单个服务器连不上不应中断整张列表；失败原因
            // 已经写入连接的 state/error，下面照常采集。返回的客户端不被使用。
            let _ = connection.get_client().await;
            report.state = connection.state().as_str().to_string();
            report.tools = connection
                .tools()
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            // TS: `Object.fromEntries(overrides)` —— 整数样式的工具名在前（升序），
            // 其余保持 tools 顺序。
            let overrides: Vec<(String, String)> = connection
                .tools()
                .iter()
                .filter_map(|tool| {
                    let exposure = get_mcp_tool_exposure(&entry.config, &tool.name);
                    (exposure.as_str() != report.exposure)
                        .then(|| (tool.name.clone(), exposure.as_str().to_string()))
                })
                .collect();
            if !overrides.is_empty() {
                report.tool_exposure = Some(indexmap_with_js_order(&overrides));
            }
            if connection.has_resources() {
                report.resources = Some(true);
            }
            // 只有未连接的状态才把连接上最近一次错误带进报告；连接成功时不展示 `error`。
            if report.state != "connected"
                && let Some(error) = connection.error()
            {
                report.error = Some(error);
            }
            connection.close().await;
            report
        }
    }))
    .await;
    // 失败有两类来源：配置本身读不出来/校验不过（`loaded.errors`），以及启用的服务器
    // 最终没能连上。禁用的条目不算失败。
    let failed = !loaded.errors.is_empty()
        || reports
            .iter()
            .any(|report| report.enabled && report.state != "connected");

    if json {
        // 顶层键顺序是 servers、errors、note；`note` 只在项目未被信任且存在项目配置时出现。
        // 两处 `unwrap_or` 都是防御性回退：`ServerReport` 与字符串都可序列化，正常不会触发。
        let mut output = Map::new();
        output.insert(
            "servers".to_string(),
            serde_json::to_value(&reports).unwrap_or(Value::Array(Vec::new())),
        );
        output.insert(
            "errors".to_string(),
            Value::Array(loaded.errors.iter().cloned().map(Value::String).collect()),
        );
        if let Some(note) = untrusted_note {
            output.insert("note".to_string(), Value::String(note.to_string()));
        }
        log(&serde_json::to_string_pretty(&Value::Object(output))
            .unwrap_or_else(|_| "{}".to_string()));
        return if failed { 1 } else { 0 };
    }
    if reports.is_empty() && loaded.errors.is_empty() {
        log(&format!(
            "No MCP servers configured. Add them to {} or {CONFIG_DIR_NAME}/mcp.json.",
            options.agent_dir.join("mcp.json").to_string_lossy()
        ));
    }
    // 人类可读格式：标题行后每条附加信息缩进两格；多行错误逐行加缩进；配置错误与未受
    // 信任提示放在所有服务器之后。
    for report in &reports {
        let state = if report.state == "connected" {
            format!(
                "connected, {} tool{}",
                report.tools.len(),
                if report.tools.len() == 1 { "" } else { "s" }
            )
        } else if report.state == "needs-auth" {
            "needs sign-in".to_string()
        } else {
            report.state.clone()
        };
        log(&format!(
            "{}: {state} ({}, {})",
            report.name, report.exposure, report.scope
        ));
        log(&format!(
            "  {}",
            kiana_json::js_string::js_string(Some(&report.transport))
        ));
        if let Some(override_) = &report.override_ {
            log(&format!("  project override: {override_}"));
        }
        if report.state == "needs-auth" {
            log(&format!(
                "  sign in with: {APP_NAME} mcp login {}",
                report.name
            ));
        }
        if !report.tools.is_empty() {
            let tools = report
                .tools
                .iter()
                .map(
                    |tool| match report.tool_exposure.as_ref().and_then(|map| map.get(tool)) {
                        Some(exposure) => format!("{tool} [{exposure}]"),
                        None => tool.clone(),
                    },
                )
                .collect::<Vec<_>>()
                .join(", ");
            log(&format!("  tools: {tools}"));
        }
        if report.resources.is_some() {
            log("  offers resources");
        }
        if let Some(error) = &report.error {
            log(&format!("  {}", error.replace('\n', "\n  ")));
        }
    }
    for config_error in &loaded.errors {
        log(&format!("config error: {config_error}"));
    }
    if let Some(note) = untrusted_note {
        log(note);
    }
    if failed { 1 } else { 0 }
}

/// `AbortSignal.timeout(delay)` 的 Node 语义。
///
/// `delay` 必须是整数（`Number.isInteger`）且在 `0..=4294967295` 内，否则抛 RangeError；
/// 整数 `2147483648..=4294967295` 被 Node 的计时器钳到 1 ms（TimeoutOverflowWarning）。
/// 返回实际生效的毫秒数，错误是 `RangeError.message`（逐字）。
fn abort_signal_timeout_delay(delay: f64) -> Result<f64, String> {
    if !kiana_json::is_js_integer(delay) {
        return Err(format!(
            "The value of \"delay\" is out of range. It must be an integer. Received {}",
            js_error_out_of_range_received(delay)
        ));
    }
    if !(0.0..=4_294_967_295.0).contains(&delay) {
        return Err(format!(
            "The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received {}",
            js_error_out_of_range_received(delay)
        ));
    }
    Ok(if delay > 2_147_483_647.0 { 1.0 } else { delay })
}

/// Node `ERR_OUT_OF_RANGE` 的 `Received` 值格式（`internal/errors`）：
/// `Number.isInteger(input) && |input| > 2^32` 时用 `addNumericalSeparator(String(input))`，
/// 其余用 `util.inspect`（有限值即 `String(value)`）。例如 `5000000000` →
/// `5_000_000_000`，而恰好 `2^32` 仍是 `4294967296`。
fn js_error_out_of_range_received(value: f64) -> String {
    if kiana_json::is_js_integer(value) && value.abs() > 4_294_967_296.0 {
        add_numerical_separator(&js_number_to_string(value))
    } else {
        js_number_to_string(value)
    }
}

/// Node `internal/errors#addNumericalSeparator`：从右往左每三位插入 `_`（开头的 `-` 不分组）。
fn add_numerical_separator(value: &str) -> String {
    let start = usize::from(value.starts_with('-'));
    let mut index = value.len();
    let mut result = String::new();
    while index >= start + 4 {
        result = format!("_{}{result}", &value[index - 3..index]);
        index -= 3;
    }
    format!("{}{result}", &value[..index])
}

/// `String(number)`（ECMAScript `Number::toString`）：与 `kiana_json` 的 JSON 数字格式只在
/// 非有限值上不同（JSON 写 `null`，`String(NaN)` 是 `"NaN"`）。
fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        kiana_json::format_js_number(value)
    }
}

/// JS 的 `Math.round`（正数）。
///
/// 当前唯一调用点用正的 `timeout_ms / 1000.0` 生成错误消息里的秒数，因此只需保证正数上的
/// 行为；负数 `.5` 边界没有按 JS（向 +∞ 取整）实现，不要把它当作通用 `Math.round` 复用。
fn js_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    if value >= 0.0 {
        (value + 0.5).floor()
    } else {
        -((-value + 0.5).floor())
    }
}

/// 在一根专用线程里读一行终端输入。
///
/// `tokio::task::spawn_blocking` 的任务不能被丢弃：读 stdin 的阻塞任务会一直卡住
/// `Runtime::drop`（等所有阻塞任务结束），进程于是无法退出。TS 的 `readline.close()`
/// 不会这样；这里改用**分离的** OS 线程——取消后它可能仍阻塞在 `read` 上，但不属于 tokio
/// 的阻塞池，运行时关闭和进程退出都不会等它。
///
/// 每次调用都会新建一根线程并立即返回接收端；线程结束时发送 `read_line` 的结果：`Ok(0)`
/// （EOF）与读错误都折叠为 `None`，读到内容则保留行尾换行符（消费方会 trim）。接收端被
/// 丢弃时线程仍会跑完，只是发送结果失败。
fn read_line_detached<R>(reader: R) -> tokio::sync::oneshot::Receiver<Option<String>>
where
    R: std::io::Read + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(reader);
        let mut line = String::new();
        let result = match reader.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(line),
            Err(_) => None,
        };
        let _ = sender.send(result);
    });
    receiver
}

/// 读取一行，直到拿到输入、EOF/读错误，或 `signal` 被取消（返回 `None`）。
///
/// `select!` 让取消立即生效：被丢弃的读任务不会阻止 `signal` 分支完成。读取线程异常退出时
/// 接收端返回 `Err`，这里同样折叠成 `None`（视为没有输入）。
async fn wait_for_line_or_cancel<R>(reader: R, signal: CancellationToken) -> Option<String>
where
    R: std::io::Read + Send + 'static,
{
    let read = read_line_detached(reader);
    tokio::select! {
        result = read => result.ok().flatten(),
        () = signal.cancelled() => None,
    }
}

/// 终端里粘贴的重定向 URL；否则只有浏览器回调能完成登录。
///
/// `interactive` 为假（stdin 不是终端，或调用方注入了 `open_url`）时不读 stdin：直接等待
/// `signal` 取消并返回 `None`。交互模式下提示直接写 stderr（而不是 `log` 回调），避免污染
/// stdout 上面向脚本的输出；输入行由调用方 trim 后再解析。
async fn wait_for_redirect_url(signal: CancellationToken, interactive: bool) -> Option<String> {
    if !interactive {
        if !signal.is_cancelled() {
            signal.cancelled().await;
        }
        return None;
    }
    let _ = std::io::stderr().write_all(
        b"If the browser cannot reach this machine, paste the URL it was redirected to: ",
    );
    let _ = std::io::stderr().flush();
    wait_for_line_or_cancel(std::io::stdin(), signal).await
}

/// [`McpSignInPrompt`] 的命令行实现：把授权 URL 展示给用户并尝试打开浏览器，
/// 交互模式下允许粘贴回调 URL。
struct CliSignInPrompt {
    name: String,
    log: McpCommandCallback,
    open_url: McpCommandCallback,
    interactive: bool,
}

#[async_trait]
impl McpSignInPrompt for CliSignInPrompt {
    /// 通过 `log` 打印 URL（用户可手工复制），再调用 `open_url` 尝试打开浏览器。
    fn show_authorization_url(&self, url: &Url) {
        (self.log)(&format!(
            "Sign in to MCP server \"{}\" in your browser:\n{}",
            self.name,
            url.as_str()
        ));
        (self.open_url)(url.as_str());
    }

    /// 超时或 stdin 读到 EOF 时返回 `None`，由 `sign_in_mcp_server` 判定为取消；浏览器先完成
    /// 回调时，`tokio::select!` 会丢弃该提问 future，本方法直接不返回。
    async fn prompt_for_redirect_url(&self, signal: CancellationToken) -> Option<String> {
        wait_for_redirect_url(signal, self.interactive).await
    }
}

/// 对 OAuth 服务器执行一次登录。
///
/// # 阶段
/// 1. 先连接：成功说明当前无需登录，直接报告已登录；失败时只有状态为 `needs-auth`
///    才继续，其它失败（进程不存在、网络错误）立即报错；
/// 2. 读取 OAuth 设置并校验 `timeout_ms`（`AbortSignal.timeout` 的 Node 规则），失败都以
///    `Sign-in ... failed: ...` 结束；
/// 3. 启动一个定时任务，到点取消 `CancellationToken`；把该 token 连同挑战交给
///    `sign_in_mcp_server`（浏览器回调服务器在它内部创建并关闭）；
/// 4. 成功后清除连接上旧的 OAuth 挑战并 `reconnect()`：重连用新凭据实际验证一次，失败则
///    报 "Signed in, but ..." 并返回 1（凭据此时已保存）。
///
/// `timeout_ms` 是整次登录的预算（毫秒），由 `--timeout` 的秒数乘 1000 得到；超时会取消
/// 令牌，`sign_in_mcp_server` 把因取消而失败的请求统一收敛为 `McpSignInError::Cancelled`
/// （粘贴提示读到 EOF/空行也走这个分支）。
#[allow(clippy::too_many_arguments)]
async fn login(
    entry: &McpServerEntry,
    connection: &McpServerConnection,
    url: &str,
    timeout_ms: f64,
    options: &McpCommandOptions,
    credentials: &Arc<McpOAuthCredentialStore>,
    log: &McpCommandCallback,
    error: &McpCommandCallback,
) -> i32 {
    let name = entry.name.clone();
    // 先连接：这能回答是否需要登录，并记录服务器的挑战。
    match connection.get_client().await {
        Ok(_) => {
            log(&format!(
                "Already signed in to MCP server \"{name}\" ({} tools).",
                connection.tools().len()
            ));
            return 0;
        }
        Err(_) => {
            if connection.state() != crate::mcp::runtime::ServerState::NeedsAuth {
                error(&format!(
                    "MCP server \"{name}\" failed to connect: {}",
                    connection
                        .error()
                        .unwrap_or_else(|| "unknown error".to_string())
                ));
                return 1;
            }
        }
    }

    let open_url: McpCommandCallback = match &options.open_url {
        Some(open_url) => Arc::clone(open_url),
        None => Arc::new(kiana_core::clipboard::open_browser),
    };
    // “粘贴回调 URL”的交互回填仅在 stdin 是终端、且调用方没有注入 `open_url` 时开启
    // （与 TS 的 `interactive = process.stdin.isTTY && options.openUrl === undefined` 一致）。
    let interactive = std::io::stdin().is_terminal() && options.open_url.is_none();
    let settings = match connection.oauth_settings() {
        Ok(settings) => settings,
        Err(message) => {
            error(&format!(
                "Sign-in to MCP server \"{name}\" failed: {message}"
            ));
            return 1;
        }
    };
    // TS 的 `AbortSignal.timeout(timeoutMs)` 在构造时校验 delay；RangeError 会在 try 里被
    // 捕获并打印成 "Sign-in ... failed: <message>"。
    let delay_ms = match abort_signal_timeout_delay(timeout_ms) {
        Ok(delay) => delay,
        Err(message) => {
            error(&format!(
                "Sign-in to MCP server \"{name}\" failed: {message}"
            ));
            return 1;
        }
    };
    let cancel = CancellationToken::new();
    // 定时器只负责在预算耗尽时取消登录；`sign_in_mcp_server` 返回后立即 abort，避免定时
    // 任务继续在后台睡眠。
    let cancel_task = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(delay_ms / 1000.0)).await;
            cancel.cancel();
        })
    };
    let store: Arc<dyn kiana_mcp::oauth::McpOAuthStateStore> =
        Arc::new(credentials.for_server_with_stored_issuer(&name, url));
    let prompt = Arc::new(CliSignInPrompt {
        name: name.clone(),
        log: Arc::clone(log),
        open_url,
        interactive,
    });
    let result = sign_in_mcp_server(McpSignInOptions {
        server_url: url.to_string(),
        store,
        settings,
        challenge: connection.challenge(),
        prompt,
        signal: Some(cancel.clone()),
    })
    .await;
    cancel_task.abort();
    match result {
        Ok(()) => {
            // issuer 隔离：登录写入 discovery 的 issuer 后，清掉同一服务器其它 issuer
            // 的旧凭据，避免下一次连接复用别的授权服务器的令牌。
            let _ = credentials.invalidate_stored_issuer_mismatches(&name, url);
        }
        Err(McpSignInError::Cancelled(_)) => {
            error(&format!(
                "Sign-in to MCP server \"{name}\" was cancelled or not completed within {} seconds.",
                js_round(timeout_ms / 1000.0) as i64
            ));
            return 1;
        }
        Err(McpSignInError::Failure(failure)) => {
            error(&format!(
                "Sign-in to MCP server \"{name}\" failed: {}",
                failure.message()
            ));
            return 1;
        }
    }
    // 登录成功后旧的 OAuth 挑战（scope / resource_metadata / 401 error）已经过时；它会被
    // 下一次 `login` 当作 step-up 依据传给 `sign_in_mcp_server`，因此这里清空。锁中毒时取回
    // 内部值继续（与 runtime 的其它锁处理一致）。
    *connection
        .challenge
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    if let Err(connect_error) = connection.reconnect().await {
        error(&format!("Signed in, but {connect_error}"));
        return 1;
    }
    log(&format!(
        "Signed in to MCP server \"{name}\" ({} tools).",
        connection.tools().len()
    ));
    0
}

/// `cli.lazy.ts#loadMcpCommand` 的等价物：本模块已经静态链接。
///
/// 返回函数指针而不是立即执行，调用方可以保存后按需调用。闭包不捕获任何环境，因此能
/// 强制转换成 `fn`；`Box::pin` 把拥有 `args`/`options` 的 future 擦成 `BoxFuture<'static, i32>`
/// （`run_mcp_command` 的参数按值接收，所以不需要生命周期参数）。
pub fn load_mcp_command()
-> fn(Vec<String>, McpCommandOptions) -> futures::future::BoxFuture<'static, i32> {
    |args, options| Box::pin(run_mcp_command(args, options))
}

// 单元测试锁定几类容易回归的契约：
// - 帮助文本的关键行（与 TS 逐字对齐）；
// - `js_number` 与 `abort_signal_timeout_delay` 的 ECMAScript/Node 语义（期望值取自 Node）；
// - JS 对象键序（整数索引键升序在前，其余保持插入序）；
// - `parse_options` 的位置参数上限与错误文案；
// - 取消粘贴提示时不能等待 stdin（依赖 `read_line_detached` 的分离线程）。
// 需要真实 MCP 服务器的端到端用例在 `tests/mcp_command.rs`。
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 去掉 ANSI 颜色码（TTY 上 chalk 会着色）。
    fn plain(text: &str) -> String {
        regex::Regex::new("\u{1b}\\[[0-9;]*m")
            .unwrap()
            .replace_all(text, "")
            .into_owned()
    }

    #[test]
    fn the_help_banner_matches_the_ts_text() {
        assert_eq!(plain("\u{1b}[1mUsage:\u{1b}[22m"), "Usage:");
        let help = plain(&help_text());
        assert!(
            help.starts_with("Usage:\n  kiana mcp add <server> [options] -- <command> [args...]\n"),
            "{help}"
        );
        assert!(
            help.contains(
                "Reads ~/.kiana/agent/mcp.json and, in trusted projects, .kiana/mcp.json."
            )
        );
        assert!(help.contains("  -l, --local             Use .kiana/mcp.json in the current project instead of the global file"));
        assert!(help.contains("Send \"Authorization: Bearer ${NAME}\""));
        assert!(help.contains("OAuth client secret (may be ${NAME} or !command)"));
        assert!(help.ends_with(
            "  --timeout <seconds>     Time limit for the whole sign-in (default: 300)"
        ));
    }

    #[test]
    fn js_number_matches_the_ecmascript_grammar() {
        assert_eq!(js_number(""), 0.0);
        assert_eq!(js_number("  "), 0.0);
        assert_eq!(js_number("300"), 300.0);
        assert_eq!(js_number(" 42 "), 42.0);
        assert_eq!(js_number("1e3"), 1000.0);
        assert_eq!(js_number("0x10"), 16.0);
        assert_eq!(js_number("0b101"), 5.0);
        assert_eq!(js_number("0o17"), 15.0);
        assert_eq!(js_number("Infinity"), f64::INFINITY);
        assert_eq!(js_number("-Infinity"), f64::NEG_INFINITY);
        assert!(js_number("nan").is_nan());
        assert!(js_number("abc").is_nan());
        assert!(js_number("-0x10").is_nan());
    }

    /// `Object.fromEntries` 的键序：整数索引键按数值升序在前，其余保持插入序。
    #[test]
    fn tool_exposure_uses_javascript_object_key_order() {
        let pairs: Vec<(String, String)> = [
            ("z", "direct"),
            ("10", "direct"),
            ("a", "hidden"),
            ("2", "direct"),
            ("1", "hidden"),
        ]
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        let map = indexmap_with_js_order(&pairs);
        assert_eq!(
            map.keys().cloned().collect::<Vec<_>>(),
            vec!["1", "2", "10", "z", "a"]
        );
        assert_eq!(map.get("1").map(String::as_str), Some("hidden"));
        // JSON 序列化（preserve_order）保持同样的键序。
        let value = serde_json::to_value(&map).unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["1", "2", "10", "z", "a"]
        );
    }

    /// 取消粘贴提示时不能等 stdin：`spawn_blocking` 的读会让 `Runtime::drop` 永远等待。
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_the_paste_prompt_does_not_wait_for_stdin() {
        use std::os::unix::net::UnixStream;

        let (reader, _writer) = UnixStream::pair().unwrap();
        let signal = CancellationToken::new();
        let cancel = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(Duration::from_secs(5), read_line_detached(reader)).await;
        // 只验证读取任务可以被丢弃：真正的等待逻辑由 `wait_for_redirect_url` 的 select 完成。
        assert!(result.is_err(), "the blocking read must not be joined");

        let (reader, _writer) = UnixStream::pair().unwrap();
        let signal = CancellationToken::new();
        let cancel = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let input = tokio::time::timeout(
            Duration::from_secs(5),
            wait_for_line_or_cancel(reader, signal),
        )
        .await
        .expect("cancellation returns promptly");
        assert_eq!(input, None);
    }

    /// 期望值来自 Node `AbortSignal.timeout(delay)`（node 26）。
    #[test]
    fn abort_signal_timeout_matches_node_validation() {
        assert_eq!(abort_signal_timeout_delay(300_000.0), Ok(300_000.0));
        assert_eq!(abort_signal_timeout_delay(0.0), Ok(0.0));
        assert_eq!(
            abort_signal_timeout_delay(2_147_483_647.0),
            Ok(2_147_483_647.0)
        );
        // Node 的计时器把超过 32 位有符号上限的整数钳到 1 ms。
        assert_eq!(abort_signal_timeout_delay(2_147_483_648.0), Ok(1.0));
        assert_eq!(abort_signal_timeout_delay(4_294_967_295.0), Ok(1.0));
        assert_eq!(
            abort_signal_timeout_delay(0.1),
            Err(
                "The value of \"delay\" is out of range. It must be an integer. Received 0.1"
                    .to_string()
            )
        );
        assert_eq!(
            abort_signal_timeout_delay(f64::NAN),
            Err(
                "The value of \"delay\" is out of range. It must be an integer. Received NaN"
                    .to_string()
            )
        );
        assert_eq!(
            abort_signal_timeout_delay(4_294_967_296.0),
            Err("The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 4294967296".to_string())
        );
        // 超过 2^32 的整数按 Node 的 `addNumericalSeparator` 分组。
        assert_eq!(
            abort_signal_timeout_delay(4_294_967_297.0),
            Err("The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 4_294_967_297".to_string())
        );
        assert_eq!(
            abort_signal_timeout_delay(5_000_000_000.0),
            Err("The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 5_000_000_000".to_string())
        );
        assert_eq!(
            abort_signal_timeout_delay(1e21),
            Err("The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 1e_+21".to_string())
        );
        assert_eq!(
            abort_signal_timeout_delay(-1.0),
            Err("The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received -1".to_string())
        );
    }

    #[test]
    fn parse_options_stops_at_the_positional_limit() {
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let error = move |line: &str| sink.lock().unwrap().push(line.to_string());
        let args: Vec<String> = [
            "--env", "A=1", "files", "--", "npx", "-y", "server", "--root", ".",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        let parsed = parse_options(
            &args,
            &[("env", OptionKind::List), ("url", OptionKind::Value)],
            &error,
            Some(2),
        )
        .expect("parses");
        assert_eq!(
            parsed.positional,
            vec![
                "files".to_string(),
                "npx".to_string(),
                "-y".to_string(),
                "server".to_string(),
                "--root".to_string(),
                ".".to_string()
            ]
        );
        assert_eq!(
            parsed.lists.get("env").cloned().unwrap_or_default(),
            vec!["A=1"]
        );
        assert!(errors.lock().unwrap().is_empty());
    }

    #[test]
    fn parse_options_reports_unknown_options_and_missing_values() {
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        let error = move |line: &str| sink.lock().unwrap().push(line.to_string());
        let args: Vec<String> = ["-l", "x"].iter().map(|arg| arg.to_string()).collect();
        assert!(parse_options(&args, &[], &error, None).is_none());
        assert_eq!(
            plain(&errors.lock().unwrap()[0]),
            "Unknown option --local.\nUse \"kiana mcp --help\" for usage."
        );
        errors.lock().unwrap().clear();
        let args: Vec<String> = ["--timeout"].iter().map(|arg| arg.to_string()).collect();
        assert!(parse_options(&args, &[("timeout", OptionKind::Value)], &error, None).is_none());
        assert_eq!(
            errors.lock().unwrap().as_slice(),
            ["--timeout needs a value."]
        );
    }
}
