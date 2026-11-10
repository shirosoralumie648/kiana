//! 对应 packages/env 的 TS 客户端：SSH 部署、RemoteExecutionEnv、watch、terminal。
//!
//! TS 源：packages/env/src/{index,connection,errors,list-files,remote-env,ssh,terminal,watch}.ts
//!
//! 本文件只声明模块树，并在 crate 根再导出 `index.ts` 的 34 个符号（ARCHITECTURE.md §4.10：
//! 公开项在 `lib.rs` 再导出）。与 TS 的对应关系见各模块的文档注释。
//!
//! 与 TS 的差异（ARCHITECTURE.md §4.9 决定的错误/取消/JSON 模型带来的，逐条记录在模块里）：
//! - TS 的 `Promise` → `async fn`；`AbortSignal` → [`kiana_chord::context::AbortSignal`]。
//! - TS 的 `Json`（`Record<string, unknown>`）→ [`connection::Json`]（`serde_json::Map`，键序保留）。
//! - TS 的 `spawn` 同步返回子进程、失败走 `error` 事件；Rust 的 `Command::spawn` 同步失败。
//!   失败消息按 Node 的规则重建（`spawn_error_message`：`spawn <program> <CODE>`；Node 在
//!   `spawn()` 里同步抛出的 errno 则是 `spawn <CODE>`，没有 `lost` 标记）。Node 作为异步
//!   `error` 事件报告的五种 errno（EACCES/EAGAIN/EMFILE/ENFILE/ENOENT）之外都是同步抛出
//!   （`spawn_error_is_asynchronous`），`Connection` 用 code `unknown`、无 `lost` 的
//!   `RemoteError` 承载后者（TS 里它不是 `RemoteError`）。
//!
//! # 这个 crate 解决什么问题
//!
//! 对调用方来说，它提供的是 [`kiana_durable::env::ExecutionEnv`]（即 `FileSystem` + `Shell`）在
//! **另一台机器**上的实现：远端调用经 `ssh` 启动的 `kiana-env` daemon 执行，本 crate 负责部署并
//! 启动 daemon、按帧协议收发请求与事件，再把 Node 的路径、错误消息以及字符串/数字格式规则应用
//! 到客户端。没有它，远端执行只能各自拼接 `ssh` 命令，既无法复用 `ExecutionEnv` 契约，也无法像
//! `tests/conformance.rs` 那样用同一组一致性用例同时验证本地与远端实现。
//!
//! # 在系统中的位置
//!
//! 上游（本 crate 的依赖）：
//! - `kiana-durable`：`ExecutionEnv` / `FileSystem` / `Shell` 契约、`env::decode` 的 UTF-8 解码器、
//!   `env::node` 的 Node 路径语义与本地 `NodeExecutionEnv`。
//! - `kiana-chord`：`AbortSignal` / `Context` 取消模型。
//!
//! 下游：daemon 本体是独立 crate `kiana-env`（二进制，只讲帧协议），不在本 crate 内。本 crate 目前
//! 只被自身的集成测试（`tests/conformance.rs`、`tests/remote.rs`、`tests/ssh.rs`、
//! `tests/terminal.rs`、`tests/differential.rs`、`tests/ssh_external.rs`）直接使用；工作区其他 crate
//! 尚未在 `Cargo.toml` 里依赖它。
//!
//! # 核心概念与类型
//!
//! - [`connection::Connection`]：一次 daemon 会话。首次请求时才启动子进程、完成 `hello` 握手，
//!   之后负责帧的收发、每 5 秒 ping、静默超时与连接丢失（`fields.lost`）判定。
//! - [`ssh::connect_ssh`] / [`ssh::SshConnection`]：探测远端平台，按 sha256 校验或上传
//!   `kiana-env`，再让 `Connection` 通过 `ssh` 启动它。
//! - [`remote_env::RemoteExecutionEnv`]：远端 `FileSystem` + `Shell`（由 kiana-durable 的 blanket
//!   impl 自动实现 `ExecutionEnv`），路径与结果语义对齐 `NodeExecutionEnv`。
//! - [`list_files::LocalExecutionEnv`]：本机 `NodeExecutionEnv`，只把 `listFiles` 交给本机 daemon。
//! - [`watch::RemoteWatcher`] / [`terminal::RemoteTerminal`]：daemon 侧的文件观察与伪终端。
//! - [`connection::RemoteError`] / [`errors::EnvError`]：daemon 的错误对象，以及 `FileSystem`
//!   方法使用的 `FileError` 转换。
//!
//! 本文件（crate 根）是模块树加一组跨模块共用的 Node 保真工具：`SharedResult`（一次完成、可多次
//! 等待的结果）、整块 UTF-8 解码、JS 的 `String(value)` 与 `Number#toString`、libuv 的 errno 名与
//! 文案、spawn 失败消息、`child.kill()` 的 SIGTERM 语义，以及 JSON 数字编码。其中 `SharedResult`、
//! 解码/格式化/错误文本等跨模块使用的工具是 `pub(crate)`；`decompose_number`、`uv_err_name(_of)`、
//! `uv_strerror` 仅本文件可见。它们都不属于公开 API。
//!
//! # 一次典型调用
//!
//! 1. `connect_ssh(options)`：`detect_platform` 探测远端系统，`deploy_daemon` 读取本地二进制并
//!    计算 sha256；远端已有同哈希文件则跳过，否则上传。返回的 `ConnectedSsh.connection` 在需要
//!    时才用 `ssh` 启动 daemon。
//! 2. `RemoteExecutionEnv::new`：只保存 `connection`/`id`/`cwd` 等配置，不发请求。
//! 3. 第一次需要 daemon 的调用（如 `resolve`/`absolute_path`、文件读写、`exec`）会触发
//!    `Connection` 启动 daemon 并完成 `hello`；`id()`/`cwd()`/`set_cwd()` 等纯本地方法不会。
//!    每个协议请求编码成一个 `REQUEST` 帧；一次高层操作可能拆成多个请求（open/read/close、
//!    分页等），`EVENT` 帧交给当前请求的 `on_event`（`exec` 输出、watch/pty 事件）。
//! 4. 请求以 daemon 的回复或错误结算；`FileSystem` 方法的错误经 `to_file_error` 映射成
//!    `FileError`，`Shell::exec` 的错误是 `ExecutionError`。`close()` 置 closed、终结在途请求，
//!    并让子进程收到 SIGTERM。
//!
//! # 边界与失败模式
//!
//! - **spawn 失败**：Node 只把 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 当作子进程的异步 `error` 事件
//!   （消息带程序名），其他 errno 在 `spawn()` 里同步抛出（消息 `spawn <CODE>`）。本 crate 用
//!   `spawn_error_is_asynchronous` 区分：前者是带 `lost: true` 的 `spawn_error`，后者用 code
//!   `unknown`、无 `lost` 的 `RemoteError`（TS 里它不是 `RemoteError`）。
//! - **连接丢失**：daemon 退出、坏帧、hello 超时或 ping 静默都会 teardown，在途请求以
//!   `lost: true` 的 `RemoteError` 拒绝；`close()` 的拒绝没有 `lost`（message `Connection closed`）。
//! - **取消**：请求的 `AbortSignal` 触发时发送 `CANCEL` 帧；请求仍以 daemon 的结果或 `aborted`
//!   错误结算，客户端不会提前伪造结果。
//! - **字节与数字**：非法 UTF-8 替换为 U+FFFD 且不剥 BOM；JS 数值格式按 ECMA-262 复刻。
//! - **平台**：非 Unix 分支只保证编译（errno 名称/文案表为空，`uv_error_text` 返回 `None`），
//!   与 Node 在 Windows 上的行为不做逐字保证。
//!
//! # 阅读顺序
//!
//! 1. [`index`]（公开再导出面）→ 2. [`errors`]（错误模型）→ 3. [`connection`]（帧协议与连接生命
//! 周期）→ 4. [`ssh`]（部署与 SSH 启动）→ 5. [`remote_env`]（`ExecutionEnv` 语义主体）→
//! 6. [`list_files`]、[`watch`]、[`terminal`]。本文件其余部分是上述模块共用的保真工具，可最后读。
// `RemoteError` 是 `index.ts` 再导出的共享契约类型（对应 TS 的 `RemoteError` 类：code / message /
// path / 额外字段），与 `kiana-chord` 的错误类型同理：装箱会改变公开形状，故保持原样。
#![allow(clippy::result_large_err)]
pub mod connection;
pub mod errors;
pub mod index;
pub mod list_files;
pub mod remote_env;
pub mod ssh;
pub mod terminal;
pub mod watch;

pub use index::*;

use kiana_durable::env::RangeDecoder;

/// 只完成一次、可多次等待的结果（TS 里同一个 `Promise` 被多处 `await` 的等价物）。
///
/// 内部是 `tokio::sync::watch` 上的一个槽位（`None` = 未完成，`Some(value)` = 已完成）。
/// `wait` 每次克隆保存的值，因此要求 `T: Clone`，多个等待者互不消耗结果。和 `oneshot` 不同，
/// 它可以被任意多次等待，所以 `terminal::RemoteTerminal::exited` 与 watch 的 `close()` 都把它
/// 当作“事件只发生一次、但谁都能等”的汇合点。
pub(crate) struct SharedResult<T> {
    receiver: tokio::sync::watch::Receiver<Option<T>>,
}

/// [`SharedResult`] 的发送端。
///
/// 正常路径必须发送一次值：若在发送前被 drop，等待者会看到通道关闭
/// （见 [`SharedResult::wait`] 对这种情况的处理）。
pub(crate) struct SharedResultSender<T> {
    sender: tokio::sync::watch::Sender<Option<T>>,
}

impl<T> Clone for SharedResult<T> {
    fn clone(&self) -> Self {
        SharedResult {
            receiver: self.receiver.clone(),
        }
    }
}

impl<T: Clone> SharedResult<T> {
    /// 新建一对发送端/等待端；此时状态是“未完成”，`wait` 会挂起。
    pub(crate) fn new() -> (SharedResultSender<T>, SharedResult<T>) {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        (SharedResultSender { sender }, SharedResult { receiver })
    }

    /// 等到值被发送；之后每次等待都立即返回同一个值。
    ///
    /// 每个等待者持有独立的 `watch::Receiver` 克隆，而 watch 保存最后一个值，所以重复/并发等待
    /// 都得到同一结果的副本，不会被先到者“取走”。若发送端在发送前被 drop，`changed()` 返回
    /// `Err`：这里不 panic、也不编造默认值，而是在循环里反复 `yield_now`、永不返回——效果等于
    /// 一个永不结算的 `await`（TS 里请求不结算时 `exited` / `close()` 同样一直等下去）。正常
    /// 路径不会走到这里：发送端由完成请求的任务持有（`terminal.rs` 的 `Option` 槽位、`watch.rs`
    /// 任务末尾的 `send`），只有进程收尾，或持有发送端的任务在发送前 panic/被丢弃时才会提前消失。
    pub(crate) async fn wait(&self) -> T {
        let mut receiver = self.receiver.clone();
        loop {
            if let Some(value) = receiver.borrow().clone() {
                return value;
            }
            if receiver.changed().await.is_err() {
                // The sender was dropped without a value; only a teardown at process exit does this.
                loop {
                    tokio::task::yield_now().await;
                }
            }
        }
    }
}

impl<T> SharedResultSender<T> {
    /// `resolve(value)`：只生效一次。
    ///
    /// 忽略 `watch::Sender::send` 的错误：只有当所有接收端（`SharedResult` 及其克隆）都已丢弃时
    /// 才会失败，此时已没有等待者需要唤醒。本 crate 的调用方都只发送一次（terminal 用 `Option`
    /// 取出发送端、watch 在任务末尾发送）；watch 通道本身会覆盖旧值，重复发送不是支持的模式。
    pub(crate) fn send(&self, value: T) {
        let _ = self.sender.send(Some(value));
    }
}

/// `Buffer.toString("utf8")` / `new TextDecoder("utf-8", { ignoreBOM: true })` 的整块解码：
/// 非法序列替换为 U+FFFD，**保留**开头的字节序标记（Node 的 `Buffer.toString` 不剥 BOM）。
///
/// 用 `RangeDecoder` 分两步完成：先喂入全部字节，再以 `None` 冲刷流尾，跨块不完整的多字节
/// 序列在冲刷时变成 U+FFFD。没有错误分支——调用方包括 `readFile` 的文本路径、`listFiles` 记录里
/// 的路径、stderr 日志与握手前的 banner，以及 `exec` 输出（`remote_env.rs` 的 `on_event` 对
/// payload 解码），它们都不需要处理“解码失败”。
pub(crate) fn decode_utf8_lossy(bytes: &[u8]) -> String {
    let mut decoder = RangeDecoder::new();
    let mut text = decoder.decode(Some(bytes));
    text.push_str(&decoder.decode(None));
    text
}

/// JS 的 `String(value)`（daemon 事件里的 `String(event.message)`）。
///
/// `None` 表示属性缺失 → `"undefined"`；JSON 的 `null` → `"null"`；字符串、布尔与数字按 JS 的
/// 规则（数字整数优先走 `i64`/`u64`，其余用 `format_js_number`）。数组与对象没有实现 JS 的
/// `ToString`，走的是 `serde_json` 的文本表示；本 crate 的调用点取事件里的 `message` 与 hello
/// 的 `protocol`，期望的是字符串/数字/缺失值，数组/对象只可能来自不合协议的 daemon。
pub(crate) fn js_string_of(value: Option<&serde_json::Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(serde_json::Value::Null) => "null".to_owned(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Bool(true)) => "true".to_owned(),
        Some(serde_json::Value::Bool(false)) => "false".to_owned(),
        Some(serde_json::Value::Number(number)) => {
            if let Some(value) = number.as_i64() {
                value.to_string()
            } else if let Some(value) = number.as_u64() {
                value.to_string()
            } else if let Some(value) = number.as_f64() {
                format_js_number(value)
            } else {
                number.to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// JS 的 `Number.prototype.toString()`（基数 10 的 ECMA-262 规则，即 `String(number)`）：
/// `-0` → `"0"`；`NaN` → `"NaN"`；`±Infinity` 按符号；其余用最短往返表示并按 JS 的规则选择
/// 定点或指数形式（`1e-7`、`1e+21`；Rust 的 `Display` 永远不会切到指数形式）。
///
/// `kiana_json::js_num::format_js_number` 是工作区里的唯一实现，但 `kiana-json` 不在本 crate 的依赖
/// 边内（ARCHITECTURE.md §2.2），这里用同一算法（`ryu` 的最短往返位数 + ECMA-262 的 n/k 规则）
/// 复刻；本 crate 的调用点只有 `timeout:{seconds}` 与事件里的 `String(message)`，取值有限。
///
/// 步骤：`ryu` 给出最短往返十进制串 → `decompose_number` 归一化成 (digits, n) → 按 n/k 选定点
/// 或指数形式 → 最后补负号。回归用例见本文件的
/// `format_js_number_matches_javascript_string_conversion`（期望值采自 Node 26）。
pub(crate) fn format_js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_owned()
        } else {
            "Infinity".to_owned()
        };
    }
    if value == 0.0 {
        // -0 与 +0 都是 "0"。
        return "0".to_owned();
    }

    // ryu 给出最短往返表示，但格式不一定是 JS 的规则；先解析成 (digits, decimal_exponent)。
    let mut buf = ryu::Buffer::new();
    let ryu_text = buf.format_finite(value);
    let (digits, n) = decompose_number(ryu_text);

    let negative = value.is_sign_negative();
    let k = digits.len() as i32;
    // ECMA-262 Number::toString 的四个区间（k = 有效位数，n = 小数点位置）：
    //   1) k ≤ n ≤ 21：整数，digits 后补 n-k 个 0（如 1e20）；
    //   2) 0 < n ≤ 21：digits 在第 n 位后插入小数点（如 1234.5）；
    //   3) -6 < n ≤ 0：`0.` + (-n) 个 0 + digits（如 1e-6）；
    //   4) 其余：首位数字 [+ `.` 其余] + `e` + (n-1)，指数为正时带 `+`（如 1e-7、1e+21）。
    let body = if k <= n && n <= 21 {
        let mut text = digits;
        for _ in 0..(n - k) {
            text.push('0');
        }
        text
    } else if 0 < n && n <= 21 {
        let n_usize = n as usize;
        let mut text = String::with_capacity(digits.len() + 1);
        text.push_str(&digits[..n_usize]);
        text.push('.');
        text.push_str(&digits[n_usize..]);
        text
    } else if -6 < n && n <= 0 {
        let mut text = String::with_capacity(digits.len() + 2 + (-n) as usize);
        text.push_str("0.");
        for _ in 0..(-n) {
            text.push('0');
        }
        text.push_str(&digits);
        text
    } else {
        let mut text = String::with_capacity(digits.len() + 6);
        text.push_str(&digits[..1]);
        if digits.len() > 1 {
            text.push('.');
            text.push_str(&digits[1..]);
        }
        text.push('e');
        let exponent = n - 1;
        if exponent > 0 {
            text.push('+');
        }
        text.push_str(&exponent.to_string());
        text
    };

    if negative {
        let mut text = String::with_capacity(body.len() + 1);
        text.push('-');
        text.push_str(&body);
        text
    } else {
        body
    }
}

/// 把 ryu 的文本拆成 (有效数字串, 小数点位置 n)，`value = 0.digits × 10^n`。
///
/// ryu 的 `format_finite` 输出形如 `1234.5`、`1e-7`、`1.2345e21`、`0.0001`；
/// 统一改写为无前导/尾随零的 `digits` 与十进制指数 `n`。输入只来自 `format_finite`，因此是有限
/// 值的十进制/指数文本（没有 `inf`/`NaN`）；开头的 `-` 在这里丢掉，符号由调用方统一补。
fn decompose_number(text: &str) -> (String, i32) {
    let text = text.strip_prefix('-').unwrap_or(text);

    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => {
            let (mantissa, exponent) = text.split_at(index);
            let exponent: i32 = exponent[1..].parse().unwrap_or(0);
            (mantissa, exponent)
        }
        None => (text, 0),
    };

    let (int_part, frac_part) = match mantissa.find('.') {
        Some(index) => (&mantissa[..index], &mantissa[index + 1..]),
        None => (mantissa, ""),
    };

    let mut digits = String::with_capacity(int_part.len() + frac_part.len());
    digits.push_str(int_part);
    digits.push_str(frac_part);

    // 小数点位置：整数部分长度 + 指数。
    let mut n = int_part.len() as i32 + exponent;

    // 去掉前导零，同时左移小数点位置。
    let leading = digits.chars().take_while(|digit| *digit == '0').count() as i32;
    if leading > 0 {
        digits.drain(..leading as usize);
        n -= leading;
    }
    // 去掉尾随零；若全是零，则值本身为 0（已在入口处理）。
    let trailing = digits
        .chars()
        .rev()
        .take_while(|digit| *digit == '0')
        .count();
    if trailing > 0 && trailing < digits.len() {
        digits.truncate(digits.len() - trailing);
    }
    if digits.is_empty() {
        digits.push('0');
        n = 1;
    }
    (digits, n)
}

/// `uv_err_name(errno)`（libuv）：Node 的错误消息里的 `<CODE>`。
///
/// 非 Unix 平台的 `raw_os_error` 是 Windows 错误码，不做映射（与 `kiana_durable::env::node`
/// 的私有 `node_errno` 一致）。
///
/// 返回 `None` 的情形有两种：`io::Error` 没有 `raw_os_error()`（如 `io::Error::other`），或
/// errno 不在下面的表里。本函数的唯一调用方 `spawn_error_message` 在 `None` 时退回
/// `io::Error` 的 `Display`；直接使用 `uv_err_name_of` 的 `uv_error_text` 在 errno 未收录时
/// 写 `UNKNOWN`，没有 errno 时返回 `None`。
#[cfg(unix)]
fn uv_err_name(error: &std::io::Error) -> Option<&'static str> {
    uv_err_name_of(error.raw_os_error()?)
}

/// `uv_err_name(errno)`；未收录的 errno 返回 `None`（Node 写 `UNKNOWN`）。
///
/// 这张表与下面 `uv_strerror` 使用同一组 `libc` 常量，应同步维护；只加一边会得到“有名字但文案
/// 未知”或反之的组合（`uv_error_text` 对此有兜底，不会 panic）。
#[cfg(unix)]
fn uv_err_name_of(code: i32) -> Option<&'static str> {
    Some(match code {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ESRCH => "ESRCH",
        libc::EINTR => "EINTR",
        libc::EIO => "EIO",
        libc::ENXIO => "ENXIO",
        libc::E2BIG => "E2BIG",
        libc::ENOEXEC => "ENOEXEC",
        libc::EBADF => "EBADF",
        libc::EAGAIN => "EAGAIN",
        libc::ENOMEM => "ENOMEM",
        libc::EACCES => "EACCES",
        libc::EFAULT => "EFAULT",
        libc::EBUSY => "EBUSY",
        libc::EEXIST => "EEXIST",
        libc::EXDEV => "EXDEV",
        libc::ENODEV => "ENODEV",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENFILE => "ENFILE",
        libc::EMFILE => "EMFILE",
        libc::ENOTTY => "ENOTTY",
        libc::ETXTBSY => "ETXTBSY",
        libc::EFBIG => "EFBIG",
        libc::ENOSPC => "ENOSPC",
        libc::ESPIPE => "ESPIPE",
        libc::EROFS => "EROFS",
        libc::EMLINK => "EMLINK",
        libc::EPIPE => "EPIPE",
        libc::ERANGE => "ERANGE",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ENOSYS => "ENOSYS",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::ELOOP => "ELOOP",
        libc::EOVERFLOW => "EOVERFLOW",
        libc::EILSEQ => "EILSEQ",
        libc::ENOTSUP => "ENOTSUP",
        libc::EPROTO => "EPROTO",
        libc::ENOTSOCK => "ENOTSOCK",
        libc::EDESTADDRREQ => "EDESTADDRREQ",
        libc::EMSGSIZE => "EMSGSIZE",
        libc::EPROTOTYPE => "EPROTOTYPE",
        libc::ENOPROTOOPT => "ENOPROTOOPT",
        libc::EPROTONOSUPPORT => "EPROTONOSUPPORT",
        libc::EAFNOSUPPORT => "EAFNOSUPPORT",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::EADDRNOTAVAIL => "EADDRNOTAVAIL",
        libc::ENETDOWN => "ENETDOWN",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::ECONNABORTED => "ECONNABORTED",
        libc::ECONNRESET => "ECONNRESET",
        libc::ENOBUFS => "ENOBUFS",
        libc::EISCONN => "EISCONN",
        libc::ENOTCONN => "ENOTCONN",
        libc::ESHUTDOWN => "ESHUTDOWN",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::EHOSTDOWN => "EHOSTDOWN",
        libc::EHOSTUNREACH => "EHOSTUNREACH",
        libc::EALREADY => "EALREADY",
        libc::ECANCELED => "ECANCELED",
        _ => return None,
    })
}

#[cfg(not(unix))]
fn uv_err_name(_error: &std::io::Error) -> Option<&'static str> {
    None
}

/// `uv_strerror(errno)`（libuv 的文案表，不是 glibc 的 `strerror`）。
///
/// 文案值来自 Node 26 的 `util.getSystemErrorMessage`（与 libuv 同一张表）；Node 的 `fs`
/// 错误消息是 `<CODE>: <描述>, <syscall> '<path>'`。未收录的 errno 返回 `None`，调用方写
/// libuv 的兜底文案 `Unknown system error -N`。
///
/// 文案会原样进入用户可见的错误消息，是当前稳定契约，改动需同步测试（测试 `filesystem_error_text_matches_node`
/// 锁定常用项）。
#[cfg(unix)]
fn uv_strerror(code: i32) -> Option<&'static str> {
    Some(match code {
        libc::EPERM => "operation not permitted",
        libc::ENOENT => "no such file or directory",
        libc::ESRCH => "no such process",
        libc::EINTR => "interrupted system call",
        libc::EIO => "i/o error",
        libc::ENXIO => "no such device or address",
        libc::E2BIG => "argument list too long",
        libc::ENOEXEC => "exec format error",
        libc::EBADF => "bad file descriptor",
        libc::EAGAIN => "resource temporarily unavailable",
        libc::ENOMEM => "not enough memory",
        libc::EACCES => "permission denied",
        libc::EFAULT => "bad address in system call argument",
        libc::EBUSY => "resource busy or locked",
        libc::EEXIST => "file already exists",
        libc::EXDEV => "cross-device link not permitted",
        libc::ENODEV => "no such device",
        libc::ENOTDIR => "not a directory",
        libc::EISDIR => "illegal operation on a directory",
        libc::EINVAL => "invalid argument",
        libc::ENFILE => "file table overflow",
        libc::EMFILE => "too many open files",
        libc::ENOTTY => "inappropriate ioctl for device",
        libc::ETXTBSY => "text file is busy",
        libc::EFBIG => "file too large",
        libc::ENOSPC => "no space left on device",
        libc::ESPIPE => "invalid seek",
        libc::EROFS => "read-only file system",
        libc::EMLINK => "too many links",
        libc::EPIPE => "broken pipe",
        libc::ERANGE => "result too large",
        libc::ENAMETOOLONG => "name too long",
        libc::ENOSYS => "function not implemented",
        libc::ENOTEMPTY => "directory not empty",
        libc::ELOOP => "too many symbolic links encountered",
        libc::EOVERFLOW => "value too large for defined data type",
        libc::EILSEQ => "illegal byte sequence",
        libc::ENOTSUP => "operation not supported on socket",
        libc::EPROTO => "protocol error",
        libc::ENOTSOCK => "socket operation on non-socket",
        libc::EDESTADDRREQ => "destination address required",
        libc::EMSGSIZE => "message too long",
        libc::EPROTOTYPE => "protocol wrong type for socket",
        libc::ENOPROTOOPT => "protocol not available",
        libc::EPROTONOSUPPORT => "protocol not supported",
        libc::EAFNOSUPPORT => "address family not supported",
        libc::EADDRINUSE => "address already in use",
        libc::EADDRNOTAVAIL => "address not available",
        libc::ENETDOWN => "network is down",
        libc::ENETUNREACH => "network is unreachable",
        libc::ECONNABORTED => "software caused connection abort",
        libc::ECONNRESET => "connection reset by peer",
        libc::ENOBUFS => "no buffer space available",
        libc::EISCONN => "socket is already connected",
        libc::ENOTCONN => "socket is not connected",
        libc::ESHUTDOWN => "cannot send after transport endpoint shutdown",
        libc::ETIMEDOUT => "connection timed out",
        libc::ECONNREFUSED => "connection refused",
        libc::EHOSTDOWN => "host is down",
        libc::EHOSTUNREACH => "host is unreachable",
        libc::EALREADY => "connection already in progress",
        libc::ECANCELED => "operation canceled",
        _ => return None,
    })
}

/// `<CODE>: <描述>`：Node 错误消息（`<CODE>: <描述>, <syscall> '<path>'`）的前缀。
///
/// `None` = 不是 OS 错误（没有 errno），调用方沿用 `io::Error` 的 `Display`。非 Unix 平台的
/// `raw_os_error` 是 Windows 错误码（libuv 会把它映射成 POSIX 名），本移植只保证编译，不做映射。
///
/// 只生成前缀；调用方还要追加 `, <syscall> '<path>'`（见 `ssh.rs` 的 `fs_error_prefix` 与
/// `node_fs_error`），字段顺序与 Node 的 `ErrnoException.message` 一致。
#[cfg(unix)]
pub(crate) fn uv_error_text(error: &std::io::Error) -> Option<String> {
    let raw = error.raw_os_error()?;
    let (name, description) = match uv_err_name_of(raw) {
        Some(name) => (
            name,
            uv_strerror(raw)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Unknown system error -{raw}")),
        ),
        None => ("UNKNOWN", format!("Unknown system error -{raw}")),
    };
    Some(format!("{name}: {description}"))
}

#[cfg(not(unix))]
pub(crate) fn uv_error_text(_error: &std::io::Error) -> Option<String> {
    None
}

/// Node 把哪些 `spawn()` 失败作为子进程的异步 `error` 事件报告（消息带文件名：
/// `spawn <program> <CODE>`），而不是在 `spawn()` 调用里同步抛出 `ErrnoException`
/// （消息 `spawn <CODE>`，没有文件名）。在 Node 26 上逐项验证过。
///
/// Unix 分支按 errno 精确匹配这五个值（测试 `spawn_error_modes_match_node` 锁定）；非 Unix 分支
/// 按 `io::ErrorKind` 近似判断，而本仓库的相关测试全是 `#[cfg(unix)]`，该分支没有实测覆盖。
/// 调用方据此决定错误形状：`true` → 带 `lost: true` 的 `spawn_error`；`false` → Node 的同步抛出
/// （TS 里不是 `RemoteError`），用 code `unknown`、无 `lost` 的 `RemoteError` 或 `SshFailure`
/// 的普通错误承载。
pub(crate) fn spawn_error_is_asynchronous(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(raw) if raw == libc::EACCES
                || raw == libc::EAGAIN
                || raw == libc::EMFILE
                || raw == libc::ENFILE
                || raw == libc::ENOENT
        )
    }
    #[cfg(not(unix))]
    {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::PermissionDenied
                | std::io::ErrorKind::WouldBlock
        )
    }
}

/// `child_process.spawn` 失败时的 `error.message`。
///
/// 异步 `error` 事件带文件名（`spawn <program> <CODE>`）；同步 `ErrnoException` 没有
/// （`spawn <CODE>`，见 [`spawn_error_is_asynchronous`]）。未知 errno 回退到 `io::Error` 的
/// `Display`（Node 用 `Unknown system error -N`，两者都只出现在极端 I/O 失败下）。
///
/// `program` 必须与实际传给 `Command::new` 的名字一致（可能是 `ssh`，也可能是配置里的绝对
/// 路径）。这里只生成消息文本，错误种类由调用方按 [`spawn_error_is_asynchronous`] 决定。
pub(crate) fn spawn_error_message(program: &str, error: &std::io::Error) -> String {
    let Some(code) = uv_err_name(error) else {
        return error.to_string();
    };
    if spawn_error_is_asynchronous(error) {
        format!("spawn {program} {code}")
    } else {
        format!("spawn {code}")
    }
}

/// Node 的 `child.kill()`：POSIX 上是 SIGTERM（不是 tokio 的 `start_kill`/`kill` 用的
/// SIGKILL），Windows 上是 TerminateProcess。进程已经退出时 `kill(2)` 失败，与 Node 返回
/// `false` 一样被忽略。
///
/// 只发信号，不等待也不回收子进程：调用方仍持有 `&mut Child`，负责随后的 `wait`——`ssh.rs` 的
/// 超时路径会 `spawn` 一个回收任务，`connection.rs` 的退出监测继续 `try_wait` 轮询。SIGTERM 可被
/// 进程捕获或忽略，因此调用后进程不保证立即结束；`child.id()` 为 `None`（已被 `wait` 回收）时
/// 不发信号。
pub(crate) fn kill_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: `pid` 来自仍然存活的子进程句柄（尚未 wait，所以 pid 不会被复用）。
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.start_kill();
    }
}

/// 把 f64 转成 JSON 数字，整数值写成整数（`{...}` 里 `timeoutMs: 300` 不能变成 `300.0`；
/// `JSON.stringify` 只写 `300`）。非有限值写出 `null`，与 `JSON.stringify` 一致。
///
/// 调用点都是请求体里的毫秒数/速率（`timeoutMs`、`pollIntervalMs`、节流窗口的
/// `minIntervalMs`/`bytesPerSecond`），所以整数形态很重要：`serde_json` 会把 f64 的 `300.0`
/// 序列化成 `300.0`，而 daemon 期待与 `JSON.stringify` 相同的 `300`。
pub(crate) fn js_number(value: f64) -> serde_json::Value {
    if !value.is_finite() {
        return serde_json::Value::Null;
    }
    // 2^53 以内的整数值在 f64 里精确可表示，转 i64 无损；更大的值走浮点序列化。
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        return serde_json::Value::from(value as i64);
    }
    serde_json::Number::from_f64(value)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    //! 本文件内联的回归测试：JS 的数值格式化、Node 的 spawn 失败消息与 fs 错误文案。
    //! 端到端行为（连接、SSH、watch、terminal）在 `tests/` 下的集成测试里。

    use super::*;

    /// 回归（review finding）：`timeout:${timeout}` 用 JS 的 `String(number)`，指数形式从
    /// `1e-7`（以及 `1e+21` 以上）开始；期望值来自 Node 26 的 `String(value)`。
    #[test]
    fn format_js_number_matches_javascript_string_conversion() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1, "0.1"),
            (1234.5, "1234.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1e-5, "0.00001"),
            (1e-6, "0.000001"),
            (0.000001234, "0.000001234"),
            (1e-7, "1e-7"),
            (5e-7, "5e-7"),
            (1.23e-7, "1.23e-7"),
            (1.0000000000000002e-7, "1.0000000000000002e-7"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.23e21, "1.23e+21"),
            (1e23, "1e+23"),
            (123_456_789_012_345_680_000.0, "123456789012345680000"),
            (9_007_199_254_740_992.0, "9007199254740992"),
            (2_147_483.647, "2147483.647"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
            (5e-324, "5e-324"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_js_number(*value), *expected, "String({value})");
        }
    }

    /// 回归（review finding）：Node 的 spawn 失败消息带 libuv 的 errno 名；只有
    /// EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 是异步 `error` 事件（带文件名），其余是同步
    /// `ErrnoException`（`spawn <CODE>`）。期望值来自 Node 26 的实际 `error.message`。
    #[cfg(unix)]
    #[test]
    fn spawn_errors_use_the_node_message() {
        let message = |raw| spawn_error_message("ssh", &std::io::Error::from_raw_os_error(raw));
        assert_eq!(message(libc::ENOENT), "spawn ssh ENOENT");
        assert_eq!(message(libc::EACCES), "spawn ssh EACCES");
        assert_eq!(message(libc::ENOTDIR), "spawn ENOTDIR");
        assert_eq!(message(libc::ENAMETOOLONG), "spawn ENAMETOOLONG");
        // 回归（review finding）：libuv 的表里有 E2BIG/ENOEXEC，不能回退到 `io::Error` 的 Display。
        assert_eq!(message(libc::E2BIG), "spawn E2BIG");
        assert_eq!(message(libc::ENOEXEC), "spawn ENOEXEC");
        assert_eq!(
            spawn_error_message("/bin/sh", &std::io::Error::from_raw_os_error(libc::EPERM)),
            "spawn EPERM"
        );
    }

    /// 回归（review finding）：E2BIG/ENAMETOOLONG 等是 `spawn()` 的同步抛出（Node 的
    /// `ErrnoException`），只有 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 走异步 `error` 事件。
    #[cfg(unix)]
    #[test]
    fn spawn_error_modes_match_node() {
        for raw in [
            libc::EACCES,
            libc::EAGAIN,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOENT,
        ] {
            assert!(
                spawn_error_is_asynchronous(&std::io::Error::from_raw_os_error(raw)),
                "errno {raw} is reported asynchronously"
            );
        }
        for raw in [
            libc::E2BIG,
            libc::ENOEXEC,
            libc::ENAMETOOLONG,
            libc::ENOTDIR,
            libc::ELOOP,
            libc::EPERM,
            libc::EINVAL,
        ] {
            assert!(
                !spawn_error_is_asynchronous(&std::io::Error::from_raw_os_error(raw)),
                "errno {raw} is thrown synchronously"
            );
        }
    }

    /// Node 的 `fs` 错误消息前缀：`<CODE>: <描述>`，描述来自 libuv 的表（不是 glibc 的
    /// `strerror`）。期望值来自 Node 26 的实际 `error.message`。
    #[cfg(unix)]
    #[test]
    fn filesystem_error_text_matches_node() {
        let text =
            |raw| uv_error_text(&std::io::Error::from_raw_os_error(raw)).expect("an OS error");
        assert_eq!(text(libc::ENOENT), "ENOENT: no such file or directory");
        assert_eq!(text(libc::EACCES), "EACCES: permission denied");
        assert_eq!(
            text(libc::EISDIR),
            "EISDIR: illegal operation on a directory"
        );
        assert_eq!(text(libc::ENOTDIR), "ENOTDIR: not a directory");
        assert_eq!(text(libc::EEXIST), "EEXIST: file already exists");
        assert_eq!(text(libc::ENAMETOOLONG), "ENAMETOOLONG: name too long");
        assert_eq!(
            text(libc::ELOOP),
            "ELOOP: too many symbolic links encountered"
        );
        assert_eq!(text(libc::EIO), "EIO: i/o error");
        assert_eq!(text(libc::ENOMEM), "ENOMEM: not enough memory");
        assert_eq!(text(libc::EBUSY), "EBUSY: resource busy or locked");
        // 未收录的 errno：libuv 的兜底文案（`UNKNOWN` 名）。
        assert_eq!(
            uv_error_text(&std::io::Error::from_raw_os_error(9999)).as_deref(),
            Some("UNKNOWN: Unknown system error -9999")
        );
        // 没有 errno 的错误没有 Node 对应物。
        assert!(uv_error_text(&std::io::Error::other("x")).is_none());
    }
}
