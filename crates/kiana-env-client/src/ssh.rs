//! ssh.ts：daemon 部署与 `~/.kiana/mobile/tools/kiana-env`。
//!
//! 与 TS 的对应与差异：
//! - `node:child_process` → [`tokio::process`]；`node:fs/promises` → `tokio::fs`/`std::fs`；
//!   `node:crypto` 的 `createHash("sha256")` → `sha2`，`randomBytes` → `getrandom` +
//!   `hex`，`randomUUID` → [`crate::remote_env`]（本文件不需要）。
//! - 本地文件系统失败（mkdtemp/mkdir/readFile/writeFile/rename）用 Node 的 `fs` 错误消息
//!   （`EACCES: permission denied, open '/x'`，见 `node_fs_error`），不是 Rust `io::Error`
//!   的 `Display`。
//! - TS 的三个错误类 → [`HostKeyUnknownError`]、[`HostKeyChangedError`]、[`SshError`]，
//!   加上 [`SshFailure`] 统一承载（`Display` 逐字等于 TS 的 `error.message`）。
//! - `packageRoot`（npm 包目录）在 Rust 里不存在：`packagedDaemon` 保留
//!   `<root>/bin/kiana-env-<platform>-<arch>/kiana-env[.exe]` 的布局，`<root>` 取当前可执行文件的
//!   目录（生产路径）；测试一律显式给出 `binary`。
//! - `spawn` 的失败消息按 Node 的规则重建（`crate::spawn_error_message`：
//!   `spawn <program> ENOENT`）；超时用 `child.kill()`，即 POSIX 的 SIGTERM（不是 SIGKILL）。
//! - `mkdtemp`/Unix 权限（0700 目录、0600 临时文件）用手写实现（`tempfile` 只是 dev-dependency）。
//!
//! TS: packages/env/src/ssh.ts
//!
//! # 这个文件解决什么问题
//!
//! 远端执行环境要在一台机器上跑常驻 daemon（`kiana-env serve`），而本 crate 只能通过系统
//! `ssh` 访问那台机器。本模块负责整条引导链：拼 `ssh` 参数、探测远端系统、展示/接受/遗忘
//! host key、按内容哈希部署 daemon，最后把「ssh 参数 + 启动命令」交给
//! [`crate::connection::Connection`] 在每次启动会话时执行。帧协议与请求路由在
//! `connection.rs`；远端文件与命令语义在 `remote_env.rs`、`list_files.rs`，都不在这里。
//!
//! # 执行流程
//!
//! 1. [`scan_host_key`]：用 `StrictHostKeyChecking=accept-new` 对临时 known-hosts 文件连一次，
//!    抓取主机出示的 key。登录认证失败不影响结果（key 在认证前就写入了）；host key 变更则
//!    原样上抛。
//! 2. [`accept_host_key`] / [`forget_host_key`]：把行写进 [`SshTarget`] 的 `known_hosts_file`。
//!    同一路径的重写由进程内全局表 `KNOWN_HOSTS_CHANGES` 串行化，写入方式是「同目录临时文件 +
//!    rename」。
//! 3. [`detect_platform`]：经远端登录 shell 跑 POSIX 探测；失败（包括输出无法解析）后改用
//!    PowerShell 探测；Windows 主机再问一次远端 shell 是 cmd 还是 PowerShell。
//! 4. [`deploy_daemon`]：本地算二进制 SHA-256，远端按哈希前缀找同名文件并校验；缺失时上传到
//!    临时文件、校验通过后 rename 到位，并删掉其它内容的 daemon。
//! 5. [`ssh_connection`] / [`connect_ssh`]：构造 `ConnectionCommand::Dynamic`。`ssh_connection`
//!    完全懒执行（第一次请求才探测与部署）；`connect_ssh` 先做完探测与部署、把结果放进 state，
//!    第一次启动直接复用，之后每次启动再校验一次 daemon。
//!
//! # 重要约束
//!
//! - **诊断文本是与 TS 的契约**：错误消息、超时与退出码文案、known-hosts 与探测输出的解析都按
//!   `ssh.ts` 逐字移植；`tests/ssh.rs` 与本文件的单元测试会断言这些字符串。改措辞前先看测试。
//! - 远端命令始终是**一个字符串**（`ssh` 的最后一个参数），所以文件路径与整段脚本都要先引用
//!   （POSIX 用 `quote_posix`，PowerShell 用 `quote_powershell`，cmd.exe 的启动路径在含空格时
//!   用一对双引号），不能让远端 shell 再拆分或展开。
//! - 每次调用都是独立的 `ssh` 子进程（参数固定 `ControlMaster=no`、`ControlPath=none`），本模块
//!   不维护持久的 ssh 连接；子进程都设了 `kill_on_drop(true)`，Unix 上超时用 SIGTERM 而不是
//!   SIGKILL（非 Unix 走 `start_kill`，即 TerminateProcess）。
//! - 超时：普通探测/校验/扫描用 `SSH_TIMEOUT_MS`（60 s），上传二进制用 `UPLOAD_TIMEOUT_MS`
//!   （300 s）。
//!
//! # 失败模式
//!
//! `ssh` 的 stderr 被文本识别成两种 host key 错误：[`HostKeyUnknownError`]（key 尚未信任）与
//! [`HostKeyChangedError`]（key 变了，永远不会自动接受）；其余非零退出是带 `exit_code` 与
//! `stderr` 的 [`SshError`]。字段校验失败也走 [`SshFailure::Other`]，消息是
//! `Invalid <字段>: <JSON 引号值>`（`check_field`/`config_path`，例如
//! `Invalid host: "-oProxyCommand=bad"`，见单元测试 `invalid_fields_use_the_typescript_messages`）；
//! 只有本地文件系统失败（`open`/`read`/`rename`/`mkdir`/`mkdtemp` 这些 syscall 名）才按 Node
//! 的 `fs` 格式拼消息（`<CODE>: <描述>, <syscall> '<path>'`，见 `node_fs_error`）。经
//! [`ssh_connection`] 的懒启动路径，失败会变成 code `spawn_error` 的
//! `RemoteError`，只影响等待该次启动的请求，下一次请求重试。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::connection::{Connection, ConnectionCommand, ConnectionOptions, LogCallback};

/// 取锁守卫；mutex 被 poison 时继续使用内部数据。
///
/// poison 只可能来自持锁线程 panic。本模块的临界区都很短（追加读取缓冲、克隆缓存、查锁表），
/// 继续工作比让一次 panic 永久毒化整条 SSH 路径更合适，所以不复用 `unwrap`。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 远端主机的 key 不在 `known_hosts_file` 里；`scan_host_key` 展示它，由所有者决定是否接受。
///
/// TS: packages/env/src/ssh.ts#HostKeyUnknownError
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyUnknownError {
    /// `error.message`。
    pub message: String,
}

impl std::fmt::Display for HostKeyUnknownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostKeyUnknownError {}

/// 远端主机的 key 与 `known_hosts_file` 里的不同；永远不会被自动接受。
///
/// TS: packages/env/src/ssh.ts#HostKeyChangedError
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyChangedError {
    /// `error.message`。
    pub message: String,
}

impl std::fmt::Display for HostKeyChangedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostKeyChangedError {}

/// 因其它原因失败的 `ssh` 调用，带退出码与诊断输出。
///
/// TS: packages/env/src/ssh.ts#SshError
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshError {
    /// `error.message`。
    pub message: String,
    /// 退出码（被信号杀死或超时为 `None`，对应 TS 的 `null`）。
    pub exit_code: Option<i32>,
    /// `stderr` 的完整文本。
    pub stderr: String,
}

impl std::fmt::Display for SshError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SshError {}

/// `run_ssh` 与部署流程的失败：TS 的三种错误类，外加普通 `Error`（字段校验等）。
///
/// [`std::fmt::Display`] 与 [`SshFailure::message`] 都返回 TS 的 `error.message`；经
/// [`ssh_connection`] 的懒启动路径，这段文本会成为 `RemoteError.message`（code 为
/// `spawn_error`，见 `connection.rs` 的启动逻辑）。
///
/// TS: packages/env/src/ssh.ts#SshError
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshFailure {
    /// `SshError`。
    Ssh(SshError),
    /// `HostKeyUnknownError`。
    HostKeyUnknown(HostKeyUnknownError),
    /// `HostKeyChangedError`。
    HostKeyChanged(HostKeyChangedError),
    /// 普通 `Error`（`message`）。
    Other(String),
}

impl SshFailure {
    /// `error.message`。
    pub fn message(&self) -> &str {
        match self {
            SshFailure::Ssh(error) => &error.message,
            SshFailure::HostKeyUnknown(error) => &error.message,
            SshFailure::HostKeyChanged(error) => &error.message,
            SshFailure::Other(message) => message,
        }
    }
}

impl std::fmt::Display for SshFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for SshFailure {}

impl From<SshError> for SshFailure {
    fn from(error: SshError) -> Self {
        SshFailure::Ssh(error)
    }
}

impl From<HostKeyUnknownError> for SshFailure {
    fn from(error: HostKeyUnknownError) -> Self {
        SshFailure::HostKeyUnknown(error)
    }
}

impl From<HostKeyChangedError> for SshFailure {
    fn from(error: HostKeyChangedError) -> Self {
        SshFailure::HostKeyChanged(error)
    }
}

// ---------------------------------------------------------------------------
// 目标与平台
// ---------------------------------------------------------------------------

/// 本包提供 daemon 的远端系统。
///
/// TS: packages/env/src/ssh.ts#RemotePlatform
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemotePlatformName {
    /// Linux。
    Linux,
    /// Android（Termux）。
    Android,
    /// macOS。
    Darwin,
    /// Windows。
    Windows,
}

impl RemotePlatformName {
    /// TS 字面量拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            RemotePlatformName::Linux => "linux",
            RemotePlatformName::Android => "android",
            RemotePlatformName::Darwin => "darwin",
            RemotePlatformName::Windows => "windows",
        }
    }
}

/// 远端架构。
///
/// TS: packages/env/src/ssh.ts#RemotePlatform
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteArch {
    /// x86-64。
    X64,
    /// ARM64。
    Arm64,
}

impl RemoteArch {
    /// TS 字面量拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteArch::X64 => "x64",
            RemoteArch::Arm64 => "arm64",
        }
    }
}

/// Windows 上 `sshd` 运行命令用的 shell（`DefaultShell`）。
///
/// TS: packages/env/src/ssh.ts#RemoteShell
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteShell {
    /// cmd.exe。
    Cmd,
    /// PowerShell。
    Powershell,
}

impl RemoteShell {
    /// TS 字面量拼写。
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteShell::Cmd => "cmd",
            RemoteShell::Powershell => "powershell",
        }
    }
}

/// 一个远端系统。
///
/// `detect_platform` 产出的值里，`shell` 只在 Windows 上是 `Some`，`warnings` 目前只由
/// Android/Termux 产生；其它平台这两个字段分别是 `None` 与空 Vec。
///
/// TS: packages/env/src/ssh.ts#RemotePlatform
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePlatform {
    /// 系统。
    pub platform: RemotePlatformName,
    /// 架构。
    pub arch: RemoteArch,
    /// 远端 home 目录（远端系统自己的拼写）。
    pub home: String,
    /// Windows：`sshd` 运行命令用的 shell。
    pub shell: Option<RemoteShell>,
    /// 值得告诉所有者的注意事项，例如没有 `termux-exec` 的 Termux。
    pub warnings: Vec<String>,
}

/// 如何用系统 `ssh` 访问一台机器。
///
/// TS: packages/env/src/ssh.ts#SshTarget
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// 主机名或 `~/.ssh/config` 的别名。
    pub host: String,
    /// 登录用户。
    pub user: Option<String>,
    /// 端口。
    pub port: Option<u32>,
    /// 身份文件。
    pub identity_file: Option<String>,
    /// 本应用信任的 host key 文件；`accept_host_key` 往里加。
    pub known_hosts_file: String,
    /// key 存取用的名字，与别名、端口、跳板无关，例如 `kiana-env-<env name>`。
    pub host_key_alias: String,
    /// `ssh` 程序；默认 `ssh`。
    pub ssh: Option<String>,
    /// 代替 `~/.ssh/config` 的配置文件（`-F`）。
    pub config_file: Option<String>,
}

impl SshTarget {
    /// 必填字段的构造器。
    ///
    /// 其余字段（user/port/identity_file/ssh/config_file）都是 `None`。
    pub fn new(
        host: impl Into<String>,
        known_hosts_file: impl Into<String>,
        host_key_alias: impl Into<String>,
    ) -> Self {
        SshTarget {
            host: host.into(),
            user: None,
            port: None,
            identity_file: None,
            known_hosts_file: known_hosts_file.into(),
            host_key_alias: host_key_alias.into(),
            ssh: None,
            config_file: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 字段校验与参数
// ---------------------------------------------------------------------------

/// `JSON.stringify(value)` 的字符串形式（只用于引号内的诊断文本）。
fn json_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

/// JS 正则 `\s` 的字符集（含 BOM；Rust 的 `char::is_whitespace` 不含 U+FEFF）。
fn is_js_space(character: char) -> bool {
    matches!(
        character,
        ' ' | '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn is_control(character: char) -> bool {
    let code = character as u32;
    code <= 0x1f || code == 0x7f
}

/// JS 的 `String.prototype.trim()`：去掉两端的空白（`is_js_space`；不含 U+0085，含 U+FEFF）。
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// JS 的 `text.trim().split(/\s+/)`：按 `\s` 的连续片段切分，丢掉空片段。
fn js_whitespace_fields(text: &str) -> Vec<&str> {
    js_trim(text)
        .split(is_js_space)
        .filter(|field| !field.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Node 的 fs 错误
// ---------------------------------------------------------------------------

/// Node 的 `fs` 错误消息：`<CODE>: <描述>, <syscall> '<path>'`。
///
/// TS 的 `fs/promises` 拒绝值是 Node 的 `ErrnoException`，`error.message` 形如
/// `EACCES: permission denied, open '/x'`；调用方（`sshConnection` 的 command 函数）把它原样
/// 放进 `RemoteError.message`，所以消息是当前稳定契约，改动需同步测试；不能用 Rust `io::Error` 的 `Display`（格式不同）。
fn node_fs_error(error: &std::io::Error, syscall: &str, path: &str) -> SshFailure {
    SshFailure::Other(format!("{}{syscall} '{path}'", fs_error_prefix(error)))
}

/// 两个路径的 fs 错误：`<CODE>: <描述>, rename '<from>' -> '<to>'`。
fn node_fs_error_pair(error: &std::io::Error, syscall: &str, from: &str, to: &str) -> SshFailure {
    SshFailure::Other(format!(
        "{}{syscall} '{from}' -> '{to}'",
        fs_error_prefix(error)
    ))
}

/// `<CODE>: <描述>, ` 前缀；不是 OS 错误时沿用 `io::Error` 的 `Display`。
fn fs_error_prefix(error: &std::io::Error) -> String {
    match crate::uv_error_text(error) {
        Some(text) => format!("{text}, "),
        None => format!("{error}, "),
    }
}

/// `readFile(path)` 的错误：打开失败是 `open`；读到目录时 Node 报 `read`（`ReadFile` 先
/// `open` 再 `read`，EISDIR 来自后者）。
fn node_read_file_error(error: &std::io::Error, path: &str) -> SshFailure {
    #[cfg(unix)]
    let syscall = if error.raw_os_error() == Some(libc::EISDIR) {
        "read"
    } else {
        "open"
    };
    #[cfg(not(unix))]
    let syscall = "open";
    node_fs_error(error, syscall, path)
}

/// 没有前导 `-`（会被 `ssh` 当成选项），也没有空白或控制字符。
///
/// 这些值会直接进入 `ssh` 命令行；空串同样拒绝。错误消息用 [`json_quote`] 的引号形式（TS 的
/// `JSON.stringify`），调用方是 [`ssh_arguments_with`]（host / host key alias / user）。
fn check_field(name: &str, value: &str) -> Result<(), SshFailure> {
    let invalid = value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|c| is_js_space(c) || is_control(c));
    if invalid {
        return Err(SshFailure::Other(format!(
            "Invalid {name}: {}",
            json_quote(value)
        )));
    }
    Ok(())
}

/// 给会展开 `%` 记号的 `ssh` 选项用的路径：加引号，`%` 写成字面量。
///
/// 路径被放进 `-o Name=value` 的单个 argv 元素里；双引号会在 `ssh` 解析该选项时结束引用，
/// 控制字符会破坏选项文本，两者都直接拒绝（空格可以保留在引号内，不做限制）。
fn config_path(name: &str, path: &str) -> Result<String, SshFailure> {
    let invalid = path.is_empty()
        || path
            .chars()
            .any(|character| character == '"' || is_control(character));
    if invalid {
        return Err(SshFailure::Other(format!(
            "Invalid {name}: {}",
            json_quote(path)
        )));
    }
    Ok(format!("\"{}\"", path.replace('%', "%%")))
}

/// `sshArguments(target)`（`strictHostKeys = true`、`knownHostsFile = target.knownHostsFile`）。
///
/// 构造访问 `target` 的完整 `ssh` 参数（不含远端命令）：使用目标自己的 known-hosts 文件并严格
/// 校验 host key。返回的 Vec 以 `--` 和 `host` 结尾，调用方把远端命令作为下一个（最后一个）
/// 参数追加。只做参数拼装与字段校验，不启动进程；非法字段返回 [`SshFailure::Other`]。
///
/// TS: packages/env/src/ssh.ts#sshArguments
pub fn ssh_arguments(target: &SshTarget) -> Result<Vec<String>, SshFailure> {
    ssh_arguments_with(target, true, &target.known_hosts_file)
}

/// [`ssh_arguments`] 的完整形式。
///
/// `strict_host_keys = true` 用 `StrictHostKeyChecking=yes`，`false` 用 `accept-new`
/// （[`scan_host_key`] 借此把主机出示的 key 记进临时文件）；`known_hosts_file` 覆盖
/// [`SshTarget::known_hosts_file`]（扫描时指向临时目录）。
///
/// 参数顺序本身是契约：本文件的 `ssh_arguments_have_the_documented_order` 测试逐项锁定。
///
/// TS: packages/env/src/ssh.ts#sshArguments
pub fn ssh_arguments_with(
    target: &SshTarget,
    strict_host_keys: bool,
    known_hosts_file: &str,
) -> Result<Vec<String>, SshFailure> {
    check_field("host", &target.host)?;
    check_field("host key alias", &target.host_key_alias)?;
    if let Some(user) = &target.user {
        check_field("user", user)?;
    }
    if let Some(port) = target.port
        && !(1..=65535).contains(&port)
    {
        return Err(SshFailure::Other(format!("Invalid port: {port}")));
    }
    let option = |setting: String| vec!["-o".to_owned(), setting];
    let mut arguments: Vec<String> = Vec::new();
    if let Some(config_file) = &target.config_file {
        arguments.push("-F".to_owned());
        arguments.push(config_file.clone());
    }
    for flag in ["-T", "-a", "-x"] {
        arguments.push(flag.to_owned());
    }
    // 这些设置共同保证 ssh 只做「一条命令」：BatchMode 禁用交互提问；ClearAllForwardings /
    // ForwardAgent / ForwardX11 关掉各类转发；ControlMaster / ControlPath 关掉连接复用；
    // RemoteCommand / PermitLocalCommand 让 ~/.ssh/config 里的命令不生效；SendEnv=-* 不转发
    // locale。UserKnownHostsFile 指向本应用自己的文件，HostKeyAlias 用与主机名、端口、跳板机
    // 无关的固定别名，因此信任记录不会随连接方式变化。
    let settings = vec![
        "BatchMode=yes".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
        "ForwardAgent=no".to_owned(),
        "ForwardX11=no".to_owned(),
        "ControlMaster=no".to_owned(),
        "ControlPath=none".to_owned(),
        "RemoteCommand=none".to_owned(),
        "PermitLocalCommand=no".to_owned(),
        "SendEnv=-*".to_owned(),
        "ServerAliveInterval=15".to_owned(),
        format!(
            "StrictHostKeyChecking={}",
            if strict_host_keys {
                "yes"
            } else {
                "accept-new"
            }
        ),
        format!(
            "UserKnownHostsFile={}",
            config_path("known hosts file", known_hosts_file)?
        ),
        "GlobalKnownHostsFile=none".to_owned(),
        "HashKnownHosts=no".to_owned(),
        format!("HostKeyAlias={}", target.host_key_alias),
    ];
    arguments.extend(option_vec(&settings));
    if let Some(user) = &target.user {
        arguments.push("-l".to_owned());
        arguments.push(user.clone());
    }
    if let Some(port) = target.port {
        arguments.push("-p".to_owned());
        arguments.push(port.to_string());
    }
    if let Some(identity_file) = &target.identity_file {
        arguments.extend(option(format!(
            "IdentityFile={}",
            config_path("identity file", identity_file)?
        )));
        arguments.extend(option("IdentitiesOnly=yes".to_owned()));
    }
    arguments.push("--".to_owned());
    arguments.push(target.host.clone());
    Ok(arguments)
}

fn option_vec(settings: &[String]) -> Vec<String> {
    let mut arguments = Vec::with_capacity(settings.len() * 2);
    for setting in settings {
        arguments.push("-o".to_owned());
        arguments.push(setting.clone());
    }
    arguments
}

// ---------------------------------------------------------------------------
// 运行 ssh
// ---------------------------------------------------------------------------

/// 一次检测、校验或上传最多可以花多久。
///
/// 默认的 `SSH_TIMEOUT_MS`（60 s）用于探测、host-key 扫描与 daemon 校验；上传 daemon 二进制
/// 另用 `UPLOAD_TIMEOUT_MS`（300 s，见 [`deploy_daemon`]）。
const SSH_TIMEOUT_MS: u64 = 60_000;
const UPLOAD_TIMEOUT_MS: u64 = 300_000;

/// [`run_ssh`] 的选项。
///
/// - `stdin`：写入子进程标准输入后立即关闭（TS 的 `child.stdin.end(bytes)`）；
/// - `args`：覆盖默认的 [`ssh_arguments`]（host-key 扫描要换成 `accept-new` + 临时文件）；
/// - `timeout_ms`：不填时用 `SSH_TIMEOUT_MS`。
#[derive(Default)]
struct RunSshOptions<'a> {
    stdin: Option<&'a [u8]>,
    args: Option<Vec<String>>,
    timeout_ms: Option<u64>,
}

/// 超时消息里的秒数：整秒写成整数，非整秒按 JS 的 `String(number)` 规则（如 `0.2`、`0.001`）。
///
/// 对应 TS 的 `${timeoutMs / 1000}`；用例 `format_js_number_matches_javascript_string_conversion`
/// 锁定这套格式。
fn timeout_seconds_text(timeout_ms: u64) -> String {
    if timeout_ms.is_multiple_of(1000) {
        (timeout_ms / 1000).to_string()
    } else {
        crate::format_js_number(timeout_ms as f64 / 1000.0)
    }
}

/// 退出码在消息里的文本：`None`（被信号杀死或超时）写成 `null`，与 TS 的 `code === null` 对齐。
fn exit_code_text(code: Option<i32>) -> String {
    match code {
        Some(code) => code.to_string(),
        None => "null".to_owned(),
    }
}

/// 把 `stream` 读到 EOF，把每个 chunk 追加进 `sink`。
///
/// 读取错误与 EOF 一样直接返回：此时子进程多半已经退出或管道被关闭，已收集的字节仍用于
/// stdout 返回值或错误消息；两个调用方都忽略读取任务的 JoinHandle。
async fn read_all(mut stream: impl tokio::io::AsyncRead + Unpin, sink: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => lock(&sink).extend_from_slice(&chunk[..read]),
        }
    }
}

/// 远端 stderr 里是否写着“host key 未知”。
///
/// 逐行做两种文本判断：包含 `Host key verification failed`，或某行在 ` host key is known`
/// 之前出现 `No `（匹配 `No ED25519 host key is known for ...`）。这是对 OpenSSH 现有措辞的
/// 文本匹配，不是完整的诊断解析；两者都不匹配时会落到普通的 [`SshError`] 分支。
fn stderr_means_unknown_host_key(stderr: &str) -> bool {
    for line in stderr.split('\n') {
        if line.contains("Host key verification failed") {
            return true;
        }
        if let Some(index) = line.find(" host key is known")
            && line[..index].contains("No ")
        {
            return true;
        }
    }
    false
}

/// 经 `ssh` 跑一条远端命令，可选地喂 stdin；成功时返回 stdout，失败时拒绝。
///
/// 步骤：
/// 1. 组装父进程参数：[`ssh_arguments`]（或 `options.args`）+ 远端命令；三个标准流都是 pipe，
///    并设 `kill_on_drop(true)`。
/// 2. `spawn` 失败时按 Node 的两条路径分流：异步 errno 包成 [`SshError`]；其它 errno 是 TS 里
///    `spawn()` 同步抛出的 `ErrnoException`，用 [`SshFailure::Other`]（判定见
///    `crate::spawn_error_is_asynchronous`）。
/// 3. 两个后台任务并行读 stdout/stderr，另一个任务写 stdin 后关闭。
/// 4. 等待子进程退出；超过 `timeout_ms` 则发 SIGTERM，用**此刻已收集到的** stderr 组成消息，
///    并另起任务 reap 子进程。
/// 5. 退出码为 0 时返回 stdout（UTF-8 lossy 解码）；否则先按 stderr 文本识别 host key 变更/
///    未知，再回退到带退出码与 stderr 的 [`SshError`]。
///
/// 被信号杀死时 `ExitStatus::code()` 是 `None`，消息里写作 `null`（[`exit_code_text`]）。
async fn run_ssh(
    target: &SshTarget,
    command: &str,
    options: RunSshOptions<'_>,
) -> Result<String, SshFailure> {
    let arguments = match options.args {
        Some(arguments) => arguments,
        None => ssh_arguments(target)?,
    };
    let timeout_ms = options.timeout_ms.unwrap_or(SSH_TIMEOUT_MS);
    let program = target.ssh.clone().unwrap_or_else(|| "ssh".to_owned());
    let mut process = Command::new(&program);
    process
        .args(&arguments)
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = crate::spawn_error_message(&program, &error);
            return Err(if crate::spawn_error_is_asynchronous(&error) {
                SshFailure::Ssh(SshError {
                    message,
                    exit_code: None,
                    stderr: String::new(),
                })
            } else {
                // Node：`spawn()` 同步抛出的 ErrnoException 让 runSsh 的 Promise 直接拒绝，
                // 不是 SshError（消息同样是 `spawn <CODE>`）。
                SshFailure::Other(message)
            });
        }
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
    let stdout_task = tokio::spawn(read_all(stdout, Arc::clone(&stdout_bytes)));
    let stderr_task = tokio::spawn(read_all(stderr, Arc::clone(&stderr_bytes)));
    if let Some(mut stdin) = stdin {
        let bytes = options.stdin.unwrap_or(&[]).to_vec();
        tokio::spawn(async move {
            // `child.stdin.end(options.stdin ?? new Uint8Array(0))`
            let _ = stdin.write_all(&bytes).await;
            drop(stdin);
        });
    }
    // 读取任务必须与 wait 并行：若先等子进程退出再读，stdout/stderr 管道写满会让孩子永远
    // 阻塞在 write 上，父子的等待互相卡住。
    let wait = async {
        let status = child.wait().await;
        let _ = stdout_task.await;
        let _ = stderr_task.await;
        status
    };
    match tokio::time::timeout(Duration::from_millis(timeout_ms), wait).await {
        Err(_elapsed) => {
            // TS: `child.kill()`（SIGTERM，不是 SIGKILL），然后立即读取已经收集的 stderr。
            crate::kill_child(&mut child);
            let diagnostics = String::from_utf8_lossy(&lock(&stderr_bytes)).into_owned();
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Err(SshFailure::Ssh(SshError {
                message: format!(
                    "ssh {} did not finish within {} s: {}",
                    target.host,
                    timeout_seconds_text(timeout_ms),
                    js_trim(&diagnostics)
                ),
                exit_code: None,
                stderr: diagnostics,
            }))
        }
        Ok(status) => {
            let output = String::from_utf8_lossy(&lock(&stdout_bytes)).into_owned();
            let diagnostics = String::from_utf8_lossy(&lock(&stderr_bytes)).into_owned();
            let code = status.ok().and_then(|status| status.code());
            if code == Some(0) {
                return Ok(output);
            }
            if diagnostics.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
                return Err(SshFailure::HostKeyChanged(HostKeyChangedError {
                    message: format!(
                        "The host key of {} changed; remove the old key to continue",
                        target.host
                    ),
                }));
            }
            if stderr_means_unknown_host_key(&diagnostics) {
                return Err(SshFailure::HostKeyUnknown(HostKeyUnknownError {
                    message: format!("The host key of {} is not trusted yet", target.host),
                }));
            }
            Err(SshFailure::Ssh(SshError {
                message: format!(
                    "ssh {} failed with exit code {}: {}",
                    target.host,
                    exit_code_text(code),
                    js_trim(&diagnostics)
                ),
                exit_code: code,
                stderr: diagnostics,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// host key
// ---------------------------------------------------------------------------

/// `mkdtemp(join(tmpdir(), prefix))`：`prefix` + 6 个随机字母数字。
///
/// 目录以 0700 创建（Unix），因为它会短暂存放 known-hosts 副本；随机后缀来自 `getrandom`。
fn mkdtemp(prefix: &str) -> Result<PathBuf, SshFailure> {
    mkdtemp_in(&std::env::temp_dir(), prefix)
}

/// `mkdtemp(join(base, prefix))`；错误消息是 Node 的 `<CODE>: <描述>, mkdtemp '<path>'`，
/// 路径带尝试过的随机后缀（libuv 的 `mkdtemp` 也这样报）。
///
/// 与 libuv 一致：EEXIST 时换一个随机名重试，共 10 次；最后一次仍冲突则报 EEXIST，路径用最后
/// 一次尝试的名字。其它错误立即按 Node 的 fs 消息返回。
fn mkdtemp_in(base: &Path, prefix: &str) -> Result<PathBuf, SshFailure> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    // libuv 在 EEXIST 时换一个随机名重试 10 次，之后返回 EEXIST。
    let mut last = base.join(prefix);
    for _ in 0..10 {
        let mut bytes = [0u8; 6];
        getrandom::fill(&mut bytes).expect("the operating system provides random bytes");
        let suffix: String = bytes
            .iter()
            .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
            .collect();
        let path = base.join(format!("{prefix}{suffix}"));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last = path;
            }
            Err(error) => {
                return Err(node_fs_error(&error, "mkdtemp", &path.to_string_lossy()));
            }
        }
    }
    Err(SshFailure::Other(format!(
        "EEXIST: file already exists, mkdtemp '{}'",
        last.to_string_lossy()
    )))
}

/// 用 `accept-new` 对临时 known-hosts 文件连一次，抓取主机出示的 key（经由真实连接的同一路径：
/// `~/.ssh/config`、跳板机）。返回它的 known-hosts 行与指纹。
///
/// 返回 `(known-hosts 行, ssh-keygen -lf 的指纹行)`：两者都只丢弃 trim 后为空的行，内容本身
/// 不改写。探测失败的分类很关键：`SshError` 与 `HostKeyUnknownError` 被忽略（登录认证失败时
/// key 已在认证前写入临时文件），`HostKeyChangedError` 与其它错误原样上抛；抓不到任何行、读
/// 临时文件失败或启动 `ssh-keygen` 失败时另行报错。临时目录在返回前删除，删除失败
/// 被忽略。指纹只是展示材料，TS 注释明确说明它不等于认证，是否信任应由所有者核对后决定。
///
/// TS: packages/env/src/ssh.ts#scanHostKey
pub async fn scan_host_key(target: &SshTarget) -> Result<(Vec<String>, Vec<String>), SshFailure> {
    let directory = mkdtemp("kiana-env-hostkey-")?;
    let result = async {
        let scanned = directory.join("known_hosts");
        let scanned_text = scanned.to_string_lossy().into_owned();
        // Authentication may fail without the key; the key is recorded before authentication.
        let attempt = run_ssh(
            target,
            "exit 0",
            RunSshOptions {
                args: Some(ssh_arguments_with(target, false, &scanned_text)?),
                ..RunSshOptions::default()
            },
        )
        .await;
        match attempt {
            Ok(_) => {}
            Err(SshFailure::Ssh(_)) | Err(SshFailure::HostKeyUnknown(_)) => {}
            Err(other) => return Err(other),
        }
        let lines = if scanned.exists() {
            let bytes = tokio::fs::read(&scanned)
                .await
                .map_err(|error| node_read_file_error(&error, &scanned_text))?;
            String::from_utf8_lossy(&bytes)
                .split('\n')
                .filter(|line| !js_trim(line).is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if lines.is_empty() {
            return Err(SshFailure::Other(format!(
                "No host key received from {}",
                target.host
            )));
        }
        let mut child = Command::new("ssh-keygen")
            .arg("-lf")
            .arg(&scanned)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| SshFailure::Other(crate::spawn_error_message("ssh-keygen", &error)))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let output = Arc::new(Mutex::new(Vec::new()));
        let output_task = tokio::spawn(read_all(stdout, Arc::clone(&output)));
        let stderr_task = tokio::spawn(read_all(stderr, Arc::new(Mutex::new(Vec::new()))));
        // The exit status is not checked: a non-zero `ssh-keygen` still resolves (possibly empty).
        let _ = child.wait().await;
        let _ = output_task.await;
        let _ = stderr_task.await;
        let fingerprints = String::from_utf8_lossy(&lock(&output))
            .split('\n')
            .filter(|line| !js_trim(line).is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Ok((lines, fingerprints))
    }
    .await;
    let _ = std::fs::remove_dir_all(&directory);
    result
}

/// `alias type key [comment]` 的一行 known-hosts：没有 marker、模式或哈希主机名。
///
/// 第一个字段必须逐字等于 `alias`，类型与 key 必须通过 [`valid_key_type`] /
/// [`valid_key_base64`]；因此 `@cert-authority` 这类以 marker 开头的行会因为第一个字段不是
/// 别名而被拒绝。返回 `(key_type, key)`，注释字段被忽略。错误消息带原始行（JSON 引号形式）。
fn parse_host_key_line(alias: &str, line: &str) -> Result<(String, String), SshFailure> {
    let fields = js_whitespace_fields(line);
    match (fields.first(), fields.get(1), fields.get(2)) {
        (Some(host), Some(key_type), Some(key))
            if *host == alias && valid_key_type(key_type) && valid_key_base64(key) =>
        {
            Ok(((*key_type).to_owned(), (*key).to_owned()))
        }
        _ => Err(SshFailure::Other(format!(
            "Not a host key line for {alias}: {}",
            json_quote(line)
        ))),
    }
}

/// known-hosts 的 key 类型：`ssh-*` / `ecdsa-sha2-*` 的后缀只允许小写字母、数字与 `-`，
/// `sk-*` 额外允许 `@` 与 `.`；其余前缀一律拒绝。
fn valid_key_type(text: &str) -> bool {
    let suffix_ok = |suffix: &str| {
        !suffix.is_empty()
            && suffix.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            })
    };
    if let Some(rest) = text.strip_prefix("ssh-") {
        return suffix_ok(rest);
    }
    if let Some(rest) = text.strip_prefix("ecdsa-sha2-") {
        return suffix_ok(rest);
    }
    if let Some(rest) = text.strip_prefix("sk-") {
        return !rest.is_empty()
            && rest.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '@' | '.' | '-')
            });
    }
    false
}

/// key 主体：只接受 base64 字母表（字母、数字、`+`、`/`）与末尾最多两个 `=`；
/// 三个及以上填充、空串都拒绝。
fn valid_key_base64(text: &str) -> bool {
    let mut base = text;
    for _ in 0..2 {
        if let Some(stripped) = base.strip_suffix('=') {
            base = stripped;
        }
    }
    !base.is_empty()
        && base.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '+' || character == '/'
        })
}

/// 正在被修改的 known-hosts 文件，同一个文件的修改串行进行。
///
/// 每个路径对应一把异步互斥锁；调用者在锁内完成「读 → 改 → 临时文件 → rename」，因此同一
/// 文件的并发修改不会互相覆盖，不同路径可以并行。这是进程内状态（对应 TS 的模块级 Map）；
/// 跨进程写同一个文件没有额外加锁，只依赖同目录 rename 的替换语义（临时文件与目标在同目录）。
static KNOWN_HOSTS_CHANGES: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 一次一个地、原子地重写 known-hosts 文件。
///
/// 先按路径取（或创建）串行锁，持锁执行 [`change_known_hosts_locked`]；结束后若表项还指向
/// 同一把锁且没有其它持有者（map 自己与本地 `serial` 各一份引用），就把表项移除，避免表无限
/// 增长。`change` 返回的错误会原样传出，且不会修改文件。
async fn change_known_hosts<F>(file: &str, change: F) -> Result<(), SshFailure>
where
    F: FnOnce(Vec<String>) -> Result<Vec<String>, SshFailure>,
{
    let key = PathBuf::from(file);
    let serial = {
        let mut changes = lock(&KNOWN_HOSTS_CHANGES);
        Arc::clone(
            changes
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    };
    let _guard = serial.lock().await;
    let result = change_known_hosts_locked(file, change).await;
    {
        let mut changes = lock(&KNOWN_HOSTS_CHANGES);
        // 只有在没有别的调用者还持有这个 mutex 时才把表项删掉（map 里一份 + 本地的 `serial`）。
        // 还有排队者时删掉，后面来的调用会新建一把锁，与排队者并发重写同一个文件。
        if changes
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(entry, &serial) && Arc::strong_count(entry) == 2)
        {
            changes.remove(&key);
        }
    }
    result
}

/// 在串行锁内执行一次「读现有行 → `change` → 原子重写」。
///
/// 读取时按 `\n` 切分并丢掉空行（行内空白保留）；写出的内容若非空，每行以 `\n` 结尾。
/// 文件不存在时把空 Vec 交给 `change`。目录按 `mkdir -p` 语义以 0700 创建。只有 `rename` 失败
/// 时会删除临时文件，此时目标文件保持旧内容；`write_temp` 的 open/write_all/flush 任一失败则
/// 直接返回，已创建的 `<file>.<hex>.tmp` 不会被清理，可能残留临时文件。
async fn change_known_hosts_locked<F>(file: &str, change: F) -> Result<(), SshFailure>
where
    F: FnOnce(Vec<String>) -> Result<Vec<String>, SshFailure>,
{
    let path = Path::new(file);
    // `dirname("known_hosts")` is `"."`; only create a directory when there is one.
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        make_directory(parent)?;
    }
    let existing = if path.exists() {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|error| node_read_file_error(&error, file))?;
        String::from_utf8_lossy(&bytes)
            .split('\n')
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let lines = change(existing)?;
    // 临时文件与目标同目录（rename 不跨文件系统），名字带 6 字节随机十六进制，避免与残留的
    // 旧临时文件撞名。
    let temporary = format!("{file}.{}.tmp", random_hex(6));
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    write_temp(&temporary, content.as_bytes())
        .await
        .map_err(|error| node_fs_error(&error, "open", &temporary))?;
    match tokio::fs::rename(&temporary, path).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            Err(node_fs_error_pair(&error, "rename", &temporary, file))
        }
    }
}

/// `mkdir(path, { recursive: true, mode: 0o700 })`。
///
/// 与 Node 的递归实现一致：先 `mkdir` 完整路径；`ENOENT` 时先建父目录再重试；`EEXIST` 时只有
/// 路径是目录才算成功（否则原样报 EEXIST）。错误消息带**失败的那一级**路径（Node 的
/// `mkdir '/ro/a'`，不是完整路径）。
fn make_directory(path: &Path) -> Result<(), SshFailure> {
    match create_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if path.is_dir() {
                Ok(())
            } else {
                Err(node_fs_error(&error, "mkdir", &path.to_string_lossy()))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Node 的 `dirname` 对单段路径返回 "."。
            let parent = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            make_directory(parent)?;
            match create_directory(path) {
                Ok(()) => Ok(()),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() =>
                {
                    Ok(())
                }
                Err(error) => Err(node_fs_error(&error, "mkdir", &path.to_string_lossy())),
            }
        }
        Err(error) => Err(node_fs_error(&error, "mkdir", &path.to_string_lossy())),
    }
}

fn create_directory(path: &Path) -> std::io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// `writeFile(temporary, …, { mode: 0o600, flag: "wx" })`。
///
/// `create_new(true)` 对应 Node 的 `wx`：目标已存在时返回 EEXIST，不会截断别人正在写的文件；
/// Unix 上以 0600 创建。内容写完并 flush 后返回。
async fn write_temp(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// 随机字节的十六进制串（`randomBytes(bytes).toString("hex")`），用于临时文件名；
/// 随机源失败会 panic（`expect`）。
fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the operating system provides random bytes");
    hex::encode(buffer)
}

/// 信任 `scan_host_key` 的 host-key 行：把它们加到目标的 known-hosts 文件里。只接受目标的别名下
/// 的普通行。与已信任的同类型 key 不同时以 [`HostKeyChangedError`] 拒绝；先 `forget_host_key` 才能
/// 换 key。
///
/// 流程：先在锁外把所有输入行逐行校验（任一行不合法就整体拒绝，不碰文件），再在文件锁内收集
/// 已有的 `(别名, 类型) -> key`：同类型同 key 跳过，同类型不同 key 返回
/// [`HostKeyChangedError`]，新类型追加到文件末尾。写入是「临时文件 + rename」，被拒时目标文件
/// 保持原样。
///
/// TS: packages/env/src/ssh.ts#acceptHostKey
pub async fn accept_host_key(target: &SshTarget, lines: &[String]) -> Result<(), SshFailure> {
    let alias = target.host_key_alias.clone();
    let mut accepted: Vec<(String, String, String)> = Vec::with_capacity(lines.len());
    for line in lines {
        let (key_type, key) = parse_host_key_line(&alias, line)?;
        accepted.push((js_trim(line).to_owned(), key_type, key));
    }
    let host = target.host.clone();
    change_known_hosts(&target.known_hosts_file, move |existing| {
        let mut trusted: HashMap<String, String> = HashMap::new();
        for line in &existing {
            let fields = js_whitespace_fields(line);
            if let (Some(found_host), Some(key_type), Some(key)) =
                (fields.first(), fields.get(1), fields.get(2))
                && *found_host == alias
            {
                trusted.insert((*key_type).to_owned(), (*key).to_owned());
            }
        }
        let mut added: Vec<String> = Vec::new();
        for (line, key_type, key) in &accepted {
            let known = trusted.get(key_type);
            if known == Some(key) {
                continue;
            }
            if known.is_some() {
                return Err(SshFailure::HostKeyChanged(HostKeyChangedError {
                    message: format!(
                        "The {key_type} host key of {host} changed; forget the old key first"
                    ),
                }));
            }
            trusted.insert(key_type.clone(), key.clone());
            added.push(line.clone());
        }
        let mut lines = existing;
        lines.extend(added);
        Ok(lines)
    })
    .await
}

/// 不再信任目标的别名下存的所有 key（例如所有者确认 host key 变更之后）。
///
/// 判断只看每行第一个 JS 空白分隔的字段：等于 `host_key_alias` 的行全部删除，其它行逐字保留
/// （含行内多余空白）。以 marker 开头的行（如 `@cert-authority ...`）第一个字段不是别名，
/// 因此不会被删除。重写同样串行且原子。
///
/// TS: packages/env/src/ssh.ts#forgetHostKey
pub async fn forget_host_key(target: &SshTarget) -> Result<(), SshFailure> {
    let alias = target.host_key_alias.clone();
    change_known_hosts(&target.known_hosts_file, move |existing| {
        Ok(existing
            .into_iter()
            .filter(|line| {
                // JS: `line.trim().split(/\s+/)[0] !== target.hostKeyAlias`。
                js_trim(line)
                    .split(is_js_space)
                    .next()
                    .filter(|field| !field.is_empty())
                    != Some(alias.as_str())
            })
            .collect())
    })
    .await
}

// ---------------------------------------------------------------------------
// 平台探测
// ---------------------------------------------------------------------------

/// POSIX 探测，由远端登录 shell 执行；Windows 主机改用 PowerShell 回答。
///
/// 先输出标记行 `KIANA-ENV-PROBE`，之后固定 6 个字段：`uname -s`、`uname -m`、`uname -o`
/// （不支持时 `-`）、`$HOME`、`$TMPDIR`、`$LD_PRELOAD`（后两个未设置时都是 `-`）。整个脚本包在
/// `sh -c '...'` 里，
/// 因此远端 shell 是 cmd.exe/PowerShell 时通常会失败，由 [`detect_platform`] 改走 Windows 探测。
const POSIX_PROBE: &str = r#"sh -c 'echo KIANA-ENV-PROBE; uname -s; uname -m; uname -o 2>/dev/null || echo -; printf "%s\n" "$HOME" "${TMPDIR:--}" "${LD_PRELOAD:--}"'"#;

/// 按 UTF-16 码元小端序列化（PowerShell `-EncodedCommand` 要求的编码）。
fn utf16le_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// 包成 `powershell -NoProfile -NonInteractive -EncodedCommand <base64(utf16le)>`。
///
/// 脚本走 base64 而不是直接写进命令行：引号、空格与 `$` 都不用再经过远端 shell 的解析，
/// `-NonInteractive` 保证不会停在提示符上。
fn powershell(script: &str) -> String {
    format!(
        "powershell -NoProfile -NonInteractive -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(utf16le_bytes(script))
    )
}

/// Windows 版探测：标记行之后与 `POSIX_PROBE` 同样的 6 个字段，system 固定为 `Windows`，
/// `uname -o`、TMPDIR、LD_PRELOAD 三个位置都是 `-`。
fn windows_probe() -> String {
    powershell(
        "'KIANA-ENV-PROBE'; 'Windows'; [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString(); '-'; $HOME; '-'; '-'",
    )
}

/// 把 `uname -m` / PowerShell 给出的架构名归一化：`x86_64`、`amd64`、`x64` → [`RemoteArch::X64`]，
/// `aarch64`、`arm64` → [`RemoteArch::Arm64`]（大小写不敏感）。
///
/// 其它值返回 `Unsupported remote architecture: <原样>`，消息里保留调用方传入的拼写。
fn normalize_arch(machine: &str) -> Result<RemoteArch, SshFailure> {
    let lower = machine.to_lowercase();
    if matches!(lower.as_str(), "x86_64" | "amd64" | "x64") {
        return Ok(RemoteArch::X64);
    }
    if matches!(lower.as_str(), "aarch64" | "arm64") {
        return Ok(RemoteArch::Arm64);
    }
    Err(SshFailure::Other(format!(
        "Unsupported remote architecture: {machine}"
    )))
}

/// 探测输出的 6 个字段（布局见 `POSIX_PROBE` 与 `windows_probe`）；缺失的行由
/// [`parse_probe`] 补默认值。
#[derive(Debug)]
struct Probe {
    system: String,
    machine: String,
    os: String,
    home: String,
    tmpdir: String,
    preload: String,
}

/// 在输出里找标记行 `KIANA-ENV-PROBE`（登录 shell 可能先打印 banner），取它之后的 6 行并去掉
/// 行尾 `\r`。
///
/// 字段不足时前四个（system/machine/os/home）补空串，`tmpdir`/`preload` 补 `-`；找不到标记则
/// 用整段输出组成 `Unexpected answer from <host>: ...`。
fn parse_probe(target: &SshTarget, output: &str) -> Result<Probe, SshFailure> {
    // Login shells may print a banner first.
    let lines: Vec<&str> = output
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let Some(start) = lines.iter().position(|line| *line == "KIANA-ENV-PROBE") else {
        return Err(SshFailure::Other(format!(
            "Unexpected answer from {}: {}",
            target.host,
            js_trim(output)
        )));
    };
    let field = |index: usize, default: &str| {
        lines
            .get(start + 1 + index)
            .map(|line| (*line).to_owned())
            .unwrap_or_else(|| default.to_owned())
    };
    Ok(Probe {
        system: field(0, ""),
        machine: field(1, ""),
        os: field(2, ""),
        home: field(3, ""),
        tmpdir: field(4, "-"),
        preload: field(5, "-"),
    })
}

/// Termux 只有用自己的 `TMPDIR` 与 `termux-exec`（让 `#!/usr/bin/env` shebang 工作）才正常。
///
/// 顺序固定：TMPDIR 缺失 → termux-exec 未加载 → Termux 可能被系统挂起；第三条无条件加入。
/// 只有 [`detect_platform`] 判定为 Android 时调用，结果写进 [`RemotePlatform::warnings`]。
fn termux_warnings(probe: &Probe) -> Vec<String> {
    let mut warnings = Vec::new();
    if probe.tmpdir == "-" {
        warnings
            .push("TMPDIR is not set; Termux's sshd normally sets it to $PREFIX/tmp.".to_owned());
    }
    if !probe.preload.contains("termux-exec") {
        warnings.push(
            "termux-exec is not loaded (LD_PRELOAD); scripts with #!/usr/bin/env shebangs will fail."
                .to_owned(),
        );
    }
    warnings.push(
        "Android may suspend Termux; run termux-wake-lock on the device to keep the connection alive."
            .to_owned(),
    );
    warnings
}

/// 目标运行什么系统：经登录 shell 的 `uname`，或 Windows 上的 PowerShell。
///
/// 流程：
/// 1. 跑 POSIX 探测并解析。`HostKeyUnknownError`/`HostKeyChangedError` 直接上抛；其余失败
///    （包括「命令跑通但输出无法解析」）改跑 Windows 探测——对应 TS 把 `runSsh` 与
///    `parseProbe` 放在同一个 try 里。
/// 2. 若 `uname -s` 以 `MINGW`/`MSYS`/`CYGWIN` 开头（Git Bash 当默认 shell），再跑一次 Windows
///    探测，改用 Windows 自己的架构与 home 拼写。
/// 3. 归一化架构；Windows 主机额外跑 `echo %OS%` 判断远端 shell：输出含 `Windows_NT` 说明是
///    cmd.exe（`%OS%` 被展开），否则是 PowerShell（原样打印）。
/// 4. `uname -s` 为 Linux 且 `uname -o` 为 `Android` 时判为 Android，并收集 Termux 警告。
///
/// 失败：未知架构 `Unsupported remote architecture: ...`、未知系统
/// `Unsupported remote system: ...`，以及探测命令本身的 [`SshFailure`]。
///
/// TS: packages/env/src/ssh.ts#detectPlatform
pub async fn detect_platform(target: &SshTarget) -> Result<RemotePlatform, SshFailure> {
    let posix = match run_ssh(target, POSIX_PROBE, RunSshOptions::default()).await {
        Ok(output) => parse_probe(target, &output),
        Err(error) => Err(error),
    };
    let mut probe = match posix {
        Ok(probe) => probe,
        Err(error) => {
            // cmd.exe or PowerShell as the remote shell: no `sh`, or one that gets the probe's
            // quotes wrong. TS 把 `parseProbe` 放进同一个 try：POSIX 探测**跑通但输出无法解析**
            // （登录 shell 弄坏了引号等）同样重试 Windows 探测。
            if matches!(
                error,
                SshFailure::HostKeyUnknown(_) | SshFailure::HostKeyChanged(_)
            ) {
                return Err(error);
            }
            let output = run_ssh(target, &windows_probe(), RunSshOptions::default()).await?;
            parse_probe(target, &output)?
        }
    };
    // Git Bash as Windows' default SSH shell: ask PowerShell for Windows' own architecture and home
    // spelling.
    if probe.system.starts_with("MINGW")
        || probe.system.starts_with("MSYS")
        || probe.system.starts_with("CYGWIN")
    {
        let output = run_ssh(target, &windows_probe(), RunSshOptions::default()).await?;
        probe = parse_probe(target, &output)?;
    }
    let arch = normalize_arch(&probe.machine)?;
    if probe.system == "Windows" {
        // cmd.exe expands %OS%; PowerShell prints it as is.
        let shell = if run_ssh(target, "echo %OS%", RunSshOptions::default())
            .await?
            .contains("Windows_NT")
        {
            RemoteShell::Cmd
        } else {
            RemoteShell::Powershell
        };
        return Ok(RemotePlatform {
            platform: RemotePlatformName::Windows,
            arch,
            home: probe.home,
            shell: Some(shell),
            warnings: Vec::new(),
        });
    }
    if probe.system == "Darwin" {
        return Ok(RemotePlatform {
            platform: RemotePlatformName::Darwin,
            arch,
            home: probe.home,
            shell: None,
            warnings: Vec::new(),
        });
    }
    if probe.system == "Linux" {
        let android = probe.os == "Android";
        let warnings = if android {
            termux_warnings(&probe)
        } else {
            Vec::new()
        };
        return Ok(RemotePlatform {
            platform: if android {
                RemotePlatformName::Android
            } else {
                RemotePlatformName::Linux
            },
            arch,
            home: probe.home,
            shell: None,
            warnings,
        });
    }
    Err(SshFailure::Other(format!(
        "Unsupported remote system: {}",
        probe.system
    )))
}

// ---------------------------------------------------------------------------
// daemon 路径与部署
// ---------------------------------------------------------------------------

/// 本包为远端系统提供的 daemon 二进制。
///
/// 布局沿用 TS 的 npm 包结构：`<root>/bin/kiana-env-<platform>-<arch>/kiana-env[.exe]`。Rust 里
/// 没有 npm 包目录，`<root>` 取当前可执行文件所在目录（见 `package_root`），所以这个路径在开发
/// 环境通常不存在；测试与嵌入方应通过 `binary` 参数显式给出。
///
/// TS: packages/env/src/ssh.ts#packagedDaemon
pub fn packaged_daemon(remote: &RemotePlatform) -> String {
    let name = format!(
        "kiana-env-{}-{}",
        remote.platform.as_str(),
        remote.arch.as_str()
    );
    let root = package_root();
    let binary = if remote.platform == RemotePlatformName::Windows {
        "kiana-env.exe"
    } else {
        "kiana-env"
    };
    root.join("bin")
        .join(name)
        .join(binary)
        .to_string_lossy()
        .into_owned()
}

/// npm 包根目录在 Rust 里不存在：用当前可执行文件的目录（生产路径；测试总是显式给出 `binary`）。
///
/// `current_exe()` 失败或没有父目录时退化为 `.`（相对进程当前工作目录）。
fn package_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 这台机器本身对应的 daemon 二进制（例如本地会话的终端）。
///
/// 把 `std::env::consts::OS`/`ARCH` 翻译成 Node 的 `process.platform`/`process.arch` 拼写后复用
/// [`packaged_daemon`]；不支持的平台/架构分别返回 `No kiana-env daemon for ...` 与
/// `Unsupported remote architecture: ...`。只计算路径，不检查文件是否存在。
///
/// TS: packages/env/src/ssh.ts#localDaemon
pub fn local_daemon() -> Result<String, SshFailure> {
    let os = std::env::consts::OS;
    let platform = match os {
        "windows" => RemotePlatformName::Windows,
        "macos" => RemotePlatformName::Darwin,
        "linux" => RemotePlatformName::Linux,
        "android" => RemotePlatformName::Android,
        other => {
            return Err(SshFailure::Other(format!(
                "No kiana-env daemon for {}",
                node_platform_name(other)
            )));
        }
    };
    let arch = normalize_arch(node_arch_name(std::env::consts::ARCH))?;
    Ok(packaged_daemon(&RemotePlatform {
        platform,
        arch,
        home: String::new(),
        shell: None,
        warnings: Vec::new(),
    }))
}

/// Rust 的 OS 名 → `process.platform` 的拼写（错误消息里用）。
///
/// Node 的 `process.platform` 是 `sunos`（Solaris/illumos）、`darwin`、`win32`，不是 Rust 的
/// `solaris`/`illumos`/`macos`/`windows`。
fn node_platform_name(os: &str) -> &str {
    match os {
        "macos" => "darwin",
        "windows" => "win32",
        "solaris" | "illumos" => "sunos",
        other => other,
    }
}

/// Rust 的 `std::env::consts::ARCH` → `process.arch` 的拼写。
///
/// TS 的 `localDaemon` 用 `normalizeArch(process.arch)`，未知架构的错误消息带 Node 的名字
/// （`ia32`、`ppc`、`ppc64`、`loong64`），不是 Rust 的 `x86`/`powerpc*`/`loongarch64`。
fn node_arch_name(arch: &str) -> &str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "powerpc" => "ppc",
        "powerpc64" | "powerpc64le" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// POSIX 单引号引用：内部的 `'` 写成 `'\''`（关闭引号、转义引号、重新打开），其余字符
/// （包括 `$`、反引号与换行）在单引号内都是字面量。本文件拼接的 POSIX 脚本与路径都经它包裹
/// （固定的 `POSIX_PROBE` 字面量除外）。
fn quote_posix(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// PowerShell 单引号引用：内部的 `'` 写成 `''`。PowerShell 的单引号字符串不做变量展开，
/// 因此路径里的 `$` 不会被解释。
fn quote_powershell(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// 同样内容的 daemon 在远端机器上的位置：以 SHA-256 命名，版本不会互相覆盖。
///
/// 只取哈希的前 32 个十六进制字符（与 TS 相同）；`.min(len)` 防止调用方给出更短的字符串时切片
/// panic。Windows 用反斜杠与 `.exe` 后缀。
fn daemon_path(remote: &RemotePlatform, sha256: &str) -> String {
    let name = format!("kiana-env-{}", &sha256[..32.min(sha256.len())]);
    if remote.platform == RemotePlatformName::Windows {
        format!("{}\\.kiana\\mobile\\tools\\{name}.exe", remote.home)
    } else {
        format!("{}/.kiana/mobile/tools/{name}", remote.home)
    }
}

/// 远端 POSIX 哈希函数：依次尝试 `sha256sum`、`shasum -a 256`、`openssl dgst -sha256`，
/// 都没有时输出 `none`。校验与上传两条远端脚本都把它拼进去，保证「先校验、再落位」。
const POSIX_HASH: &str = r#"hash() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1; elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | sed 's/.*= //'; else echo none; fi; }"#;

/// 远端文件是否是这份内容：`present`、`missing`，或（POSIX 上没有哈希工具）`nohash`。
///
/// Windows 用 PowerShell 的 `Test-Path` + `Get-FileHash`，输出只做 `trim().endsWith("present")`
/// 判断（照搬 TS）。POSIX 用一个远端 `sh -c` 跑 `POSIX_HASH`：先看文件存在且哈希相等，再检查
/// `hash /dev/null` 是否为 `none`；输出取 `trim()` 后的最后一行（空输出得到空串，调用方会按
/// 「不是 present、也不是 nohash」继续部署）。
async fn check_daemon(
    target: &SshTarget,
    remote: &RemotePlatform,
    file: &str,
    sha256: &str,
) -> Result<String, SshFailure> {
    if remote.platform == RemotePlatformName::Windows {
        let check = powershell(&format!(
            "$f = {}; if ((Test-Path -LiteralPath $f) -and ((Get-FileHash -Algorithm SHA256 -LiteralPath $f).Hash.ToLower() -eq '{}')) {{ 'present' }} else {{ 'missing' }}",
            quote_powershell(file),
            sha256,
        ));
        // TS: `(await runSsh(…)).trim().endsWith("present")`（JS 的 `trim`）。
        let output = run_ssh(target, &check, RunSshOptions::default()).await?;
        return Ok(if js_trim(&output).ends_with("present") {
            "present".to_owned()
        } else {
            "missing".to_owned()
        });
    }
    let check = format!(
        "{POSIX_HASH}; f={}; if [ -f \"$f\" ] && [ \"$(hash \"$f\")\" = {sha256} ]; then echo present; elif [ \"$(hash /dev/null)\" = none ]; then echo nohash; else echo missing; fi",
        quote_posix(file)
    );
    let output = run_ssh(
        target,
        &format!("sh -c {}", quote_posix(&check)),
        RunSshOptions::default(),
    )
    .await?;
    // TS: `(await runSsh(…)).trim().split("\n").at(-1) ?? "missing"`。
    Ok(js_trim(&output)
        .split('\n')
        .next_back()
        .unwrap_or("missing")
        .to_owned())
}

/// 确保 daemon 在远端机器上、在运行之前用 SHA-256 校验过，并删除其它内容的 daemon。上传写到新的
/// 临时文件，只有哈希匹配才改名到位。返回二进制的远端路径。
///
/// 前置条件：`binary`（缺省为 [`packaged_daemon`]）在本地存在且可读，否则返回
/// `No kiana-env daemon for <platform>-<arch> at <path>` 或 Node 风格的 fs 错误。
///
/// 流程：
/// 1. 本地读整个二进制并算 SHA-256；远端路径由哈希前缀决定（`daemon_path`）。
/// 2. `check_daemon`：`present` 直接返回；`nohash` 拒绝部署（不能验证就不落位）；`missing`
///    继续。
/// 3. Windows：二进制先 base64、按 76 列折行、以 `KIANA-ENV-END` 结束，PowerShell 侧逐行收集
///    到标记、解码、写临时文件、校验哈希、必要时重试 `Move-Item`，最后删除其它
///    `kiana-env-*.exe`。POSIX：远端 `set -e` 脚本用 `cat > $t` 收原始字节，`hash` 校验后
///    `chmod 700`、`mv -f`，再删除其它 `kiana-env-*`。
/// 4. 上传用 `UPLOAD_TIMEOUT_MS`；返回远端路径（不保证远端能执行它）。
///
/// 注意：整个二进制会被读进内存并一次性通过 stdin 发送；部署成功后目标目录里只保留本内容
/// 命名的 daemon。
///
/// TS: packages/env/src/ssh.ts#deployDaemon
pub async fn deploy_daemon(
    target: &SshTarget,
    remote: &RemotePlatform,
    binary: Option<&str>,
) -> Result<String, SshFailure> {
    let binary = match binary {
        Some(binary) => binary.to_owned(),
        None => packaged_daemon(remote),
    };
    if !Path::new(&binary).exists() {
        return Err(SshFailure::Other(format!(
            "No kiana-env daemon for {}-{} at {binary}",
            remote.platform.as_str(),
            remote.arch.as_str()
        )));
    }
    let bytes = tokio::fs::read(&binary)
        .await
        .map_err(|error| node_read_file_error(&error, &binary))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hex::encode(hasher.finalize());
    let file = daemon_path(remote, &sha256);
    let state = check_daemon(target, remote, &file, &sha256).await?;
    if state == "present" {
        return Ok(file);
    }
    if state == "nohash" {
        return Err(SshFailure::Other(format!(
            "{} has no sha256sum, shasum or openssl to verify kiana-env",
            target.host
        )));
    }
    if remote.platform == RemotePlatformName::Windows {
        // Windows 侧必须把二进制 base64：PowerShell 从重定向 stdin 读的是文本行（`$input`），
        // 无法安全承载任意字节。
        let upload = powershell(&[
            "$ErrorActionPreference = 'Stop'".to_owned(),
            format!("$f = {}", quote_powershell(&file)),
            "$d = Split-Path -Parent $f".to_owned(),
            "New-Item -ItemType Directory -Force -Path $d | Out-Null".to_owned(),
            "$t = Join-Path $d ('.kiana-env-' + [guid]::NewGuid().ToString() + '.tmp')".to_owned(),
            "$text = New-Object System.Text.StringBuilder".to_owned(),
            "foreach ($line in $input) { if ($line -eq 'KIANA-ENV-END') { break }; [void]$text.Append($line) }"
                .to_owned(),
            "$bytes = [Convert]::FromBase64String($text.ToString())".to_owned(),
            "$out = [IO.File]::Open($t, 'CreateNew', 'Write', 'None'); $out.Write($bytes, 0, $bytes.Length); $out.Close()"
                .to_owned(),
            format!(
                "if ((Get-FileHash -Algorithm SHA256 -LiteralPath $t).Hash.ToLower() -ne '{sha256}') {{ Remove-Item -LiteralPath $t; throw 'kiana-env upload is corrupt' }}"
            ),
            "for ($i = 0; ; $i++) { try { Move-Item -Force -LiteralPath $t -Destination $f; break } catch { if ($i -ge 20) { throw }; Start-Sleep -Milliseconds 250 } }"
                .to_owned(),
            "Get-ChildItem -LiteralPath $d -Filter 'kiana-env-*.exe' | Where-Object { $_.FullName -ne $f } | ForEach-Object { Remove-Item -LiteralPath $_.FullName -ErrorAction SilentlyContinue }"
                .to_owned(),
            "'deployed'".to_owned(),
        ]
        .join("; "));
        // 每行 76 个字符（TS 的 `.replace(/.{1,76}/g, "$&\n")`），最后追加结束标记；
        // 远端脚本读到 `KIANA-ENV-END` 就停止，不依赖 stdin 的 EOF。
        let mut stdin = String::new();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let encoded_bytes = encoded.as_bytes();
        let mut offset = 0usize;
        while offset < encoded_bytes.len() {
            let end = (offset + 76).min(encoded_bytes.len());
            stdin.push_str(&encoded[offset..end]);
            stdin.push('\n');
            offset = end;
        }
        stdin.push_str("KIANA-ENV-END\n");
        run_ssh(
            target,
            &upload,
            RunSshOptions {
                stdin: Some(stdin.as_bytes()),
                timeout_ms: Some(UPLOAD_TIMEOUT_MS),
                ..RunSshOptions::default()
            },
        )
        .await?;
        return Ok(file);
    }
    // POSIX 远端脚本：`set -e` 让任一步失败立即退出；临时文件由 `mktemp` 建在同目录，
    // 哈希不符就删除并退出，成功后才 `chmod 700` 并 `mv -f` 落位。
    let upload = [
        "set -e".to_owned(),
        POSIX_HASH.to_owned(),
        format!("f={}", quote_posix(&file)),
        "d=$(dirname \"$f\")".to_owned(),
        "mkdir -p \"$d\"".to_owned(),
        "chmod 700 \"$d\"".to_owned(),
        "t=$(mktemp \"$d/.kiana-env.XXXXXX\")".to_owned(),
        "cat > \"$t\"".to_owned(),
        format!(
            "if [ \"$(hash \"$t\")\" != {sha256} ]; then rm -f \"$t\"; echo \"kiana-env upload is corrupt\" >&2; exit 1; fi"
        ),
        "chmod 700 \"$t\"".to_owned(),
        "mv -f \"$t\" \"$f\"".to_owned(),
        // Older daemons; running ones keep their file open until they exit.
        "for old in \"$d\"/kiana-env-*; do [ \"$old\" = \"$f\" ] || rm -f \"$old\"; done".to_owned(),
        "echo deployed".to_owned(),
    ]
    .join("\n");
    run_ssh(
        target,
        &format!("sh -c {}", quote_posix(&upload)),
        RunSshOptions {
            stdin: Some(&bytes),
            timeout_ms: Some(UPLOAD_TIMEOUT_MS),
            ..RunSshOptions::default()
        },
    )
    .await?;
    Ok(file)
}

// ---------------------------------------------------------------------------
// 连接
// ---------------------------------------------------------------------------

/// `ssh_connection` 与 `connect_ssh` 的选项：目标，加上要部署的二进制（默认本包提供的）。
///
/// `binary` 为 `None` 时按探测到的远端平台用 [`packaged_daemon`] 计算；[`ssh_connection`] 是懒
/// 启动，[`connect_ssh`] 则在返回前完成探测与部署。
///
/// TS: packages/env/src/ssh.ts#SshConnectOptions
#[derive(Clone)]
pub struct SshConnectOptions {
    /// 目标。
    pub target: SshTarget,
    /// 要部署的 daemon 二进制路径。
    ///
    /// `None` 表示用 [`packaged_daemon`]。
    pub binary: Option<String>,
    /// POSIX：经用户的登录 shell（`$SHELL -l`）启动 daemon。默认关闭。
    ///
    /// 只有 `Some(true)` 会启用；`None` 与 `Some(false)` 等效（见 `launch_command`）。
    pub login_shell: Option<bool>,
    /// 诊断输出。
    ///
    /// 远端平台的警告逐条以 `warning\n` 写入；同一个回调也交给构造出的 `Connection`，用于
    /// daemon 的 stderr 与握手前的 stdout 噪声。
    pub on_log: Option<LogCallback>,
}

impl SshConnectOptions {
    /// 只给出目标的选项。
    ///
    /// `binary`、`login_shell`、`on_log` 都是 `None`。
    pub fn new(target: SshTarget) -> Self {
        SshConnectOptions {
            target,
            binary: None,
            login_shell: None,
            on_log: None,
        }
    }
}

/// 在 `file` 处启动 daemon 的远端命令（在 `serve` 参数之前）。
///
/// Windows：PowerShell 必须用调用运算符 `&` 才能执行带引号的路径；cmd.exe 只在路径含空格时
/// 用一对双引号包住。POSIX：默认用 [`quote_posix`] 的单引号；`login_shell` 为真时改成
/// `exec "$SHELL" -lc 'exec "$0" "$@"' '<file>'`，让 daemon 成为登录 shell 的 `$0`，后续
/// `serve` 参数原样进入 `"$@"`。
fn launch_command(remote: &RemotePlatform, file: &str, login_shell: bool) -> String {
    if remote.platform == RemotePlatformName::Windows {
        // PowerShell runs a quoted path only with the call operator; cmd.exe keeps one pair of
        // quotes around a program.
        if remote.shell == Some(RemoteShell::Powershell) {
            return format!("& {}", quote_powershell(file));
        }
        return if file.contains(' ') {
            format!("\"{file}\"")
        } else {
            file.to_owned()
        };
    }
    // `$0` is the daemon, `"$@"` its arguments; `$SHELL` is the login shell's own name for itself.
    if login_shell {
        return format!(
            "exec \"$SHELL\" -lc 'exec \"$0\" \"$@\"' {}",
            quote_posix(file)
        );
    }
    quote_posix(file)
}

/// SSH 连接了解到的远端系统，以及它最后一次校验过的 daemon。
///
/// `verified` 是 [`connect_ssh`] 预部署的结果：只对紧接的第一次启动有效，`connection_for` 用
/// `take()` 取走；之后的每次启动都会重新校验/部署。
struct SshState {
    remote: Option<RemotePlatform>,
    verified: Option<String>,
}

/// 一个 SSH 上的 kiana-env 连接：每次启动都探测远端系统（仅第一次）、校验或部署 daemon，然后启动它。
///
/// `connection` 可直接交给上层执行环境；`state` 跨多次启动保存已探测的平台与一次性的
/// `verified` 路径。
///
/// TS: packages/env/src/ssh.ts#sshConnection
pub struct SshConnection {
    /// 连接。
    pub connection: Arc<Connection>,
    state: Arc<Mutex<SshState>>,
}

impl SshConnection {
    /// 已探测到的远端系统（在第一次启动之前是 `None`）。
    ///
    /// 探测失败不会写入状态，所以失败后仍返回 `None`。
    pub fn remote(&self) -> Option<RemotePlatform> {
        lock(&self.state).remote.clone()
    }
}

/// 把「探测 → 校验/部署 → 启动」串成 [`ConnectionCommand::Dynamic`]。
///
/// 闭包在 `Connection` 每次尝试启动时执行：
/// 1. `state.remote` 为空时探测一次并写回（只在成功时写）；只有这次探测会把
///    `remote.warnings` 逐条写进 `on_log`（之后 `state.remote` 已有值，不再重复告警）。
/// 2. 用 `take()` 取走 `state.verified`：`connect_ssh` 预部署的路径只对紧接的第一次启动有效；
///    否则重新 [`deploy_daemon`]（`binary` 缺省为 [`packaged_daemon`]）。因此断线重连会重新
///    校验一次 daemon（见 `tests/ssh.rs` 的 “verifies the daemon again…” 用例）。
/// 3. 组装 `[ssh 程序, ssh 参数..., 启动命令]` 返回给 `Connection`；由它 spawn 子进程并追加
///    `serve --token ...`。
///
/// 返回的 future 用 `kiana_durable::env::BoxError`（`Box<dyn Error + Send + Sync>`）承载
/// [`SshFailure`]；`Connection` 把启动失败统一包成 code `spawn_error` 的 `RemoteError`。
fn connection_for(options: SshConnectOptions, state: Arc<Mutex<SshState>>) -> Arc<Connection> {
    let command_options = options.clone();
    let command_state = Arc::clone(&state);
    let command = ConnectionCommand::Dynamic(Arc::new(move || {
        let options = command_options.clone();
        let state = Arc::clone(&command_state);
        Box::pin(async move {
            let remote = {
                let known = lock(&state).remote.clone();
                match known {
                    Some(remote) => remote,
                    None => {
                        let remote = detect_platform(&options.target).await?;
                        if let Some(on_log) = &options.on_log {
                            for warning in &remote.warnings {
                                on_log(&format!("{warning}\n"));
                            }
                        }
                        lock(&state).remote = Some(remote.clone());
                        remote
                    }
                }
            };
            // A deployment that just verified the binary counts for the first start.
            let verified = lock(&state).verified.take();
            let file = match verified {
                Some(file) => file,
                None => {
                    let binary = options
                        .binary
                        .clone()
                        .unwrap_or_else(|| packaged_daemon(&remote));
                    deploy_daemon(&options.target, &remote, Some(&binary)).await?
                }
            };
            let mut command = vec![
                options
                    .target
                    .ssh
                    .clone()
                    .unwrap_or_else(|| "ssh".to_owned()),
            ];
            command.extend(ssh_arguments(&options.target)?);
            command.push(launch_command(
                &remote,
                &file,
                options.login_shell == Some(true),
            ));
            Ok::<Vec<String>, kiana_durable::env::BoxError>(command)
        })
    }));
    let mut connection_options = ConnectionOptions::new(Vec::new());
    connection_options.command = command;
    connection_options.on_log = options.on_log.clone();
    Arc::new(Connection::new(connection_options))
}

/// 到目标、但在第一个请求之前什么都不做的 `Connection`：每次启动探测远端系统（第一次时）、校验
/// 并在需要时部署 daemon，然后经 `ssh` 启动它。失败（没有网络、未信任或变更的 host key）让等待这次
/// 启动的请求以 code `spawn_error` 失败，消息是 `ssh` 的诊断输出；下一次请求重试。
/// `remote()` 返回探测到的系统（一旦知道）。
///
/// 消息的具体形式：普通 ssh 失败是 `ssh <host> failed with exit code <n>: <stderr>`；未信任/
/// 变更的 host key 是固定文案（见 `run_ssh` 的识别分支），不是原始 stderr。
///
/// TS: packages/env/src/ssh.ts#sshConnection
pub fn ssh_connection(options: SshConnectOptions) -> SshConnection {
    let state = Arc::new(Mutex::new(SshState {
        remote: None,
        verified: None,
    }));
    let connection = connection_for(options, Arc::clone(&state));
    SshConnection { connection, state }
}

/// 已连接并探测过的 SSH 目标。
///
/// TS: packages/env/src/ssh.ts#connectSsh
pub struct ConnectedSsh {
    /// 连接。
    ///
    /// 第一次启动直接复用 [`connect_ssh`] 已经校验过的 daemon 路径。
    pub connection: Arc<Connection>,
    /// 探测到的远端系统。
    pub remote: RemotePlatform,
}

/// 现在就探测远端系统并部署 daemon，然后返回经 `ssh` 启动它的 `Connection`。
///
/// 顺序：`detect_platform` → `deploy_daemon`（`binary` 缺省为 [`packaged_daemon`]）→ 逐条写
/// warnings 到 `on_log` → 构造 `Connection`，并把已校验路径放进 state，让第一次启动不再重复
/// 校验。host key 未信任时返回 [`HostKeyUnknownError`]（需要先 [`scan_host_key`] 再
/// [`accept_host_key`]）。
///
/// TS: packages/env/src/ssh.ts#connectSsh
pub async fn connect_ssh(options: SshConnectOptions) -> Result<ConnectedSsh, SshFailure> {
    let remote = detect_platform(&options.target).await?;
    let binary = options
        .binary
        .clone()
        .unwrap_or_else(|| packaged_daemon(&remote));
    let verified = deploy_daemon(&options.target, &remote, Some(&binary)).await?;
    if let Some(on_log) = &options.on_log {
        for warning in &remote.warnings {
            on_log(&format!("{warning}\n"));
        }
    }
    let state = Arc::new(Mutex::new(SshState {
        remote: Some(remote.clone()),
        verified: Some(verified),
    }));
    Ok(ConnectedSsh {
        connection: connection_for(options, state),
        remote,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> SshTarget {
        SshTarget {
            host: "gpu-box".to_owned(),
            user: Some("me".to_owned()),
            port: Some(2222),
            identity_file: Some("/keys/id".to_owned()),
            known_hosts_file: "/home/me/.kiana/known_hosts".to_owned(),
            host_key_alias: "kiana-env-gpu".to_owned(),
            ssh: None,
            config_file: Some("/home/me/.ssh/config".to_owned()),
        }
    }

    /// 测试脚本刚写完时，并发测试 fork 出的子进程可能短暂继承写 fd，`exec` 报 ETXTBSY
    /// （Node 与 Rust 都把它当同步 spawn 失败）。这与被测行为无关，测试里重试。
    #[cfg(unix)]
    fn is_text_file_busy(error: &SshFailure) -> bool {
        error.message().ends_with("ETXTBSY")
    }

    /// 回归（review finding）：缺失的 ssh 程序必须报 Node 的 `spawn <program> ENOENT`
    /// （`child.on("error", error => new SshError(error.message, null, ""))`）。
    #[tokio::test]
    async fn a_missing_ssh_reports_the_node_spawn_message() {
        let mut target = target();
        target.ssh = Some("/definitely-not-a-real-ssh-command".to_owned());
        match run_ssh(&target, "exit 0", RunSshOptions::default()).await {
            Err(SshFailure::Ssh(error)) => {
                assert_eq!(
                    error.message,
                    "spawn /definitely-not-a-real-ssh-command ENOENT"
                );
                assert_eq!(error.exit_code, None);
                assert_eq!(error.stderr, "");
            }
            other => panic!("expected SshError, got {other:?}"),
        }
    }

    /// 回归（review finding）：动态命令（探测/部署）里的 ssh 启动失败经 `Connection` 的
    /// `spawn_error` 报出 Node 的消息（TS: `#start` 的 catch 用 `error.message`）。
    #[tokio::test]
    async fn a_missing_ssh_program_fails_the_connection_with_the_node_message() {
        let mut target = target();
        target.ssh = Some("/definitely-not-a-real-ssh-command".to_owned());
        let connection = ssh_connection(SshConnectOptions::new(target));
        let error = connection
            .connection
            .info()
            .await
            .expect_err("ssh is missing");
        assert_eq!(error.code, "spawn_error");
        assert_eq!(
            error.message(),
            "spawn /definitely-not-a-real-ssh-command ENOENT"
        );
        assert!(crate::connection::is_connection_lost(&error));
    }

    /// 回归（review finding）：超时走 `child.kill()`，即 POSIX 的 SIGTERM（不是 SIGKILL），
    /// ssh 仍能收到信号做清理；拒绝消息是当前稳定契约，改动需同步测试。
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_ssh_is_terminated_with_sigterm() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("signal");
        let program = directory.path().join("ssh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ntrap 'echo TERM > {}; exit 1' TERM\nwhile :; do sleep 0.02; done\n",
                marker.display()
            ),
        )
        .expect("write the fake ssh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make the fake ssh executable");
        let mut target = target();
        target.ssh = Some(program.to_string_lossy().into_owned());
        // 刚写好的脚本可能被并发测试的 fork 短暂继承了写 fd（exec 报 ETXTBSY）；与本用例要测的
        // 超时/信号行为无关，重试即可。
        let mut tries = 0;
        let error = loop {
            let result = run_ssh(
                &target,
                "exit 0",
                RunSshOptions {
                    timeout_ms: Some(200),
                    ..RunSshOptions::default()
                },
            )
            .await;
            match result {
                Err(error) if is_text_file_busy(&error) && tries < 20 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                other => break other.expect_err("the fake ssh never exits"),
            }
        };
        match error {
            SshFailure::Ssh(error) => {
                assert_eq!(error.exit_code, None);
                assert_eq!(error.stderr, "");
                assert!(
                    error
                        .message
                        .starts_with("ssh gpu-box did not finish within 0.2 s: "),
                    "unexpected message: {}",
                    error.message
                );
            }
            other => panic!("expected SshError, got {other:?}"),
        }
        for _ in 0..100 {
            if marker.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            marker.exists(),
            "the child must receive SIGTERM (child.kill()), not SIGKILL"
        );
    }

    #[test]
    fn ssh_arguments_have_the_documented_order() {
        let arguments = ssh_arguments(&target()).expect("arguments");
        let expected: Vec<String> = [
            "-F",
            "/home/me/.ssh/config",
            "-T",
            "-a",
            "-x",
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "RemoteCommand=none",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "SendEnv=-*",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "UserKnownHostsFile=\"/home/me/.kiana/known_hosts\"",
            "-o",
            "GlobalKnownHostsFile=none",
            "-o",
            "HashKnownHosts=no",
            "-o",
            "HostKeyAlias=kiana-env-gpu",
            "-l",
            "me",
            "-p",
            "2222",
            "-o",
            "IdentityFile=\"/keys/id\"",
            "-o",
            "IdentitiesOnly=yes",
            "--",
            "gpu-box",
        ]
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
        assert_eq!(arguments, expected);
    }

    #[test]
    fn ssh_arguments_default_to_accept_new_and_double_percent() {
        let mut target = target();
        target.user = None;
        target.port = None;
        target.identity_file = None;
        target.config_file = None;
        target.known_hosts_file = "/tmp/known%hosts".to_owned();
        let arguments = ssh_arguments_with(&target, false, &target.known_hosts_file).expect("args");
        assert!(arguments.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(arguments.contains(&"UserKnownHostsFile=\"/tmp/known%%hosts\"".to_owned()));
        assert!(!arguments.contains(&"-l".to_owned()));
        assert_eq!(arguments.last().map(String::as_str), Some("gpu-box"));
    }

    #[test]
    fn invalid_fields_use_the_typescript_messages() {
        let mut invalid = target();
        invalid.host = "-oProxyCommand=bad".to_owned();
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid host: \"-oProxyCommand=bad\""
        );
        let mut invalid = target();
        invalid.host_key_alias = "a b".to_owned();
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid host key alias: \"a b\""
        );
        let mut invalid = target();
        invalid.user = Some(String::new());
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid user: \"\""
        );
        let mut invalid = target();
        invalid.port = Some(0);
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid port: 0"
        );
        let mut invalid = target();
        invalid.port = Some(65536);
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid port: 65536"
        );
        let mut invalid = target();
        invalid.known_hosts_file = "/tmp/a\"b".to_owned();
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid known hosts file: \"/tmp/a\\\"b\""
        );
        let mut invalid = target();
        invalid.identity_file = Some("/tmp/a\u{0007}".to_owned());
        assert_eq!(
            ssh_arguments(&invalid).unwrap_err().to_string(),
            "Invalid identity file: \"/tmp/a\\u0007\""
        );
    }

    #[test]
    fn control_characters_are_escaped_like_json_stringify() {
        assert_eq!(json_quote("a\nb\t"), "\"a\\nb\\t\"");
        assert_eq!(json_quote("a\u{0001}"), "\"a\\u0001\"");
        assert_eq!(json_quote("a\\b\"c"), "\"a\\\\b\\\"c\"");
    }

    #[test]
    fn host_key_lines_are_validated() {
        let key = "AAAAC3NzaC1lZDI1NTE5AAAAIExample+Key/WithChars=";
        let (key_type, parsed) = parse_host_key_line(
            "kiana-env-test",
            &format!("kiana-env-test ssh-ed25519 {key}"),
        )
        .expect("line");
        assert_eq!(key_type, "ssh-ed25519");
        assert_eq!(parsed, key);
        assert!(
            parse_host_key_line("other", &format!("kiana-env-test ssh-ed25519 {key}")).is_err()
        );
        assert!(
            parse_host_key_line(
                "kiana-env-test",
                "@cert-authority kiana-env-test ssh-ed25519 AAAA"
            )
            .is_err()
        );
        assert!(parse_host_key_line("kiana-env-test", "kiana-env-test ssh-ed25519 <bad>").is_err());
        assert_eq!(
            parse_host_key_line("kiana-env-test", "nope")
                .unwrap_err()
                .to_string(),
            "Not a host key line for kiana-env-test: \"nope\""
        );
        assert!(
            parse_host_key_line(
                "kiana-env-test",
                "kiana-env-test sk-ssh-ed25519@openssh.com AAAA"
            )
            .is_ok()
        );
        assert!(
            parse_host_key_line("kiana-env-test", "kiana-env-test ecdsa-sha2-nistp256 AAAA")
                .is_ok()
        );
        assert!(parse_host_key_line("kiana-env-test", "kiana-env-test ssh-ed25519 A===").is_err());
    }

    /// 回归（review finding）：known-hosts 的切分用 JS 的 `String.trim()`/`split(/\s+/)`
    /// （ssh.ts:210-221,258-286），不是 Rust 的 `char::is_whitespace`：U+FEFF 在 JS 里是空白，
    /// U+0085 不是。
    #[test]
    fn known_hosts_whitespace_follows_javascript() {
        let key = "AAAAC3NzaC1lZDI1NTE5AAAAIExample+Key/WithChars=";
        // U+FEFF 是 JS 的 `\s`（Rust 的 `char::is_whitespace` 不含它），所以它会分开别名与类型。
        let (key_type, parsed) = parse_host_key_line(
            "kiana-env-test",
            &format!("kiana-env-test\u{feff}ssh-ed25519 {key}"),
        )
        .expect("U+FEFF splits like JavaScript whitespace");
        assert_eq!(key_type, "ssh-ed25519");
        assert_eq!(parsed, key);
        // U+0085 是 Rust 的空白但不是 JS 的 `\s`：它留在第一个字段里，这一行不是别名下的 key。
        assert!(
            parse_host_key_line(
                "kiana-env-test",
                &format!("kiana-env-test\u{0085}ssh-ed25519 {key}")
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn forget_host_key_uses_javascript_whitespace() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("known_hosts");
        std::fs::write(
            &file,
            "kiana-env-gpu\u{feff}ssh-ed25519 AAAA\n\
             kiana-env-gpu\u{0085}ssh-ed25519 BBBB\n\
             other ssh-ed25519 CCCC\n",
        )
        .expect("write known_hosts");
        let mut target = target();
        target.known_hosts_file = file.to_string_lossy().into_owned();
        forget_host_key(&target).await.expect("forgetHostKey");
        let remaining = std::fs::read_to_string(&file).expect("known_hosts");
        assert!(
            !remaining.contains("AAAA"),
            "the U+FEFF line belongs to the alias and is removed"
        );
        assert!(
            remaining.contains("BBBB"),
            "U+0085 is not JavaScript whitespace, so the line stays"
        );
        assert!(remaining.contains("CCCC"), "other aliases stay");
    }

    #[tokio::test]
    async fn accept_host_key_finds_a_changed_key_behind_a_javascript_space() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("known_hosts");
        std::fs::write(&file, "kiana-env-gpu\u{feff}ssh-ed25519 OLD\n").expect("write known_hosts");
        let mut target = target();
        target.known_hosts_file = file.to_string_lossy().into_owned();
        match accept_host_key(&target, &["kiana-env-gpu ssh-ed25519 NEW".to_owned()]).await {
            Err(SshFailure::HostKeyChanged(_)) => {}
            Err(error) => panic!("expected HostKeyChanged, got {error}"),
            Ok(()) => panic!("the old U+FEFF host key is trusted, so the new key must be refused"),
        }
        assert_eq!(
            std::fs::read_to_string(&file).expect("known_hosts"),
            "kiana-env-gpu\u{feff}ssh-ed25519 OLD\n",
            "a refused key is not written"
        );
    }

    #[test]
    fn architectures_normalize_like_the_typescript_helper() {
        assert_eq!(normalize_arch("x86_64").expect("arch"), RemoteArch::X64);
        assert_eq!(normalize_arch("AMD64").expect("arch"), RemoteArch::X64);
        assert_eq!(normalize_arch("x64").expect("arch"), RemoteArch::X64);
        assert_eq!(normalize_arch("aarch64").expect("arch"), RemoteArch::Arm64);
        assert_eq!(normalize_arch("ARM64").expect("arch"), RemoteArch::Arm64);
        assert_eq!(
            normalize_arch("mips").unwrap_err().to_string(),
            "Unsupported remote architecture: mips"
        );
    }

    #[test]
    fn daemon_paths_are_named_by_the_hash_prefix() {
        let remote = RemotePlatform {
            platform: RemotePlatformName::Linux,
            arch: RemoteArch::X64,
            home: "/home/me".to_owned(),
            shell: None,
            warnings: Vec::new(),
        };
        assert_eq!(
            daemon_path(
                &remote,
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            "/home/me/.kiana/mobile/tools/kiana-env-0123456789abcdef0123456789abcdef"
        );
        let windows = RemotePlatform {
            platform: RemotePlatformName::Windows,
            arch: RemoteArch::Arm64,
            home: "C:\\Users\\me".to_owned(),
            shell: Some(RemoteShell::Cmd),
            warnings: Vec::new(),
        };
        assert_eq!(
            daemon_path(
                &windows,
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            "C:\\Users\\me\\.kiana\\mobile\\tools\\kiana-env-0123456789abcdef0123456789abcdef.exe"
        );
    }

    #[test]
    fn powershell_scripts_are_encoded_as_utf16le_base64() {
        // Node: Buffer.from("'KIANA-ENV-PROBE'", "utf16le").toString("base64")
        assert_eq!(
            powershell("'KIANA-ENV-PROBE'"),
            "powershell -NoProfile -NonInteractive -EncodedCommand JwBLAEkAQQBOAEEALQBFAE4AVgAtAFAAUgBPAEIARQAnAA=="
        );
    }

    #[test]
    fn base64_upload_lines_are_wrapped_at_76_characters() {
        let bytes: Vec<u8> = (0..200u8).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let mut wrapped = String::new();
        let encoded_bytes = encoded.as_bytes();
        let mut offset = 0usize;
        while offset < encoded_bytes.len() {
            let end = (offset + 76).min(encoded_bytes.len());
            wrapped.push_str(&encoded[offset..end]);
            wrapped.push('\n');
            offset = end;
        }
        wrapped.push_str("KIANA-ENV-END\n");
        assert_eq!(encoded.len(), 268);
        let lines: Vec<&str> = wrapped.split('\n').collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].len(), 76);
        assert_eq!(lines[1].len(), 76);
        assert_eq!(lines[2].len(), 76);
        assert_eq!(lines[3].len(), 40);
        assert_eq!(lines[4], "KIANA-ENV-END");
        assert!(lines.iter().take(4).all(|line| line.len() <= 76));
        assert_eq!(
            lines[..4].concat(),
            encoded,
            "the lines carry the whole base64 payload"
        );
    }

    #[test]
    fn probe_output_is_parsed_after_the_marker() {
        let target = target();
        let probe = parse_probe(
            &target,
            "banner\r\nKIANA-ENV-PROBE\r\nLinux\r\naarch64\r\nAndroid\r\n/home/me\r\n/tmp\r\nlibtermux-exec.so\r\n",
        )
        .expect("probe");
        assert_eq!(probe.system, "Linux");
        assert_eq!(probe.machine, "aarch64");
        assert_eq!(probe.os, "Android");
        assert_eq!(probe.home, "/home/me");
        assert_eq!(probe.tmpdir, "/tmp");
        assert!(probe.preload.contains("termux-exec"));
        assert_eq!(
            parse_probe(&target, "nothing").unwrap_err().to_string(),
            "Unexpected answer from gpu-box: nothing"
        );
        let sparse =
            parse_probe(&target, "KIANA-ENV-PROBE\nLinux\nx86_64\nLinux\n/h").expect("probe");
        assert_eq!(sparse.tmpdir, "-");
        assert_eq!(sparse.preload, "-");
    }

    #[test]
    fn termux_warnings_are_in_order() {
        let probe = Probe {
            system: "Linux".to_owned(),
            machine: "aarch64".to_owned(),
            os: "Android".to_owned(),
            home: "/".to_owned(),
            tmpdir: "-".to_owned(),
            preload: "-".to_owned(),
        };
        assert_eq!(
            termux_warnings(&probe),
            vec![
                "TMPDIR is not set; Termux's sshd normally sets it to $PREFIX/tmp.".to_owned(),
                "termux-exec is not loaded (LD_PRELOAD); scripts with #!/usr/bin/env shebangs will fail."
                    .to_owned(),
                "Android may suspend Termux; run termux-wake-lock on the device to keep the connection alive."
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn unknown_host_key_stderr_is_recognized() {
        assert!(stderr_means_unknown_host_key(
            "Host key verification failed."
        ));
        assert!(stderr_means_unknown_host_key(
            "No ED25519 host key is known for [127.0.0.1]:2222 and you have requested strict checking."
        ));
        assert!(!stderr_means_unknown_host_key(
            "REMOTE HOST IDENTIFICATION HAS CHANGED!"
        ));
        assert!(!stderr_means_unknown_host_key(
            "Permission denied (publickey)."
        ));
    }

    #[test]
    fn launch_commands_match_the_typescript_forms() {
        let linux = RemotePlatform {
            platform: RemotePlatformName::Linux,
            arch: RemoteArch::X64,
            home: "/home/me".to_owned(),
            shell: None,
            warnings: Vec::new(),
        };
        assert_eq!(
            launch_command(&linux, "/home/me/.kiana/mobile/tools/kiana-env-abc", false),
            "'/home/me/.kiana/mobile/tools/kiana-env-abc'"
        );
        assert_eq!(
            launch_command(&linux, "/x", true),
            "exec \"$SHELL\" -lc 'exec \"$0\" \"$@\"' '/x'"
        );
        let powershell_shell = RemotePlatform {
            platform: RemotePlatformName::Windows,
            arch: RemoteArch::X64,
            home: "C:\\Users\\me".to_owned(),
            shell: Some(RemoteShell::Powershell),
            warnings: Vec::new(),
        };
        assert_eq!(
            launch_command(&powershell_shell, "C:\\a b\\kiana-env.exe", false),
            "& 'C:\\a b\\kiana-env.exe'"
        );
        let cmd = RemotePlatform {
            platform: RemotePlatformName::Windows,
            arch: RemoteArch::X64,
            home: "C:\\Users\\me".to_owned(),
            shell: Some(RemoteShell::Cmd),
            warnings: Vec::new(),
        };
        assert_eq!(
            launch_command(&cmd, "C:\\a b\\kiana-env.exe", false),
            "\"C:\\a b\\kiana-env.exe\""
        );
        assert_eq!(
            launch_command(&cmd, "C:\\kiana-env.exe", false),
            "C:\\kiana-env.exe"
        );
    }

    /// 写一个假的 `ssh`：把收到的远端命令逐行记到 `log`，再用 `body` 回答。
    #[cfg(unix)]
    fn fake_ssh(directory: &std::path::Path, body: &str, log: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("ssh");
        let script = format!(
            "#!/bin/sh\nfor last do :; done\nprintf '%s\\n' \"$last\" >> '{}'\n{body}\n",
            log.display()
        );
        std::fs::write(&path, script).expect("write the fake ssh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make the fake ssh executable");
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn fake_target(directory: &std::path::Path, ssh: String) -> SshTarget {
        SshTarget {
            host: "fake-host".to_owned(),
            user: None,
            port: None,
            identity_file: None,
            known_hosts_file: directory.join("known_hosts").to_string_lossy().into_owned(),
            host_key_alias: "kiana-env-fake".to_owned(),
            ssh: Some(ssh),
            config_file: None,
        }
    }

    /// 跑 `detect_platform`，跳过“POSIX 探测的 `ssh` spawn 因临时脚本被并发 fork 继承写 fd 而
    /// ETXTBSY”的污染：那种情况下 `detect_platform` 会按设计改跑 Windows 探测，日志的第一行
    /// 不是 `uname`。清空日志后重试（与本用例要测的行为无关）。
    #[cfg(unix)]
    async fn detect_platform_with_clean_log(
        target: &SshTarget,
        log: &std::path::Path,
    ) -> Result<RemotePlatform, SshFailure> {
        for _ in 0..20 {
            std::fs::write(log, "").expect("truncate the fake ssh log");
            let result = detect_platform(target).await;
            let invocations = std::fs::read_to_string(log).unwrap_or_default();
            let posix_ran = invocations
                .lines()
                .next()
                .is_some_and(|line| line.contains("uname"));
            let contaminated = !posix_ran || result.as_ref().err().is_some_and(is_text_file_busy);
            if !contaminated {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        detect_platform(target).await
    }

    /// 回归（review finding）：POSIX 探测**跑通但输出无法解析**时也要重试 Windows 探测
    /// （TS 把 `parseProbe` 与 `runSsh` 放在同一个 try 里，ssh.ts:329-337）。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detect_platform_retries_the_windows_probe_when_the_posix_answer_is_unparseable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = directory.path().join("ssh.log");
        let body = r#"case "$last" in
  *uname*) echo 'Welcome to the machine';;
  *powershell*) printf '%s\n' 'KIANA-ENV-PROBE' 'Windows' 'AMD64' '-' 'C:\Users\tester' '-' '-';;
  *%OS%*) echo 'Windows_NT';;
  *) printf '%s\n' 'KIANA-ENV-PROBE' 'Linux' 'x86_64' 'GNU/Linux' '/home/tester' '-' '-';;
esac"#;
        let ssh = fake_ssh(directory.path(), body, &log);
        let target = fake_target(directory.path(), ssh);
        let remote = detect_platform_with_clean_log(&target, &log)
            .await
            .expect("detectPlatform");
        assert_eq!(
            remote,
            RemotePlatform {
                platform: RemotePlatformName::Windows,
                arch: RemoteArch::X64,
                home: "C:\\Users\\tester".to_owned(),
                shell: Some(RemoteShell::Cmd),
                warnings: Vec::new(),
            }
        );
        let invocations = std::fs::read_to_string(&log).expect("the fake ssh log");
        assert_eq!(
            invocations.lines().count(),
            3,
            "POSIX probe, Windows probe, echo %OS%"
        );
    }

    /// host key 变化不在重试之列：只调用一次 `ssh`，错误原样上抛（ssh.ts:331-333）。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detect_platform_propagates_a_changed_host_key_without_retrying() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = directory.path().join("ssh.log");
        let body = r#"case "$last" in
  *uname*) echo 'REMOTE HOST IDENTIFICATION HAS CHANGED!' >&2; exit 255;;
  *) printf '%s\n' 'KIANA-ENV-PROBE' 'Windows' 'AMD64' '-' 'C:\Users\tester' '-' '-';;
esac"#;
        let ssh = fake_ssh(directory.path(), body, &log);
        let target = fake_target(directory.path(), ssh);
        match detect_platform_with_clean_log(&target, &log).await {
            Err(SshFailure::HostKeyChanged(_)) => {}
            Err(error) => panic!("expected HostKeyChanged, got {error}"),
            Ok(remote) => panic!("expected HostKeyChanged, got {remote:?}"),
        }
        let invocations = std::fs::read_to_string(&log).expect("the fake ssh log");
        assert_eq!(invocations.lines().count(), 1, "no Windows-probe retry");
    }

    /// 回归（review finding）：同一个 known-hosts 文件的所有重写严格串行，无论多少个调用者重叠。
    /// TS 的 `knownHostsChanges` 永远存着该文件最新的 promise（ssh.ts:224-251）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn known_hosts_changes_are_serialized_for_every_overlapping_caller() {
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory
            .path()
            .join("known_hosts")
            .to_string_lossy()
            .into_owned();
        let (a_started_tx, a_started) = channel::<()>();
        let (release_a_tx, release_a) = channel::<()>();
        let (b_started_tx, b_started) = channel::<()>();
        let (release_b_tx, release_b) = channel::<()>();
        let (d_started_tx, d_started) = channel::<()>();
        let (release_d_tx, release_d) = channel::<()>();

        let a = {
            let file = file.clone();
            tokio::spawn(async move {
                change_known_hosts(&file, move |mut lines| {
                    a_started_tx.send(()).expect("signal A");
                    release_a
                        .recv_timeout(Duration::from_secs(10))
                        .expect("release A");
                    lines.push("a".to_owned());
                    Ok(lines)
                })
                .await
            })
        };
        a_started
            .recv_timeout(Duration::from_secs(10))
            .expect("A starts");
        let b = {
            let file = file.clone();
            tokio::spawn(async move {
                change_known_hosts(&file, move |mut lines| {
                    b_started_tx.send(()).expect("signal B");
                    release_b
                        .recv_timeout(Duration::from_secs(10))
                        .expect("release B");
                    lines.push("b".to_owned());
                    Ok(lines)
                })
                .await
            })
        };
        release_a_tx.send(()).expect("release A");
        a.await.expect("A completes").expect("A succeeds");
        b_started
            .recv_timeout(Duration::from_secs(10))
            .expect("B starts");
        // A 完成时旧实现会删掉表项（B 还持有旧锁）。修复后 D 必须排在 B 后面。
        let d = {
            let file = file.clone();
            tokio::spawn(async move {
                change_known_hosts(&file, move |mut lines| {
                    d_started_tx.send(()).expect("signal D");
                    release_d
                        .recv_timeout(Duration::from_secs(10))
                        .expect("release D");
                    lines.push("d".to_owned());
                    Ok(lines)
                })
                .await
            })
        };
        assert!(
            d_started.recv_timeout(Duration::from_millis(500)).is_err(),
            "D must wait for B's change to finish"
        );
        release_b_tx.send(()).expect("release B");
        b.await.expect("B completes").expect("B succeeds");
        release_d_tx.send(()).expect("release D");
        d.await.expect("D completes").expect("D succeeds");
        let mut lines: Vec<String> = std::fs::read_to_string(&file)
            .expect("known_hosts")
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        assert_eq!(lines, ["a", "b", "d"], "no rewrite is lost");
    }

    // -----------------------------------------------------------------------
    // 回归（review findings）：本地文件系统失败用 Node 的 fs 消息；local 的 Node 名字
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    fn running_as_root() -> bool {
        // SAFETY: `geteuid` 只读进程的 euid。
        unsafe { libc::geteuid() == 0 }
    }

    fn remote_for_tests() -> RemotePlatform {
        RemotePlatform {
            platform: RemotePlatformName::Linux,
            arch: RemoteArch::X64,
            home: "/home/me".to_owned(),
            shell: None,
            warnings: Vec::new(),
        }
    }

    /// 回归（review finding）：`deployDaemon` 读二进制失败时是 Node 的
    /// `<CODE>: <描述>, open|read '<path>'`，不是 Rust `io::Error` 的 Display。
    #[cfg(unix)]
    #[tokio::test]
    async fn deploy_daemon_reports_node_fs_errors_for_the_binary() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let remote = remote_for_tests();
        let target = target();
        // 目录：Node 的 readFile 先 open 后 read，EISDIR 的 syscall 是 read。
        let as_directory = directory.path().to_string_lossy().into_owned();
        let error = deploy_daemon(&target, &remote, Some(&as_directory))
            .await
            .expect_err("a directory cannot be read as a binary");
        assert_eq!(
            error.to_string(),
            format!("EISDIR: illegal operation on a directory, read '{as_directory}'")
        );
        if !running_as_root() {
            let file = directory.path().join("daemon");
            std::fs::write(&file, b"x").expect("write the binary");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))
                .expect("make the binary unreadable");
            let as_file = file.to_string_lossy().into_owned();
            let error = deploy_daemon(&target, &remote, Some(&as_file))
                .await
                .expect_err("the binary is unreadable");
            assert_eq!(
                error.to_string(),
                format!("EACCES: permission denied, open '{as_file}'")
            );
        }
    }

    /// 回归（review finding）：`changeKnownHosts` 的 mkdir/read/write/rename 失败都用 Node 的
    /// fs 消息；写入失败的 syscall 是 `open`，路径是临时文件。
    #[cfg(unix)]
    #[tokio::test]
    async fn change_known_hosts_reports_node_write_errors() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return;
        }
        let directory = tempfile::tempdir().expect("temporary directory");
        let read_only = directory.path().join("ro");
        std::fs::create_dir(&read_only).expect("create the read-only directory");
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500))
            .expect("make the directory read-only");
        let mut target = target();
        target.known_hosts_file = read_only.join("known_hosts").to_string_lossy().into_owned();
        let error = forget_host_key(&target)
            .await
            .expect_err("the file cannot be written");
        let message = error.to_string();
        assert!(
            message.starts_with(&format!(
                "EACCES: permission denied, open '{}.",
                target.known_hosts_file
            )),
            "unexpected message: {message}"
        );
        assert!(message.ends_with(".tmp'"), "unexpected message: {message}");
    }

    /// 回归（review finding）：`rename` 失败的 Node 消息是
    /// `<CODE>: <描述>, rename '<from>' -> '<to>'`。
    #[cfg(unix)]
    #[test]
    fn rename_errors_use_the_node_message() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            node_fs_error_pair(&error, "rename", "/x", "/y").to_string(),
            "ENOENT: no such file or directory, rename '/x' -> '/y'"
        );
        let error = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(
            node_fs_error(&error, "open", "/tmp/x").to_string(),
            "EACCES: permission denied, open '/tmp/x'"
        );
    }

    /// 回归（review finding）：递归 `mkdir` 的错误带失败的那一级路径（Node 的
    /// `mkdir '/ro/a'`），已存在的文件报 EEXIST，已存在的目录成功。
    #[cfg(unix)]
    #[test]
    fn make_directory_reports_the_failing_component() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return;
        }
        let directory = tempfile::tempdir().expect("temporary directory");
        let read_only = directory.path().join("ro");
        std::fs::create_dir(&read_only).expect("create the read-only directory");
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500))
            .expect("make the directory read-only");
        let error = make_directory(&read_only.join("a/b/c")).expect_err("mkdir fails");
        assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, mkdir '{}'",
                read_only.join("a").display()
            )
        );
        let file = directory.path().join("file");
        std::fs::write(&file, "x").expect("write the file");
        assert_eq!(
            make_directory(&file).unwrap_err().to_string(),
            format!("EEXIST: file already exists, mkdir '{}'", file.display())
        );
        make_directory(&read_only).expect("an existing directory is fine");
    }

    /// 回归（review finding）：`mkdtemp` 失败用 Node 的 `mkdtemp '<path>'` 消息（路径带随机
    /// 后缀），不是固定的 "could not create a temporary directory"。
    #[cfg(unix)]
    #[test]
    fn mkdtemp_reports_the_node_error() {
        use std::os::unix::fs::PermissionsExt;
        if running_as_root() {
            return;
        }
        let directory = tempfile::tempdir().expect("temporary directory");
        let read_only = directory.path().join("ro");
        std::fs::create_dir(&read_only).expect("create the read-only directory");
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500))
            .expect("make the directory read-only");
        let message = mkdtemp_in(&read_only, "kiana-env-hostkey-")
            .expect_err("mkdtemp fails")
            .to_string();
        let prefix = format!(
            "EACCES: permission denied, mkdtemp '{}/kiana-env-hostkey-",
            read_only.display()
        );
        assert!(
            message.starts_with(&prefix),
            "unexpected message: {message}"
        );
        assert!(message.ends_with('\''), "unexpected message: {message}");
    }

    /// 回归（review finding）：`localDaemon` 的错误消息用 `process.platform`/`process.arch`
    /// 的拼写（`sunos`、`ia32`、`ppc`、`ppc64`、`loong64`），不是 Rust 的 `solaris`/`x86`/…
    #[test]
    fn local_names_follow_process_platform_and_arch() {
        assert_eq!(node_platform_name("solaris"), "sunos");
        assert_eq!(node_platform_name("illumos"), "sunos");
        assert_eq!(node_platform_name("macos"), "darwin");
        assert_eq!(node_platform_name("windows"), "win32");
        assert_eq!(node_platform_name("linux"), "linux");
        assert_eq!(node_arch_name("x86_64"), "x64");
        assert_eq!(node_arch_name("aarch64"), "arm64");
        assert_eq!(node_arch_name("x86"), "ia32");
        assert_eq!(node_arch_name("powerpc"), "ppc");
        assert_eq!(node_arch_name("powerpc64"), "ppc64");
        assert_eq!(node_arch_name("powerpc64le"), "ppc64");
        assert_eq!(node_arch_name("loongarch64"), "loong64");
        assert_eq!(node_arch_name("riscv64"), "riscv64");
        assert_eq!(
            normalize_arch(node_arch_name("x86"))
                .unwrap_err()
                .to_string(),
            "Unsupported remote architecture: ia32"
        );
    }

    /// 回归（review finding）：`spawn()` 同步抛出的 errno（ENAMETOOLONG 等）不是 `SshError`
    /// （Node 的 ErrnoException 直接让 runSsh 的 Promise 拒绝），消息仍按 Node 拼写。
    #[cfg(unix)]
    #[tokio::test]
    async fn a_synchronously_rejected_spawn_is_not_an_ssh_error() {
        let mut target = target();
        // 单个路径分量超过 NAME_MAX：`spawn()` 同步失败。
        target.ssh = Some(format!("/tmp/{}", "x".repeat(300)));
        match run_ssh(&target, "exit 0", RunSshOptions::default()).await {
            Err(SshFailure::Other(message)) => assert_eq!(message, "spawn ENAMETOOLONG"),
            other => panic!("expected a plain error, got {other:?}"),
        }
    }
}
