//! connection.ts：daemon 帧、hello、ping（5s）、lost 判定。
//!
//! 帧格式（docs/protocol.md、spec §4.1）：13 字节头（u32 BE length = 9 + json + payload、
//! u8 type、u32 BE id、u32 BE jsonLength）+ JSON + payload。JSON 对象的 `op` 由客户端最后插入
//! （TS 的 `{...json, op}`），`handle` / `target` 同样最后追加。
//!
//! 与 TS 的对应与差异：
//! - `spawn` + stdin/stdout/stderr 管道 → [`tokio::process`]。Node 的 `spawn` 把启动失败作为
//!   异步 `error` 事件；Rust 的 `Command::spawn` 同步返回 `io::Error`，失败消息按 Node 的规则
//!   重建（`crate::spawn_error_message`：`spawn <program> <CODE>`）。Node 只把
//!   EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 作为异步事件（`spawn_error`，`lost: true`）；其余
//!   errno（E2BIG、ENOEXEC、ENAMETOOLONG、ENOTDIR…）是 `spawn()` 的同步抛出，在 TS 里不是
//!   `RemoteError`，这里用 code `unknown`、无 `lost` 的 [`RemoteError`] 承载。
//! - `child.stdin.write` 是同步入队；Rust 侧用无界 mpsc + 写任务，保证**帧的先后顺序是调用顺序**
//!   （terminal 的“输入保序”用例依赖它）。
//! - `setInterval(5000).unref()` → 自有 ping 任务，teardown/close 时 abort；进程可正常退出。
//! - `#ready` 的共享：并发的 `#connect()` 共享同一次启动；启动失败不记忆（下一次请求重试）。
//! - `buffer.subarray` 的 payload 视图 → 复制成 `Vec<u8>`（spec §2.1.3 第 121 行）。
//!
//! # 这个文件负责什么
//! `Connection` 是 daemon 的传输层：启动/重启子进程、收发帧、派发回复与进度事件、在连接
//! 丢失时结算在途请求。除 `hello` 外它不认识任何具体操作——`remote_env.rs`、`list_files.rs`、
//! `terminal.rs`、`watch.rs` 只通过 [`Connection::request`]（或 `request_now` /
//! `request_detached`）发送 `op`。`remote_env.rs` 与 `list_files.rs` 的文件操作把
//! [`RemoteError`] 转成 `FileError` / `EnvError`；`terminal.rs` 的公开入口（`open_terminal`、
//! `RemoteTerminal::exited` / `foreground`）与 `watch.rs` 的 `RemoteWatcher::open` /
//! `start_watch` 直接返回 [`RemoteError`]，`watch.rs` 仅把事件里的错误转成 `FileError`。
//!
//! # 一次连接的生命周期
//! 1. [`Connection::new`] 不启动进程；第一次 `request` / `info` / `session` 才调用 `connect`。
//! 2. `connect` 让一个调用者执行 `start`，其余并发调用者在 `State::starting` 上排队等同一个
//!    结果；启动失败不缓存，下一次调用会重新启动。
//! 3. `start` 拼出 `… serve --token <hex>`、spawn 子进程、装好各后台任务，然后以普通请求发
//!    `hello { protocol: 1 }`；stdout 读任务要先找到握手行 `KIANA-ENV <token>\n` 才会解析帧。
//! 4. 每个请求分配到 u32 id，登记进 `Session::pending`，REQUEST 帧推进该 session 的 stdin
//!    队列；stdout 读任务解析帧并 `dispatch` 到对应的 oneshot。
//! 5. 子进程退出、坏帧、ping 超时或 `close()` 都走 `teardown`：把 `live` 置 false、清空
//!    `State`、通知 exit watcher 杀子进程，并让全部在途请求以同一个 [`RemoteError`] 失败。
//!    除 `close()` 外，下一次请求会启动新的 daemon；`close()` 置位的 `closed` 不再复位，
//!    之后的 `connect` 始终返回 `Connection closed`。
//!
//! # 并发与顺序
//! - 共享状态用 `std::sync::Mutex`；锁中毒时 `lock` 辅助函数取回内部值继续用。
//!   `State` 与某个 `Session` 的 `pending` 不会同时持有：`teardown` 先释放 `state` 再锁
//!   `pending`，`begin_send` 也分成两个独立的临界区。
//! - 帧写入走无界 mpsc：调用线程只入队、不等待 pipe；每个 session 只有一个写任务，所以帧的
//!   写出顺序就是入队顺序。同一线程上依次发起的请求因此保序（terminal 的输入保序依赖它）。
//! - 回调在不同任务里同步执行：`on_event` 在 stdout 读任务，`on_log` 在 stdout/stderr 读任务，
//!   `on_start` 在 `begin_send` 的当前执行线程执行；同步入口（`request_now` /
//!   `request_detached`）就是调用线程，而 async 的 [`Connection::request`] 由执行器决定在哪个
//!   线程 poll（普通请求还会先经过 `connect().await`），不保证线程身份；类型都要求
//!   `Send + Sync`，实现方不能依赖线程身份。
//!
//! # 错误与失败模式
//! - 所有失败都是 [`RemoteError`]：`fields["lost"] == true` 表示连接丢失
//!   （[`is_connection_lost`]），否则是 daemon 明确返回的错误或启动配置问题。
//! - `teardown` 以它收到的错误结算全部在途请求（只有第一次生效，之后是 no-op）。连接丢失的
//!   原因包括 `kiana-env connection lost`、`… timed out` 与 `Corrupt frame …`；`close()` 也走
//!   `teardown`，但结算错误是不带 `lost` 标记的 `Connection closed`（见下一条）。已经写出的帧
//!   不会重发到新 daemon，所以写操作的结果可能是未知的。
//! - `RequestOptions::session` 绑定 daemon 的 handle：daemon 重启后这类请求直接以 lost 失败，
//!   不会误发到新 daemon。
//! - `close()` 之后 `connect` 一律返回 `Connection closed`（不带 `lost` 标记）；`kill` /
//!   `request_detached` 这类只写帧的入口则静默不做任何事。
//!
//! # 阅读顺序
//! [`RemoteError`] / [`RemoteInfo`] → `frame` → `Connection::connect` / `Connection::request`
//! → `Inner::start`（启动与握手）→ `read_stdout` / `dispatch`（收帧）→ `teardown`。
//!
//! TS: packages/env/src/connection.ts

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kiana_chord::context::{AbortSignal, AbortSubscription};
use kiana_durable::env::BoxError;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

/// 帧类型。
pub(crate) const REQUEST: u8 = 1;
/// 请求成功的结果；`id` 对应请求，收到即结束该请求。
pub(crate) const RESULT: u8 = 2;
/// 请求失败（JSON 为 `{ code, message, … }`）；同样结束请求。
pub(crate) const ERROR: u8 = 3;
/// 运行中请求的进度事件（`exec` / `pty` 输出、`watch` 变更），不结束请求。
pub(crate) const EVENT: u8 = 4;
/// 取消请求；`{ mode: "kill" }` 只杀进程组、不标记中止（见 [`Connection::kill`]）。
pub(crate) const CANCEL: u8 = 5;
/// 心跳；接收方忽略内容，`id` 为 0。
pub(crate) const PING: u8 = 6;

/// 一帧的上限（16 MiB）；daemon 与客户端都拒绝更大的帧。
///
/// 接收侧在 `read_stdout` 里校验 `length`（`9..=MAX_FRAME`）；发送侧 `frame` 不校验，
/// 分块由调用方负责（terminal 每片 1 MiB，remote_env 每片 512 KiB）。
pub(crate) const MAX_FRAME: usize = 16 * 1024 * 1024;
/// 客户端 ping 间隔；协议规定双方都每 5 秒发一次 PING（`docs/protocol.md` 的 Liveness）。
const PING_INTERVAL: Duration = Duration::from_secs(5);
/// daemon 每五秒 ping 一次；这么久的静默说明连接已断。
///
/// `last_seen` 只在 stdout 读到字节时刷新（`read_stdout`），所以判据是“stdout 静默”，
/// 而不是“没收到 PING”。
const SILENCE_LIMIT_MS: i64 = 30_000;
/// 启动 daemon 并完成 `hello` 的时限。
///
/// 从 `spawn` 之后开始计时，覆盖握手行与 `hello` 往返；`ConnectionCommand::Dynamic` 自身的
/// 耗时不在其中（它在 spawn 之前）。超时后以 `kiana-env did not answer within 60 s` 拆掉
/// session。
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// 握手标记之前允许的噪音（ssh banner）上限。
///
/// 超过后只保留末尾 `marker.len()` 字节，避免把可能被分片截断的握手行一起丢掉
/// （`read_stdout`）。
const NOISE_LIMIT: usize = 1024 * 1024;

/// TS 的 `Json = Record<string, unknown>`。
///
/// 工作区为 `serde_json` 启用了 `preserve_order`，插入顺序就是序列化顺序，与 JS 对象的键序
/// 一致。
///
/// TS: packages/env/src/connection.ts#Json
pub type Json = serde_json::Map<String, Value>;

/// daemon 报告的失败，带 Node 风格错误码。
///
/// `code` / `message` / `path` 是构造时从 `fields` 读出的副本；`fields` 保留 daemon 发来的
/// 完整 JSON，其中 `lost: true` 标记连接丢失（[`is_connection_lost`]），调用方据此区分
/// “daemon 拒绝了请求”和“连接没了、结果未知”。
///
/// TS: packages/env/src/connection.ts#RemoteError
#[derive(Debug, Clone)]
pub struct RemoteError {
    /// 机器可读的错误码（`ENOENT`、`aborted`、`spawn_error` …）。
    pub code: String,
    /// daemon 报告的路径（如果有）。
    pub path: Option<String>,
    /// 完整的错误对象（`fields.lost` 区分连接丢失）。
    pub fields: Json,
    message: String,
}

impl RemoteError {
    /// `new RemoteError(fields)`：由 ERROR 帧的 JSON 构造。
    ///
    /// - `message` 不是字符串时用 `"Remote operation failed"`，`code` 不是字符串时用
    ///   `"unknown"`（TS 的 `typeof` 检查）；
    /// - `path` 只在是字符串时设置；
    /// - `fields` 原样保存，之后不再改动，`is_connection_lost` 读的就是它。
    pub fn new(fields: Json) -> Self {
        let message = match fields.get("message") {
            Some(Value::String(message)) => message.clone(),
            _ => "Remote operation failed".to_owned(),
        };
        let code = match fields.get("code") {
            Some(Value::String(code)) => code.clone(),
            _ => "unknown".to_owned(),
        };
        let path = match fields.get("path") {
            Some(Value::String(path)) => Some(path.clone()),
            _ => None,
        };
        RemoteError {
            code,
            path,
            fields,
            message,
        }
    }

    /// `error.message`。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 连接失败或结束（`fields.lost === true`）。
    pub(crate) fn lost(message: impl Into<String>) -> Self {
        let message = message.into();
        let mut fields = Json::new();
        fields.insert("code".to_owned(), Value::String("unknown".to_owned()));
        fields.insert("message".to_owned(), Value::String(message.clone()));
        fields.insert("lost".to_owned(), Value::Bool(true));
        RemoteError {
            code: "unknown".to_owned(),
            path: None,
            fields,
            message,
        }
    }

    /// `spawn_error`（`lost` 标记按调用点决定）。
    fn spawn_error(message: impl Into<String>, lost: bool) -> Self {
        let message = message.into();
        let mut fields = Json::new();
        fields.insert("code".to_owned(), Value::String("spawn_error".to_owned()));
        fields.insert("message".to_owned(), Value::String(message.clone()));
        if lost {
            fields.insert("lost".to_owned(), Value::Bool(true));
        }
        RemoteError {
            code: "spawn_error".to_owned(),
            path: None,
            fields,
            message,
        }
    }

    /// `spawn()` 同步抛出的错误（Node 的 `ErrnoException`）：TS 里它不是 `RemoteError`，也没有
    /// `lost` 标记，`error.message` 是 `spawn <CODE>`。
    ///
    /// Rust 的 [`Connection`] API 只有 `RemoteError` 一种错误类型，因此用 `code: "unknown"`、
    /// 无 `lost` 的 `RemoteError` 承载：对 `is_connection_lost`、`to_file_error` 与 `exec` 的
    /// code 映射（非 `RemoteError` → `unknown`）而言，与 TS 的观察结果一致。
    fn thrown(message: impl Into<String>) -> Self {
        let message = message.into();
        let mut fields = Json::new();
        fields.insert("code".to_owned(), Value::String("unknown".to_owned()));
        fields.insert("message".to_owned(), Value::String(message.clone()));
        RemoteError {
            code: "unknown".to_owned(),
            path: None,
            fields,
            message,
        }
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RemoteError {}

/// `hello` 报告的远端机器信息。
///
/// 由 `hello` 回复的 JSON 反序列化；`#[serde(default)]` 让旧 daemon 缺失的字段落到默认值
/// （`features: None` 表示 daemon 不认识后续新增的操作）。
///
/// TS: packages/env/src/connection.ts#RemoteInfo
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteInfo {
    /// 协议版本；`start` 只接受 `1.0`（JSON 的 `1` 与 `1.0` 解析成同一个 `f64`）。
    pub protocol: f64,
    /// daemon 版本。
    pub version: String,
    /// Rust 的系统名（`linux`、`macos`、`windows`、`android`）。
    pub os: String,
    /// Rust 的架构名（`x86_64`、`aarch64`）。
    pub arch: String,
    /// 远端 home 目录。
    pub home: String,
    /// 远端临时目录。
    pub tmpdir: String,
    /// 路径分隔符。
    pub separator: String,
    /// daemon 的工作目录。
    pub cwd: String,
    /// Windows 的每盘符工作目录（`C:` → `C:\work`）。
    pub drive_cwds: indexmap::IndexMap<String, String>,
    /// daemon 进程号。
    pub pid: u32,
    /// 这个 daemon 支持的可选操作，例如 `listFiles`；旧 daemon 没有。
    pub features: Option<Vec<String>>,
}

/// Node 的系统名与架构名（`nodePlatform` 的返回值）。
///
/// TS: packages/env/src/connection.ts#nodePlatform
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePlatform {
    /// Node 的 `process.platform` 拼写。
    pub platform: String,
    /// Node 的 `process.arch` 拼写。
    pub arch: String,
}

/// `windows` → `win32`、`macos` → `darwin`、`x86_64` → `x64`、`aarch64` → `arm64`、
/// `x86` → `ia32`，其它名字保留。
///
/// TS: packages/env/src/connection.ts#nodePlatform
pub fn node_platform(info: &RemoteInfo) -> NodePlatform {
    let platform = match info.os.as_str() {
        "windows" => "win32".to_owned(),
        "macos" => "darwin".to_owned(),
        other => other.to_owned(),
    };
    let arch = match info.arch.as_str() {
        "x86_64" => "x64".to_owned(),
        "aarch64" => "arm64".to_owned(),
        "x86" => "ia32".to_owned(),
        other => other.to_owned(),
    };
    NodePlatform { platform, arch }
}

/// 连接诊断输出（daemon 的 stderr 与 ssh banner）。
///
/// 在 stdout/stderr 读任务里**同步**调用：回调阻塞会推迟后续帧的读取与派发，回调里不要做
/// 长时间的工作。
///
/// TS: packages/env/src/connection.ts#ConnectionOptions
pub type LogCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// [`ConnectionCommand::Dynamic`] 返回的 future。
///
/// 每次启动尝试都会重新调用一次（`start` 在 spawn 前 `await` 它），所以动态命令可以承担
/// “先探测/部署再返回命令”的工作。
///
/// TS: packages/env/src/connection.ts#ConnectionOptions
pub type CommandFuture = Pin<Box<dyn Future<Output = Result<Vec<String>, BoxError>> + Send>>;

/// 启动 daemon 的命令：固定数组，或每次启动时计算的函数。
///
/// TS: packages/env/src/connection.ts#ConnectionOptions#command
#[derive(Clone)]
pub enum ConnectionCommand {
    /// 固定命令（`["ssh", …, "host", "~/.kiana/mobile/tools/kiana-env"]`）。
    Static(Vec<String>),
    /// 每次启动时计算（先探测并部署）；失败则这次启动失败，下一次请求重试。
    Dynamic(Arc<dyn Fn() -> CommandFuture + Send + Sync>),
}

/// `new Connection(options)` 的选项。
///
/// TS: packages/env/src/connection.ts#ConnectionOptions
#[derive(Clone)]
pub struct ConnectionOptions {
    /// 启动 daemon 的命令，不含末尾的 `serve --token <hex>`。
    ///
    /// `start` 会在数组末尾追加 `serve --token <token>`；命令本身可以是 `ssh host …`。
    pub command: ConnectionCommand,
    /// 接收 daemon 与传输层的诊断输出。
    pub on_log: Option<LogCallback>,
}

impl ConnectionOptions {
    /// 固定命令的选项。
    pub fn new(command: Vec<String>) -> Self {
        ConnectionOptions {
            command: ConnectionCommand::Static(command),
            on_log: None,
        }
    }
}

impl From<Vec<String>> for ConnectionOptions {
    fn from(command: Vec<String>) -> Self {
        ConnectionOptions::new(command)
    }
}

impl From<ConnectionCommand> for ConnectionOptions {
    fn from(command: ConnectionCommand) -> Self {
        ConnectionOptions {
            command,
            on_log: None,
        }
    }
}

/// 一个请求的回复。
///
/// TS: packages/env/src/connection.ts#Reply
#[derive(Debug, Clone)]
pub struct Reply {
    /// 回复的 JSON 对象。
    pub json: Json,
    /// 回复的 payload（复制，不是接收缓冲区的视图）。
    pub payload: Vec<u8>,
    /// 回答的 daemon session；它的 handle 只在这个 session 存活期间有效。
    ///
    /// 把后续请求的 `RequestOptions::session` 设为它，就能继续使用这个 daemon 的 handle；
    /// daemon 重启后这类请求会以 lost 失败，而不是落到新 daemon 上。
    pub session: u64,
}

/// 一个**已经发出**的请求的回复 future。
///
/// 发送在 `request_now` / `request_detached` 返回之前完成（帧已入队），future 只等待结果；
/// 丢弃 future 不会撤回请求，回复到达后 `dispatch` 仍会移除 pending 条目。
pub(crate) type ReplyNow =
    Pin<Box<dyn Future<Output = Result<Reply, RemoteError>> + Send + 'static>>;

/// 请求进度事件（`exec` 的输出等）。
///
/// 在 stdout 读任务里同步调用，只投递给仍在途的请求：请求结算后 pending 条目已被移除，
/// 迟到的事件直接丢弃。
///
/// TS: packages/env/src/connection.ts#RequestOptions
pub type EventCallback = Arc<dyn Fn(Json, Vec<u8>) + Send + Sync>;

/// 请求开始时的回调：请求 id 与 session，用于 `kill`。
///
/// 在 REQUEST 帧入队之前调用一次：调用方先拿到 id 才能取消；帧一旦入队就可能已被 daemon 处理。
///
/// TS: packages/env/src/connection.ts#RequestOptions
pub type StartCallback = Arc<dyn Fn(u32, u64) + Send + Sync>;

/// `request(op, json, options)` 的选项。
///
/// 默认值不设 payload、signal、回调，也不绑定 session：这样的请求可能触发惰性连接或重启
/// daemon。设置了 `session` 后不会再调用 `connect`（见 [`Connection::request`]）。
///
/// TS: packages/env/src/connection.ts#RequestOptions
#[derive(Clone, Default)]
pub struct RequestOptions {
    /// 请求的 payload。
    pub payload: Option<Vec<u8>>,
    /// 中止时发送 `cancel`；请求仍以 daemon 的结果或错误结算。
    pub signal: Option<AbortSignal>,
    /// 请求的进度事件。
    pub on_event: Option<EventCallback>,
    /// 请求 id 与 session。
    pub on_start: Option<StartCallback>,
    /// 只在这个 daemon session 里执行（handle 请求）；连接丢失并重启后，这类请求直接失败。
    pub session: Option<u64>,
}

impl std::fmt::Debug for RequestOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestOptions")
            .field("payload", &self.payload.as_ref().map(Vec::len))
            .field("signal", &self.signal)
            .field("on_event", &self.on_event.as_ref().map(|_| "<callback>"))
            .field("on_start", &self.on_start.as_ref().map(|_| "<callback>"))
            .field("session", &self.session)
            .finish()
    }
}

/// 错误是否意味着连接丢失，而不是 daemon 拒绝了请求。
///
/// 只检查 `fields["lost"] == true`。`close()` 产生的 `Connection closed` 与 spawn 的同步
/// 抛出都不带这个标记，因此对它们返回 `false`。
///
/// TS: packages/env/src/connection.ts#isConnectionLost
pub fn is_connection_lost(error: &RemoteError) -> bool {
    matches!(error.fields.get("lost"), Some(Value::Bool(true)))
}

/// TS 的 `closed()`：code `unknown`、message `Connection closed`，**没有** `lost` 标记。
fn closed() -> RemoteError {
    let mut fields = Json::new();
    fields.insert("code".to_owned(), Value::String("unknown".to_owned()));
    fields.insert(
        "message".to_owned(),
        Value::String("Connection closed".to_owned()),
    );
    RemoteError {
        code: "unknown".to_owned(),
        path: None,
        fields,
        message: "Connection closed".to_owned(),
    }
}

/// `closed()`（`watch.rs` 也用它：安装观察器时发现连接已关闭）。
pub(crate) fn closed_error() -> RemoteError {
    closed()
}

/// UNIX 毫秒时间；只用于 `last_seen` 的差值判断（不要求单调），系统时钟早于 epoch 时返回 0。
fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(_) => 0,
    }
}

/// [`Inner::begin_send`] 的返回值：回复的接收端，以及（带 `signal` 时）取消订阅。
type SendStart = (
    oneshot::Receiver<Result<Reply, RemoteError>>,
    Option<AbortSubscription>,
);

/// 本文件所有内部锁的统一入口：中毒（持锁期间 panic）时取回内部值继续用，避免一次 panic
/// 让整个连接永久失效。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一帧的字节：13 字节头 + JSON + payload。
///
/// 布局（全部大端序）：`u32 length`（= 9 + JSON 字节数 + payload 字节数）、`u8 kind`、
/// `u32 id`、`u32 json_length`、JSON、payload。`id` 由调用方决定（PING 用 0）；本函数不做
/// `MAX_FRAME` 检查，超长 payload 必须由调用方分块。
///
/// TS: packages/env/src/connection.ts#frame
pub(crate) fn frame(kind: u8, id: u32, json: Json, payload: &[u8]) -> Vec<u8> {
    // 序列化一个 `Map<String, Value>` 不会失败；失败只可能是 serde_json 的内部不变量被破坏。
    let body = serde_json::to_vec(&Value::Object(json))
        .expect("a JSON object of values always serializes");
    let length = 9usize + body.len() + payload.len();
    let mut bytes = Vec::with_capacity(13 + body.len() + payload.len());
    bytes.extend_from_slice(&(length as u32).to_be_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(&id.to_be_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(payload);
    bytes
}

/// 一个在途请求的结算通道与进度回调；由 `Session::pending` 按请求 id 索引。
struct Pending {
    /// 回复的发送端；`dispatch` 或 `teardown` 发送一次后丢弃。
    result: oneshot::Sender<Result<Reply, RemoteError>>,
    /// EVENT 帧的回调；只在请求结算前调用。
    on_event: Option<EventCallback>,
}

/// 一个运行中的 daemon。
///
/// 由一次 `start` 创建并安装到 `State`；`State` 与 `Ready` 持有同一个 `Arc`，所以 teardown
/// 之后仍可能被引用。后台任务里只有 exit watcher、ping 与 stdout 读任务持有它：ping/stdout
/// 检查 `live` 后退出，exit watcher 不检查 `live`（靠 kill 通道与 `try_wait` 收尾）；写任务
/// 与 stderr 任务不持有 `Session`，前者在发送端被 drop 后结束，后者读到 EOF/读错误后结束。
struct Session {
    /// 递增的会话编号（从 1 开始）；`Reply.session`、`kill` 与 handle 请求用它标识 daemon。
    id: u64,
    /// `serve --token` 的随机串；stdout 握手行用它确认对面是本次 spawn 的 daemon。
    token: String,
    /// teardown 的幂等开关（`swap(false)`）：false 之后不再写帧，读任务在下一次循环检查时
    /// 退出。
    live: AtomicBool,
    /// 最近一次从 stdout 读到字节的 UNIX 毫秒时间；ping 任务据此判断静默。
    last_seen: AtomicI64,
    /// 在途请求：id → 结算通道与进度回调；teardown 时整体 drain。
    pending: Mutex<HashMap<u32, Pending>>,
    /// 写任务的队列；teardown 取走并 drop，写任务随之结束、子进程 stdin 关闭。
    stdin: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    /// 请求 monitor 任务杀死子进程。
    kill: mpsc::UnboundedSender<()>,
    /// ping 任务句柄；teardown 时 abort。
    ping: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// exit watcher 看到的退出码（`Some(None)` = 被信号杀死）；`Some` 表示已经看到退出。
    exit_code: Mutex<Option<Option<i32>>>,
    /// 第一次 teardown 用的错误（之后的 teardown 都是 no-op）；`start` 用它把 hello 的通用
    /// `lost` 还原成真正的原因（exit 的退出码、`close()` 的 "Connection closed"）。
    teardown_error: Mutex<Option<RemoteError>>,
}

impl Session {
    fn is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    fn touch(&self) {
        self.last_seen.store(now_ms(), Ordering::SeqCst);
    }
}

/// 测试专用钩子（`#[cfg(test)]`）：在 `start` 把新 session 装进 state 之前调用。
///
/// 用于确定性地覆盖 `close()` 与 `#start` 的竞态（TS 里两者处于同一个同步回合）。
#[cfg(test)]
pub(crate) mod test_hooks {
    use super::*;

    /// 钩子类型（`Send` 足够：只在 `start` 的任务里调用一次）。
    pub(crate) type Hook = Box<dyn Fn() + Send>;

    /// 按启动命令的程序名匹配；只触发一次。
    pub(crate) static BEFORE_SESSION_INSTALL: Mutex<Option<(String, Hook)>> = Mutex::new(None);

    /// 按请求的 `op` 匹配，在 `begin_send` 登记 pending 之前调用；只触发一次。
    pub(crate) static BEFORE_PENDING: Mutex<Option<(String, Hook)>> = Mutex::new(None);
}

/// 一次成功启动的结果：`hello` 信息与已安装的 session。
struct Ready {
    /// `hello` 回复反序列化后的机器信息。
    info: RemoteInfo,
    /// 与 `State::session` 指向同一个 session 的共享句柄。
    session: Arc<Session>,
}

impl Clone for Ready {
    fn clone(&self) -> Self {
        Ready {
            info: self.info.clone(),
            session: Arc::clone(&self.session),
        }
    }
}

/// 正在启动时排队等同一个结果的接收端。
type StartWaiters = Vec<oneshot::Sender<Result<Ready, RemoteError>>>;

/// `Connection` 的可变状态；全部字段由同一把互斥锁保护。
struct State {
    /// 当前存活的 daemon（若有）；`close` / `kill` / 绑定 session 的请求都读它。
    session: Option<Arc<Session>>,
    /// 启动成功的缓存（`connect` 的快路径）；teardown 拆掉当前 session 时清空。
    ready: Option<Ready>,
    /// 非 `None` 表示已有 leader 在执行 `start`；新的 `connect` 在这里排队。
    starting: Option<StartWaiters>,
    /// 已经安装过的 daemon 数，用作下一个 session id（从 1 开始）。
    sessions: u64,
    /// 下一个请求 id；`u32` 回绕（`wrapping_add`）。
    next_id: u32,
    /// `close()` 的单向开关：置位后 `connect` 直接失败，不再启动 daemon。
    closed: bool,
}

/// `Connection` 与部分后台任务（exit watcher、ping、stdout 读任务）共享的内部状态；写任务与
/// stderr 任务不持有它。
struct Inner {
    /// 构造时固定；`ConnectionCommand::Dynamic` 的命令在每次启动时才求值。
    options: ConnectionOptions,
    /// 连接状态与启动/关闭的同步点。
    state: Mutex<State>,
}

/// 与一个 kiana-env daemon 的连接：首个请求时惰性启动，丢失后再次启动。连接丢失时在途请求以
/// code `unknown` 失败（对修改类操作，结果未知）。
///
/// 构造不启动进程；调用方用 `Arc<Connection>` 在多个任务/环境之间共享（例如
/// `RemoteExecutionEnvOptions::connection`）。`close()` 之后 `closed` 不再复位，需要新的
/// `Connection` 才能重新连接。
///
/// TS: packages/env/src/connection.ts#Connection
pub struct Connection {
    /// 所有状态都在这份共享内部结构里；`Connection` 本身只是一个句柄。
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl Connection {
    /// `new Connection(options)`：只保存选项，不启动进程、不做 IO。
    pub fn new(options: impl Into<ConnectionOptions>) -> Self {
        Connection {
            inner: Arc::new(Inner {
                options: options.into(),
                state: Mutex::new(State {
                    session: None,
                    ready: None,
                    starting: None,
                    sessions: 0,
                    next_id: 1,
                    closed: false,
                }),
            }),
        }
    }

    /// 需要时连接，返回 daemon 报告的机器信息。
    ///
    /// 首次调用会启动 daemon 并完成 `hello`；之后返回缓存 `Ready` 中 `RemoteInfo` 的克隆。
    pub async fn info(&self) -> Result<RemoteInfo, RemoteError> {
        Ok(self.connect().await?.info)
    }

    /// 需要时连接，返回存活 session 的 id。
    ///
    /// id 每次启动 daemon 递增（从 1 开始），可作为 `RequestOptions::session` 或 `kill` 的
    /// 第二个参数。
    pub async fn session(&self) -> Result<u64, RemoteError> {
        Ok(self.connect().await?.session.id)
    }

    /// 发一个请求。
    ///
    /// `json` 按值接收（移动到 `begin_send`），在那里插入或覆盖 `op`（已有的 `op` 键位置
    /// 保持不变），随后原样序列化进帧，不克隆。`options.payload` 原样作为帧尾发送。不设
    /// `options.session` 时先确保连接（必要时启动或重启 daemon）；设了则只在该 session 上
    /// 发送，不匹配或已死时立即返回 lost 错误。
    pub async fn request(
        &self,
        op: &str,
        json: Json,
        options: RequestOptions,
    ) -> Result<Reply, RemoteError> {
        if let Some(session_id) = options.session {
            // 绑定 session 的请求（daemon 的 handle）：不触发 connect。session 不匹配说明 daemon
            // 已经重启，handle 不再有意义，按连接丢失失败。
            let current = lock(&self.inner.state).session.clone();
            match current {
                Some(session) if session.id == session_id && session.is_live() => {
                    self.inner.send(session, op, json, options).await
                }
                _ => Err(RemoteError::lost("kiana-env connection lost")),
            }
        } else {
            // 普通请求：确保有存活的 daemon（必要时启动/重启）。
            let ready = self.connect().await?;
            self.inner.send(ready.session, op, json, options).await
        }
    }

    /// 停止 daemon；它会杀掉自己启动的所有东西。
    ///
    /// `closed` 是单向开关，重复调用没有额外效果；进行中的 `start` 会在安装 session 前复查
    /// 这个标记，看到 `close()` 就先杀掉刚 spawn 的子进程。`teardown` 只发信号、不等待进程
    /// 退出，所以 `close()` 返回时 daemon 可能还在退出过程中。
    pub fn close(&self) {
        let session = {
            let mut state = lock(&self.inner.state);
            state.closed = true;
            state.session.clone()
        };
        if let Some(session) = session {
            self.inner.teardown(&session, closed());
        }
    }

    /// 杀掉一个 `exec` 而不中止它。
    ///
    /// 只写一帧 `CANCEL { mode: "kill" }`；daemon 会杀掉进程组，但请求仍以被杀的退出状态
    /// 正常结算。`session` 不是当前存活的 session 时静默忽略（不报错、不写帧）。
    ///
    /// TS: packages/env/src/connection.ts#Connection.kill
    pub fn kill(&self, id: u32, session: u64) {
        let current = lock(&self.inner.state).session.clone();
        if let Some(current) = current
            && current.id == session
            && current.is_live()
        {
            // daemon 靠 `mode` 区分中止（`cancel`）与杀死（`kill`）：见 kiana-env 的
            // `control.cancel(json["mode"] == Some("kill"))`。
            let mut json = Json::new();
            json.insert("mode".to_owned(), Value::String("kill".to_owned()));
            self.inner
                .write_frame(&current, frame(CANCEL, id, json, &[]));
        }
    }

    /// 立即发出一个固定在 `session` 上的请求并返回只等待回复的 future。
    ///
    /// TS 的 `handle.request(...)` 在**调用时**就写出帧；Rust 的 `async fn` 要到第一次
    /// poll 才发送。`RemoteExecutionEnv` 的 `pread`/`writeChunk` 需要同时有多个请求在途，
    /// 且发送顺序与调用顺序一致（daemon 按到达顺序向文件追加 `writeChunk` 的 payload），
    /// 因此这里把「发送」留在调用时完成（与 [`Connection::request_detached`] 同理）。
    ///
    /// TS: packages/env/src/connection.ts#Connection.request
    pub(crate) fn request_now(
        &self,
        session_id: u64,
        op: &str,
        json: Json,
        options: RequestOptions,
    ) -> ReplyNow {
        let started = {
            let current = lock(&self.inner.state).session.clone();
            match current {
                Some(session) if session.id == session_id && session.is_live() => {
                    self.inner.begin_send(session, op, json, options)
                }
                _ => Err(RemoteError::lost("kiana-env connection lost")),
            }
        };
        match started {
            Ok((receiver, subscription)) => Box::pin(async move {
                // 订阅与 TS 的监听器一样，存活到请求结算。
                let _subscription = subscription;
                receiver
                    .await
                    .unwrap_or_else(|_| Err(RemoteError::lost("kiana-env connection lost")))
            }),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    /// 发一个不等结果的请求（terminal 的 `ptyWrite`/`ptyResize`）：帧同步写出，回复被丢弃。
    ///
    /// TS 的 `#request(...).catch(() => {})` 在 Promise 上完成；Rust 的 future 惰性执行，若直接
    /// 丢弃就根本不会写帧，因此这里在调用线程上同步写帧（保序），只把回复丢掉。
    pub(crate) fn request_detached(&self, op: &str, json: Json, options: RequestOptions) {
        let Some(session_id) = options.session else {
            return;
        };
        let session = lock(&self.inner.state).session.clone();
        let Some(session) = session else { return };
        if session.id != session_id || !session.is_live() {
            return;
        }
        self.inner.send_detached(session, op, json, options);
    }

    async fn connect(&self) -> Result<Ready, RemoteError> {
        let receiver = {
            let mut state = lock(&self.inner.state);
            // 快路径：已经握手成功，直接复制（Ready 只含 info 克隆与 Arc）。
            if state.closed {
                return Err(closed());
            }
            if let Some(ready) = &state.ready {
                return Ok(ready.clone());
            }
            // leader/follower：`starting` 非空说明已有任务在启动，只挂一个 oneshot 等结果；
            // 否则自己当 leader。spawn 之前必须先 drop 锁，不能在持锁时调度启动任务。
            let (sender, receiver) = oneshot::channel();
            match &mut state.starting {
                Some(waiters) => waiters.push(sender),
                None => {
                    state.starting = Some(vec![sender]);
                    drop(state);
                    let inner = Arc::clone(&self.inner);
                    tokio::spawn(async move {
                        // 再套一层 spawn：`start()` 里的 `expect`/用户闭包 panic 会让
                        // `JoinHandle` 返回 `Err`，而不是把 leader 任务直接炸掉——否则
                        // `finish_start` 永远不会执行，`starting` 一直非空，后续所有
                        // `connect()` 都排进一个永不完成的 oneshot。
                        let starter = Arc::clone(&inner);
                        let result = match tokio::spawn(async move { starter.start().await }).await
                        {
                            Ok(result) => result,
                            Err(error) if error.is_panic() => {
                                Err(RemoteError::lost("kiana-env start panicked"))
                            }
                            Err(_) => Err(RemoteError::lost("kiana-env start task was cancelled")),
                        };
                        inner.finish_start(result);
                    });
                }
            }
            receiver
        };
        // 发送端被丢弃（启动结果没有送达）时按连接丢失处理。
        receiver
            .await
            .unwrap_or_else(|_| Err(RemoteError::lost("kiana-env connection lost")))
    }
}

impl Inner {
    /// leader 的启动任务结束后调用一次：缓存成功结果并唤醒所有排队的 `connect`。
    fn finish_start(self: &Arc<Self>, result: Result<Ready, RemoteError>) {
        let waiters = {
            let mut state = lock(&self.state);
            // 取走整个队列，之后的 connect 会重新成为 leader。
            let waiters = state.starting.take().unwrap_or_default();
            // 失败不缓存（下一次 connect 重试）；成功但 session 已经死了也不缓存——否则
            // connect 会把一个已死的 daemon 当成可用。
            if let Ok(ready) = &result
                && ready.session.is_live()
            {
                state.ready = Some(ready.clone());
            }
            waiters
        };
        // 在锁外唤醒：等待者恢复后可以立即再取 state 锁，不必等这把锁释放。
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }

    async fn send(
        self: &Arc<Self>,
        session: Arc<Session>,
        op: &str,
        json: Json,
        options: RequestOptions,
    ) -> Result<Reply, RemoteError> {
        let (receiver, _subscription) = self.begin_send(session, op, json, options)?;
        receiver
            .await
            .unwrap_or_else(|_| Err(RemoteError::lost("kiana-env connection lost")))
    }

    fn send_detached(
        self: &Arc<Self>,
        session: Arc<Session>,
        op: &str,
        json: Json,
        options: RequestOptions,
    ) {
        // The reply is dropped: the daemon's RESULT/ERROR has no waiter, and `dispatch` removes the
        // pending entry all the same.
        let _ = self.begin_send(session, op, json, options);
    }

    /// 分配 id、登记 pending、注册 abort 监听，并把 REQUEST 帧入队；返回等待回复的接收端
    /// （以及需要活到请求结算的取消订阅）。
    fn begin_send(
        self: &Arc<Self>,
        session: Arc<Session>,
        op: &str,
        json: Json,
        options: RequestOptions,
    ) -> Result<SendStart, RemoteError> {
        // 第一道存活检查：已拆掉的 session 不再分配 id、也不再登记 pending。
        if !session.is_live() {
            return Err(RemoteError::lost("kiana-env connection lost"));
        }
        // 请求 id 从共享计数器取；帧格式里的 id 是 u32，所以这里显式回绕。
        let id = {
            let mut state = lock(&self.state);
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            id
        };
        // `op` 最后插入（TS 的 `{ ...json, op }`）；preserve_order 的 Map 对已存在的键原地
        // 覆盖值、保留位置，与 JS 对象展开的键序一致。
        let mut json = json;
        json.insert("op".to_owned(), Value::String(op.to_owned()));

        #[cfg(test)]
        {
            // 测试钩子：让 close() 的 teardown 正好落在存活检查与 pending 登记之间。
            let hook = {
                let mut slot = lock(&test_hooks::BEFORE_PENDING);
                if slot.as_ref().is_some_and(|(name, _)| name == op) {
                    slot.take().map(|(_, hook)| hook)
                } else {
                    None
                }
            };
            if let Some(hook) = hook {
                hook();
            }
        }

        let (sender, receiver) = oneshot::channel();
        {
            // live 检查与 pending 注册放在同一把锁里：TS 的 `#send` 与 `#teardown` 在同一个
            // 同步回合，teardown 不会插在“检查存活”和“登记 pending”之间；Rust 里如果插进去，
            // teardown 会先 drain（此时还是空的），这条请求就永远等不到结果。
            let mut pending = lock(&session.pending);
            if !session.is_live() {
                return Err(RemoteError::lost("kiana-env connection lost"));
            }
            pending.insert(
                id,
                Pending {
                    result: sender,
                    on_event: options.on_event.clone(),
                },
            );
        }
        // 在写 REQUEST 之前注册 abort 监听：abort 与发送并发时不会漏掉。若 abort 恰好落在
        // 注册与写帧之间，回调会先入队 CANCEL；下面写帧后的 `is_aborted` 复查再补一个，
        // 因为 kiana-chord 的 `on_abort` 不会给已经中止的信号补调回调。
        let subscription = options.signal.as_ref().map(|signal| {
            let session = Arc::clone(&session);
            let inner = Arc::clone(self);
            signal.on_abort(move |_reason| {
                inner.write_frame(&session, frame(CANCEL, id, Json::new(), &[]));
            })
        });
        // 先回调 on_start（调用方拿到 id/session），再把帧入队：调用方要能在请求发出前用 id/session 取消。
        if let Some(on_start) = &options.on_start {
            on_start(id, session.id);
        }
        // 帧由调用线程入队（不 await）：`request_now` / `request_detached` 依赖“调用即发送”，
        // 写出顺序由无界通道的 FIFO 保证。
        let payload = options.payload.unwrap_or_default();
        self.write_frame(&session, frame(REQUEST, id, json, &payload));
        // 信号可能在本请求入队前就已经中止（`on_abort` 不会补调），这里补发 CANCEL。
        if options.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            self.write_frame(&session, frame(CANCEL, id, Json::new(), &[]));
        }
        Ok((receiver, subscription))
    }

    /// 把一帧放进写任务的队列。已拆掉的 session 与已关闭的 stdin 都静默丢弃（与 TS 的
    /// `#write` 只在 `session.live` 时写一致）；无界通道不会阻塞，也不在这里做 IO。
    fn write_frame(&self, session: &Session, bytes: Vec<u8>) {
        if !session.is_live() {
            return;
        }
        if let Some(stdin) = lock(&session.stdin).as_ref() {
            let _ = stdin.send(bytes);
        }
    }

    /// 启动一个 daemon 并完成 `hello` 握手；由 `connect` 的 leader 任务调用。
    async fn start(self: &Arc<Self>) -> Result<Ready, RemoteError> {
        // 阶段 1：确定启动命令。Dynamic 可能在远端探测/部署 daemon，失败按 TS 的
        // `spawn_error`（lost: true）上报，下一次 connect 会重试。
        let mut command = match &self.options.command {
            ConnectionCommand::Static(command) => command.clone(),
            ConnectionCommand::Dynamic(command) => match command().await {
                Ok(command) => command,
                Err(error) => {
                    return Err(RemoteError::spawn_error(error.to_string(), true));
                }
            },
        };
        // 求值期间可能已经 close()；空命令是配置错误，TS 的 `No daemon command` 不带 lost。
        if lock(&self.state).closed {
            return Err(closed());
        }
        if command.is_empty() {
            return Err(RemoteError::spawn_error("No daemon command", false));
        }
        let program = command.remove(0);
        let token = random_token();
        // 阶段 2：spawn。命令 = 调用方数组 + `serve --token <hex>`；三条标准流都由客户端接管，
        // `kill_on_drop` 让 Child 被丢弃时杀掉子进程（正常路径由 exit watcher 回收）。
        let mut process = Command::new(&program);
        process
            .args(&command)
            .arg("serve")
            .arg("--token")
            .arg(&token)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // spawn 同步失败：消息按 Node 重建。只有 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 在 Node
        // 里是异步 error 事件（带 lost），其余 errno 是同步抛出的 ErrnoException（不带 lost）。
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                let message = crate::spawn_error_message(&program, &error);
                return Err(if crate::spawn_error_is_asynchronous(&error) {
                    RemoteError::spawn_error(message, true)
                } else {
                    // Node：`spawn()` 同步抛出的 ErrnoException 不是 RemoteError，也没有 `lost`。
                    RemoteError::thrown(message)
                });
            }
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let log = self.options.on_log.clone();

        let (stdin_sender, mut stdin_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
        let (kill_sender, mut kill_receiver) = mpsc::unbounded_channel::<()>();
        let (exit_sender, exit_receiver) = oneshot::channel::<Option<i32>>();

        #[cfg(test)]
        {
            // 测试钩子：让 close() 正好落在 closed 检查与 session 安装之间。
            let hook = {
                let mut slot = lock(&test_hooks::BEFORE_SESSION_INSTALL);
                if slot.as_ref().is_some_and(|(name, _)| name == &program) {
                    slot.take().map(|(_, hook)| hook)
                } else {
                    None
                }
            };
            if let Some(hook) = hook {
                hook();
            }
        }

        // `close()` 与这里用同一把 state 锁：TS 的 `#start` 在同一个同步回合里完成 `spawn()`、
        // `this.#session = session`，所以 `close()` 要么在 closed 检查之前（看到 closed），要么
        // 在安装之后（teardown 杀子进程）。Rust 的 `start` 是 async，另一线程的 `close()` 可能
        // 正好落在两者之间；在锁内复查，抢先的 `close()` 让这次启动失败、子进程被杀掉。
        let session = {
            let mut state = lock(&self.state);
            if state.closed {
                drop(state);
                // 这个子进程还没有拥有者：`close()` 已经返回，显式 SIGTERM 并回收。
                crate::kill_child(&mut child);
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
                return Err(closed());
            }
            state.sessions += 1;
            let session = Arc::new(Session {
                id: state.sessions,
                token,
                live: AtomicBool::new(true),
                last_seen: AtomicI64::new(now_ms()),
                pending: Mutex::new(HashMap::new()),
                stdin: Mutex::new(Some(stdin_sender)),
                kill: kill_sender,
                ping: Mutex::new(None),
                exit_code: Mutex::new(None),
                teardown_error: Mutex::new(None),
            });
            state.session = Some(Arc::clone(&session));
            session
        };

        // 阶段 3：后台任务。写任务独占 stdin；stderr 任务只做诊断输出；exit watcher 独占 child
        // 的回收；stdout 任务解析帧。exit watcher、ping 与 stdout 任务持有 session 的 Arc；
        // ping/stdout 检查 live 后退出，写任务在发送端被 drop 后结束，stderr 任务读到 EOF/读
        // 错误后结束。
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(bytes) = stdin_receiver.recv().await {
                if stdin.write_all(&bytes).await.is_err() {
                    // `child.stdin.on("error", () => {})`
                    break;
                }
            }
            // Dropping `stdin` ends it, like `child.stdin.end()`.
        });
        tokio::spawn(async move {
            let mut chunk = vec![0u8; 16 * 1024];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if let Some(log) = &log {
                            log(&crate::decode_utf8_lossy(&chunk[..read]));
                        }
                    }
                }
            }
        });

        {
            // `child.on("exit")`：先记录退出码，再 teardown（在途的 hello 以通用 lost 拒绝），
            // 最后通知 `start` 的 select。TS 的 `failed`（exit 事件）先于通用的 teardown 结算，
            // 所以 `start_failure` 用这里记下的退出码还原 `kiana-env exited with code …`。
            //
            // 退出检测用 `try_wait` 轮询（20 ms）而不是 `wait()`：`child` 必须由本任务独占，
            // 杀进程的请求只能通过 kill 通道传进来；两次轮询之间 select 等待该通道或定时器。
            let inner = Arc::clone(self);
            let task_session = Arc::clone(&session);
            let mut child = child;
            tokio::spawn(async move {
                let status = loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) => {}
                        Err(_) => break None,
                    }
                    tokio::select! {
                        received = kill_receiver.recv() => {
                            if received.is_none() {
                                break None;
                            }
                            // `session.child.kill()` → SIGTERM（见 [`crate::kill_child`]）。
                            crate::kill_child(&mut child);
                        }
                        () = tokio::time::sleep(Duration::from_millis(20)) => {}
                    }
                };
                let code = status.and_then(|status| status.code());
                *lock(&task_session.exit_code) = Some(code);
                inner.teardown(
                    &task_session,
                    RemoteError::lost("kiana-env connection lost"),
                );
                let _ = exit_sender.send(code);
            });
        }

        {
            // ping 任务：`last_seen` 由 stdout 读任务刷新；静默超过 30 s 就按连接超时 teardown。
            // `MissedTickBehavior::Delay` 让运行时阻塞后错过的 tick 不补发，避免一次醒来连发
            // 多个 PING。
            let inner = Arc::clone(self);
            let task_session = Arc::clone(&session);
            let handle = tokio::spawn(async move {
                // `setInterval(…, PING_INTERVAL)` 的第一次触发在一个间隔之后，而
                // `tokio::time::interval` 的第一次 tick 立即完成；从 +5 s 开始，避免在 hello
                // 之后立刻多发一个 PING。
                let mut interval = tokio::time::interval_at(
                    tokio::time::Instant::now() + PING_INTERVAL,
                    PING_INTERVAL,
                );
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    if !task_session.is_live() {
                        return;
                    }
                    inner.write_frame(&task_session, frame(PING, 0, Json::new(), &[]));
                    if now_ms() - task_session.last_seen.load(Ordering::SeqCst) > SILENCE_LIMIT_MS {
                        inner.teardown(
                            &task_session,
                            RemoteError::lost("kiana-env connection timed out"),
                        );
                        return;
                    }
                }
            });
            *lock(&session.ping) = Some(handle);
        }
        {
            // stdout 读任务：握手行同步与帧解析。它因 EOF/读错误返回时**不**主动 teardown；
            // 若子进程还活着，由 ping 任务的静默超时（或 exit watcher）收尾。
            let inner = Arc::clone(self);
            let task_session = Arc::clone(&session);
            tokio::spawn(async move {
                inner.read_stdout(task_session, stdout).await;
            });
        }

        // 阶段 4：hello。它是带 id 的普通请求，因此同样由 pending 与 teardown 结算；下面的
        // select 只是额外加上“子进程退出”和 60 s 总超时两个竞争者。
        let mut hello = Box::pin(self.send(
            Arc::clone(&session),
            "hello",
            hello_body(),
            RequestOptions::default(),
        ));
        let result = tokio::select! {
            biased;
            // `hello` 已经结算时优先（TS 的 Promise.race 里先结算的一方；`close()`、坏帧的
            // teardown 都是同步拒绝 hello）。纯粹的 exit 由 [`start_failure`] 还原。
            reply = &mut hello => reply,
            code = exit_receiver => {
                // `Err`（watcher 没有送出结果）与 `Ok(None)`（被信号杀死）都写成 `null`，
                // 与 Node 里 `code === null` 的拼接结果一致。
                Err(RemoteError::lost(format!(
                    "kiana-env exited with code {} before it was ready",
                    exit_code_text(code.ok().flatten())
                )))
            }
            () = tokio::time::sleep(START_TIMEOUT) => {
                Err(RemoteError::lost("kiana-env did not answer within 60 s"))
            }
        };
        let json = match result {
            Ok(reply) => reply.json,
            Err(error) => {
                let error = start_failure(&session, error);
                self.teardown(&session, error.clone());
                return Err(error);
            }
        };
        // 只接受 protocol == 1.0（JSON 的 1 与 1.0 是同一个 Number）；缺失时消息里是
        // JS 的 `String(undefined)`，即 `Unsupported protocol undefined`。
        let protocol = json.get("protocol");
        let matches_protocol =
            matches!(protocol, Some(Value::Number(number)) if number.as_f64() == Some(1.0));
        if !matches_protocol {
            let error = RemoteError::lost(format!(
                "Unsupported protocol {}",
                crate::js_string_of(protocol)
            ));
            self.teardown(&session, error.clone());
            return Err(error);
        }
        // `hello` 的其余字段解析失败也是一次失败的启动：先拆掉 session（TS 的 try/catch
        // 覆盖整个 hello 阶段），否则子进程、读任务和写任务都会活在已无人使用的 session 上。
        let info: RemoteInfo = match serde_json::from_value(Value::Object(json)) {
            Ok(info) => info,
            Err(error) => {
                let error = RemoteError::lost(error.to_string());
                self.teardown(&session, error.clone());
                return Err(error);
            }
        };
        Ok(Ready { info, session })
    }

    /// 拆掉一个 session：幂等、记录首个原因、清空 `State`、杀子进程，并让全部在途请求以
    /// 同一个 `error` 失败。
    fn teardown(self: &Arc<Self>, session: &Arc<Session>, error: RemoteError) {
        // 只有第一个调用者能把 live 从 true 翻成 false；之后的调用是 no-op，也不会覆盖
        // `teardown_error` 里记录的首次原因（`start_failure` 依赖它）。
        if !session.live.swap(false, Ordering::SeqCst) {
            return;
        }
        *lock(&session.teardown_error) = Some(error.clone());
        if let Some(handle) = lock(&session.ping).take() {
            handle.abort();
        }
        {
            // 只有当前 session 才清空 State：迟到的旧 teardown 不能抹掉已经安装的新 session。
            let mut state = lock(&self.state);
            if state
                .session
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                state.session = None;
                state.ready = None;
            }
        }
        // 通过通道让 exit watcher 发 SIGTERM（它独占 child）；无界通道不阻塞。teardown 只发
        // 信号、不等待子进程退出。
        let _ = session.kill.send(());
        if let Some(stdin) = lock(&session.stdin).take() {
            drop(stdin);
        }
        // 先摘掉当前 session（上面）再拒绝旧请求：下一次 connect 可以立刻启动新 daemon，
        // 旧请求只持有自己的 session Arc。drain 在锁内、发送在锁外。
        let pending: Vec<Pending> = {
            let mut pending = lock(&session.pending);
            pending.drain().map(|(_, request)| request).collect()
        };
        for request in pending {
            let _ = request.result.send(Err(error.clone()));
        }
    }

    /// stdout 读任务：先用握手行同步，再按 `u32 length + 9 字节头 + JSON + payload` 解析帧并
    /// 派发。任何坏帧都拆掉整个连接——与 TS 一样不做重同步，因为字节流已经失步。
    async fn read_stdout(
        self: &Arc<Self>,
        session: Arc<Session>,
        mut stdout: tokio::process::ChildStdout,
    ) {
        let mut buffer: Vec<u8> = Vec::new();
        let mut synced = false;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let read = match stdout.read(&mut chunk).await {
                Ok(0) => return,
                Ok(read) => read,
                Err(_) => return,
            };
            if !session.is_live() {
                return;
            }
            session.touch();
            buffer.extend_from_slice(&chunk[..read]);
            if !synced {
                // 阶段 1：握手行同步。Shell 启动文件可能在 daemon 运行前打印；跳过同步行之前
                // 的所有内容。
                let marker = format!("KIANA-ENV {}\n", session.token).into_bytes();
                match find_subslice(&buffer, &marker) {
                    None => {
                        // 噪音尚未结束：限制缓冲增长，但保留末尾 marker.len() 字节，使被分片
                        // 截断的握手行还能在下一次读取时拼出来。
                        if buffer.len() > NOISE_LIMIT {
                            let keep = marker.len();
                            let excess = buffer.len() - keep;
                            buffer.drain(..excess);
                        }
                        continue;
                    }
                    Some(index) => {
                        let noise = crate::decode_utf8_lossy(&buffer[..index]);
                        // JS 的 `String.prototype.trim()`（`\s` 集含 U+FEFF、不含 U+0085）。
                        let noise = noise.trim_matches(kiana_chord::contract::is_js_whitespace);
                        if !noise.is_empty()
                            && let Some(log) = &self.options.on_log
                        {
                            log(noise);
                        }
                        buffer.drain(..index + marker.len());
                        synced = true;
                    }
                }
            }
            // 阶段 2：帧循环。`length` 是长度前缀之后的所有字节（9 字节头 + JSON + payload）；
            // 不足一帧就 break，等下一次 read 补齐。
            while session.is_live() && buffer.len() >= 4 {
                let length =
                    u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
                // 帧长不合法说明流已经失步：拆连接，不尝试重新同步。
                if !(9..=MAX_FRAME).contains(&length) {
                    self.teardown(&session, RemoteError::lost("Corrupt frame from kiana-env"));
                    return;
                }
                if buffer.len() < 4 + length {
                    break;
                }
                let body = &buffer[4..4 + length];
                let kind = body[0];
                let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
                let json_length = u32::from_be_bytes([body[5], body[6], body[7], body[8]]) as usize;
                // json_length 必须落在帧体内；解析结果还必须是 JSON 对象（数组、`null`、
                // 空 JSON 都算坏帧）。daemon 的帧读取端对解析失败更宽松；客户端把损坏回复当作连接丢失，避免把垃圾数据当成成功结果。
                let parsed = if 9 + json_length > length {
                    None
                } else {
                    serde_json::from_slice::<Value>(&body[9..9 + json_length]).ok()
                };
                let json = match parsed {
                    Some(Value::Object(json)) => json,
                    _ => {
                        self.teardown(&session, RemoteError::lost("Corrupt frame from kiana-env"));
                        return;
                    }
                };
                // payload 复制成 Vec：buffer 随后会被 drain，不能留下借用。
                let payload = body[9 + json_length..].to_vec();
                buffer.drain(..4 + length);
                self.dispatch(&session, kind, id, json, payload);
            }
        }
    }

    /// 把一帧交给对应的等待者：EVENT 调 `on_event`，RESULT/ERROR 结束请求；其余类型忽略。
    fn dispatch(&self, session: &Arc<Session>, kind: u8, id: u32, json: Json, payload: Vec<u8>) {
        if kind == EVENT {
            // 只投递给仍在途的请求；请求结算后 pending 条目已被移除，迟到的事件被丢弃。
            // 回调在 `pending` 锁释放后执行（guard 只活到这条 let 语句结束），因此回调里
            // 可以安全地再次发起请求。
            let callback = lock(&session.pending)
                .get(&id)
                .and_then(|request| request.on_event.clone());
            if let Some(callback) = callback {
                callback(json, payload);
            }
            return;
        }
        if kind != RESULT && kind != ERROR {
            // PING、CANCEL 以及未知类型都不需要处理。
            return;
        }
        // RESULT/ERROR 都结束请求：先移除再结算，同一个 id 的重复帧不会二次投递。
        let pending = lock(&session.pending).remove(&id);
        if let Some(pending) = pending {
            // ERROR 帧的 JSON 直接就是 RemoteError 的字段（code/message/path/…）。
            let result = if kind == RESULT {
                Ok(Reply {
                    json,
                    payload,
                    session: session.id,
                })
            } else {
                Err(RemoteError::new(json))
            };
            // 接收端可能已被丢弃（例如 `request_now` 返回的 future 被 drop），失败被忽略。
            let _ = pending.result.send(result);
        }
    }
}

/// `hello` 失败时 TS 报的错。
///
/// `#start` 的 `Promise.race` 里 `failed`（60 s 超时、spawn `error`、`exit`）与 hello 竞争：
/// - `close()` 与坏帧的 teardown 同步拒绝 hello，没有竞争者，原样上报；
/// - 子进程退出时，`failed` 的 exit 监听器先注册、先结算，报 `kiana-env exited with code …`，而
///   hello 被随后的 teardown 以通用 `lost("kiana-env connection lost")` 拒绝。
///
/// Rust 侧 hello 可能先被 teardown 拒绝（通用 lost）；用 session 上记下的 teardown 原因与退出码
/// 还原 TS 的消息。
fn start_failure(session: &Session, error: RemoteError) -> RemoteError {
    // 只有“通用 lost”需要还原；协议不支持、spawn 失败、`close()` 的错误都原样返回。
    let generic = error.code == "unknown"
        && error.message() == "kiana-env connection lost"
        && is_connection_lost(&error);
    if !generic {
        return error;
    }
    match lock(&session.teardown_error).clone() {
        // exit watcher 先写 exit_code 再 teardown，所以这里能读到退出码；`Some(None)` 表示
        // 被信号杀死，还原出的消息里是 `null`。
        Some(reason)
            if is_connection_lost(&reason) && reason.message() == "kiana-env connection lost" =>
        {
            match *lock(&session.exit_code) {
                Some(code) => RemoteError::lost(format!(
                    "kiana-env exited with code {} before it was ready",
                    exit_code_text(code)
                )),
                None => error,
            }
        }
        // `close()` 的 teardown：hello 的拒绝就是 "Connection closed"。也覆盖 send 因 session
        // 已死而提前返回通用 lost 的窄窗口。
        Some(reason) if reason.message() == "Connection closed" => reason,
        _ => error,
    }
}

/// `hello` 的请求体：固定 `{ protocol: 1 }`；daemon 在回复里报告它自己的协议版本。
fn hello_body() -> Json {
    let mut json = Json::new();
    json.insert("protocol".to_owned(), Value::from(1));
    json
}

/// Node 在进程被信号杀死时给出 `code === null`，这里同样写出 `null`。
fn exit_code_text(code: Option<i32>) -> String {
    match code {
        Some(code) => code.to_string(),
        None => "null".to_owned(),
    }
}

/// 在 `haystack` 里找 `needle` 的第一个字节位置；只在握手行同步时使用。
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `randomBytes(16).toString("hex")`：32 个小写十六进制字符。
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system provides random bytes");
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_the_documented_header() {
        let mut json = Json::new();
        json.insert("op".to_owned(), Value::String("hello".to_owned()));
        let bytes = frame(REQUEST, 7, json, &[1, 2, 3]);
        let body = b"{\"op\":\"hello\"}";
        assert_eq!(
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize,
            9 + body.len() + 3
        );
        assert_eq!(bytes[4], REQUEST);
        assert_eq!(
            u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
            7
        );
        assert_eq!(
            u32::from_be_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]) as usize,
            body.len()
        );
        assert_eq!(&bytes[13..13 + body.len()], body);
        assert_eq!(&bytes[13 + body.len()..], &[1, 2, 3]);
    }

    #[test]
    fn an_empty_json_object_is_two_braces() {
        let bytes = frame(PING, 0, Json::new(), &[]);
        assert_eq!(&bytes[13..], b"{}");
        assert_eq!(
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            11
        );
    }

    #[test]
    fn node_platform_translates_rust_names() {
        let info = RemoteInfo {
            os: "windows".to_owned(),
            arch: "aarch64".to_owned(),
            ..RemoteInfo::default()
        };
        assert_eq!(
            node_platform(&info),
            NodePlatform {
                platform: "win32".to_owned(),
                arch: "arm64".to_owned()
            }
        );
        let info = RemoteInfo {
            os: "macos".to_owned(),
            arch: "x86_64".to_owned(),
            ..RemoteInfo::default()
        };
        assert_eq!(
            node_platform(&info),
            NodePlatform {
                platform: "darwin".to_owned(),
                arch: "x64".to_owned()
            }
        );
        let info = RemoteInfo {
            os: "linux".to_owned(),
            arch: "x86".to_owned(),
            ..RemoteInfo::default()
        };
        assert_eq!(
            node_platform(&info),
            NodePlatform {
                platform: "linux".to_owned(),
                arch: "ia32".to_owned()
            }
        );
        let info = RemoteInfo {
            os: "freebsd".to_owned(),
            arch: "riscv64".to_owned(),
            ..RemoteInfo::default()
        };
        assert_eq!(
            node_platform(&info),
            NodePlatform {
                platform: "freebsd".to_owned(),
                arch: "riscv64".to_owned()
            }
        );
    }

    #[test]
    fn remote_errors_default_like_the_typescript_class() {
        let error = RemoteError::new(Json::new());
        assert_eq!(error.code, "unknown");
        assert_eq!(error.message(), "Remote operation failed");
        assert!(error.path.is_none());
        assert!(!is_connection_lost(&error));

        let mut fields = Json::new();
        fields.insert("code".to_owned(), Value::String("ENOENT".to_owned()));
        fields.insert("message".to_owned(), Value::String("gone".to_owned()));
        fields.insert("path".to_owned(), Value::String("/x".to_owned()));
        let error = RemoteError::new(fields);
        assert_eq!(error.code, "ENOENT");
        assert_eq!(error.message(), "gone");
        assert_eq!(error.path.as_deref(), Some("/x"));

        let lost_error = RemoteError::lost("kiana-env connection lost");
        assert!(is_connection_lost(&lost_error));
        assert_eq!(lost_error.message(), "kiana-env connection lost");
    }

    /// 回归（review finding）：缺失的 daemon 程序必须报 Node 的 `spawn <program> ENOENT`
    /// （`child.once("error", error => new RemoteError({ code: "spawn_error", message:
    /// error.message, lost: true }))`），而不是 Rust 的 `io::Error` Display。
    #[tokio::test]
    async fn a_missing_daemon_reports_the_node_spawn_message() {
        let connection =
            Connection::new(vec!["/definitely-not-a-real-kiana-env-command".to_owned()]);
        let error = connection
            .info()
            .await
            .expect_err("the program does not exist");
        assert_eq!(error.code, "spawn_error");
        assert_eq!(
            error.message(),
            "spawn /definitely-not-a-real-kiana-env-command ENOENT"
        );
        assert!(is_connection_lost(&error));
    }

    #[test]
    fn find_subslice_locates_the_marker() {
        assert_eq!(
            find_subslice(b"xxKIANA-ENV ab\nrest", b"KIANA-ENV ab\n"),
            Some(2)
        );
        assert_eq!(find_subslice(b"nope", b"KIANA-ENV ab\n"), None);
        assert_eq!(find_subslice(b"", b"KIANA-ENV ab\n"), None);
    }

    #[test]
    fn tokens_are_thirty_two_lowercase_hex_characters() {
        let token = random_token();
        assert_eq!(token.len(), 32);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(token, random_token());
    }

    /// 回归（review finding）：`spawn()` 同步抛出的 errno（ENAMETOOLONG 等）在 TS 里不是
    /// `RemoteError`（没有 `lost`），`exec` 的 code 映射是 `unknown`，不是 `spawn_error`。
    #[cfg(unix)]
    #[tokio::test]
    async fn a_synchronous_spawn_error_is_not_a_lost_connection() {
        let connection = Connection::new(vec![format!("/tmp/{}", "x".repeat(300))]);
        let error = connection
            .info()
            .await
            .expect_err("spawn fails synchronously");
        assert_eq!(error.message(), "spawn ENAMETOOLONG");
        assert_eq!(error.code, "unknown");
        assert!(!is_connection_lost(&error));
    }

    /// E2BIG（参数超过 `MAX_ARG_STRLEN`，Linux 的限制）也是同步抛出。
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_e2big_spawn_error_is_not_a_lost_connection() {
        let connection = Connection::new(vec!["/bin/echo".to_owned(), "x".repeat(200_000)]);
        let error = connection.info().await.expect_err("spawn fails with E2BIG");
        assert_eq!(error.message(), "spawn E2BIG");
        assert_eq!(error.code, "unknown");
        assert!(!is_connection_lost(&error));
    }

    // -----------------------------------------------------------------------
    // 回归（review findings）：close/hello/exit 的竞态
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    fn wait_for_pid(file: &std::path::Path) -> i32 {
        for _ in 0..500 {
            if let Ok(text) = std::fs::read_to_string(file)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the fake daemon did not write its pid");
    }

    #[cfg(unix)]
    fn wait_for_process_exit(pid: i32) {
        for _ in 0..500 {
            // SAFETY: `kill(pid, 0)` 只检查进程是否存在，不发送信号。
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the daemon process {pid} is still alive");
    }

    /// 回归（review finding）：`close()` 落在 `#start` 的 closed 检查与 session 安装之间时，
    /// 启动必须失败（"Connection closed"），子进程必须被杀掉——不能出现请求在 close() 之后仍然
    /// 成功、daemon 继续运行的情况。TS 里这两步在同一个同步回合，不可能被打断。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_between_the_check_and_the_session_install_kills_the_child() {
        use std::sync::mpsc;

        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_file = directory.path().join("pid");
        // 用符号链接而不是刚写好的脚本：并发测试的 fork 可能短暂继承写 fd，让 exec 报
        // ETXTBSY（Node 与 Rust 的同步 spawn 失败），与本用例无关。
        let program = directory.path().join("daemon-sh");
        std::os::unix::fs::symlink("/bin/sh", &program).expect("link the shell");
        let program = program.to_string_lossy().into_owned();

        let connection = Arc::new(Connection::new(vec![
            program.clone(),
            "-c".to_owned(),
            format!("echo $$ > '{}'; exec sleep 60", pid_file.display()),
            "sh".to_owned(),
        ]));
        let (reached_sender, reached) = mpsc::channel::<()>();
        let (release_sender, release) = mpsc::channel::<()>();
        *lock(&test_hooks::BEFORE_SESSION_INSTALL) = Some((
            program,
            Box::new(move || {
                let _ = reached_sender.send(());
                let _ = release.recv();
            }),
        ));

        let request_connection = Arc::clone(&connection);
        let request = tokio::spawn(async move {
            request_connection
                .request("ping", Json::new(), RequestOptions::default())
                .await
        });

        reached
            .recv_timeout(Duration::from_secs(10))
            .expect("start reaches the window between the check and the install");
        // 子进程已经 spawn（钩子在 spawn 之后）；等它写下 pid 再 close。
        let pid = wait_for_pid(&pid_file);
        connection.close();
        release_sender.send(()).expect("release the start");

        let result = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .expect("the start fails promptly after close()")
            .expect("the request task does not panic");
        let error = result.expect_err("a request must not succeed after close()");
        assert_eq!(error.message(), "Connection closed");
        assert!(!is_connection_lost(&error));

        // close() 时 session 还没有安装；由 start 在锁内的复查负责杀掉子进程。
        wait_for_process_exit(pid);
    }

    /// 回归（review finding）：hello 还在等待时 close()，启动报 "Connection closed"
    /// （TS 的 close 同步 teardown 拒绝了 hello），不是退出码消息。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_during_hello_reports_connection_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_file = directory.path().join("pid");
        let program = directory.path().join("daemon-sh");
        std::os::unix::fs::symlink("/bin/sh", &program).expect("link the shell");
        let program = program.to_string_lossy().into_owned();
        let connection = Arc::new(Connection::new(vec![
            program,
            "-c".to_owned(),
            format!("echo $$ > '{}'; exec sleep 60", pid_file.display()),
            "sh".to_owned(),
        ]));
        let request_connection = Arc::clone(&connection);
        let request = tokio::spawn(async move {
            request_connection
                .request("ping", Json::new(), RequestOptions::default())
                .await
        });
        // daemon 启动后永不回 hello；pid 文件说明 hello 已经发出。
        let pid = wait_for_pid(&pid_file);
        connection.close();
        let error = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .expect("the start fails promptly after close()")
            .expect("the request task does not panic")
            .expect_err("no request succeeds after close()");
        assert_eq!(error.message(), "Connection closed");
        assert!(!is_connection_lost(&error));
        wait_for_process_exit(pid);
    }

    /// 回归（review finding）：子进程在 hello 之前退出时报 TS 的 `failed` 消息（退出码），
    /// 不是通用的 `kiana-env connection lost`。
    #[cfg(unix)]
    #[tokio::test]
    async fn an_exit_before_hello_reports_the_exit_code() {
        let connection =
            Connection::new(vec!["sh".to_owned(), "-c".to_owned(), "exit 3".to_owned()]);
        let error = connection
            .info()
            .await
            .expect_err("the child exits before hello");
        assert_eq!(
            error.message(),
            "kiana-env exited with code 3 before it was ready"
        );
        assert!(is_connection_lost(&error));
    }

    /// 被信号杀死时 Node 的 `code` 是 `null`。
    #[cfg(unix)]
    #[tokio::test]
    async fn an_exit_by_signal_before_hello_reports_a_null_code() {
        let connection = Connection::new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "kill -9 $$".to_owned(),
        ]);
        let error = connection.info().await.expect_err("the child is killed");
        assert_eq!(
            error.message(),
            "kiana-env exited with code null before it was ready"
        );
        assert!(is_connection_lost(&error));
    }

    #[cfg(unix)]
    const UNIT_DAEMON_ENV: &str = "KIANA_ENV_UNIT_DAEMON_MODE";
    #[cfg(unix)]
    const UNIT_DAEMON_TOKEN_ENV: &str = "KIANA_ENV_UNIT_DAEMON_TOKEN";

    /// 重新执行本测试二进制的“daemon”：写同步行、回答 hello，然后退出或保持空闲。
    ///
    /// 未设置标记变量时立即返回（普通 `cargo test` 也会运行这个用例）。
    #[cfg(unix)]
    #[test]
    fn unit_daemon_child() {
        let Ok(mode) = std::env::var(UNIT_DAEMON_ENV) else {
            return;
        };
        if mode != "hello_then_exit" && mode != "hello_then_idle" {
            return;
        }
        use std::io::{Read, Write};
        let token = std::env::var(UNIT_DAEMON_TOKEN_ENV).unwrap_or_default();
        let stdout = std::io::stdout();
        {
            let mut stdout = stdout.lock();
            let _ = writeln!(stdout, "KIANA-ENV {token}");
            let _ = stdout.flush();
        }
        let mut stdin = std::io::stdin();
        let mut header = [0u8; 4];
        if stdin.read_exact(&mut header).is_err() {
            std::process::exit(1);
        }
        let length = u32::from_be_bytes(header) as usize;
        let mut body = vec![0u8; length];
        if stdin.read_exact(&mut body).is_err() {
            std::process::exit(1);
        }
        let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
        let hello = serde_json::json!({
            "protocol": 1,
            "version": "1.1.0",
            "os": "linux",
            "arch": "x86_64",
            "home": "/home/u",
            "separator": "/",
            "tmpdir": "/tmp",
            "cwd": "/work",
            "driveCwds": {},
            "pid": std::process::id(),
        });
        let json = hello.as_object().cloned().expect("hello is an object");
        let reply = frame(RESULT, id, json, &[]);
        let _ = stdout.lock().write_all(&reply);
        let _ = stdout.lock().flush();
        if mode == "hello_then_idle" {
            // 忽略 SIGTERM 并保持 stdout 打开：read_stdout 任务会一直持有 session（本用例要
            // 覆盖 close() 与 pending 登记的竞态）；测试末尾用 SIGKILL 清理。
            // SAFETY: 只是给本进程设置信号处理。
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
            std::thread::sleep(Duration::from_secs(60));
        }
        std::process::exit(0);
    }

    /// 回归（同类并发问题）：`close()` 的 teardown 不能插在 `#send` 的存活检查与 pending 登记
    /// 之间——TS 里两者在同一个同步回合，插进去的 teardown 会先 drain 空的 pending，请求就
    /// 永远等不到结果（挂起）。修复后请求以 `lost` 立刻结算。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_between_the_liveness_check_and_the_pending_install_settles_the_request() {
        use std::sync::mpsc;

        let executable = std::env::current_exe()
            .expect("the test binary path")
            .to_string_lossy()
            .into_owned();
        let connection = Arc::new(Connection::new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!(
                "{UNIT_DAEMON_ENV}=hello_then_idle {UNIT_DAEMON_TOKEN_ENV}=\"$4\" exec \"$1\" --exact connection::tests::unit_daemon_child --nocapture"
            ),
            "sh".to_owned(),
            executable,
        ]));
        let info = connection.info().await.expect("hello with the idle daemon");
        let daemon_pid = info.pid as i32;

        let (reached_sender, reached) = mpsc::channel::<()>();
        let (release_sender, release) = mpsc::channel::<()>();
        *lock(&test_hooks::BEFORE_PENDING) = Some((
            "ping".to_owned(),
            Box::new(move || {
                let _ = reached_sender.send(());
                let _ = release.recv();
            }),
        ));

        let request_connection = Arc::clone(&connection);
        let request = tokio::spawn(async move {
            request_connection
                .request("ping", Json::new(), RequestOptions::default())
                .await
        });
        reached
            .recv_timeout(Duration::from_secs(10))
            .expect("the request reaches the window before registering pending");
        connection.close();
        release_sender.send(()).expect("release the request");
        let error = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .expect("the request settles after close() instead of hanging")
            .expect("the request task does not panic")
            .expect_err("the daemon was torn down");
        assert!(is_connection_lost(&error));
        assert_eq!(error.message(), "kiana-env connection lost");

        // daemon 忽略了 SIGTERM；用 SIGKILL 清理并等退出观察任务回收它。
        // SAFETY: `kill` 只向本测试启动的子进程发信号。
        unsafe {
            libc::kill(daemon_pid, libc::SIGKILL);
        }
        wait_for_process_exit(daemon_pid);
    }

    /// 回归（review finding）：daemon 回了 hello 之后立刻退出时，已经派发的 hello 优先
    /// （TS 里 stdout 的 data 事件通常先于 'exit'），启动成功，而不是报
    /// `exited ... before it was ready`。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hello_reply_that_arrives_before_the_exit_starts_the_connection() {
        let executable = std::env::current_exe()
            .expect("the test binary path")
            .to_string_lossy()
            .into_owned();
        let command = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!(
                "{UNIT_DAEMON_ENV}=hello_then_exit {UNIT_DAEMON_TOKEN_ENV}=\"$4\" exec \"$1\" --exact connection::tests::unit_daemon_child --nocapture"
            ),
            "sh".to_owned(),
            executable,
        ];
        let connection = Connection::new(command);
        let info = connection
            .info()
            .await
            .expect("the hello reply starts the connection");
        assert_eq!(info.protocol, 1.0);
        assert_eq!(info.os, "linux");
    }
}
