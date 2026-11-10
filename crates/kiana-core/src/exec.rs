//! core/exec.ts。
//!
//! 逐字移植 `packages/coding-agent/src/core/exec.ts`（116 行）与 `spec/ca-session.md` §2.10。
//!
//! ## 唯一权威定义（跨文档冲突，见交付说明）
//!
//! `ARCHITECTURE.md` §3.5 把 `ExecOptions`/`ExecResult` 指派给 `kiana-ext-api::tool_result`，
//! 但 `kiana-ext-api`（L7）依赖 `kiana-core`（L3），且 `exec_command` 的实现必须在本 crate。
//! 因此**唯一的 `struct` 定义在这里**；下游 `kiana-ext-api::tool_result` 必须
//! `pub use kiana_core::exec::{ExecOptions, ExecResult};`，不得重复定义（workspace 单一定义规则）。
//!
//! ## 签名
//!
//! TS 的 `execCommand` 是 `async`；冻结的 Rust 签名是**同步**的
//! （`-> ExecResult`，永不 reject / 永不 `Result`）。实现用 `std::process::Command` +
//! 主线程轮询（非阻塞读两个管道）驱动，语义与 TS 对齐（增量 UTF-8 解码、`killed` 标志、
//! timeout/abort 的 SIGTERM、`waitForChildProcess` 的 100ms 空闲宽限）。
//!
//! `ExecOptions.cwd` 在 TS 源码里**从不读取**（真正生效的是第 3 个位置参数 `cwd`）：
//! Rust 侧保留字段但同样忽略，**不要**把它接到 `Command::current_dir`。
//!
//! ## 执行流程
//!
//! 1. 空命令名：直接返回 Node 风格的错误结果，不 spawn（见下节边界）。
//! 2. `Command` 设置 stdin=null、stdout/stderr 为管道后 spawn；失败立即返回 `code = 1`。
//! 3. **存活期主循环**：每 2ms 先 `try_wait`，再检查 cancel/timeout（必要时发 SIGTERM），
//!    然后非阻塞排空两个管道并交给增量 UTF-8 解码器。
//! 4. **退出后宽限循环**：退出瞬间武装 100ms 空闲计时器，读到数据就重置；两流都结束或
//!    计时到点即停（#5303 的孙进程尾部输出语义）。
//! 5. finalize：关闭两个读端（等价 `stream.destroy()`），flush 解码器尾部，按
//!    `(exit, wait_error)` 组装 [`ExecResult`]。
//!
//! ## finalize 关闭管道（`stream.destroy()`）
//!
//! `waitForChildProcess` 的 `finalize` 会 `stdout.destroy()`/`stderr.destroy()`。Rust 侧在
//! 返回前显式 `PipePump::destroy`（关闭读端 fd），因此：
//! - 退出后仍持有继承管道的孙进程在**下一次写入**时收到 EPIPE/SIGPIPE（关闭读端的直接结果）；
//! - 不会为每次调用泄漏读线程/fd（Unix 上没有读线程）。
//!
//! ## SIGTERM 之后 5s 升级为 SIGKILL
//!
//! TS 的 `killProcess` 在 SIGTERM 后安排了一个 `if (!proc.killed) proc.kill("SIGKILL")`
//! 的 5s 定时器；但 Node 在 `kill()` 成功时就把 `proc.killed` 置为 `true`，所以该分支
//! **永远不会执行**（源码注释的“5 秒后强杀”意图与实际行为不符）。忽略 SIGTERM 的子进程
//! 会让 TS 的 `execCommand` 永不 resolve。Rust 侧不能复制这个挂死行为：超时/取消后的
//! SIGTERM 再等 [`SIGKILL_GRACE`]，仍存活就发 SIGKILL（SIGKILL 不可捕获，保证调用最终
//! 返回）。`killed` 仍然只表示“触发过 timeout/abort”，不区分实际发出的信号种类。
//!
//! ## 与 Node 的边界对齐
//!
//! - 空命令名：Node 同步抛 `TypeError [ERR_INVALID_ARG_VALUE]`，Rust 签名不能抛错 →
//!   返回 `{stderr: "The argument 'file' cannot be empty. Received ''", code: 1, killed: false}`。
//! - 空 `cwd`：Node 的异步 `spawn` 把 `cwd: ""` 当作未设置（在进程 cwd 里运行），Rust 同样
//!   跳过 `current_dir`。
//!
//! ## 退出后的 100ms 宽限（#5303）
//!
//! TS 的 `waitForChildProcess` 在收到 `exit` 时**重新武装**空闲计时器，之后每个 `data`
//! 块再重置；timeout/abort 监听器直到 wait resolve 才清理。本实现同样：
//!
//! - 进程退出的瞬间把宽限 deadline 设为 `now + 100ms`（与退出前的静默时长无关）；
//! - 宽限期内每读到一块数据就重置 deadline；
//! - 宽限期内 timeout/abort 触发时仍把 `killed` 置为 `true`（TS 的 killProcess 会执行，
//!   只是对已退出的进程发 SIGTERM 没有副作用；Rust 侧已 reap，不再发信号以免 pid 复用）。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::child_process::{
    EMPTY_COMMAND_MESSAGE, EXIT_STDIO_GRACE_MS, PipePump, node_spawn_error,
};

/// `ExecOptions`：执行选项。`cwd` 字段与 TS 一样**不被读取**（见模块文档）。
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// 取消信号（TS 的 `AbortSignal`）。已取消时主循环在下一轮（约 2ms 内）对仍存活的
    /// 子进程发 SIGTERM 并置 `killed = true`；子进程可捕获/忽略该信号。
    pub signal: Option<CancellationToken>,
    /// 超时（毫秒），从 `spawn` 成功后开始计时。`0`/`None` 表示不设超时。
    pub timeout: Option<u64>,
    /// TS 保留了该字段但从不读取；Rust 同样忽略它（真正生效的是 `exec_command` 的 `cwd` 参数）。
    pub cwd: Option<PathBuf>,
}

/// `ExecResult`：执行结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecResult {
    /// stdout 的 UTF-8 流式解码结果；非法字节替换为 U+FFFD（见 `Utf8StreamDecoder`）。
    pub stdout: String,
    /// stderr 的 UTF-8 流式解码结果。
    pub stderr: String,
    /// 退出码；子进程被信号终止（`ExitStatus::code()` 为 `None`）时为 `0`
    /// （测试 `timeout_kills_the_process` 注释：Node 的 exit code 是 `null` 时映射为 0）。
    /// 启动失败或 `wait` 出错时为 `1`。
    pub code: i32,
    /// timeout/abort 的触发标志。各条路径的取值不同：
    ///
    /// - 空命令名：恒为 `false`（直接返回，不检查 signal/timeout）；
    /// - `spawn` 失败：取 `signal.is_cancelled()`（没有子进程，也就没有发过信号）；
    /// - 存活期主循环：`try_wait` 确认子进程仍存活后，timeout 到期或 signal 已取消时置
    ///   `true` 并发送 SIGTERM；若这一轮 `try_wait` 已报告退出，则直接跳出、不置位；
    ///   等待 [`SIGKILL_GRACE`] 后仍存活则升级为 SIGKILL；
    /// - 退出后的宽限循环：触发时只置 `true`、不再发信号。正常退出路径下进程已被 reap；
    ///   `try_wait` 出错转入宽限循环时进程状态未知，同样不会收到信号。
    ///
    /// 因此 `killed == true` 只表示“触发过 timeout/abort”，不保证真的发送过信号。
    /// 子进程可以忽略 SIGTERM 后自行退出，所以 `killed` 为真也不代表结果码是 0（测试
    /// `timeout_escalates_to_sigkill_when_sigterm_is_ignored`）。
    pub killed: bool,
}

/// 轮询间隔（替代 TS 的事件循环；对 timeout 的精度影响 < 2ms）。
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// SIGTERM 与 SIGKILL 之间的等待时间（对齐 TS `killProcess` 注释里的 5s 意图）。
const SIGKILL_GRACE: Duration = Duration::from_secs(5);

/// 执行一个命令并返回 stdout/stderr/退出码。**永不返回 `Err`**。
///
/// 参数：
/// - `command`/`args`：直接交给 `Command`，不经过 shell，因此管道、通配符、重定向都不生效；
/// - `cwd`：子进程工作目录；空路径视为未设置（在父进程 cwd 中运行，见模块文档）；
/// - `options`：signal/timeout；其中的 `cwd` 字段被忽略。
///
/// 这是**同步阻塞**函数：内部用 `std::process::Command` 加 2ms 轮询排空两个管道，直到
/// 子进程退出并走完退出后的 stdio 宽限。在异步上下文里必须放到 `spawn_blocking` 执行
/// （`kiana-extensions/src/loader/mod.rs` 的 `exec` 与 `handle_exec` 都是这样调用的），
/// 否则会占住执行器线程直到命令结束。
///
/// 失败路径全部映射成 `ExecResult`：空命令名/启动失败 → `code = 1`、`stderr` 为 Node 风格
/// 的错误文本；`wait` 出错 → `stderr` 为空时填入错误文本、`code = 1`。调用方不能通过
/// `Result` 区分“命令退出非零”和“命令根本没启动”，只能看 `stderr`/`killed`。
pub fn exec_command(
    command: &str,
    args: &[String],
    cwd: &Path,
    options: Option<&ExecOptions>,
) -> ExecResult {
    if command.is_empty() {
        // Node 的 `spawn("")` 在 Promise executor 里同步抛 TypeError → Promise reject；
        // 冻结签名不能失败，返回与 spawn 错误路径同形状的结果（stderr = error.message）。
        return ExecResult {
            stdout: String::new(),
            stderr: EMPTY_COMMAND_MESSAGE.to_string(),
            code: 1,
            killed: false,
        };
    }

    let mut builder = Command::new(command);
    builder
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Node 的异步 spawn 把 `cwd: ""` 当作未设置（在进程 cwd 里运行）。
    if !cwd.as_os_str().is_empty() {
        builder.current_dir(cwd);
    }

    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(error) => {
            // TS：spawn 的 'error' 事件 → reject(error) → catch：
            // `stderr: stderr || error.message`、`code: 1`、`killed` 保持当前值。
            let killed = signal_aborted(options);
            return ExecResult {
                stdout: String::new(),
                stderr: node_spawn_error(command, "spawn", error).to_string(),
                code: 1,
                killed,
            };
        }
    };

    let mut stdout_decoder = Utf8StreamDecoder::new();
    let mut stderr_decoder = Utf8StreamDecoder::new();
    let mut stdout = String::new();
    let mut stderr = String::new();

    let mut stdout_pump = child.stdout.take().map(PipePump::new);
    let mut stderr_pump = child.stderr.take().map(PipePump::new);

    let started = Instant::now();
    let timeout_deadline = options
        .and_then(|options| options.timeout)
        .filter(|timeout| *timeout > 0)
        .map(|timeout| started + Duration::from_millis(timeout));

    let mut killed = false;
    // SIGTERM 发出后开始计时；到点仍存活就升级为 SIGKILL。
    let mut sigkill_deadline: Option<Instant> = None;
    let mut sigkill_sent = false;
    // 两个终态互斥：`exit = Some(..)` 是正常 reap 到的退出状态（`None` 表示被信号杀死），
    // `wait_error = Some(..)` 是 `try_wait` 本身失败（例如底层 waitpid 出错）。
    let mut exit: Option<Option<i32>> = None;
    let mut wait_error: Option<std::io::Error> = None;

    // 进程存活期的主循环：每 2ms 依次检查退出、取消/超时、两个管道。timeout/abort 只发
    // 一次 SIGTERM；`SIGKILL_GRACE` 后仍存活则发 SIGKILL，保证调用不会无限挂起。
    loop {
        // 先确认子进程仍存活再发信号：避免在“已退出但尚未 reap”的窗口里对复用的 pid
        // 发 SIGTERM（Node 的 `proc.kill()` 在 exit 之后是空操作）。
        match child.try_wait() {
            Ok(Some(status)) => {
                exit = Some(status.code());
                break;
            }
            Ok(None) => {}
            Err(error) => {
                wait_error = Some(error);
                break;
            }
        }

        if !killed {
            let aborted = signal_aborted(options);
            let timed_out = timeout_deadline
                .map(|deadline| Instant::now() >= deadline)
                .unwrap_or(false);
            if aborted || timed_out {
                killed = true;
                // TS 的 killProcess 只发 SIGTERM；`if (!proc.killed) SIGKILL` 的 5s 定时器
                // 因为 Node 在 kill() 成功时即置 proc.killed=true 而永不触发（见模块文档）。
                // Rust 侧按注释的意图补上升级，否则忽略 SIGTERM 的子进程会让本函数挂死。
                send_sigterm(&mut child);
                sigkill_deadline = Some(Instant::now() + SIGKILL_GRACE);
            }
        } else if !sigkill_sent
            && sigkill_deadline.is_some_and(|deadline| Instant::now() >= deadline)
        {
            send_sigkill(&mut child);
            sigkill_sent = true;
        }

        pump_into(&mut stdout_pump, &mut stdout_decoder, &mut stdout);
        pump_into(&mut stderr_pump, &mut stderr_decoder, &mut stderr);
        std::thread::sleep(POLL_INTERVAL);
    }

    // 进程已退出：TS 的 `onExit` 在 exit 时立即武装 100ms 空闲计时器（child-process.ts:116-123），
    // 与退出前是否安静无关；每个数据块再重置（#5303）。timeout/abort 监听器在 wait 完成前
    // 仍然有效，因此宽限期内触发也要把 `killed` 置为 true（TS 的 exec.ts:57-84,96-105）。
    let grace = Duration::from_millis(EXIT_STDIO_GRACE_MS);
    let mut idle_deadline = Instant::now() + grace;
    let mut out_ended = false;
    let mut err_ended = false;
    // 宽限循环的退出条件：两个流都 EOF/被关闭，或距最后一次数据（退出时刻本身也算起点）
    // 已满 100ms。先到者生效；超时退出后仍留在管道里的数据被 finalize 丢弃。
    loop {
        if !killed {
            let aborted = signal_aborted(options);
            let timed_out = timeout_deadline
                .map(|deadline| Instant::now() >= deadline)
                .unwrap_or(false);
            if aborted || timed_out {
                // TS 调用 `proc.kill("SIGTERM")`；进程已被 reap，发信号只会冒 pid 复用风险，
                // 因此这里只更新可观察的 `killed` 标志。
                killed = true;
            }
        }
        let (got_out, out_done) = pump_into(&mut stdout_pump, &mut stdout_decoder, &mut stdout);
        let (got_err, err_done) = pump_into(&mut stderr_pump, &mut stderr_decoder, &mut stderr);
        out_ended |= out_done;
        err_ended |= err_done;
        if got_out || got_err {
            idle_deadline = Instant::now() + grace;
        }
        if out_ended && err_ended {
            break;
        }
        if Instant::now() >= idle_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    // finalize：TS 在这里 `stdout.destroy()`/`stderr.destroy()`。关闭读端后，继承管道的
    // 孙进程下一次写入会收到 EPIPE/SIGPIPE，且不会泄漏 fd/读线程。
    if let Some(pump) = stdout_pump.as_mut() {
        pump.destroy();
    }
    if let Some(pump) = stderr_pump.as_mut() {
        pump.destroy();
    }

    stdout.push_str(&stdout_decoder.end());
    stderr.push_str(&stderr_decoder.end());

    match (exit, wait_error) {
        (Some(code), _) => ExecResult {
            stdout,
            stderr,
            code: code.unwrap_or(0),
            killed,
        },
        (None, Some(error)) => ExecResult {
            stdout,
            stderr: if stderr.is_empty() {
                error.to_string()
            } else {
                stderr
            },
            code: 1,
            killed,
        },
        (None, None) => ExecResult {
            stdout,
            stderr,
            code: 1,
            killed,
        },
    }
}

fn signal_aborted(options: Option<&ExecOptions>) -> bool {
    options
        .and_then(|options| options.signal.as_ref())
        .map(|signal| signal.is_cancelled())
        .unwrap_or(false)
}

/// 给子进程发送 SIGTERM（Windows 上 `Child::kill` 即 TerminateProcess，等价 Node 的 kill）。
///
/// Unix 上直接调用 `kill(2)` 并忽略返回值：进程已消失（ESRCH）或权限不足都不影响后续
/// 等待逻辑，调用方仍会继续 `try_wait` 直到 reap 到结果。
fn send_sigkill(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        if pid != 0 {
            // SAFETY: `kill(2)` 只接收 pid 与信号编号，无内存安全前提。
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn send_sigterm(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        if pid != 0 {
            // SAFETY: `kill(2)` 只接收 pid 与信号编号，无内存安全前提。
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

/// 排空一个管道；返回 `(是否读到数据, 是否已结束/关闭)`。
///
/// 该流不存在（`None`，例如调用方先 `take()` 走了）时按“已结束”返回 `(false, true)`，
/// 让上层循环的结束条件成立。
fn pump_into<R: crate::child_process::ReadPipe>(
    pump: &mut Option<PipePump<R>>,
    decoder: &mut Utf8StreamDecoder,
    sink: &mut String,
) -> (bool, bool) {
    let Some(pump) = pump.as_mut() else {
        return (false, true);
    };
    let mut received = false;
    let ended = pump.drain(&mut |chunk| {
        received = true;
        sink.push_str(&decoder.write(chunk));
    });
    (received, ended)
}

/// `StringDecoder("utf8")` 的等价物：保存不完整尾部到下一次 `write`，`end` 时用 U+FFFD
/// flush 非法/不完整字节。
///
/// 行为必须与 Node 的 `new StringDecoder("utf8")` 在**任意字节切分**下一致：
/// `crates/kiana-core/tests/exec.rs` 的 `decodes_multibyte_output_split_across_chunks`
/// 与文件内 `matches_node_string_decoder_goldens`（黄金值由 node 26 生成）锁定了这一点。
/// 不能改成一次性 `String::from_utf8_lossy`：那会把跨块的多字节字符提前替换成 U+FFFD。
pub(crate) struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub(crate) fn write(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let hold = incomplete_tail_len(&self.pending);
        let decode_len = self.pending.len() - hold;
        let text = String::from_utf8_lossy(&self.pending[..decode_len]).into_owned();
        self.pending.drain(..decode_len);
        text
    }

    pub(crate) fn end(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

/// 尾部“不完整 UTF-8 序列”的字节数（0..=3）。判定规则与 Node 的
/// `utf8CheckIncomplete` 一致：只看最后一个序列，且不校验它是否**合法**，只看是否**完整**。
fn incomplete_tail_len(buffer: &[u8]) -> usize {
    let len = buffer.len();
    if len == 0 {
        return 0;
    }
    let last = buffer[len - 1];
    if last < 0x80 {
        return 0;
    }
    if last < 0xC0 {
        // continuation 字节：向前最多 3 个字节找 lead。
        let mut index = len - 1;
        let mut steps = 0;
        let mut lead = None;
        while index > 0 && steps < 3 {
            index -= 1;
            steps += 1;
            let byte = buffer[index];
            if byte >= 0xC0 {
                lead = Some(byte);
                break;
            }
            if byte < 0x80 {
                break;
            }
        }
        let Some(lead) = lead else {
            return 0;
        };
        let expected = utf8_sequence_len(lead);
        if expected == 0 {
            return 0;
        }
        let available = len - index;
        if available < expected && available <= 3 {
            available
        } else {
            0
        }
    } else {
        if utf8_sequence_len(last) >= 2 { 1 } else { 0 }
    }
}

fn utf8_sequence_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead < 0xC0 {
        0
    } else if lead < 0xE0 {
        2
    } else if lead < 0xF0 {
        3
    } else if lead < 0xF8 {
        4
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 黄金值由 node 26 的 `new StringDecoder("utf8")` 生成。
    fn write_then_end(chunks: &[&[u8]]) -> (String, String) {
        let mut decoder = Utf8StreamDecoder::new();
        let mut written = String::new();
        for chunk in chunks {
            written.push_str(&decoder.write(chunk));
        }
        (written, decoder.end())
    }

    #[test]
    fn splits_multibyte_characters_across_chunks() {
        let (written, end) = write_then_end(&[&[0xE2], &[0x82, 0xAC]]);
        assert_eq!(written, "€");
        assert_eq!(end, "");
    }

    #[test]
    fn matches_node_string_decoder_goldens() {
        let cases: Vec<(Vec<&[u8]>, &str, &str)> = vec![
            (vec![&[0x66, 0x6F, 0x80, 0x6F]], "fo\u{FFFD}o", ""),
            (vec![&[0x66, 0xE2]], "f", "\u{FFFD}"),
            (vec![&[0xF0, 0x9F]], "", "\u{FFFD}"),
            (vec![&[0xF0, 0x9F, 0x41]], "\u{FFFD}A", ""),
            (vec![&[0xE0, 0x80]], "", "\u{FFFD}\u{FFFD}"),
            (vec![&[0xED, 0xA0, 0x80]], "\u{FFFD}\u{FFFD}\u{FFFD}", ""),
            (vec![&[0xE2, 0x82]], "", "\u{FFFD}"),
            (vec![&[0x80]], "\u{FFFD}", ""),
            (vec![&[0xF8]], "\u{FFFD}", ""),
            (vec![&[0xFC]], "\u{FFFD}", ""),
            (vec![&[0xC3, 0xA9]], "é", ""),
        ];
        for (chunks, expected_write, expected_end) in cases {
            let (written, end) = write_then_end(&chunks);
            assert_eq!(written, expected_write, "write({chunks:?})");
            assert_eq!(end, expected_end, "end({chunks:?})");
        }
    }

    #[test]
    fn incomplete_tail_detection() {
        assert_eq!(incomplete_tail_len(b""), 0);
        assert_eq!(incomplete_tail_len(b"abc"), 0);
        assert_eq!(incomplete_tail_len(&[0xE2]), 1);
        assert_eq!(incomplete_tail_len(&[0xE2, 0x82]), 2);
        assert_eq!(incomplete_tail_len(&[0xE0, 0x80]), 2);
        assert_eq!(incomplete_tail_len(&[0xED, 0xA0, 0x80]), 0);
        assert_eq!(incomplete_tail_len(b"caf\xc3\xa9"), 0);
    }
}
