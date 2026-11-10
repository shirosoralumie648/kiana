//! terminal.ts：daemon 伪终端上的程序。
//!
//! TS 的 `#request(...).catch(() => {})` 是“发出去就不管”：成功/失败都吞掉，状态由 `exited`
//! 报告。Rust 的 future 惰性执行，因此写帧走 `crate::connection::Connection::request_detached`
//! （同步写帧、丢弃回复），保证 `write()` 的先后顺序就是帧的先后顺序（“keeps input in order”）。
//!
//! # 这个文件解决什么问题
//! `RemoteTerminal` 是 daemon 上伪终端的前端句柄：打开时发一个长期存活的 `pty` 请求，
//! daemon 用 `output` 事件送回原始字节、用 `started` 事件报告程序 pid；请求的 RESULT 帧
//! 携带退出码。`write`/`resize`/`foreground` 是同一个请求 id 上的旁路操作。
//!
//! # 一次典型调用
//! 1. `open_terminal` 组装 `{cwd, argv?, env?, inheritEnv?, cols, rows}` 并 `request("pty", …)`；
//! 2. `on_start` 记下 `(id, session)`，`started` 事件到达后构造 `RemoteTerminal` 并结算 open；
//! 3. 调用方通过 `write` 发送输入（每片 1 MiB，同步写帧保序）；
//! 4. 程序退出后 RESULT 的 `exitCode` 成为 `exited()` 的结果；`close()` 则中止请求，daemon
//!    杀掉进程组，`exited()` 以 `RemoteError { code: "aborted" }` 结算。
//!
//! # 状态机
//! ```text
//! Opening --started--> Running --RESULT/ERROR--> Exited
//!    |                    |                       ^
//!    +--- 请求失败 --------+-----------------------+
//! ```
//! - Opening：`open_terminal` 已发出 `pty`，等 `started`；
//! - Running：已拿到 pid 与 `(id, session)`；
//! - Exited：`exited` 已结算（正常退出码、daemon 错误、取消或连接丢失）。
//! 失败/取消的入口不止一种：请求在 `started` 之前失败 → open 返回 `Err`；`started` 之后失败
//! → 只体现在 `exited`；`close()` → abort → daemon 取消 → `exited` 为 `aborted`。
//!
//! # 并发
//! `on_data` 与 `started` 事件都在连接的读任务（`connection.rs` 的 `read_stdout` →
//! `dispatch`）里同步调用；回调阻塞会推迟后续事件与回复的读取。`write`/`resize` 可在任意
//! 线程调用，但顺序保证只覆盖顺序调用：`request_detached` 只在入队时短暂持锁，同一线程的
//! 连续调用保持调用顺序；并发调用之间没有全序，且 `write_bytes` 按 1 MiB 分片逐帧入队、
//! 分片之间不持锁，切片可能与其他线程的帧交错。
//!
//! TS: packages/env/src/terminal.ts

use std::sync::{Arc, Mutex, MutexGuard};

use kiana_chord::context::AbortSignal;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::connection::{Connection, Json, RemoteError, RequestOptions};
use crate::{SharedResult, SharedResultSender};

/// 输入大于这个值就分几次写；帧最大 16 MiB。
const WRITE_CHUNK: usize = 1024 * 1024;

/// 取锁；中毒（持锁线程 panic）时取回内部值继续，不把 panic 传播给后续调用。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `openTerminal` 的选项。
///
/// 与 TS 一样，可选字段用 `Option` 表示“不传”：`None` 时字段不出现在请求 JSON 里，由 daemon
/// 决定默认值（例如缺省 `argv` 启动登录 shell）。`on_data` 在连接读任务里同步调用，必须快速
/// 返回，否则会推迟后续输出事件与请求回复的处理。
///
/// TS: packages/env/src/terminal.ts#TerminalOptions
#[derive(Clone)]
pub struct TerminalOptions {
    /// 程序的工作目录；必须存在。
    pub cwd: String,
    /// 程序与参数。缺省：用户 shell（`env` 里的 `SHELL` 或 daemon 的环境）作为登录 shell。
    pub argv: Option<Vec<String>>,
    /// 加到 daemon 环境上，或在 `inheritEnv: false` 时作为全部环境。`TERM` 放在这里。
    pub env: Option<indexmap::IndexMap<String, String>>,
    /// 是否继承 daemon 的环境。
    pub inherit_env: Option<bool>,
    /// 列数。
    pub cols: u32,
    /// 行数。
    pub rows: u32,
    /// 终端的原始输出，按顺序。
    ///
    /// 在连接的读任务里同步调用；阻塞会推迟后续帧（包括请求回复）的读取。
    pub on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
}

impl TerminalOptions {
    /// 必填字段的构造器。
    pub fn new(
        cwd: impl Into<String>,
        cols: u32,
        rows: u32,
        on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    ) -> Self {
        TerminalOptions {
            cwd: cwd.into(),
            argv: None,
            env: None,
            inherit_env: None,
            cols,
            rows,
            on_data,
        }
    }
}

/// 终端的前台任务：进程组 id，以及（系统告知时的）组长名。
///
/// TS: packages/env/src/terminal.ts#TerminalForeground
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalForeground {
    /// 前台进程组 id。
    pub pid: u32,
    /// 组长的名字（如果系统告知）。
    pub name: Option<String>,
}

/// 终端的退出结果（可多次等待）。
///
/// 用 `SharedResult` 而不是 `oneshot`：TS 的 `exited` 是同一个 Promise，可以被多处 `await`；
/// `SharedResult` 缓存结果，之后的等待立即返回同一个值。
type Exited = SharedResult<Result<i32, RemoteError>>;

/// 把 `exited` 交给首次 `started` 事件的那个共享槽位。
///
/// 收尾任务在 `take()` 之后才 `send`，因此无论 `started` 先到还是请求先结束，结果都只交付
/// 一次；`exited` 的等待端已经复制进 `RemoteTerminal`。
type ExitSender = Arc<Mutex<Option<SharedResultSender<Result<i32, RemoteError>>>>>;

/// kiana-env daemon 伪终端里运行的程序。程序退出、`close()` 杀掉它、或连接丢失（daemon 随即杀掉它）
/// 时结束。
///
/// # 字段与不变量
/// - `pid`：程序本身的进程号，来自 `started` 事件；前台任务要用 [`RemoteTerminal::foreground`]
///   查，不是这个值。
/// - `id`：`pty` 请求 id，`ptyWrite`/`ptyResize`/`ptyForeground` 都用它作为 `target`。
/// - `session`：请求所属的 daemon session；handle 请求固定发在这个 session 上，连接丢失重启后
///   这些操作会以 `lost` 失败，而不会误发到新 daemon 里同号的终端。
/// - `abort`：`close()` 用的信号；连接在收到 abort 时写 `CANCEL` 帧。
///
/// TS: packages/env/src/terminal.ts#RemoteTerminal
pub struct RemoteTerminal {
    /// 程序本身的进程号。
    pub pid: u32,
    exited: Exited,
    connection: Arc<Connection>,
    id: u32,
    session: u64,
    abort: AbortSignal,
}

impl std::fmt::Debug for RemoteTerminal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteTerminal")
            .field("pid", &self.pid)
            .field("id", &self.id)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl RemoteTerminal {
    /// 退出码；以 daemon 的错误拒绝（`close()` 之后 code 为 `aborted`）。
    /// 可以多次等待；每次返回同一个结果。
    ///
    /// 正常退出时取 RESULT 里的 `exitCode`；字段缺失或不是数字时按 0（`number_to_i32` 的
    /// 兜底）。连接丢失时错误带 `lost` 标记（[`crate::connection::is_connection_lost`]）。
    pub async fn exited(&self) -> Result<i32, RemoteError> {
        self.exited.wait().await
    }

    /// 发送输入；字符串按 UTF-8 编码（TS 的 `write(data: Uint8Array | string)`）。
    /// 终端结束后输入被丢弃。
    ///
    /// 不返回错误：帧在调用线程同步写出，daemon 的回复（例如终端已消失时的 `EBADF`）被
    /// `request_detached` 丢弃；终端是否还活着由 [`RemoteTerminal::exited`] 报告。
    pub fn write<T: AsRef<[u8]>>(&self, data: T) {
        self.write_bytes(data.as_ref());
    }

    /// 发送原始字节输入。
    ///
    /// 按 1 MiB 切片，每片一帧；`request_detached` 在调用线程同步写帧，所以切片之间以及连续
    /// 多次 `write` 之间的顺序就是调用顺序（终端测试 `keeps input in order` 锁定）。
    pub fn write_bytes(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(WRITE_CHUNK) {
            self.request("ptyWrite", Json::new(), Some(chunk.to_vec()));
        }
    }

    /// 改变尺寸；程序收到 `SIGWINCH`。
    ///
    /// 与 [`RemoteTerminal::write`] 一样不返回错误：只发 `ptyResize` 帧，回复被丢弃。
    pub fn resize(&self, cols: u32, rows: u32) {
        let mut json = Json::new();
        json.insert("cols".to_owned(), Value::from(cols));
        json.insert("rows".to_owned(), Value::from(rows));
        self.request("ptyResize", json, None);
    }

    /// 杀掉程序及其会话里的所有进程。
    ///
    /// 只发 abort，不等待退出：连接随即写 `CANCEL` 帧，daemon 杀掉该终端的进程组并以
    /// `aborted` 结束 `pty` 请求，之后 [`RemoteTerminal::exited`] 返回 code `"aborted"` 的
    /// 错误。可以重复调用（`AbortSignal::abort` 只生效一次）。
    pub fn close(&self) {
        self.abort.abort_without_reason();
    }

    /// 终端的前台任务（不是程序本身时）：shell 正在运行的东西，例如编辑器。程序在前台（shell 停在
    /// 提示符）时是 `None`。终端结束后请求以 daemon 的 `EBADF` 失败。
    ///
    /// 解析规则：`foreground` 不是对象、或 `pid` 不是数字时返回 `Ok(None)`；`name` 只在非空
    /// 字符串时保留；`pid` 经 `number_to_u32` 收敛（非有限或越界 → 0）。请求固定在创建时的
    /// session 上，连接丢失后返回带 `lost` 的错误。
    pub async fn foreground(&self) -> Result<Option<TerminalForeground>, RemoteError> {
        let mut json = Json::new();
        json.insert("target".to_owned(), Value::from(self.id));
        let reply = self
            .connection
            .request(
                "ptyForeground",
                json,
                RequestOptions {
                    session: Some(self.session),
                    ..RequestOptions::default()
                },
            )
            .await?;
        let foreground = match reply.json.get("foreground") {
            Some(Value::Object(foreground)) => foreground,
            _ => return Ok(None),
        };
        let pid = match foreground.get("pid") {
            Some(Value::Number(pid)) => pid.as_f64().unwrap_or(0.0),
            _ => return Ok(None),
        };
        let name = match foreground.get("name") {
            Some(Value::String(name)) if !name.is_empty() => Some(name.clone()),
            _ => None,
        };
        Ok(Some(TerminalForeground {
            pid: number_to_u32(pid),
            name,
        }))
    }

    /// `ptyWrite`/`ptyResize` 的共同发送路径：补上 `target`，固定 session，丢弃回复。
    ///
    /// 用 `request_detached` 而不是 `request`：调用方不 await，若用惰性 future 就根本不会发帧。
    fn request(&self, op: &str, json: Json, payload: Option<Vec<u8>>) {
        let mut body = json;
        body.insert("target".to_owned(), Value::from(self.id));
        self.connection.request_detached(
            op,
            body,
            RequestOptions {
                session: Some(self.session),
                payload,
                ..RequestOptions::default()
            },
        );
    }
}

/// JS 的 `Number` → `u32` 的收敛：非有限或超出 `0..=u32::MAX` 一律 0（`started.pid` 缺失时
/// 调用方传 0.0，所以这里也覆盖缺省值）。
fn number_to_u32(value: f64) -> u32 {
    if value.is_finite() && value >= 0.0 && value <= f64::from(u32::MAX) {
        value as u32
    } else {
        0
    }
}

/// JS 的 `Number` → `i32`：非有限值（缺少 `exitCode` 时是 NaN）按 0；有限值按 Rust 的
/// 饱和转换（超出范围夹到 `i32::MIN`/`i32::MAX`，不是回绕）。
fn number_to_i32(value: f64) -> i32 {
    if value.is_finite() { value as i32 } else { 0 }
}

/// 在 `connection` 背后的 daemon 里新开一个伪终端运行程序。
///
/// 请求体只包含调用方给出的可选字段（见 [`TerminalOptions`]）；daemon 校验 `cwd`/尺寸等并
/// 启动程序，然后以事件报告进度：
/// - `started`：带程序 pid，构造 [`RemoteTerminal`] 并结算本函数；
/// - `output`：原始输出字节，直接交给 `TerminalOptions::on_data`。
/// 请求的 RESULT 带 `exitCode`，成为 [`RemoteTerminal::exited`] 的结果。
///
/// # 失败模式
/// - 请求在 `started` 之前失败（cwd 不存在、spawn 失败、连接丢失等）→ 本函数返回 `Err`；
/// - `started` 之后失败 → 只通过 `exited()` 报告，本函数已经返回；
/// - 请求结束却没有 `started`（不遵守协议的 daemon）→ 本函数永久挂起，与 TS 的 promise
///   永不结算一致；测试 `open_terminal_does_not_settle_without_a_started_event` 锁定这一点。
///
/// TS: packages/env/src/terminal.ts#openTerminal
pub async fn open_terminal(
    connection: Arc<Connection>,
    options: TerminalOptions,
) -> Result<RemoteTerminal, RemoteError> {
    let abort = AbortSignal::new();
    // 三个共享点：started 一次性 oneshot（open 只等它）、request 槽（on_start 写入 id/session，
    // started 事件据此构造终端）、exited（SharedResult，可被多个任务重复等待）。
    let (started_sender, started_receiver) =
        oneshot::channel::<Result<RemoteTerminal, RemoteError>>();
    let started_sender = Arc::new(Mutex::new(Some(started_sender)));
    let request: Arc<Mutex<Option<(u32, u64)>>> = Arc::new(Mutex::new(None));
    let (exit_sender, exited) = SharedResult::<Result<i32, RemoteError>>::new();

    // 请求体：cwd/cols/rows 必发；argv/env/inheritEnv 只在 Some 时插入，未提供的字段由 daemon
    // 取默认值（与 TS 的可选展开一致）。
    let mut body = Json::new();
    body.insert("cwd".to_owned(), Value::String(options.cwd.clone()));
    if let Some(argv) = &options.argv {
        body.insert(
            "argv".to_owned(),
            Value::Array(argv.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(env) = &options.env {
        let mut map = Json::new();
        for (key, value) in env {
            map.insert(key.clone(), Value::String(value.clone()));
        }
        body.insert("env".to_owned(), Value::Object(map));
    }
    if let Some(inherit_env) = options.inherit_env {
        body.insert("inheritEnv".to_owned(), Value::Bool(inherit_env));
    }
    body.insert("cols".to_owned(), Value::from(options.cols));
    body.insert("rows".to_owned(), Value::from(options.rows));

    let callback_connection = Arc::clone(&connection);
    let callback_abort = abort.clone();
    let callback_exited = exited.clone();
    let start_request = Arc::clone(&request);
    let event_request = Arc::clone(&request);
    let callback_started = Arc::clone(&started_sender);
    let on_data = Arc::clone(&options.on_data);
    let exit_sender: ExitSender = Arc::new(Mutex::new(Some(exit_sender)));

    let request_connection = Arc::clone(&connection);
    let request_future = async move {
        request_connection
            .request(
                "pty",
                body,
                RequestOptions {
                    signal: Some(abort.clone()),
                    // 连接在写 REQUEST 帧之前同步调用 on_start，所以第一个事件到达时 request
                    // 槽一定已经填好。
                    on_start: Some(Arc::new(move |id, session| {
                        *lock(&start_request) = Some((id, session));
                    })),
                    on_event: Some(Arc::new(move |json: Json, payload: Vec<u8>| {
                        match json.get("kind").and_then(Value::as_str) {
                            Some("output") => on_data(payload),
                            Some("started") => {
                                // 只认第一个 started：`take()` 之后重复事件不会重复结算 open。
                                let Some((id, session)) = *lock(&event_request) else {
                                    return;
                                };
                                let pid = match json.get("pid") {
                                    Some(Value::Number(pid)) => pid.as_f64().unwrap_or(0.0),
                                    _ => 0.0,
                                };
                                let terminal = RemoteTerminal {
                                    pid: number_to_u32(pid),
                                    exited: callback_exited.clone(),
                                    connection: Arc::clone(&callback_connection),
                                    id,
                                    session,
                                    abort: callback_abort.clone(),
                                };
                                if let Some(sender) = lock(&callback_started).take() {
                                    let _ = sender.send(Ok(terminal));
                                }
                            }
                            _ => {}
                        }
                    })),
                    ..RequestOptions::default()
                },
            )
            .await
    };
    // 收尾任务：把请求结果写进 exited；如果 open 还没被 started 结算，同时把错误写给它。
    tokio::spawn(async move {
        let result = request_future.await;
        let failed = match result {
            Ok(reply) => {
                let code = match reply.json.get("exitCode") {
                    Some(Value::Number(code)) => code.as_f64().unwrap_or(0.0),
                    _ => f64::NAN,
                };
                Ok(number_to_i32(code))
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &failed {
            // Failing before it started rejects the open; afterwards, `exited` carries it.
            if let Some(sender) = lock(&started_sender).take() {
                let _ = sender.send(Err(error.clone()));
            }
        }
        if let Some(sender) = lock(&exit_sender).take() {
            sender.send(failed);
        }
    });

    match started_receiver.await {
        Ok(result) => result,
        // The request settled without a `started` event. TS resolves `started` only from that
        // event, so its promise never settles then; mirror that instead of inventing a lost
        // connection (only reachable with a daemon that does not follow the protocol).
        Err(_) => std::future::pending::<Result<RemoteTerminal, RemoteError>>().await,
    }
}
