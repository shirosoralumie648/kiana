//! `exec.rs`：Durable `NodeExecutionEnv.exec` 的语义（docs/semantics.md），输出以 `output` 事件流式发送。
//!
//! TS 源：packages/env/daemon/src/exec.rs（原 daemon 本身就是 Rust；逐字移植）。
//! `exec`: Durable's `NodeExecutionEnv.exec` semantics (docs/semantics.md), output streamed as `output` events.
//!
//! # 这个模块负责什么
//! 收到一个 `exec` 请求后：按 `command`（字符串，交给 shell）或 `argv`（数组，直接执行）启动子进程，
//! 把 stdout/stderr 的字节流解码成文本、以 `EVENT` 帧流式发出，等待进程退出后返回 `{ exitCode }`。
//! 期间支持超时、取消（abort / kill）、完整输出的溢写（spill）和输出窗口合并（`window.rs`）。
//! 入口是 `run`：由 `main.rs` 的 `Server::dispatch` 在收到 `exec` 操作时调用；事件帧经 `Output`
//! 交给唯一的 stdout 写线程；取消来自 `main.rs` 对 CANCEL 帧的处理（`Control::cancel`）。
//!
//! # 一次执行的阶段
//! 1. 解析请求（`ExecRequest::from_json`）并注册取消通道（`Control::attach`）；若已 abort 直接返回 `aborted`；
//! 2. 确定程序与参数：`command` 走平台 shell（`sys::shell_config`），`argv` 直接执行；
//! 3. 校验工作目录存在，否则 `spawn_error`；
//! 4. 启动子进程；stdout/stderr 各由一个读线程读取，另有一个线程等待退出状态；
//! 5. 主循环从 mpsc 通道收集数据 / EOF / 退出 / 取消消息，负责解码、spill 与按窗口投递事件；
//! 6. 进程退出且两个管道 EOF 后（或静默宽限期到）收尾：停止读线程、冲刷解码器、按优先级判定结果。
//!
//! # 输出如何变成事件
//! 原始字节按流解码为 UTF-8（`crate::decode::StreamDecoder`，每条流一个，丢弃流开头的 BOM）。
//! 没有 `window` 时每块文本立即作为一个 `bulk` 事件；有 `window` 时先进入 `crate::window::Pending`，
//! 由窗口按字节/行合并、裁剪，并把被裁掉的量报告为 `skipped`。
//!
//! # 并发模型
//! - 两个读线程各自独占一个管道，保证单条流内部顺序；两条流之间的事件顺序取决于到达主循环的先后，
//!   不承诺固定的交错方式。
//! - `Control` 可在任意线程被 `cancel`：标志先置位，再通过 mpsc 把消息送到主循环；主循环对进程组发
//!   `SIGKILL`（Unix）或 `taskkill /T`（Windows），平台差异封装在 `sys::kill_tree`。
//! - 主循环是唯一产生本请求事件的地方；`Output` 的 control/bulk 队列再把帧交给唯一的 stdout 写线程。
//! - 两把互斥锁的临界区都很短：`Control` 的 `sender` 只在 `attach` / `cancel` 时持有；
//!   本模块只在插入与移除 pid 时持有 `Groups`；持锁期间不等待其他锁，也不阻塞在通道上。
//! - 无窗口时，读线程在全局未发送输出超过 4 MiB 时阻塞（`Output::wait_for_room`），形成反压，
//!   效果类似管道写满；有窗口时由主循环按 `unsent` 计数控制投递节奏。
//!
//! # 结果的判定顺序
//! 超时 > abort（`aborted`）> spill 失败（`unknown`）> 等待进程失败（`spawn_error`）> 正常退出码。
//! 这与 docs/semantics.md 中客户端的结果优先级一致：超时与取消都排在 spill 失败和退出码之前。
//! 被信号杀死的进程按 `128 + signal` 报告 `exitCode`（Unix；Windows 直接使用进程的 32 位退出码）；
//! kill（而非 abort）不改变结果的成功语义。
//!
//! # 注意事项
//! - `timeoutMs` 用 `Duration::try_from_secs_f64` 换算：负数、NaN 或过大的值返回
//!   `EINVAL`，不会 panic（守护进程不能因单个畸形请求退出）。
//! - 超时从子进程启动后开始计时，并且在命令结算前一直有效，包括退出后的输出收尾阶段。
//! - spill 在输出“超过”任一阈值时才启动（恰好等于不算）；启动前的块保存在内存 `prefix` 中，
//!   启动时先写入它们，因此文件包含从第一个字节起的完整原始输出。
//! - spill 失败后不再记录后续输出，并杀掉命令；在未超时、未 abort 时以 `unknown` 失败。

use crate::decode::StreamDecoder;
use crate::errors::Failure;
use crate::frame::{EVENT, Frame};
use crate::output::Output;
use crate::sys;
use crate::window::{Pending, Window};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// 进程退出后等待两个管道收尾的静默时长：超过这么久没有新输出就不再等（后代进程可能握着管道）。
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
/// How often a delivery waiting for the link to drain looks again.
const UNSENT_POLL: Duration = Duration::from_millis(20);

/// Commands this daemon started and has not reaped; killed when the connection ends.
///
/// 元素是子进程 pid。Unix 上子进程经 `setsid` 成为新会话/进程组的组长，因此 pid 同时是进程组 id，
/// `kill_group` 可以直接对整组发信号。
///
/// 生命周期：`run` 在 spawn 后插入，命令结算后移除；连接结束（`main::kill_all`）或 30 秒无输入时，
/// 守护进程遍历剩余元素逐个杀掉，避免远端残留命令。锁只保护集合本身，`run` 持锁时间很短。
pub type Groups = Arc<Mutex<HashSet<u32>>>;

/// 读线程、等待线程与主循环之间的消息。
///
/// `Data` / `Eof` 的 `usize` 是流编号：0 = stdout，1 = stderr，与 `names`、`decoders` 数组下标一致。
/// 通道是无界 mpsc：读线程不会因主循环处理慢而阻塞在 `send` 上。无窗口时反压由
/// `Output::wait_for_room` 实现；有窗口时不向读线程施加反压：超出窗口的未交付输出被裁剪并计为
/// `skipped`，只有发往客户端的帧按 `unsent`/`next_send` 限速。
pub enum Message {
    /// 某个流到达一段原始字节（块大小由平台层读缓冲决定，当前两种实现都是 64 KiB）。
    Data(usize, Vec<u8>),
    /// 某个流结束（管道 EOF）。
    Eof(usize),
    /// 子进程退出，携带退出状态。
    Exit(ExitStatus),
    /// 等待子进程本身失败（而不是子进程以非零状态退出）。
    WaitFailed(String),
    /// Abort: the command settles as `aborted`.
    Cancel,
    /// Kill without aborting, as `cleanup()` does: the command settles with the killed process's status.
    Kill,
}

/// Cancellation of one request, registered before the request runs so a cancel that arrives first is not lost.
///
/// 两种取消语义：
/// - abort（`cancel(false)`）：若请求尚未先超时/结束，则以 `aborted` 错误结束
///   （超时优先，见模块文档的结果判定顺序）；
/// - kill（`cancel(true)`）：只杀进程组，不改变结果语义，按被杀死进程的状态结算
///   （Unix 上被 `SIGKILL` 杀死时为 137）。
///
/// 标志先于消息写入：即使 `sender` 尚未 `attach`（请求还没开始跑）或通道发送失败，
/// 请求开始时也能看到这些标志并立即处理。`sender` 由 `Mutex` 保护，`attach` 只调用一次；
/// `cancel` 在持锁期间向无界通道发送，不会阻塞在接收方。
#[derive(Default)]
pub struct Control {
    /// 是否收到过 abort 语义的取消。
    pub aborted: AtomicBool,
    /// 是否收到过 kill 语义的取消（不产生 `aborted` 结果）。
    pub killed: AtomicBool,
    sender: Mutex<Option<Sender<Message>>>,
}

impl Control {
    /// 记录取消，并在已有接收者时把 `Cancel` / `Kill` 消息送入执行主循环。
    ///
    /// `kill = true` 对应协议中的 `{ mode: "kill" }`：只杀进程组，命令按被杀进程的退出状态结算。
    /// 重复取消是幂等的：标志重复置位；主循环内的 `kill` 闭包保证对进程组只发一次信号。
    pub fn cancel(&self, kill: bool) {
        let sender = self.sender.lock().unwrap();
        if kill {
            self.killed.store(true, Ordering::SeqCst);
        } else {
            self.aborted.store(true, Ordering::SeqCst);
        }
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send(if kill { Message::Kill } else { Message::Cancel });
        }
    }

    /// 绑定执行主循环的消息接收端；由 `run` 在启动时调用一次。
    ///
    /// 绑定之前发生的取消只会置位标志、消息被丢弃，因此 `run` 在使用前必须检查这两个标志。
    pub fn attach(&self, sender: Sender<Message>) {
        *self.sender.lock().unwrap() = Some(sender);
    }
}

/// 一个 `exec` 请求解析后的形式；字段名与协议 JSON 对应（见 docs/protocol.md 的 `exec` 一节）。
///
/// `command` 与 `argv` 二选一，两者都给时 `command` 优先（`run` 的匹配顺序）；`cwd` 必填。
/// `env` 只应用值为字符串的键（`sys::configure` 忽略其他类型）；`inherit_env` 决定是否继承守护进程
/// 自身环境；`timeout`、`spill`、`window` 为可选项。
pub struct ExecRequest {
    /// 要交给 shell 执行的命令字符串（与 `argv` 二选一）。
    pub command: Option<String>,
    /// 直接执行的参数数组：首元素是程序名，其余是参数（与 `command` 二选一）。
    pub argv: Option<Vec<String>>,
    /// 工作目录，必填；不存在时命令以 `spawn_error` 失败。
    pub cwd: String,
    /// 追加 / 覆盖的环境变量；值为非字符串的键在 `sys::configure` 中被忽略。
    pub env: Map<String, Value>,
    /// 是否继承守护进程自身的环境（协议字段 `inheritEnv`，默认 true）。
    pub inherit_env: bool,
    /// 指定 shell 路径（协议字段 `shellPath`）；路径不存在时失败为 `shell_unavailable`。
    pub shell_path: Option<String>,
    /// 整个命令（含输出收集）的超时；由协议字段 `timeoutMs` 换算而来。
    pub timeout: Option<Duration>,
    /// 完整输出的溢写阈值 `(afterBytes, afterLines)`；两个字段都必须是 u64 才生效。
    pub spill: Option<(u64, u64)>,
    /// 输出窗口；`window` 对象缺少任一字段时视为没有窗口。
    pub window: Option<Window>,
}

impl ExecRequest {
    /// 从请求 JSON 解析；缺少 `cwd` 时返回 `EINVAL`。
    ///
    /// 各字段采用“类型不符即视为缺失”的宽松解析：
    /// - `command` / `argv` 缺失或类型错误时为 `None`，两者都缺在 `run` 阶段报 `EINVAL`；
    /// - `argv` 中任一元素不是字符串会让整个 `argv` 变成 `None`（测试 `a_bad_argv_element_drops_the_whole_argv`），
    ///   而不是只丢弃那个元素；
    /// - `spill` 必须同时给出 `afterBytes` 与 `afterLines`，否则整体视为没有 spill；
    /// - `window` 由 `crate::window::Window::from_json` 解析，四个字段缺一不可；
    /// - `timeoutMs` 用 f64 换算，负数、NaN 或过大的值返回 `EINVAL`（不 panic）。
    pub fn from_json(json: &Value) -> Result<ExecRequest, Failure> {
        let strings = |value: &Value| -> Option<Vec<String>> {
            value
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect()
        };
        Ok(ExecRequest {
            command: json["command"].as_str().map(str::to_string),
            argv: strings(&json["argv"]),
            cwd: json["cwd"]
                .as_str()
                .ok_or_else(|| Failure::new("EINVAL", "exec needs cwd"))?
                .to_string(),
            env: json["env"].as_object().cloned().unwrap_or_default(),
            inherit_env: json["inheritEnv"].as_bool().unwrap_or(true),
            shell_path: json["shellPath"].as_str().map(str::to_string),
            timeout: match json["timeoutMs"].as_f64() {
                Some(ms) => {
                    let timeout = Duration::try_from_secs_f64(ms / 1000.0).map_err(|_| {
                        Failure::new(
                            "EINVAL",
                            "exec timeoutMs must be a non-negative finite number",
                        )
                    })?;
                    // `Duration` 能表示约 5.8e11 年，但 `Instant::now() + timeout` 只能
                    // 表示到平台的 `Instant` 上限；1e22ms 这类值能通过
                    // `try_from_secs_f64`，随后在 `Instant::now() + timeout` 处 panic。
                    // 这里把上限收紧到 10 年：既覆盖任何合理的命令超时，也保证后续
                    // `checked_add` 不会溢出。
                    const MAX_TIMEOUT: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);
                    if timeout > MAX_TIMEOUT {
                        return Err(Failure::new(
                            "EINVAL",
                            "exec timeoutMs must be a non-negative finite number",
                        ));
                    }
                    Some(timeout)
                }
                None => None,
            },
            spill: json["spill"].as_object().and_then(|spill| {
                Some((
                    spill.get("afterBytes")?.as_u64()?,
                    spill.get("afterLines")?.as_u64()?,
                ))
            }),
            window: Window::from_json(&json["window"]),
        })
    }
}

/// 杀掉一个命令的整个进程组。
///
/// 平台差异封装在 `sys::kill_tree`：Unix 对 `-pid` 发 `SIGKILL`（组不存在时退回只杀 pid），
/// Windows 用 `taskkill /F /T`。重复调用无害，进程已不存在时忽略错误。
pub fn kill_group(pid: u32) {
    sys::kill_tree(pid);
}

/// 生成一个随机的 version-4 UUID（小写十六进制），仅用于溢出文件名。
///
/// 第 7 字节高 4 位固定为 4（版本），第 9 字节高 2 位固定为 10（RFC 4122 变体）。
/// 随机字节来自 `sys::random_bytes`（其文档注明“不用于密钥”），因此不能当作安全令牌使用。
fn random_uuid() -> String {
    let mut bytes = sys::random_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Spill of the complete raw output, started once it crosses either threshold, like Node's.
///
/// 一个三态结构：
/// - **仅内存**（`file == None` 且 `failed == None`）：累计字节与行数，块保存在 `prefix`；
/// - **溢写中**（`file == Some`）：此后的块直接追加到文件；
/// - **失败**（`failed == Some`，优先级高于前两者）：无论文件是否已打开，都不再接收任何输出，
///   错误由 `run` 转成 `unknown` 结果。
///
/// 阈值判断用严格大于：恰好等于 `after_bytes` / `after_lines` 时仍留在内存，下一次 `push` 才启动文件。
struct Spill {
    /// 协议给出的字节阈值（`afterBytes`）。
    after_bytes: u64,
    /// 协议给出的行数阈值（`afterLines`）。
    after_lines: u64,
    /// 启动溢写前已经看到的原始字节数。
    seen_bytes: u64,
    /// 启动溢写前已经看到的换行符数量。
    seen_newlines: u64,
    /// 启动溢写前暂存的块，用于在文件建好后从头补齐。
    prefix: Vec<Vec<u8>>,
    /// 已打开的溢写文件；`None` 表示尚未启动。
    file: Option<File>,
    /// 溢写文件路径；仅在成功启动后存在。
    path: Option<String>,
    /// 第一次失败的错误文本；一旦置位，后续输出都被丢弃。
    failed: Option<String>,
}

impl Spill {
    /// 记录一块原始输出，必要时启动溢写。
    ///
    /// 失败之后或空块直接忽略：空块不能触发溢写（`afterLines: 0` 时尤其重要）；
    /// 失败后直接返回，不做任何 IO，也不再改变已记录的第一个错误。
    ///
    /// 行数把“未以换行结尾的最后一段”也算作一行（`lines` 在换行数上多加了 1），
    /// 因此 `afterLines: 0` 时第一块就触发，`afterLines: 1` 时恰好一整行仍不触发（测试锁定）。
    fn push(&mut self, chunk: &[u8], tmpdir: &str) {
        if self.failed.is_some() || chunk.is_empty() {
            return;
        }
        if self.file.is_none() {
            self.seen_bytes += chunk.len() as u64;
            self.seen_newlines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
            let lines = self.seen_newlines + u64::from(chunk.last() != Some(&b'\n'));
            if self.seen_bytes <= self.after_bytes && lines <= self.after_lines {
                self.prefix.push(chunk.to_vec());
                return;
            }
            if let Err(error) = self.start(tmpdir) {
                self.failed = Some(error);
                return;
            }
        }
        if let Some(file) = &mut self.file
            && let Err(error) = file.write_all(chunk)
        {
            self.failed = Some(error.to_string());
        }
    }

    /// `createTempFile({ prefix: "kiana-output-", suffix: ".log" })`: a fresh `tmp-` directory holding the file.
    ///
    /// 先 `sys::mkdtemp("<tmpdir>/tmp-")` 建一个全新目录，再在其中创建 `kiana-output-<uuid>.log`，
    /// 与 Node `createTempFile` 的路径规则对齐；随后把启动前缓存的 `prefix` 按原顺序写入，
    /// 保证文件从输出的第一个字节开始完整。任何一步失败都返回错误文本，由调用方记入 `failed`。
    fn start(&mut self, tmpdir: &str) -> Result<(), String> {
        let prefix = Path::new(tmpdir)
            .join("tmp-")
            .to_string_lossy()
            .into_owned();
        let directory = sys::mkdtemp(&prefix).map_err(|error| error.to_string())?;
        let path = Path::new(&directory)
            .join(format!("kiana-output-{}.log", random_uuid()))
            .to_string_lossy()
            .into_owned();
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        for chunk in self.prefix.drain(..) {
            file.write_all(&chunk).map_err(|error| error.to_string())?;
        }
        self.file = Some(file);
        self.path = Some(path);
        Ok(())
    }
}

/// 按请求的 `cwd`、stdio 策略启动程序；参数、环境与平台特有的进程设置交给
/// `sys::configure`（Unix 的 `setsid`、Windows 的创建标志）和 `sys::after_spawn`（Windows Job Object）。
///
/// stdin 只在“命令通过 stdin 交给 shell”（旧版 WSL bash，`shell.command_on_stdin`）时 piped，
/// 其余情况为 `Stdio::null()`：守护进程的 stdin 是协议帧输入，绝不能被命令读取。
/// stdout/stderr 始终 piped，由 `spawn_reader` 消费。
fn spawn(
    request: &ExecRequest,
    program: &str,
    args: &[String],
    piped_stdin: bool,
) -> std::io::Result<Child> {
    let mut command = Command::new(program);
    command
        .current_dir(&request.cwd)
        .stdin(if piped_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sys::configure(&mut command, args, &request.env, request.inherit_env);
    let child = command.spawn()?;
    sys::after_spawn(&child);
    Ok(child)
}

/// Read one pipe on its own thread. Without an output window, reading waits while too much output is unsent.
///
/// 每读到一块就发送 `Message::Data`，管道结束（或 `stop` 置位、回调返回 false）后发送 `Message::Eof`。
/// `stop` 由主循环在收尾时置位：Unix 上 `read_pipe` 每 100 ms 轮询一次，Windows 上
/// `sys::interrupt_reader` 用 `CancelSynchronousIo` 打断阻塞读。回调返回 `false` 表示主循环已退出
/// （`send` 失败），读线程随即停止。传入 `output` 时，每块之后等待未发送输出降到 4 MiB 以下，形成反压。
fn spawn_reader(
    stream: usize,
    pipe: impl sys::Pipe + Send + 'static,
    sender: Sender<Message>,
    stop: Arc<AtomicBool>,
    output: Option<Arc<Output>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        sys::read_pipe(pipe, &stop, |bytes| {
            let delivered = sender.send(Message::Data(stream, bytes.to_vec())).is_ok();
            if let Some(output) = &output {
                output.wait_for_room(&stop);
            }
            delivered
        });
        let _ = sender.send(Message::Eof(stream));
    })
}

/// 构造一个 `kind: "output"` 的 `EVENT` 帧。
///
/// JSON 只放元数据：流名（`stdout` / `stderr`）与可选的 `skipped` 计数；文本本身作为 payload 原始字节，
/// 避免 JSON 转义。`skipped` 存在时表示这之前的输出因窗口被丢弃，payload 只含丢弃点之后的文本。
fn output_frame(id: u32, stream: &str, text: String, skipped: Option<Value>) -> Frame {
    let mut json = json!({ "kind": "output", "stream": stream });
    if let Some(skipped) = skipped {
        json["skipped"] = skipped;
    }
    Frame::with_payload(EVENT, id, json, text.into_bytes())
}

/// Run one command, sending output events for request `id`, until it settles.
///
/// # 输入
/// `request` 已解析；`tmpdir` 是 spill 文件所在临时目录；`output` 是 stdout 帧队列；
/// `control` 提供取消状态与消息通道；`groups` 记录在跑的命令 pid，供连接结束时统一清理。
///
/// # 执行阶段
/// 1. 建立消息通道并 `attach`；若已 abort 直接返回 `aborted`（取消可能早于本函数开始执行）；
/// 2. 解析程序：字符串命令交给 shell（`shellPath` 不存在则 `shell_unavailable`），
///    argv 直接执行（空数组 `spawn_error`，两者都缺 `EINVAL`）；
/// 3. 检查 `cwd` 存在，否则 `spawn_error`；
/// 4. 启动进程，必要时把命令写到它的 stdin，并把 pid 登记到 `groups`；
/// 5. 启动两个读线程与一个等待线程；主循环负责解码、spill、窗口投递与超时/取消判定；
/// 6. 收尾：停止读线程、注销 pid、冲刷解码器、投递剩余事件，按优先级返回结果。
///
/// # 失败与副作用
/// - 失败以 `Failure` 返回，由 `main::respond` 转成 ERROR 帧；只有 timeout 与 aborted 在 spill
///   已建立时会附带 `spillPath`；
/// - 超时、abort 会杀掉整个进程组；spill 失败也会杀掉命令（避免继续产生无法保存的输出）；
/// - 无论成功失败都会把 pid 从 `groups` 移除，并通知两个读线程停止（置 `stop` + `interrupt_reader`，
///   不 join 线程）；
/// - 事件通过 `Output` 队列异步写出：返回时已把剩余事件入队，但不等待对端读取。
///
/// # 关键边界
/// - 子进程退出后仍等待两个管道 EOF；若后代进程握着管道不放，则在最后一次输出或退出后
///   `EXIT_STDIO_GRACE`（100 ms）内没有新数据时放弃（`idle_until`）；
/// - 超时计时器在命令结算前一直有效，包括退出后的收尾阶段；
/// - 无窗口时输出不合并，靠 `Output::wait_for_room` 反压；有窗口时按 `unsent` 计数分批投递。
pub fn run(
    id: u32,
    request: ExecRequest,
    tmpdir: &str,
    output: &Arc<Output>,
    control: &Control,
    groups: &Groups,
) -> Result<Value, Failure> {
    let (sender, receiver) = mpsc::channel();
    control.attach(sender.clone());
    // 取消可能在本函数开始执行之前就到达：标志已经置位而消息无处投递，这里不再启动进程。
    if control.aborted.load(Ordering::SeqCst) {
        return Err(Failure::new("aborted", "aborted"));
    }
    // A string runs through the shell (as its last argument, or on stdin for legacy WSL bash); argv runs directly.
    // 两者都提供时 `command` 优先（匹配顺序）；`argv` 是空数组时没有程序可执行。
    let (program, args, stdin_command) = match (&request.command, &request.argv) {
        (Some(command), _) => {
            let shell = sys::shell_config(request.shell_path.as_deref())?;
            let mut args = shell.args;
            if shell.command_on_stdin {
                (shell.program, args, Some(command.clone()))
            } else {
                args.push(command.clone());
                (shell.program, args, None)
            }
        }
        (None, Some(argv)) => match argv.split_first() {
            Some((first, rest)) => (
                sys::resolve_program(first, &request.cwd, &request.env)?,
                rest.to_vec(),
                None,
            ),
            None => return Err(Failure::new("spawn_error", "Empty argv: no program to run")),
        },
        (None, None) => return Err(Failure::new("EINVAL", "exec needs command or argv")),
    };
    // 与 Node 一致：先检查工作目录存在，不存在时给出固定文案的 spawn_error，而不是等 spawn 失败。
    if !Path::new(&request.cwd).exists() {
        return Err(Failure::new(
            "spawn_error",
            format!(
                "Working directory does not exist: {}\nCannot execute bash commands.",
                request.cwd
            ),
        ));
    }
    let mut child = spawn(&request, &program, &args, stdin_command.is_some())
        .map_err(|error| Failure::new("spawn_error", error.to_string()))?;
    // 旧版 WSL bash 通过 stdin 接收命令；写入失败被忽略，stdin 在本作用域结束时关闭，shell 会读到 EOF。
    if let (Some(command), Some(mut stdin)) = (stdin_command, child.stdin.take()) {
        let _ = stdin.write_all(command.as_bytes());
    }
    let pid = child.id();
    groups.lock().unwrap().insert(pid);
    // 超时从进程启动后开始计时（不是从收到请求时开始）。
    let deadline = request.timeout.map(|timeout| Instant::now() + timeout);
    let stop = Arc::new(AtomicBool::new(false));
    // Without a window, output is not coalesced; reading then waits for the link.
    // 有窗口时读线程不阻塞：输出交给主循环按窗口节奏投递，阻塞会同时拖慢 spill 与退出检测。
    let gate = if request.window.is_none() {
        Some(output.clone())
    } else {
        None
    };
    // stdout 与 stderr 各一个读线程：一个管道只有一个读者，因此单条流内部的事件顺序得到保证。
    // 等待退出用独立线程，因为 `Child::wait` 会阻塞，而主循环必须继续处理输出与取消消息。
    let readers = [
        spawn_reader(
            0,
            child.stdout.take().unwrap(),
            sender.clone(),
            stop.clone(),
            gate.clone(),
        ),
        spawn_reader(
            1,
            child.stderr.take().unwrap(),
            sender.clone(),
            stop.clone(),
            gate,
        ),
    ];
    let waiter = sender.clone();
    thread::spawn(move || {
        let _ = waiter.send(match child.wait() {
            Ok(status) => Message::Exit(status),
            Err(error) => Message::WaitFailed(error.to_string()),
        });
    });
    // `Control::attach` 仍保存着一个 sender 克隆（用于转发取消），所以运行期间通道不会因所有
    // 发送端消失而断开；这里 drop 只是不再增加引用。
    drop(sender);

    // 每条流一个解码器：跨块的多字节字符状态互不影响，且各自丢弃流开头的 BOM。
    let mut decoders = [StreamDecoder::new(), StreamDecoder::new()];
    // spill 状态只在请求同时给出两个阈值时创建。
    let mut spill = request.spill.map(|(after_bytes, after_lines)| Spill {
        after_bytes,
        after_lines,
        seen_bytes: 0,
        seen_newlines: 0,
        prefix: Vec::new(),
        file: None,
        path: None,
        failed: None,
    });
    // 有窗口时输出先进入 `Pending` 合并、裁剪；无窗口时 `emit` 直接 bulk 发送。
    let mut pending = request.window.map(Pending::new);
    // 窗口投递的“未写完帧”计数：归零才投递下一批，避免慢链路堆积输出。
    let unsent = Arc::new(AtomicUsize::new(0));
    // 流编号到协议字段的映射；下标与 `Message::Data` / `Eof` 携带的编号一致。
    let names = ["stdout", "stderr"];
    let emit = |pending: &mut Option<Pending>, stream: usize, text: String| {
        if text.is_empty() {
            return;
        }
        match pending {
            Some(pending) => pending.push(names[stream], text),
            None => output.bulk(output_frame(id, names[stream], text, None), None),
        }
    };
    let deliver = |pending: &mut Pending| {
        // `take` 在有 skipped 时把所有段折叠成一个事件，否则按流分段返回，因此这里可能发出多个帧。
        for event in pending.take() {
            output.bulk(
                output_frame(id, event.stream, event.text, event.skipped),
                Some(unsent.clone()),
            );
        }
    };
    let mut ended = [false, false];
    let mut status: Option<Result<ExitStatus, String>> = None;
    let mut idle_until: Option<Instant> = None;
    let mut timed_out = false;
    let mut aborted = false;
    let mut killed = false;
    // 用局部布尔去重：重复的 Cancel / Kill 消息只对进程组发一次信号。
    let kill = |killed: &mut bool| {
        if !*killed {
            *killed = true;
            kill_group(pid);
        }
    };
    // kill 可能早于本请求开始（标志已在 `Control` 中置位）：spawn 之后立即补杀。
    if control.killed.load(Ordering::SeqCst) {
        kill(&mut killed);
    }
    // 主循环是唯一的控制点：所有等待都带超时，以便重新检查截止时间、静默宽限和窗口发送时刻。
    loop {
        let now = Instant::now();
        // Node's timer stays armed until the command settles, including the grace period after exit.
        if !timed_out && deadline.is_some_and(|deadline| now >= deadline) {
            timed_out = true;
            kill(&mut killed);
        }
        // 正常结算：进程已退出，且 stdout、stderr 都已读到 EOF（与 Node 等流结束一致）。
        if status.is_some() && ended[0] && ended[1] {
            break;
        }
        if idle_until.is_some_and(|idle| now >= idle) {
            // A descendant may hold the pipes open after the process exited; stop waiting for it.
            // 静默宽限到期：不再等待可能仍握着管道的后代进程。
            break;
        }
        if let Some(pending) = &mut pending
            && !pending.is_empty()
            && now >= pending.next_send()
            && unsent.load(Ordering::SeqCst) == 0
        {
            // 窗口到点且上一批帧已全部写出，才投递下一批。
            deliver(pending);
        }
        // 本次接收最多等最近的事件：截止时间、静默宽限、窗口发送时刻；上一批未写完时改用短轮询。
        let mut wait = Duration::from_secs(3600);
        if let Some(deadline) = deadline.filter(|_| !timed_out) {
            wait = wait.min(deadline.saturating_duration_since(now));
        }
        if let Some(idle) = idle_until {
            wait = wait.min(idle.saturating_duration_since(now));
        }
        if let Some(pending) = pending.as_ref().filter(|pending| !pending.is_empty()) {
            wait = wait.min(if unsent.load(Ordering::SeqCst) > 0 {
                UNSENT_POLL
            } else {
                pending.next_send().saturating_duration_since(now)
            });
        }
        match receiver.recv_timeout(wait) {
            Ok(Message::Data(stream, bytes)) => {
                // 先解码（可能暂存半个字符），再让 spill 记录原始字节；两者互不影响。
                emit(&mut pending, stream, decoders[stream].decode(&bytes, false));
                if let Some(spill) = &mut spill {
                    spill.push(&bytes, tmpdir);
                    if spill.failed.is_some() {
                        // 无法保存完整输出：杀掉命令，避免继续产生无法留存的输出；
                        // 收尾时以 `unknown` 失败（除非超时或 abort 的优先级更高）。
                        kill(&mut killed);
                    }
                }
                if status.is_some() {
                    // 进程已退出但管道仍有数据：每收到一块就顺延静默宽限。
                    idle_until = Some(Instant::now() + EXIT_STDIO_GRACE);
                }
            }
            Ok(Message::Eof(stream)) => ended[stream] = true,
            Ok(Message::Exit(exit)) => {
                status = Some(Ok(exit));
                // 退出后仍收集剩余输出，最多再等 100 ms 没有新数据。
                idle_until = Some(Instant::now() + EXIT_STDIO_GRACE);
            }
            Ok(Message::WaitFailed(error)) => {
                status = Some(Err(error));
                // `wait` 本身失败：不再等管道，立即进入收尾。
                idle_until = Some(Instant::now());
            }
            Ok(Message::Cancel) => {
                // abort：记录结果语义并杀掉进程组；循环继续，直到进程退出与管道收尾完成。
                aborted = true;
                kill(&mut killed);
            }
            Ok(Message::Kill) => kill(&mut killed),
            Err(RecvTimeoutError::Timeout) => {}
            // 所有发送端都已消失；正常运行期间 `Control` 保存的克隆会让通道保持连通。
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Like Node destroying its streams: stop reading, so the pipes close even if a descendant still holds them.
    stop.store(true, Ordering::SeqCst);
    for reader in &readers {
        sys::interrupt_reader(reader);
    }
    // 命令已结算：从全局集合移除，连接收尾时不必再杀它。
    groups.lock().unwrap().remove(&pid);
    // 冲刷解码器中可能残留的不完整 UTF-8 序列（变成 U+FFFD），再投递窗口里最后的内容。
    emit(&mut pending, 0, decoders[0].decode(&[], true));
    emit(&mut pending, 1, decoders[1].decode(&[], true));
    if let Some(pending) = &mut pending
        && !pending.is_empty()
    {
        deliver(pending);
    }
    let spill_path = spill.as_ref().and_then(|spill| spill.path.clone());
    let with_spill = |failure: Failure| match &spill_path {
        Some(path) => failure.extra(json!({ "spillPath": path })),
        None => failure,
    };
    // 结果优先级：超时 > abort > spill 失败 > wait 失败 > 退出码。
    // 超时与 abort 在 spill 已建立时附带 spillPath，便于客户端定位完整输出。
    if timed_out {
        return Err(with_spill(Failure::new("timeout", "timeout")));
    }
    if aborted {
        return Err(with_spill(Failure::new("aborted", "aborted")));
    }
    if let Some(error) = spill.as_ref().and_then(|spill| spill.failed.clone()) {
        return Err(Failure::new(
            "unknown",
            format!("Failed to preserve complete shell output: {error}"),
        ));
    }
    let status = match status {
        Some(Ok(status)) => status,
        Some(Err(error)) => return Err(Failure::new("spawn_error", error)),
        None => {
            return Err(Failure::new(
                "spawn_error",
                "process did not report its exit",
            ));
        }
    };
    let mut result = json!({ "exitCode": sys::exit_code(status) });
    if let Some(path) = spill_path {
        result["spillPath"] = json!(path);
    }
    Ok(result)
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    #[test]
    fn requests_take_the_documented_defaults() {
        let request = ExecRequest::from_json(&json!({ "cwd": "/w" })).unwrap();
        assert_eq!(request.command, None);
        assert_eq!(request.argv, None);
        assert_eq!(request.cwd, "/w");
        assert!(request.env.is_empty());
        assert!(request.inherit_env, "inheritEnv defaults to true");
        assert_eq!(request.shell_path, None);
        assert_eq!(request.timeout, None);
        assert_eq!(request.spill, None);
        assert!(request.window.is_none());

        let failure = ExecRequest::from_json(&json!({})).err().unwrap().to_json();
        assert_eq!(failure["code"], json!("EINVAL"));
        assert_eq!(failure["message"], json!("exec needs cwd"));

        let request = ExecRequest::from_json(&json!({
            "command": "echo hi",
            "argv": ["sh", "-c", "true"],
            "cwd": "/w",
            "env": { "B": "2", "A": "1" },
            "inheritEnv": false,
            "shellPath": "/bin/zsh",
            "timeoutMs": 1500,
            "spill": { "afterBytes": 10, "afterLines": 2 },
            "window": { "maxBytes": 4, "maxLines": 1, "minIntervalMs": 20, "bytesPerSecond": 1000 },
        }))
        .unwrap();
        assert_eq!(request.command.as_deref(), Some("echo hi"));
        assert_eq!(request.argv.unwrap(), ["sh", "-c", "true"]);
        assert!(!request.inherit_env);
        assert_eq!(request.shell_path.as_deref(), Some("/bin/zsh"));
        assert_eq!(request.timeout, Some(Duration::from_millis(1500)));
        assert_eq!(request.spill, Some((10, 2)));
        assert!(request.window.is_some());
        // The env map keeps the request's key order (this workspace enables serde_json's
        // `preserve_order`; the reference daemon's serde_json sorts these keys instead, which only
        // changes the order the child's environment is passed in, not its contents).
        let keys: Vec<&String> = request.env.keys().collect();
        assert_eq!(keys, ["B", "A"]);
    }

    #[test]
    fn a_bad_argv_element_drops_the_whole_argv() {
        // Any non-string makes `argv` None, so the request fails as "needs command or argv".
        let request = ExecRequest::from_json(&json!({ "cwd": "/w", "argv": ["sh", 7] })).unwrap();
        assert_eq!(request.argv, None);
        let request = ExecRequest::from_json(&json!({ "cwd": "/w", "argv": "sh" })).unwrap();
        assert_eq!(request.argv, None);
        let request = ExecRequest::from_json(&json!({ "cwd": "/w", "argv": [] })).unwrap();
        assert_eq!(request.argv, Some(Vec::new()));
    }

    #[test]
    fn a_spill_needs_both_thresholds() {
        let request =
            ExecRequest::from_json(&json!({ "cwd": "/w", "spill": { "afterBytes": 1 } })).unwrap();
        assert_eq!(request.spill, None);
        let request = ExecRequest::from_json(
            &json!({ "cwd": "/w", "spill": { "afterBytes": 1, "afterLines": "2" } }),
        )
        .unwrap();
        assert_eq!(request.spill, None);
        let request = ExecRequest::from_json(&json!({ "cwd": "/w", "spill": 3 })).unwrap();
        assert_eq!(request.spill, None);
    }

    #[test]
    fn a_fractional_timeout_is_seconds_in_floating_point() {
        let request = ExecRequest::from_json(&json!({ "cwd": "/w", "timeoutMs": 300.5 })).unwrap();
        let timeout = request.timeout.unwrap();
        assert_eq!(timeout.as_secs_f64(), 0.3005);
    }

    #[test]
    fn invalid_timeouts_are_einval_instead_of_panicking() {
        for timeout in [json!(-1), json!(1e300), json!(1e22), json!(9.3e21)] {
            let failure =
                match ExecRequest::from_json(&json!({ "cwd": "/w", "timeoutMs": timeout })) {
                    Err(failure) => failure,
                    Ok(_) => panic!("invalid timeout must fail"),
                };
            assert_eq!(failure.code, "EINVAL");
        }
    }

    #[test]
    fn cancel_sets_the_flag_and_forwards_the_message() {
        let (sender, receiver) = mpsc::channel();
        let control = Control::default();
        control.attach(sender);
        control.cancel(false);
        assert!(control.aborted.load(Ordering::SeqCst));
        assert!(!control.killed.load(Ordering::SeqCst));
        assert!(matches!(receiver.try_recv(), Ok(Message::Cancel)));

        let (sender, receiver) = mpsc::channel();
        let control = Control::default();
        control.attach(sender);
        control.cancel(true);
        assert!(control.killed.load(Ordering::SeqCst));
        assert!(!control.aborted.load(Ordering::SeqCst));
        assert!(matches!(receiver.try_recv(), Ok(Message::Kill)));

        // Without an attached sender, cancelling only flips the flags.
        let control = Control::default();
        control.cancel(false);
        assert!(control.aborted.load(Ordering::SeqCst));
    }

    #[test]
    fn uuids_are_version_four_and_lowercase_hex() {
        let uuid = random_uuid();
        assert_eq!(uuid.len(), 36);
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert!(uuid.chars().all(|c| !c.is_ascii_uppercase()));
        assert_eq!(&uuid[14..15], "4", "version nibble");
        assert!(
            matches!(&uuid[19..20], "8" | "9" | "a" | "b"),
            "variant nibble"
        );
        assert_ne!(uuid, random_uuid());
    }

    fn new_spill(after_bytes: u64, after_lines: u64) -> Spill {
        Spill {
            after_bytes,
            after_lines,
            seen_bytes: 0,
            seen_newlines: 0,
            prefix: Vec::new(),
            file: None,
            path: None,
            failed: None,
        }
    }

    #[test]
    fn a_spill_below_both_thresholds_keeps_a_prefix_only() {
        let dir = std::env::temp_dir().join(format!("kiana-env-spill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmpdir = dir.to_string_lossy().into_owned();
        let mut kept = new_spill(100, 100);
        kept.push(b"abc", &tmpdir);
        kept.push(b"def\n", &tmpdir);
        assert_eq!(kept.prefix, [b"abc".to_vec(), b"def\n".to_vec()]);
        assert!(kept.file.is_none());
        assert_eq!(kept.seen_bytes, 7);
        assert_eq!(kept.seen_newlines, 1);
        assert!(kept.failed.is_none());
        assert_eq!(kept.path, None);
        // An empty chunk is ignored, so it cannot start the spill.
        let mut empty = new_spill(0, 0);
        empty.push(b"", &tmpdir);
        assert!(empty.file.is_none());
        assert_eq!(empty.seen_bytes, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spill_starts_once_a_threshold_is_crossed_and_writes_everything_after() {
        let dir = std::env::temp_dir().join(format!("kiana-env-spill2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmpdir = dir.to_string_lossy().into_owned();
        let mut over = new_spill(5, 100);
        over.push(b"abc", &tmpdir);
        assert!(over.file.is_none(), "below afterBytes");
        // The third byte of "def" crosses afterBytes: the prefix is flushed and the whole stream is kept.
        over.push(b"def", &tmpdir);
        let path = over.path.clone().expect("spill file");
        assert!(path.contains("/tmp-"), "in a fresh tmp- directory: {path}");
        assert!(path.contains("kiana-output-"));
        assert!(path.ends_with(".log"));
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        over.push(b"ghi", &tmpdir);
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdefghi");
        // An unterminated last chunk counts as one more line: with `afterLines: 0` the very first chunk starts
        // the spill, and with `afterLines: 1` a whole line is exactly at the limit while the next chunk crosses it.
        let mut no_lines = new_spill(1000, 0);
        no_lines.push(b"x", &tmpdir);
        assert!(no_lines.file.is_some(), "afterLines 0 starts at once");
        let mut whole_line = new_spill(1000, 1);
        whole_line.push(b"a\n", &tmpdir);
        assert!(
            whole_line.file.is_none(),
            "one terminated line is exactly the limit"
        );
        whole_line.push(b"b", &tmpdir);
        assert!(
            whole_line.file.is_some(),
            "the unterminated next line crosses the limit"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spill_in_a_missing_tmpdir_records_the_failure() {
        let mut broken = new_spill(0, 0);
        broken.push(b"abc", "/nonexistent-kiana-env-tmpdir");
        assert!(broken.file.is_none());
        assert!(
            broken
                .failed
                .as_deref()
                .is_some_and(|error| !error.is_empty()),
            "the error text is kept: {:?}",
            broken.failed
        );
    }

    #[test]
    fn output_frames_carry_the_stream_and_optional_skipped_counts() {
        let frame = output_frame(5, "stdout", "hi".to_string(), None);
        assert_eq!(frame.kind, crate::frame::EVENT);
        assert_eq!(frame.id, 5);
        assert_eq!(frame.json, json!({ "kind": "output", "stream": "stdout" }));
        assert_eq!(frame.payload, b"hi");
        let skipped = json!({ "bytes": 3, "newlines": 1, "endsWithNewline": false });
        let frame = output_frame(6, "stderr", "x".to_string(), Some(skipped.clone()));
        assert_eq!(
            frame.json,
            json!({ "kind": "output", "stream": "stderr", "skipped": skipped })
        );
        // This daemon's insertion order (serde_json with `preserve_order`); the reference daemon's
        // serde_json sorts keys on the wire (`kind`, `skipped`, `stream`). Key order is not part of
        // the wire contract: clients parse the JSON object.
        let keys: Vec<&String> = frame.json.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["kind", "stream", "skipped"]);
    }
}
