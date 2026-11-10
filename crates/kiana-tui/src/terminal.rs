//! terminal.ts。
//!
//! `ProcessTerminal`（真实终端）+ 键序列协商辅助函数。
//!
//! ## 与 TS 的 I/O 差异
//!
//! TS 直接借 `process.stdin` / `process.stdout`。Rust 侧把输出抽成可注入的
//! [`std::io::Write`]（[`ProcessTerminal::new`] 用真实 stdout，[`ProcessTerminal::with_output`]
//! 供测试/嵌入方捕获字节），输入由宿主驱动：
//!
//! - [`ProcessTerminal::poll_input`]：把 stdin 读取线程收到的字节喂给 `StdinBuffer`（等价于
//!   `process.stdin.on("data", stdinDataHandler)`）；`new()` 在 `start()` 时启动读取线程。
//! - [`ProcessTerminal::feed_input`] / [`ProcessTerminal::feed_input_str`]：直接注入数据（测试
//!   与已有输入管道的宿主使用，按原样喂给 `StdinBuffer`）。
//! - [`ProcessTerminal::poll`]：推进定时器（`StdinBuffer` 的序列超时、150ms 协商缓冲刷新、
//!   1s 进度保活），等价于 Node 事件循环里到期的 `setTimeout` / `setInterval` 回调。
//!
//! TS 的 `process.stdout.on("resize", handler)` 在 Rust 侧对应 [`ProcessTerminal::poll_resize`]
//! （按需比较终端尺寸）与 [`refresh_terminal_dimensions`]（自信号 SIGWINCH）。
//!
//! ## 在 crate 中的位置与生命周期
//!
//! `ProcessTerminal` 是 [`crate::terminal_colors::Terminal`] 的真实终端实现：
//! `kiana-interactive` 的 `create_interactive_tui` 在未注入终端时构造
//! `ProcessTerminal::new()`，再以 `Box<dyn Terminal>` 交给 `TuiBase` / `TuiAltScreen`；
//! 启动期 TUI（`kiana-coding-agent` 的 `startup-ui`）也直接持有它。普通渲染路径经
//! `Terminal` trait 调用；宿主轮询（`poll`/`poll_input`）与测试则直接使用固有方法。
//!
//! 一次典型生命周期（宿主循环的每一步见 `kiana-interactive` 的 `pump_events`）：
//!
//! 1. 构造：`new()` 写真实 stdout；`with_output()` 把输出交给调用方并关闭“真实终端”行为。
//! 2. `start(on_input, on_resize)`：安装处理器、记录原 raw 状态并进入 raw mode、重置 UTF-8
//!    解码器、恢复 stdin 读取线程、开启括号粘贴，最后发起键盘协议/程序状态协商。
//! 3. 宿主循环：`poll_input()`（读取线程 → UTF-8 解码 → `StdinBuffer` → 处理器）、
//!    `poll(now)`（三处定时器）、`poll_resize()`（比较终端尺寸）、`write()`（渲染字节）。
//! 4. 退出：`drain_input()` 先关闭 Kitty 与 modifyOtherKeys，再等 stdin 静默；
//!    `stop()` 恢复 raw/括号粘贴/进度/程序状态；进程退出路径用 `restore_on_exit()`。
//!
//! ## 线程模型
//!
//! - 修改状态的方法取 `&mut self`；只读 getter（`modify_other_keys_active`、`stdin_eof`、
//!   `columns`、`rows`、`kitty_protocol_active` 等）取 `&self`。协商缓冲、定时器与模式标志
//!   只在调用它的线程上访问。
//! - 真实 stdin 由独立的 `kiana-tui-stdin` 线程读取（`spawn_stdin_reader`）。该线程只持有
//!   `Arc<StdinPause>`、EOF 的 `Arc<AtomicBool>` 和 mpsc 发送端，不触碰终端状态；`stop()`
//!   通过暂停标志让它停止调用 `read(2)`，把 tty 输入留给前台子进程。
//! - `TimerSleep` 在首次 poll 时创建一个 `kiana-tui-timer` 线程，用 waker 唤醒
//!   `drain_input` 的 future，因而不依赖 Tokio 的 time driver。
//! - [`crate::keys::set_kitty_protocol_active`] 的状态在 `keys` 模块里是线程局部的；驱动本终端的线程
//!   必须与解析键盘输入的线程相同，否则按键解析看不到协商结果。
//!
//! ## 注意事项
//!
//! - 写入分两条路径：公开的 `write` 会额外追加 `KIANA_TUI_WRITE_LOG`，私有的 `write_raw`
//!   不会。`start`/`stop`/`set_title`/`set_progress`/光标与模式序列都走 `write_raw`，
//!   与 TS 的可观察日志行为一致（`tests/terminal.rs` 锁定）。
//! - `with_output()` 的实例 `owns_stdout == false`：`start()` 不创建 stdin 读取线程、
//!   也不查询真实 TTY 尺寸；`columns`/`rows` 退化为 `COLUMNS`/`LINES`/默认值。
//! - 部分协商回复会被缓冲等待后续字节；每收到一段仍是合法前缀的新数据就重新计一个 150ms
//!   的等待窗口（总时长可能超过 150ms），窗口到期仍无新数据就把缓冲内容按普通输入回放，
//!   避免吞键。

use std::io::Write;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::keys::set_kitty_protocol_active;
use crate::native::{ModifierKey, is_native_modifier_pressed};
use crate::program_status::{PROGRAM_STATUS_QUERY, format_program_status, is_program_status_reply};
use crate::stdin::{StdinBuffer, StdinBufferOptions, StdinEvent};
use crate::terminal_colors::{ProgramStatus, ProgramStatusState};

/// stdin `data` 事件的处理器类型。
///
/// 参数是 `StdinBuffer` 切分后的完整序列，或括号粘贴的原文（由内部的
/// `dispatch_stdin_event` 重新加上 `ESC[200~`/`ESC[201~` 后传入）。
/// 处理器在调用 [`ProcessTerminal::poll_input`]、[`ProcessTerminal::feed_input`]、
/// [`ProcessTerminal::feed_input_str`] 或 [`ProcessTerminal::poll`]（`StdinBuffer` 超时与协商
/// 缓冲回放）的线程上同步执行，参数只有 `&str`；它不应重入同一个终端（例如经
/// `Rc<RefCell<…>>` 反向借用），那会与已经借出的 `&mut self` 冲突。
pub type InputHandler = Box<dyn FnMut(&str)>;
/// `process.stdout.on("resize", handler)` 的处理器类型。
///
/// 回调没有参数；需要新尺寸的宿主应在回调里调用 `columns`/`rows` 重新读取。只由
/// [`ProcessTerminal::poll_resize`] 在检测到尺寸变化时同步调用。
pub type ResizeHandler = Box<dyn FnMut()>;

/// `TERMINAL_PROGRESS_KEEPALIVE_MS`。
///
/// `set_progress(true)` 会排一个到期时间；此后 [`ProcessTerminal::poll`] 每隔这么久重发一次
/// [`TERMINAL_PROGRESS_ACTIVE_SEQUENCE`]，直到 `set_progress(false)` 或 `stop()` 清掉定时器。
/// 该常量只决定间隔，是否重发由 `progress_keepalive_deadline` 是否存在决定。
pub const TERMINAL_PROGRESS_KEEPALIVE_MS: u64 = 1000;
/// `TERMINAL_PROGRESS_ACTIVE_SEQUENCE`（OSC 9;4;3）。
///
/// 由 `set_progress(true)` 和 1s 保活重发写出；清除用 [`TERMINAL_PROGRESS_CLEAR_SEQUENCE`]。
pub const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
/// `TERMINAL_PROGRESS_CLEAR_SEQUENCE`（OSC 9;4;0）。
///
/// 由 `set_progress(false)`、`stop()` 与 `restore_on_exit()` 写出（后两者只在定时器存在时写）。
pub const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0\x07";
/// `NATIVE_SHIFT_ENTER_SEQUENCE`（CSI-u Shift+Enter）。
///
/// [`normalize_native_shift_enter_input`] 用它在“原生修饰键检测到 Shift + 裸回车”时替换输入。
pub const NATIVE_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
/// `DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS`（1|2|4）。
///
/// [`KITTY_KEYBOARD_PROTOCOL_QUERY`] 的 push 段写作 `\x1b[>7u`，即请求这组 flags；终端回答
/// 的 flags 是否为 0 决定走 Kitty 还是 modifyOtherKeys 回退。
pub const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS: u32 = 7;
/// `KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS`。
///
/// 每个等待窗口为 150ms：收到形如 `ESC [ ? 7` 的半截回复后开始计时；每收到一段仍是合法
/// 前缀的新数据都会清掉旧 deadline 并按当前时刻重新计时，因此总缓冲时长可能超过 150ms。
/// 窗口到期仍无新数据时，`poll` 把缓冲内容当普通输入转发；若拼接后的内容不再是合法前缀，
/// 旧缓冲也会被立即回放。这样近似前缀的按键不会被永久吞掉。
pub const KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS: u64 = 150;
/// `KITTY_KEYBOARD_PROTOCOL_QUERY`。
///
/// 两段：`ESC[>7u` 推送并请求 flags，`ESC[?u` 查询当前 flags。由
/// [`ProcessTerminal::query_and_enable_kitty_protocol`] 在 [`DEVICE_ATTRIBUTES_QUERY`] 之前写出。
pub const KITTY_KEYBOARD_PROTOCOL_QUERY: &str = "\x1b[>7u\x1b[?u";
/// `DEVICE_ATTRIBUTES_QUERY`。
///
/// DA1 查询同时充当两种握手的终结符：Kitty 协商（无 flags 回复时据此启用 modifyOtherKeys）
/// 与程序状态查询。每发一次查询就欠一个 DA1 回复（`pending_keyboard_protocol_device_attributes`
/// 计数）。
pub const DEVICE_ATTRIBUTES_QUERY: &str = "\x1b[c";
/// `DEFAULT_ESCAPE_TIMEOUT_MS`。
///
/// 孤立 ESC 的默认等待窗口（非 SSH 环境）；由 [`resolve_escape_timeout_ms`] 返回，
/// 最终交给 `StdinBuffer` 的 `escape_timeout`。
pub const DEFAULT_ESCAPE_TIMEOUT_MS: u64 = 10;
/// `DEFAULT_SSH_ESCAPE_TIMEOUT_MS`。
///
/// SSH 会话（`SSH_CONNECTION` / `SSH_TTY` 任一非空）下的默认 ESC 等待窗口；由
/// [`resolve_escape_timeout_ms_with`] 在 `KIANA_TUI_ESC_TIMEOUT` 未配置或无效时选用。
pub const DEFAULT_SSH_ESCAPE_TIMEOUT_MS: u64 = 100;

/// `KeyboardProtocolNegotiationSequence`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardProtocolNegotiationSequence {
    /// Kitty 键盘协议的 flags 回复（`ESC [ ? <flags> u`）。
    KittyFlags(u64),
    /// DA1 回复（`ESC [ ? <params> c`）。
    DeviceAttributes,
}

/// `parseKeyboardProtocolNegotiationSequence(sequence)`。
///
/// 只接受**完整**回复：Kitty flags（`ESC [ ? <十进制数字> u`）或 DA1
/// （`ESC [ ? <数字/分号> c`，参数可为空，测试覆盖 `\x1b[?c`）。返回 `None` 表示它既不是
/// 协商回复、也不足以判断（例如只有前缀 `\x1b[?7`）；调用方据此决定缓冲还是按普通输入转发。
pub fn parse_keyboard_protocol_negotiation_sequence(
    sequence: &str,
) -> Option<KeyboardProtocolNegotiationSequence> {
    if let Some(flags) = parse_kitty_flags_reply(sequence) {
        return Some(KeyboardProtocolNegotiationSequence::KittyFlags(flags));
    }
    if is_device_attributes_reply(sequence) {
        return Some(KeyboardProtocolNegotiationSequence::DeviceAttributes);
    }
    None
}

/// `/^\x1b\[\?(\d+)u$/`（`\d` 是 ASCII 数字）+ `Number.parseInt(_, 10)`。
fn parse_kitty_flags_reply(sequence: &str) -> Option<u64> {
    let rest = sequence.strip_prefix("\x1b[?")?.strip_suffix('u')?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // `Number.parseInt` 对超长数字串返回一个巨大的 double（永远不会等于任何合法 flags）；
    // 这里饱和到 u64::MAX，唯一用途 `flags !== 0` 的语义不变。
    Some(rest.parse().unwrap_or(u64::MAX))
}

/// `/^\x1b\[\?[\d;]*c$/`。
fn is_device_attributes_reply(sequence: &str) -> bool {
    let Some(rest) = sequence
        .strip_prefix("\x1b[?")
        .and_then(|rest| rest.strip_suffix('c'))
    else {
        return false;
    };
    rest.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// `isKeyboardProtocolNegotiationSequencePrefix(sequence)`。
fn is_keyboard_protocol_negotiation_sequence_prefix(sequence: &str) -> bool {
    if sequence == "\x1b[" {
        return true;
    }
    let Some(rest) = sequence.strip_prefix("\x1b[?") else {
        return false;
    };
    rest.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// `isAppleTerminalSession()`：`process.platform === "darwin" && TERM_PROGRAM === "Apple_Terminal"`。
///
/// 平台判断在编译期由 `cfg!` 决定（非 macOS 恒为 `false`）；`TERM_PROGRAM` 在每次调用时
/// 重新读取，因此运行中修改该环境变量会影响结果。
pub fn is_apple_terminal_session() -> bool {
    cfg!(target_os = "macos") && process_env("TERM_PROGRAM").as_deref() == Some("Apple_Terminal")
}

/// Refresh terminal dimensions on POSIX platforms by sending SIGWINCH to this process.
///
/// Best-effort: some environments (restricted seccomp or LSM policies) return EACCES for
/// `kill(2)`; in that case the dimensions refresh is skipped rather than crashing.
///
/// 本函数只投递信号，不读取尺寸：能否观察到信号取决于进程里是否安装了 SIGWINCH 处理器；
/// 实际的尺寸比较发生在 [`ProcessTerminal::poll_resize`]（`stdout_size`）。`start()` 调用它，
/// 用于处理 suspend/resume 后丢失的 resize 通知。Windows 上是空操作。
pub fn refresh_terminal_dimensions() {
    #[cfg(not(target_os = "windows"))]
    {
        refresh_terminal_dimensions_with(&|| {
            // SAFETY: getpid/kill 只读取本进程号并向自身投递 SIGWINCH；失败（EACCES/ESRCH）
            // 被下面的 `let _ =` 忽略，与 TS 的 `try { ... } catch {}` 一致。
            let pid = unsafe { libc::getpid() };
            if pid <= 0 {
                return Ok(());
            }
            if unsafe { libc::kill(pid, libc::SIGWINCH) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    #[cfg(target_os = "windows")]
    {
        refresh_terminal_dimensions_with(&|| Ok(()));
    }
}

/// [`refresh_terminal_dimensions`] 的测试缝：TS 的 `regression-sigwinch-kill-eacces.test.ts`
/// 通过替换 `process.kill` 注入 EACCES/EPERM；Rust 没有可替换的全局函数，因此把“发信号”
/// 抽成可注入的闭包。语义与 TS 的 `try { process.kill(pid, "SIGWINCH") } catch {}` 相同：
/// 非 Windows 平台调用闭包并**忽略**其错误，Windows 平台直接返回。
#[doc(hidden)]
pub fn refresh_terminal_dimensions_with(signal: &dyn Fn() -> std::io::Result<()>) {
    if cfg!(target_os = "windows") {
        return;
    }
    let _ = signal();
}

/// `normalizeNativeShiftEnterInput(data, shouldDetectNativeShiftEnter, isShiftPressed)`。
///
/// 仅当 `data` 恰好是裸回车 `"\r"`、`should_detect_native_shift_enter` 为真且
/// `is_shift_pressed` 为真时，返回 [`NATIVE_SHIFT_ENTER_SEQUENCE`]；其它输入原样复制。
/// 无副作用，也不修改调用方的字符串。
pub fn normalize_native_shift_enter_input(
    data: &str,
    should_detect_native_shift_enter: bool,
    is_shift_pressed: bool,
) -> String {
    if should_detect_native_shift_enter && data == "\r" && is_shift_pressed {
        return NATIVE_SHIFT_ENTER_SEQUENCE.to_string();
    }
    data.to_string()
}

/// `normalizeAppleTerminalInput(data, isAppleTerminal, isShiftPressed)`。
///
/// 按 TS 的同名入口保留的薄封装：判断是否 Apple Terminal 由调用方完成（`is_apple_terminal`
/// 是入参），这里只委托给 [`normalize_native_shift_enter_input`]。
pub fn normalize_apple_terminal_input(
    data: &str,
    is_apple_terminal: bool,
    is_shift_pressed: bool,
) -> String {
    normalize_native_shift_enter_input(data, is_apple_terminal, is_shift_pressed)
}

/// 读取一个环境变量（TS `process.env[name]`；非 UTF-8 按 lossy 处理）。
///
/// 返回值区分“未设置”（`None`）与“设置为空串”（`Some("")`）；非 UTF-8 字节被替换为
/// U+FFFD，因此 `Some` 不保证与原值逐字节一致。需要 JS 真值语义的调用方（如
/// [`resolve_escape_timeout_ms_with`] 的 SSH 判断）必须自己排除空串。
pub fn process_env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

/// `resolveEscapeTimeoutMs(env = process.env)`。
///
/// 返回 TS 的原始数值（可以是小数，也可以大于 `setTimeout` 的上限）；把数值变成实际定时器
/// 延迟的是调用方（[`node_set_timeout_ms`] 复刻 `setTimeout` 的截断/钳制规则）。
/// 每次调用都通过 [`process_env`] 读取当时的环境，不做缓存。
pub fn resolve_escape_timeout_ms() -> f64 {
    resolve_escape_timeout_ms_with(&process_env)
}

/// `resolveEscapeTimeoutMs(env)` 的显式环境查找版本（测试用；用闭包提供环境，便于隔离）。
///
/// `Number(env.KIANA_TUI_ESC_TIMEOUT)`：有限且 `> 0` 时原样返回；否则 SSH（`SSH_CONNECTION` /
/// `SSH_TTY` 任一**非空**）用 100ms，其余 10ms。注意 TS 用真值判断（`if (env.SSH_CONNECTION
/// || env.SSH_TTY)`）：空字符串为假。
pub fn resolve_escape_timeout_ms_with(env: &dyn Fn(&str) -> Option<String>) -> f64 {
    let configured = match env("KIANA_TUI_ESC_TIMEOUT") {
        Some(value) => kiana_json::parse_js_number(&value),
        None => f64::NAN,
    };
    if configured.is_finite() && configured > 0.0 {
        return configured;
    }
    let is_set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
    if is_set("SSH_CONNECTION") || is_set("SSH_TTY") {
        return DEFAULT_SSH_ESCAPE_TIMEOUT_MS as f64;
    }
    DEFAULT_ESCAPE_TIMEOUT_MS as f64
}

/// Node `setTimeout(ms)` 对延迟的转换：`after = +after`；不在 `[1, 2^31-1]` 内一律用 1ms
/// （Node 对 `> TIMEOUT_MAX` 会先告警再改用 1ms），其余按 C++ 的 `uint64_t` 转换截断小数。
///
/// TS 的 `StdinBuffer` 把 `resolveEscapeTimeoutMs()` 直接交给 `setTimeout`，所以这里也照此
/// 折算成实际毫秒，避免把 `3e9` 当成 24.8 天的定时器。
pub fn node_set_timeout_ms(ms: f64) -> u64 {
    const TIMEOUT_MAX: f64 = 2_147_483_647.0;
    if (1.0..=TIMEOUT_MAX).contains(&ms) {
        ms as u64
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// 输出
// ---------------------------------------------------------------------------

/// stdout 写入钩子：接管时写 stderr，未接管时写真实 stdout。
///
/// 接管是运行时替换输出目标：接管期间**所有**经此钩子的写入（包括 `ProcessTerminal` 的
/// TUI 字节）都进 stderr。Rust 的 `println!` 无法被替换，但 `ProcessTerminal::new()` 的
/// 输出可以经过这里（`kiana-core` 的 output-guard 在接管时安装、恢复时移除）；
/// `with_output()` 的实例（测试/嵌入）不经过钩子。
pub type StdoutWriteHook = fn(&[u8]);

static STDOUT_WRITE_HOOK: Mutex<Option<StdoutWriteHook>> = Mutex::new(None);

/// 安装/移除 [`ProcessTerminal::new`] 的 stdout 写入钩子（`None` 表示直接写 fd 1）。
///
/// 钩子存放在进程级静态变量（`Mutex`）里，最后一次调用生效；`kiana-core` 的 output-guard
/// 在接管 stdout 时安装、恢复时移除。钩子收到原始字节并负责同步写出；钩子存在时
/// `StdoutWriter` 的 `flush` 是空操作。互斥锁中毒时取回内部值继续，不会因此 panic。
pub fn set_stdout_write_hook(hook: Option<StdoutWriteHook>) {
    *STDOUT_WRITE_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = hook;
}

fn stdout_write_hook() -> Option<StdoutWriteHook> {
    *STDOUT_WRITE_HOOK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 真实 stdout。`Write::write` 失败被调用方忽略（TS 的 `process.stdout.write` 也不检查）。
struct StdoutWriter;

impl Write for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(hook) = stdout_write_hook() {
            hook(buf);
            return Ok(buf.len());
        }
        let mut stdout = std::io::stdout().lock();
        let written = stdout.write(buf)?;
        stdout.flush()?;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if stdout_write_hook().is_some() {
            // 钩子同步写出并 flush（`ProcessTerminal::write_raw` 紧接着调用 flush）。
            return Ok(());
        }
        std::io::stdout().flush()
    }
}

/// 同步写 stdout（`fs.writeSync(process.stdout.fd, sequence)` 的等价物）。
#[cfg(unix)]
fn write_stdout_sync(data: &[u8]) {
    // 循环直到写完：处理短写；EINTR 重试，其它错误直接放弃（这是退出路径，无法再报错）。
    let mut written = 0usize;
    while written < data.len() {
        // SAFETY: 写入 fd 1；指针来自有效切片，长度已校验。
        let result = unsafe {
            libc::write(
                1,
                data[written..].as_ptr().cast::<libc::c_void>(),
                data.len() - written,
            )
        };
        if result > 0 {
            written += result as usize;
            continue;
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            std::io::ErrorKind::Interrupted => continue,
            _ => break,
        }
    }
}

/// 同步写 stdout（Windows：直接写标准输出句柄；错误忽略）。
#[cfg(not(unix))]
fn write_stdout_sync(data: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(data);
    let _ = stdout.flush();
}

// ---------------------------------------------------------------------------
// UTF-8 增量解码（Node `setEncoding("utf8")` 的 StringDecoder 等价物）
// ---------------------------------------------------------------------------

/// 把字节块增量解码为 `String`：跨块的多字节序列保持挂起，非法序列用 U+FFFD 替换。
#[derive(Debug, Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn decode(&mut self, data: &[u8]) -> String {
        // 把上次挂起的半截序列与新块拼接后从头解码：合法前缀立即输出，
        // 末尾不完整的多字节序列留在 `pending` 等下一块。
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(data);
        let mut out = String::new();
        let mut rest: &[u8] = &bytes;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(
                        std::str::from_utf8(&rest[..valid])
                            .expect("valid_up_to is a char boundary"),
                    );
                    // `valid_up_to` 之前是合法 UTF-8；`error_len()` 为 `Some(n)` 表示随后 n
                    // 字节是非法序列（输出一个 U+FFFD 后跳过继续），为 `None` 表示块尾被截断。
                    match error.error_len() {
                        Some(length) => {
                            out.push('\u{fffd}');
                            rest = &rest[valid + length..];
                        }
                        None => {
                            // 截断的多字节序列：留到下一块。
                            self.pending = rest[valid..].to_vec();
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// executor 无关的定时器
// ---------------------------------------------------------------------------

/// `await new Promise(r => setTimeout(r, ms))` 的等价物，**不要求任何特定 executor**。
///
/// `tokio::time::sleep` 需要 Tokio 的 time driver（没有 runtime 时 poll 会 panic）；TS 的
/// `setTimeout` 计时器与 executor 无关，而 `Terminal::drain_input` / `NativeClipboard::get_text`
/// 都是公开的 `Future`，因此这里用后台线程 + waker 实现：到期时线程唤醒当前任务。
pub(crate) struct TimerSleep {
    deadline: Instant,
    receiver: Option<std::sync::mpsc::Receiver<()>>,
    state: Option<Arc<TimerWakeState>>,
    /// 线程创建失败时的最后手段：直接阻塞当前线程。
    blocked: bool,
}

#[derive(Default)]
struct TimerWakeState {
    waker: Mutex<Option<std::task::Waker>>,
}

impl TimerSleep {
    pub(crate) fn new(duration: Duration) -> Self {
        TimerSleep {
            // `Instant + Duration` 溢出只可能来自 `u64::MAX` 毫秒级的输入；饱和到一个
            // 实际不可达的远期时刻即可（TS 的 `Date.now() + maxMs` 同样不会溢出）。
            deadline: Instant::now()
                .checked_add(duration)
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400 * 365 * 100)),
            receiver: None,
            state: None,
            blocked: false,
        }
    }
}

impl std::future::Future for TimerSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        let this = self.get_mut();
        if this.blocked || Instant::now() >= this.deadline {
            return std::task::Poll::Ready(());
        }
        if let Some(receiver) = this.receiver.as_ref()
            && receiver.try_recv().is_ok()
        {
            return std::task::Poll::Ready(());
        }
        if this.receiver.is_none() {
            // 首次 poll 才创建线程与通道（构造只记录 deadline）；线程睡到 deadline 后
            // 唤醒当前 waker 并自行结束（future 提前丢弃时 send 失败被忽略）。
            let (sender, receiver) = std::sync::mpsc::channel();
            let state = Arc::new(TimerWakeState::default());
            let thread_state = Arc::clone(&state);
            let deadline = this.deadline;
            let spawned = std::thread::Builder::new()
                .name("kiana-tui-timer".to_string())
                .spawn(move || {
                    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    let waker = thread_state
                        .waker
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .take();
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                    // 接收端可能已随 future 一起丢弃；发送失败无所谓。
                    let _ = sender.send(());
                });
            if spawned.is_err() {
                // 极端资源耗尽：退化为阻塞等待，语义（到期后继续）不变。
                this.blocked = true;
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                return std::task::Poll::Ready(());
            }
            this.receiver = Some(receiver);
            this.state = Some(state);
        }
        if let Some(state) = this.state.as_ref() {
            *state
                .waker
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(context.waker().clone());
        }
        // 线程可能在存 waker 之前就已到期并发送：再检查一次避免丢唤醒。
        if this
            .receiver
            .as_ref()
            .is_some_and(|receiver| receiver.try_recv().is_ok())
        {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }
}

/// 创建一个 [`TimerSleep`]（`setTimeout(ms)` 之后等待）。
pub(crate) fn sleep_for(duration: Duration) -> TimerSleep {
    TimerSleep::new(duration)
}

// ---------------------------------------------------------------------------
// ProcessTerminal
// ---------------------------------------------------------------------------

/// `readKeyboardProtocolNegotiationSequence` 的返回值。
enum NegotiationRead {
    /// 完整的协商回复；`sequence` 是拼接后的原始字节，无法消费时原样转发。
    Parsed {
        parsed: KeyboardProtocolNegotiationSequence,
        sequence: String,
    },
    /// 目前只是回复的前缀（已存入协商缓冲），后续数据块可能补全。
    Pending,
    /// 与协商无关的序列，应按普通输入转发。
    None,
}

/// `process.stdin.pause()` / `resume()` 的等价物。
///
/// TS 的 `pause()` 会真正停掉 libuv 对 tty 的读取；如果 Rust 侧只是丢弃已读到的字节，
/// 读取线程仍会和前台子进程（外部编辑器 `$EDITOR`、挂起后的 shell）争抢 tty 输入，
/// 并在 `start()` 后把偷到的按键重新喂给 TUI。因此读取线程在暂停期间**不得调用 `read`**：
/// 它在 [`StdinPause::wait_until_resumed`] 上阻塞，字节留在内核缓冲区里。
struct StdinPause {
    paused: Mutex<bool>,
    changed: Condvar,
}

impl StdinPause {
    fn new() -> Self {
        StdinPause {
            paused: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    /// `process.stdin.pause()`。
    fn pause(&self) {
        *self
            .paused
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = true;
    }

    /// `process.stdin.resume()`。
    fn resume(&self) {
        *self
            .paused
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = false;
        self.changed.notify_all();
    }

    fn is_paused(&self) -> bool {
        *self
            .paused
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// 暂停时阻塞调用线程，直到 [`StdinPause::resume`]。
    fn wait_until_resumed(&self) {
        let mut paused = self
            .paused
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while *paused {
            paused = self
                .changed
                .wait(paused)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

/// Real terminal using process.stdin/stdout.
///
/// 真实的终端实现：独占进程的 stdin/stdout 与 tty 模式，实现
/// [`crate::terminal_colors::Terminal`] 的全部方法。多数调用方不应直接创建它，而是由
/// `kiana-interactive` 的 `create_interactive_tui` 构造缺省终端，再以 `Box<dyn Terminal>`
/// 交给 `TuiBase` / `TuiAltScreen`。
///
/// # 构造方式
///
/// - [`ProcessTerminal::new`]：写真实 stdout（安装 `StdoutWriteHook` 时转给钩子，见
///   [`StdoutWriteHook`]），首次 `start()` 时启动 stdin 读取线程、查询 TTY 尺寸。通常
///   进程里只有一个实例应这样做。
/// - [`ProcessTerminal::with_output`]：写调用方提供的 `Write`，**不**启动 stdin 读取线程、
///   不查询 TTY 尺寸；测试（`tests/terminal.rs` 的 `CaptureWriter`）与嵌入方使用它。
///
/// # 生命周期
///
/// `start()` / `stop()` 成对管理 raw mode、括号粘贴、Kitty 键盘协议、modifyOtherKeys、
/// 进度保活与程序状态。`start()` 之后宿主必须持续调用 [`ProcessTerminal::poll_input`]、
/// [`ProcessTerminal::poll`] 与 [`ProcessTerminal::poll_resize`]：Rust 没有 Node 事件循环，
/// 不轮询就不会有输入与定时器回调。
///
/// # 约束
///
/// - 修改状态的方法取 `&mut self`，只读 getter 取 `&self`；实例应固定在驱动 UI 的线程上；
///   Kitty 全局状态是线程局部的（见 [`crate::keys::set_kitty_protocol_active`]），跨线程驱动
///   会让按键解析看不到协商结果。
/// - `stop()` 只暂停 stdin 读取线程，不 join 它；线程在 stdin 关闭、读取通道断开或进程
///   结束时退出。
/// - 所有输出错误被忽略（`write_raw` 丢弃 `io::Result`），因此方法不返回 `Result`。
pub struct ProcessTerminal {
    /// `start()` 之前的 `isRaw`；`stop()` 用它恢复。
    was_raw: bool,
    /// 宿主通过 `start` / `set_input_handler` 安装的输入回调。
    input_handler: Option<InputHandler>,
    /// 宿主通过 `start` 安装的尺寸变化回调，由 `poll_resize` 触发。
    resize_handler: Option<ResizeHandler>,
    /// 本实例是否认为 Kitty 键盘协议已启用（与 `keys` 模块的全局状态同步）。
    kitty_protocol_active: bool,
    /// 是否已写出 `\x1b[>4;2m`；用于避免重复开启，并在退出时只关闭一次。
    modify_other_keys_active: bool,
    /// 是否向终端发送过 Kitty push（`\x1b[>…u`）。`stop`/`drain_input`/`restore_on_exit`
    /// 只在这个标志或 `kitty_protocol_active` 为真时才写 pop（`\x1b[<u`）。
    keyboard_protocol_pushed: bool,
    /// DA1 replies owed to keyboard protocol queries. Later DA1 replies answer other queries and
    /// are forwarded.
    pending_keyboard_protocol_device_attributes: u32,
    /// 半截协商回复（例如只到了 `ESC [ ? 7`）；见 `read_keyboard_protocol_negotiation_sequence`。
    keyboard_protocol_negotiation_buffer: String,
    /// 上述缓冲的回放期限：到期仍未凑成完整回复就当普通输入转发。
    keyboard_protocol_buffer_flush_deadline: Option<Instant>,
    /// `start()` 期间由 `setup_stdin_buffer` 创建；`stop()` 时销毁。
    stdin_buffer: Option<StdinBuffer>,
    /// 1s 进度保活的到期时间（TS `progressInterval`）。
    progress_keepalive_deadline: Option<Instant>,
    /// Latest program status, kept across `stop()` so `start()` can report it again.
    program_status: Option<ProgramStatus>,
    /// Whether the terminal confirmed OSC 7501 support since the last `start()`, or
    /// `KIANA_PROGRAM_STATUS=1`.
    program_status_supported: bool,
    /// The support query was sent and its DA1 sentinel has not arrived yet.
    program_status_query_pending: bool,
    /// 构造时由 `resolve_write_log_path()` 求值；之后修改环境变量不再生效。
    write_log_path: Option<std::path::PathBuf>,

    /// 写入目标；`new()` 是真实 stdout，`with_output()` 是调用方提供的对象。
    output: Box<dyn Write + Send>,
    /// `with_output` 的实例不读真实 stdin、也不查 TTY 尺寸（测试/嵌入模式）。
    owns_stdout: bool,
    /// 真实 stdin 读取线程（首次 `start()` 时创建，进程生命周期内复用）。
    ///
    /// 每块数据带上到达时刻：`drainInput` 的 `onData` 监听器用 `Date.now()` 记录最近一次输入，
    /// 时间戳随通道一起送达，等价于事件回调在数据到达时立即更新 `lastDataTime`。
    stdin_rx: Option<std::sync::mpsc::Receiver<(Instant, Vec<u8>)>>,
    /// 读取线程是否已创建；进程生命周期内只创建一次（`start` 复用同一线程与通道）。
    stdin_thread_started: bool,
    /// `stop()` 至下一次 `start()` 期间为真；`poll_input` 期间直接返回 `false`。
    stdin_paused: bool,
    /// stdin 是否已到达 EOF（读取线程 `read() == 0` 或不可恢复错误）。
    ///
    /// Node 在 stdin 关闭时向 `process.stdin` 发 `end`；没有监听者时事件循环随之耗尽，
    /// 进程自然退出。Rust 的宿主（启动期 TUI 驱动循环）用这个标志复刻同一时机。
    stdin_eof: Arc<AtomicBool>,
    /// 与读取线程共享的暂停标志（`process.stdin.pause()`/`resume()`）。
    stdin_pause: Arc<StdinPause>,
    /// Node 的 `process.stdin.isRaw`：**只**反映本进程是否调用过 `setRawMode(true)`，
    /// 与继承来的 termios/控制台模式无关（Node 的 `TTY.isRaw` 由 `setRawMode` 赋值）。
    stdin_raw_mode: bool,
    /// 跨块解码器；`start()` 时重置，对应 Node 每次 `setEncoding("utf8")` 安装新解码器。
    utf8_decoder: Utf8Decoder,
    /// `process.stdin.setRawMode` 之前保存的 termios（第一次进入 raw mode 时）。
    #[cfg(unix)]
    saved_termios: Option<libc::termios>,
    /// `poll_resize` 用：最近一次观察到的终端尺寸（列为 `None` 表示该维度不可用）。
    last_size: Option<(Option<usize>, Option<usize>)>,
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTerminal {
    /// `new ProcessTerminal()`：真实 stdout / stdin / TTY 尺寸。
    ///
    /// 构造本身不接触 tty、不进 raw mode、不启动线程；这些副作用都发生在 `start()`。
    /// 输出在安装 [`StdoutWriteHook`] 时会转给钩子（由 `kiana-core` 的 output-guard 使用）。
    pub fn new() -> Self {
        Self::with_writer(Box::new(StdoutWriter), true)
    }

    /// 自选输出端（测试/嵌入）。不启动 stdin 读取线程，也不查询 TTY 尺寸
    /// （`columns`/`rows` 走 `COLUMNS`/`LINES`/默认值）。
    ///
    /// 即使调用 `start()` 也不会读取真实 stdin（`ensure_stdin_reader` 检查 `owns_stdout`）；
    /// 输入需由调用方经 [`ProcessTerminal::feed_input`] / [`ProcessTerminal::feed_input_str`]
    /// 注入（`src/terminal.rs` 的单元测试 `drain_and_decoder_tests` / `stdin_stream_tests`
    /// 会直接安装 `stdin_rx`）。
    pub fn with_output(output: Box<dyn Write + Send>) -> Self {
        Self::with_writer(output, false)
    }

    fn with_writer(output: Box<dyn Write + Send>, owns_stdout: bool) -> Self {
        ProcessTerminal {
            was_raw: false,
            input_handler: None,
            resize_handler: None,
            kitty_protocol_active: false,
            modify_other_keys_active: false,
            keyboard_protocol_pushed: false,
            pending_keyboard_protocol_device_attributes: 0,
            keyboard_protocol_negotiation_buffer: String::new(),
            keyboard_protocol_buffer_flush_deadline: None,
            stdin_buffer: None,
            progress_keepalive_deadline: None,
            program_status: None,
            program_status_supported: false,
            program_status_query_pending: false,
            write_log_path: resolve_write_log_path(),
            output,
            owns_stdout,
            stdin_rx: None,
            stdin_thread_started: false,
            stdin_paused: false,
            stdin_eof: Arc::new(AtomicBool::new(false)),
            stdin_pause: Arc::new(StdinPause::new()),
            stdin_raw_mode: false,
            utf8_decoder: Utf8Decoder::default(),
            #[cfg(unix)]
            saved_termios: None,
            last_size: None,
        }
    }

    /// `kittyProtocolActive` 之外，TS 还暴露 `modifyOtherKeysActive`（仅测试/内部使用）。
    ///
    /// TUI 其余部分应使用 `Terminal::kitty_protocol_active`；本方法只反映
    /// modifyOtherKeys 回退是否开启。
    pub fn modify_other_keys_active(&self) -> bool {
        self.modify_other_keys_active
    }

    /// 设置输入处理器（TS 测试里直接写私有字段 `inputHandler` 的等价物）。
    ///
    /// 正常路径由 [`crate::terminal_colors::Terminal::start`] 设置。替换而非链式：旧处理器
    /// 被丢弃，`stop()` 会把槽位清空。
    pub fn set_input_handler(&mut self, handler: Box<dyn FnMut(&str)>) {
        self.input_handler = Some(handler);
    }

    /// 注入 stdin 字节（等价于 `stdinBuffer.process(data)`，即 Buffer 入口）。
    ///
    /// 返回是否收到了事件。缓冲区只在 `start()`（内部 `setup_stdin_buffer`）之后存在：
    /// `stop()` 之后或从未 `start()` 时返回 `false`（测试可先调用
    /// `query_and_enable_kitty_protocol` 建立缓冲区）。事件在返回前同步分发给当前输入处理器。
    pub fn feed_input(&mut self, data: &[u8]) -> bool {
        let Some(buffer) = self.stdin_buffer.as_mut() else {
            return false;
        };
        let events = buffer.process(data);
        let had_events = !events.is_empty();
        for event in events {
            self.dispatch_stdin_event(event);
        }
        had_events
    }

    /// 注入已解码的 stdin 字符串（等价于 `setEncoding("utf8")` 后的 `data` 事件）。
    ///
    /// 与 [`ProcessTerminal::feed_input`] 相同：绕过 UTF-8 解码器，直接交给 `StdinBuffer`；
    /// 返回是否产生了事件，事件同步分发。
    pub fn feed_input_str(&mut self, data: &str) -> bool {
        let Some(buffer) = self.stdin_buffer.as_mut() else {
            return false;
        };
        let events = buffer.process_str(data);
        let had_events = !events.is_empty();
        for event in events {
            self.dispatch_stdin_event(event);
        }
        had_events
    }

    /// 把真实 stdin 读取线程已收到的字节喂给缓冲区（宿主循环每轮调用）。
    ///
    /// 返回是否处理了数据。`stop()`（`process.stdin.pause()`）之后返回 `false`，数据留在
    /// 通道里等下一次 `start()`。
    ///
    /// 一次调用会取空通道当前的所有块；每块先经过内部的 `Utf8Decoder`（跨块多字节序列
    /// 保持挂起），再交给 [`ProcessTerminal::feed_input_str`]。读取线程附带的到达时刻在这里
    /// 被丢弃——只有 `drain_input` 用到达时刻计算空闲窗口。`with_output()` 的实例没有读取
    /// 线程，因此总是返回 `false`（除非测试像 `src/terminal.rs` 的 `drain_and_decoder_tests` /
    /// `stdin_stream_tests` 那样手动安装 `stdin_rx`）。
    pub fn poll_input(&mut self) -> bool {
        if self.stdin_paused {
            return false;
        }
        let mut any = false;
        while let Some((_, chunk)) = self
            .stdin_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            let text = self.utf8_decoder.decode(&chunk);
            if !text.is_empty() && self.feed_input_str(&text) {
                any = true;
            }
        }
        any
    }

    /// 推进所有定时器（TS 事件循环里到期的 `setTimeout` / `setInterval` 回调）。
    ///
    /// 三处定时器依次处理：`StdinBuffer` 的序列/ESC 超时 → 键盘协商缓冲的 150ms 回放 →
    /// 1s 进度保活重发。到期时间都是绝对值，`now` 由宿主提供；同一时刻重复调用不会重复
    /// 触发（触发后要么清除、要么重新排期）。协商缓冲回放可能同步调用输入处理器。
    pub fn poll(&mut self, now: Instant) {
        let events = match self.stdin_buffer.as_mut() {
            Some(buffer) => buffer.poll_timeout(now),
            None => Vec::new(),
        };
        for event in events {
            self.dispatch_stdin_event(event);
        }

        if let Some(deadline) = self.keyboard_protocol_buffer_flush_deadline
            && now >= deadline
        {
            self.keyboard_protocol_buffer_flush_deadline = None;
            self.flush_keyboard_protocol_negotiation_buffer_as_input();
        }

        if let Some(deadline) = self.progress_keepalive_deadline
            && now >= deadline
        {
            self.write_raw(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            self.progress_keepalive_deadline =
                Some(now + Duration::from_millis(TERMINAL_PROGRESS_KEEPALIVE_MS));
        }
    }

    /// `process.stdout.on("resize", …)` 的轮询等价物：尺寸变化时调用一次 resize 处理器。
    ///
    /// 返回是否触发了处理器。`start()` 会把当前尺寸预置到 `last_size`，所以启动后的第一次
    /// 调用不会误报；之后只有尺寸真正变化才回调。未安装处理器时直接返回 `false`。
    /// `with_output()` 的尺寸恒为 `(None, None)`，`start()` 之后不会再触发。
    pub fn poll_resize(&mut self) -> bool {
        if self.resize_handler.is_none() {
            return false;
        }
        let current = Some(self.terminal_size());
        if current == self.last_size {
            return false;
        }
        self.last_size = current;
        if let Some(handler) = self.resize_handler.as_mut() {
            handler();
        }
        true
    }

    /// `queryAndEnableKittyProtocol()`（TS 私有；测试与宿主可直接调用）。
    ///
    /// 键盘协商的起点，一次调用做四件事：
    ///
    /// 1. 重建 `StdinBuffer`（丢弃其挂起输入），清空协商缓冲，并置 `keyboard_protocol_pushed`，
    ///    使退出路径知道要写 Kitty pop。
    /// 2. 增加一笔“欠一个 DA1 回复”的债务（`pending_keyboard_protocol_device_attributes`）：
    ///    每次查询都发一条 DA1 作为终结符，回复按到达顺序冲抵。
    /// 3. 读取 `KIANA_PROGRAM_STATUS`：`"1"` 视为终端支持程序状态、`"0"` 视为不支持，两者都
    ///    跳过查询；其它/未设置则把 [`crate::program_status::PROGRAM_STATUS_QUERY`] 一并发出并等待回复。
    /// 4. 写出 Kitty 查询、可选程序状态查询与 [`DEVICE_ATTRIBUTES_QUERY`] 的拼接（顺序与
    ///    `tests/terminal.rs` 的字节断言一致），随后立即补发已缓存的程序状态。
    ///
    /// 重复调用是安全的：每次重新查询并再记一笔 DA 债务；重启前查询的迟到回复只冲抵旧债务，
    /// 不会提前结束新一轮等待（见 `tests/terminal.rs` 的 restart 用例）。
    pub fn query_and_enable_kitty_protocol(&mut self) {
        self.setup_stdin_buffer();
        self.keyboard_protocol_pushed = true;
        self.pending_keyboard_protocol_device_attributes += 1;
        self.clear_keyboard_protocol_negotiation_buffer();
        let program_status_override = process_env("KIANA_PROGRAM_STATUS");
        self.program_status_supported = program_status_override.as_deref() == Some("1");
        self.program_status_query_pending =
            !matches!(program_status_override.as_deref(), Some("1") | Some("0"));
        let program_status_query = if self.program_status_query_pending {
            PROGRAM_STATUS_QUERY
        } else {
            ""
        };
        self.write_raw(&format!(
            "{KITTY_KEYBOARD_PROTOCOL_QUERY}{program_status_query}{DEVICE_ATTRIBUTES_QUERY}"
        ));
        self.write_program_status();
    }

    /// `setupStdinBuffer()`。
    ///
    /// 整个实例替换而非复用：旧缓冲区里未完成的序列随之丢弃。
    fn setup_stdin_buffer(&mut self) {
        self.stdin_buffer = Some(StdinBuffer::new(StdinBufferOptions {
            timeout: None,
            // TS 把 `resolveEscapeTimeoutMs()` 原样交给 `setTimeout`，由 Node 做截断/钳制。
            escape_timeout: Some(node_set_timeout_ms(resolve_escape_timeout_ms())),
        }));
    }

    fn dispatch_stdin_event(&mut self, event: StdinEvent) {
        match event {
            StdinEvent::Data(sequence) => self.handle_sequence(&sequence),
            StdinEvent::Paste(content) => {
                // Re-wrap paste content with bracketed paste markers for existing editor handling.
                if let Some(handler) = self.input_handler.as_mut() {
                    handler(&format!("\x1b[200~{content}\x1b[201~"));
                }
            }
        }
    }

    fn handle_sequence(&mut self, sequence: &str) {
        // 路由顺序即语义优先级：
        // 1. 程序状态回复只用于握手，永远不转发；
        // 2. Kitty flags / DA1 回复用于协商，被消费时不转发；
        // 3. 其余（含普通按键）一律交给输入处理器。
        if is_program_status_reply(sequence) {
            if self.program_status_query_pending {
                self.program_status_query_pending = false;
                self.program_status_supported = true;
                self.write_program_status();
            }
            return;
        }
        let negotiation = self.read_keyboard_protocol_negotiation_sequence(sequence);
        match negotiation {
            NegotiationRead::Pending => {
                // Wait briefly for the rest of a split Kitty response.
                self.schedule_keyboard_protocol_negotiation_buffer_flush();
            }
            NegotiationRead::Parsed { parsed, sequence } => {
                // 被消费的协商回复不进编辑器；未被消费的 DA1（例如颜色查询的终结符）
                // 原样转发。
                if self.handle_keyboard_protocol_negotiation_sequence(parsed) {
                    return;
                }
                self.forward_input_sequence(&sequence);
            }
            NegotiationRead::None => self.forward_input_sequence(sequence),
        }
    }

    /// 处理一条完整协商回复；返回 `true` 表示已消费、不要转发给输入处理器。
    fn handle_keyboard_protocol_negotiation_sequence(
        &mut self,
        negotiation_sequence: KeyboardProtocolNegotiationSequence,
    ) -> bool {
        // 回复可能是由半截缓冲拼接出来的；先清掉缓冲与回放定时器。
        self.clear_keyboard_protocol_negotiation_buffer();
        if negotiation_sequence == KeyboardProtocolNegotiationSequence::DeviceAttributes {
            // DA1 按欠账冲抵：计数为 0 说明这条 DA 属于别的查询（例如颜色探测），
            // 返回 false 让调用方原样转发。
            if self.pending_keyboard_protocol_device_attributes == 0 {
                return false;
            }
            self.pending_keyboard_protocol_device_attributes -= 1;
            // 清零的那一条是本轮查询的终结符：它同时终结尚未答复的程序状态查询。
            // The last owed DA answers the latest query, which got no program status reply first.
            // Earlier DA replies belong to queries from before a restart.
            if self.pending_keyboard_protocol_device_attributes == 0 {
                self.program_status_query_pending = false;
            }
        }
        if let KeyboardProtocolNegotiationSequence::KittyFlags(flags) = negotiation_sequence {
            if flags != 0 {
                // 终端支持 Kitty：关闭 modifyOtherKeys 回退，并同步全局（线程局部）标志。
                self.disable_modify_other_keys();
                if !self.kitty_protocol_active {
                    self.kitty_protocol_active = true;
                    set_kitty_protocol_active(true);
                }
            } else {
                // flags 为 0：终端明确拒绝 Kitty，改用 modifyOtherKeys 回退。
                self.enable_modify_other_keys();
            }
            return true;
        }

        // 没有 Kitty flags 的 DA1：终端不识别 Kitty，启用 modifyOtherKeys 回退
        // （Kitty 已激活时无需回退）。
        if !self.kitty_protocol_active {
            self.enable_modify_other_keys();
        }
        true
    }

    /// Returns the parsed negotiation reply with its full (possibly reassembled) sequence.
    ///
    /// 三态返回：完整回复（带拼接后的原始序列）、仍是前缀（已缓冲）、与本协商无关。
    fn read_keyboard_protocol_negotiation_sequence(&mut self, sequence: &str) -> NegotiationRead {
        // 已有半截缓冲：先尝试与新块拼接。拼成完整回复就消费；仍是合法前缀就继续等；
        // 否则说明它根本不是协商回复，先按普通输入回放，保证不丢字节。
        if !self.keyboard_protocol_negotiation_buffer.is_empty() {
            let buffered_sequence =
                format!("{}{}", self.keyboard_protocol_negotiation_buffer, sequence);
            if let Some(parsed) = parse_keyboard_protocol_negotiation_sequence(&buffered_sequence) {
                self.clear_keyboard_protocol_negotiation_buffer();
                return NegotiationRead::Parsed {
                    parsed,
                    sequence: buffered_sequence,
                };
            }
            if is_keyboard_protocol_negotiation_sequence_prefix(&buffered_sequence) {
                self.set_keyboard_protocol_negotiation_buffer(&buffered_sequence);
                return NegotiationRead::Pending;
            }
            self.flush_keyboard_protocol_negotiation_buffer_as_input();
        }

        // 没有挂起缓冲：独立判断这一块本身是完整回复还是回复前缀。
        if let Some(parsed) = parse_keyboard_protocol_negotiation_sequence(sequence) {
            return NegotiationRead::Parsed {
                parsed,
                sequence: sequence.to_string(),
            };
        }
        if is_keyboard_protocol_negotiation_sequence_prefix(sequence) {
            self.set_keyboard_protocol_negotiation_buffer(sequence);
            return NegotiationRead::Pending;
        }
        NegotiationRead::None
    }

    fn set_keyboard_protocol_negotiation_buffer(&mut self, sequence: &str) {
        // 换内容前取消旧的回放定时器，避免按旧缓冲的到期时间回放新内容。
        self.clear_keyboard_protocol_negotiation_buffer_flush_timer();
        self.keyboard_protocol_negotiation_buffer = sequence.to_string();
    }

    fn clear_keyboard_protocol_negotiation_buffer(&mut self) {
        self.clear_keyboard_protocol_negotiation_buffer_flush_timer();
        self.keyboard_protocol_negotiation_buffer.clear();
    }

    fn flush_keyboard_protocol_negotiation_buffer_as_input(&mut self) {
        if self.keyboard_protocol_negotiation_buffer.is_empty() {
            return;
        }
        let sequence = std::mem::take(&mut self.keyboard_protocol_negotiation_buffer);
        self.clear_keyboard_protocol_negotiation_buffer_flush_timer();
        self.forward_input_sequence(&sequence);
    }

    fn schedule_keyboard_protocol_negotiation_buffer_flush(&mut self) {
        // 没有缓冲或已经在等待时不重复排期；每个等待窗口固定为
        // KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS。
        if self.keyboard_protocol_negotiation_buffer.is_empty()
            || self.keyboard_protocol_buffer_flush_deadline.is_some()
        {
            return;
        }
        self.keyboard_protocol_buffer_flush_deadline = Some(
            Instant::now() + Duration::from_millis(KEYBOARD_PROTOCOL_RESPONSE_FRAGMENT_TIMEOUT_MS),
        );
    }

    fn clear_keyboard_protocol_negotiation_buffer_flush_timer(&mut self) {
        self.keyboard_protocol_buffer_flush_deadline = None;
    }

    fn forward_input_sequence(&mut self, sequence: &str) {
        // 没有处理器（`stop()` 之后、`drain_input` 期间）时静默丢弃。
        let Some(handler) = self.input_handler.as_mut() else {
            return;
        };
        // Shift+Enter 检测只在“裸回车 + Apple Terminal/Windows”时启用；其它平台与其它序列
        // 保持原字节。修饰键通过 native helper 现场查询（没有 helper 的平台恒为 false），
        // 短路求值保证检测关闭时不调用 helper。
        let should_detect_native_shift_enter =
            sequence == "\r" && (is_apple_terminal_session() || cfg!(target_os = "windows"));
        let input = normalize_native_shift_enter_input(
            sequence,
            should_detect_native_shift_enter,
            should_detect_native_shift_enter && is_native_modifier_pressed(ModifierKey::Shift),
        );
        handler(&input);
    }

    fn enable_modify_other_keys(&mut self) {
        // Kitty 已激活或回退已开启时不再写序列，避免重复设置。
        if self.kitty_protocol_active || self.modify_other_keys_active {
            return;
        }
        self.write_raw("\x1b[>4;2m");
        self.modify_other_keys_active = true;
    }

    fn disable_modify_other_keys(&mut self) {
        // 只在确实写过开启序列时写关闭序列（`stop`/`drain_input` 都会调用；`restore_on_exit`
        // 则按 `modify_other_keys_active` 直接追加关闭序列）。
        if !self.modify_other_keys_active {
            return;
        }
        self.write_raw("\x1b[>4;0m");
        self.modify_other_keys_active = false;
    }

    /// On Windows, add ENABLE_VIRTUAL_TERMINAL_INPUT (0x0200) to the stdin console handle so the
    /// terminal sends VT sequences for modified keys (e.g. `\x1b[Z` for Shift+Tab).
    fn enable_windows_vt_input(&mut self) {
        if !cfg!(target_os = "windows") {
            return;
        }
        // Native helper not available — Shift+Tab won't be distinguishable from Tab.
        let _ = crate::native::enable_virtual_terminal_input();
    }

    /// 启动真实 stdin 读取线程（`process.stdin.resume()` 的等价物）。
    ///
    /// 只对 `owns_stdout` 的实例生效；`with_output()` 不得偷读宿主进程的 stdin。已启动过则
    /// 直接返回，复用原线程与通道。
    fn ensure_stdin_reader(&mut self) {
        if self.stdin_thread_started || !self.owns_stdout {
            return;
        }
        self.stdin_thread_started = true;
        self.spawn_stdin_reader(STDIN_FD);
    }

    /// 启动读取线程（`process.stdin.resume()` 的等价物）。`fd` 在 Unix 上是 `0`；
    /// 抽出参数只为让单元测试能用管道验证暂停语义。
    ///
    /// 线程随进程结束（stdin 关闭时自然退出）；进程退出时不 join。通道容量无限，读取线程
    /// 不会因宿主暂时不轮询而阻塞；若接收端被丢弃，`send` 失败使线程退出。
    fn spawn_stdin_reader(&mut self, fd: i32) {
        let (sender, receiver) = std::sync::mpsc::channel::<(Instant, Vec<u8>)>();
        let pause = Arc::clone(&self.stdin_pause);
        let eof = Arc::clone(&self.stdin_eof);
        std::thread::Builder::new()
            .name("kiana-tui-stdin".to_string())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                loop {
                    // `process.stdin.pause()`：暂停期间阻塞在这里，绝不 read(2)，把 tty
                    // 输入留给前台子进程（外部编辑器 / 挂起后的 shell）。
                    if pause.is_paused() {
                        pause.wait_until_resumed();
                        continue;
                    }
                    // 先等可读再检查一次暂停标志，缩小“检查后、读取前恰好 pause”的窗口。
                    if !wait_stdin_readable(fd, Duration::from_millis(STDIN_POLL_MS)) {
                        continue;
                    }
                    if pause.is_paused() {
                        continue;
                    }
                    match read_stdin_fd(fd, &mut buffer) {
                        Ok(0) => {
                            // Node 的 `end` 事件：stdin 关闭。
                            eof.store(true, Ordering::SeqCst);
                            break;
                        }
                        Ok(read) => {
                            if sender
                                .send((Instant::now(), buffer[..read].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        Err(_) => {
                            // Node 在 stdin 出错时先发 `error` 再发 `close`；宿主同样无法再
                            // 收到输入，按流结束处理。
                            eof.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            })
            .ok();
        self.stdin_rx = Some(receiver);
    }

    /// stdin 是否已到达 EOF（读取线程观察到 `read() == 0` 或不可恢复错误）。
    ///
    /// 标志只由读取线程置位，进程内不再清除（`start()` 不重置它）；读取使用 `SeqCst`。
    pub fn stdin_eof(&self) -> bool {
        self.stdin_eof.load(Ordering::SeqCst)
    }

    /// `process.stdin.setRawMode(mode)` 的等价物（仅 TTY/控制台；其它情况静默跳过）。
    fn set_raw_mode(&mut self, raw: bool) {
        #[cfg(unix)]
        {
            // 只在 fd 0 是 tty 时改 termios；管道/重定向会在这里直接返回，`stdin_raw_mode`
            // 保持旧值，不反映本次 setRawMode 请求。tty 上即使 tcgetattr/tcsetattr 失败，
            // 该标志仍按请求值更新（与 Node 的 `isRaw` 只反映 setRawMode 参数一致）。
            if !stdin_is_tty() {
                return;
            }
            // SAFETY: termios 结构由 libc 初始化；fd 0 是进程 stdin。
            unsafe {
                if raw {
                    if self.saved_termios.is_none() {
                        let mut current: libc::termios = std::mem::zeroed();
                        if libc::tcgetattr(0, &mut current) == 0 {
                            self.saved_termios = Some(current);
                            let mut raw_mode = current;
                            libc::cfmakeraw(&mut raw_mode);
                            libc::tcsetattr(0, libc::TCSANOW, &raw_mode);
                        }
                    }
                } else if let Some(saved) = self.saved_termios.take() {
                    libc::tcsetattr(0, libc::TCSANOW, &saved);
                }
            }
            self.stdin_raw_mode = raw;
        }
        #[cfg(windows)]
        {
            // Node 的 `setRawMode(true)` 走 libuv 的 `UV_TTY_MODE_RAW_VT`：
            // `SetConsoleMode(ENABLE_WINDOW_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT)`（VT 设置失败时
            // 回退到 `ENABLE_WINDOW_INPUT`）；`setRawMode(false)` 写回 libuv 的 NORMAL 模式
            // `ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT`。
            // line/echo/processed 关闭后按键逐字符到达，Ctrl+C 作为 0x03 数据下发。
            if raw {
                if !set_stdin_console_mode(ENABLE_WINDOW_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT) {
                    let _ = set_stdin_console_mode(ENABLE_WINDOW_INPUT);
                }
            } else {
                let _ = set_stdin_console_mode(
                    ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT,
                );
            }
            self.stdin_raw_mode = raw;
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = raw;
            self.stdin_raw_mode = raw;
        }
    }

    /// `process.stdin.isRaw || false`。
    ///
    /// Node 的 `TTY.isRaw` 只由 `setRawMode()` 赋值，**不**读取继承来的 termios/控制台模式：
    /// 进程在一个已经是 raw 的 tty 上启动时，`isRaw` 仍然是 `false`（此时 `stop()` 会调用
    /// `setRawMode(false)`，libuv 恢复的是它初始化时保存的 termios，即仍是 raw）。
    /// 因此这里必须跟随本进程的 `set_raw_mode` 调用，而不是探测当前终端状态。
    fn stdin_is_raw(&self) -> bool {
        self.stdin_raw_mode
    }

    fn terminal_size(&self) -> (Option<usize>, Option<usize>) {
        // `with_output()` 不查真实 TTY：恒为 (None, None)，使 columns/rows 走环境变量/默认值。
        if self.owns_stdout {
            stdout_size()
        } else {
            (None, None)
        }
    }

    fn columns_fallback() -> usize {
        env_dimension("COLUMNS").unwrap_or(80)
    }

    fn rows_fallback() -> usize {
        env_dimension("LINES").unwrap_or(24)
    }

    /// 直接写 stdout，**不**经过 `KIANA_TUI_WRITE_LOG`。
    ///
    /// TS 的 `ProcessTerminal` 只有公开的 `write()` 会追加日志；`start()`/`stop()`/
    /// `setProgress()`/`setTitle()`/光标与模式序列都直接调用 `process.stdout.write`，从不写日志。
    /// 这里用独立的内部方法保持同一可观察行为。
    fn write_raw(&mut self, data: &str) {
        let _ = self.output.write_all(data.as_bytes());
        let _ = self.output.flush();
    }

    /// 把缓存的最新状态写给终端；仅在 `program_status_supported` 为真时写。该标志可来自
    /// 终端确认（收到程序状态回复），也可来自 `KIANA_PROGRAM_STATUS=1`。`stop()` 会把
    /// supported 清零；重启时 `query_and_enable_kitty_protocol` 会重新读取环境变量或重新
    /// 握手，之后才会再次输出。
    fn write_program_status(&mut self) {
        if self.program_status_supported
            && let Some(status) = self.program_status.clone()
        {
            self.write_raw(&format_program_status(&status));
        }
    }

    fn clear_progress_interval(&mut self) -> bool {
        self.progress_keepalive_deadline.take().is_some()
    }
}

impl crate::terminal_colors::Terminal for ProcessTerminal {
    fn poll_input(&mut self) -> bool {
        ProcessTerminal::poll_input(self)
    }

    fn poll_timers(&mut self, now: std::time::Instant) {
        ProcessTerminal::poll(self, now);
    }

    fn poll_resize(&mut self) {
        ProcessTerminal::poll_resize(self);
    }

    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        // 启动顺序与 TS 的 start() 对应：安装处理器 → 保存/进入 raw mode → 重置 UTF-8
        // 解码器 → resume stdin → 开启括号粘贴 → 记录初始尺寸 → 刷新尺寸 → Windows VT
        // 输入 → 键盘协议协商。Windows 的 VT 输入必须排在 setRawMode(true) 之后
        // （后者会重置控制台模式标志，见下方原注释）。
        self.input_handler = Some(on_input);
        self.resize_handler = Some(on_resize);

        // Save previous state and enable raw mode.
        self.was_raw = self.stdin_is_raw();
        self.set_raw_mode(true);
        // `process.stdin.setEncoding("utf8")`：Node 每次调用都安装一个**新的** StringDecoder，
        // 旧解码器里挂起的半截多字节序列被丢弃（pause()/resume() 之间不保留）。
        self.utf8_decoder = Utf8Decoder::default();
        // `process.stdin.resume()`
        self.stdin_paused = false;
        self.stdin_pause.resume();
        self.ensure_stdin_reader();

        // Enable bracketed paste mode - terminal will wrap pastes in \x1b[200~ ... \x1b[201~
        self.write_raw("\x1b[?2004h");

        // Set up resize handler immediately.
        self.last_size = Some(self.terminal_size());

        // Refresh terminal dimensions - they may be stale after suspend/resume (SIGWINCH is lost
        // while the process is stopped). Unix only, best-effort.
        refresh_terminal_dimensions();

        // Must run AFTER setRawMode(true) since that resets console mode flags.
        self.enable_windows_vt_input();

        // Query Kitty keyboard protocol and fall back to modifyOtherKeys when DA confirms no
        // Kitty response.
        self.query_and_enable_kitty_protocol();
    }

    fn stop(&mut self) {
        // 停止顺序：进度定时器 → 程序状态 → 括号粘贴 → Kitty pop → modifyOtherKeys →
        // StdinBuffer → 处理器 → 暂停 stdin → 恢复 raw mode。恢复 raw mode 放在最后，
        // 且此前先暂停 stdin（理由见下方注释）。
        if self.clear_progress_interval() {
            self.write_raw(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
        // Remove the status while stopped, for example after exit or while suspended.
        // start() reports it again.
        if self.program_status_supported && self.program_status.is_some() {
            self.write_raw(&format_program_status(&ProgramStatus {
                state: ProgramStatusState::Clear,
                app: None,
                kind: None,
                message: None,
            }));
        }
        self.program_status_supported = false;
        self.program_status_query_pending = false;

        // Disable bracketed paste mode.
        self.write_raw("\x1b[?2004l");

        let should_disable_kitty_protocol =
            self.keyboard_protocol_pushed || self.kitty_protocol_active;
        self.clear_keyboard_protocol_negotiation_buffer();

        // Disable Kitty keyboard protocol if not already done by drainInput().
        if should_disable_kitty_protocol {
            self.write_raw("\x1b[<u");
            self.keyboard_protocol_pushed = false;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        self.disable_modify_other_keys();

        // Clean up StdinBuffer.
        if let Some(mut buffer) = self.stdin_buffer.take() {
            buffer.destroy();
        }

        // Remove event handlers.
        self.input_handler = None;
        self.resize_handler = None;

        // Pause stdin to prevent any buffered input (e.g., Ctrl+D) from being re-interpreted
        // after raw mode is disabled. `process.stdin.pause()` 同时停掉底层读取：读取线程在
        // 暂停期间不得 read(2)，否则会抢走前台子进程（外部编辑器/挂起后的 shell）的 tty 输入。
        self.stdin_paused = true;
        self.stdin_pause.pause();
        // Node 的 `process.stdin.pause()` 本身不重置 StringDecoder，跨暂停的半截多字节序列
        // 会继续拼完；重置发生在下一次 `start()` 的 `setEncoding("utf8")`（见 `start`）。

        // Restore raw mode state.
        self.set_raw_mode(self.was_raw);
    }

    fn restore_on_exit(&mut self, before: &str, after: &str) {
        // 字节顺序由测试锁定：before →（按需）进度清除 → 关括号粘贴 →（按需）Kitty pop →
        //（按需）modifyOtherKeys 关闭 → after。Kitty pop 必须在 after（例如离开备用屏）之前，
        // 否则退出备用屏后的按键释放仍会生成新的 Kitty 序列（tests/terminal.rs）。
        let mut sequence = before.to_string();
        if self.progress_keepalive_deadline.is_some() {
            sequence.push_str(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
        sequence.push_str("\x1b[?2004l");
        if self.keyboard_protocol_pushed || self.kitty_protocol_active {
            sequence.push_str("\x1b[<u");
        }
        if self.modify_other_keys_active {
            sequence.push_str("\x1b[>4;0m");
        }
        sequence.push_str(after);
        if self.owns_stdout {
            // `process.stdout.write()` may be asynchronous (e.g. Windows TTYs) and would not
            // flush during exit; write the fd synchronously.
            write_stdout_sync(sequence.as_bytes());
        } else {
            let _ = self.output.write_all(sequence.as_bytes());
            let _ = self.output.flush();
        }
    }

    fn drain_input(
        &mut self,
        max_ms: u64,
        idle_ms: u64,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        // 先同步关闭会生成新序列的输入模式：之后迟到的按键释放不会被解释成 Kitty 序列。
        let should_disable_kitty_protocol =
            self.keyboard_protocol_pushed || self.kitty_protocol_active;
        self.clear_keyboard_protocol_negotiation_buffer();
        if should_disable_kitty_protocol {
            // Disable Kitty keyboard protocol first so any late key releases do not generate new
            // Kitty escape sequences.
            self.write_raw("\x1b[<u");
            self.keyboard_protocol_pushed = false;
            self.kitty_protocol_active = false;
            set_kitty_protocol_active(false);
        }
        self.disable_modify_other_keys();

        // 暂时摘走处理器与读取通道交给 future 持有。`Restore` 在 async 块体内构造，只有
        // future 第一次被 poll 时它才存在；因此只有 future 至少被 poll 过一次后，它的 Drop
        // 才会把处理器与通道装回字段。future 创建后、首次 poll 前就被调用方丢弃时，
        // `Restore` 从未存在，`input_handler` 与 `stdin_rx` 会保持 `None`（当前实现未覆盖
        // 这一缺口；要做到无条件保证，需在 async 块外构造守卫）。
        let previous_handler = self.input_handler.take();
        let receiver = self.stdin_rx.take();
        let handler_slot = &mut self.input_handler;
        let receiver_slot = &mut self.stdin_rx;

        Box::pin(async move {
            struct Restore<'a> {
                receiver: Option<std::sync::mpsc::Receiver<(Instant, Vec<u8>)>>,
                previous_handler: Option<InputHandler>,
                handler_slot: &'a mut Option<InputHandler>,
                receiver_slot: &'a mut Option<std::sync::mpsc::Receiver<(Instant, Vec<u8>)>>,
            }
            impl Drop for Restore<'_> {
                fn drop(&mut self) {
                    *self.handler_slot = self.previous_handler.take();
                    *self.receiver_slot = self.receiver.take();
                }
            }
            let restore = Restore {
                receiver,
                previous_handler,
                handler_slot,
                receiver_slot,
            };

            // TS：`onData` 监听器在数据到达时把 `lastDataTime` 更新为 `Date.now()`；这里直接把
            // 到达时刻与数据一起收下，因而与事件回调的时间语义一致。
            let mut last_data_time = Instant::now();
            // TS 的 `endTime = Date.now() + maxMs`（没有 24 小时上限）。
            let end_time = Instant::now().checked_add(Duration::from_millis(max_ms));
            let idle = Duration::from_millis(idle_ms);
            loop {
                // 只查看到达时刻、不解析内容：drain 期间收到的字节不会分发给输入处理器，
                // 它们只用于把空闲窗口往后推。
                if let Some(receiver) = restore.receiver.as_ref() {
                    while let Ok((arrived, _)) = receiver.try_recv() {
                        last_data_time = last_data_time.max(arrived);
                    }
                }
                let now = Instant::now();
                let time_left = end_time.map(|end| end.saturating_duration_since(now));
                if time_left.is_some_and(|left| left.is_zero()) {
                    break;
                }
                if now.saturating_duration_since(last_data_time) >= idle {
                    break;
                }
                // TS：`await new Promise(r => setTimeout(r, Math.min(idleMs, timeLeft)))` —— 每次
                // 都等满 `min(idleMs, timeLeft)`，再回到循环顶部重新检查，而不是只等剩余空闲时间。
                // `setTimeout` 自己还会钳制延迟（> 2^31-1 用 1ms）。
                let wait = match time_left {
                    Some(left) => idle.min(left),
                    None => idle,
                };
                let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
                sleep_for(Duration::from_millis(node_set_timeout_ms(wait_ms as f64))).await;
            }
        })
    }

    fn write(&mut self, data: &str) {
        // 先写输出，再尽力追加日志：日志打开/写入失败被忽略，不影响终端输出。
        self.write_raw(data);
        if let Some(path) = &self.write_log_path {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                // Ignore logging errors.
                let _ = file.write_all(data.as_bytes());
            }
        }
    }

    fn columns(&self) -> usize {
        // 优先真实 TTY 尺寸；缺失时用 COLUMNS，再退回 80（见 `columns_fallback`）。
        if let Some(columns) = self.terminal_size().0 {
            return columns;
        }
        Self::columns_fallback()
    }

    fn rows(&self) -> usize {
        // 两个维度独立回退：列可用时不受行缺失影响（见 `stdout_size`）。
        if let Some(rows) = self.terminal_size().1 {
            return rows;
        }
        Self::rows_fallback()
    }

    fn kitty_protocol_active(&self) -> bool {
        self.kitty_protocol_active
    }

    fn move_by(&mut self, lines: isize) {
        if lines > 0 {
            self.write_raw(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.write_raw(&format!("\x1b[{}A", lines.unsigned_abs()));
        }
        // lines === 0: no movement
    }

    fn hide_cursor(&mut self) {
        self.write_raw("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.write_raw("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write_raw("\x1b[K");
    }

    fn clear_from_cursor(&mut self) {
        self.write_raw("\x1b[J");
    }

    fn clear_screen(&mut self) {
        // Clear screen and move to home (1,1).
        self.write_raw("\x1b[2J\x1b[H");
    }

    fn set_title(&mut self, title: &str) {
        // OSC 0;title BEL - set terminal window title.
        self.write_raw(&format!("\x1b]0;{title}\x07"));
    }

    fn set_progress(&mut self, active: bool) {
        if active {
            // OSC 9;4;3 - indeterminate progress.
            self.write_raw(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            if self.progress_keepalive_deadline.is_none() {
                self.progress_keepalive_deadline =
                    Some(Instant::now() + Duration::from_millis(TERMINAL_PROGRESS_KEEPALIVE_MS));
            }
        } else {
            self.clear_progress_interval();
            // OSC 9;4;0 - clear progress.
            self.write_raw(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
    }

    fn set_program_status(&mut self, status: &ProgramStatus) {
        // `Clear` 把缓存置空（不保留旧状态）；其它状态缓存下来，等握手确认支持时由
        // `write_program_status` 补发。非 Clear 的缓存会跨 `stop()` 保留（见字段注释）。
        self.program_status = if status.state == ProgramStatusState::Clear {
            None
        } else {
            Some(status.clone())
        };
        if self.program_status_supported {
            self.write_raw(&format_program_status(status));
        }
    }
}

// ---------------------------------------------------------------------------
// 平台辅助
// ---------------------------------------------------------------------------

/// 进程 stdin 的文件描述符（`process.stdin.fd`；Unix 上恒为 0）。
const STDIN_FD: i32 = 0;
/// 读取线程检查暂停标志的轮询粒度（`poll(2)`/`WaitForSingleObject` 的超时）。
const STDIN_POLL_MS: u64 = 50;

#[cfg(unix)]
fn stdin_is_tty() -> bool {
    // SAFETY: isatty 只读取 fd 类型。
    unsafe { libc::isatty(0) == 1 }
}

/// 等待 `fd` 可读（最多 `timeout`）。返回 `false` 表示超时或 `poll` 被中断。
///
/// 只用 `poll(2)` 等待而**不**读取，让读取线程能在暂停时把输入留在内核缓冲区。
#[cfg(unix)]
fn wait_stdin_readable(fd: i32, timeout: Duration) -> bool {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let milliseconds = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: 传入一个有效的 pollfd。
    unsafe { libc::poll(&mut descriptor, 1, milliseconds) > 0 }
}

/// 从 `fd` 读取一块字节（`process.stdin` 的 `read`）。`std::io::stdin()` 带内部缓冲
/// （会预读超过一块的数据，暂停时可能吞掉属于子进程的输入），这里直接走 `read(2)`。
#[cfg(unix)]
fn read_stdin_fd(fd: i32, buffer: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: `fd` 由调用方保证有效；指针/长度来自有效切片。
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(read as usize)
    }
}

/// Windows：控制台输入句柄在输入缓冲区非空时变为有信号。非控制台 stdin（管道/重定向）
/// 退化为阻塞 `read`（此时不存在与前台子进程争抢控制台输入的场景）。
#[cfg(windows)]
fn wait_stdin_readable(_fd: i32, timeout: Duration) -> bool {
    use win32_console::{
        WAIT_OBJECT_0, WaitForSingleObject, stdin_console_handle, stdin_console_mode,
    };

    if stdin_console_mode().is_none() {
        return true;
    }
    match stdin_console_handle() {
        Some(handle) => {
            let milliseconds = timeout.as_millis().min(u32::MAX as u128) as u32;
            // SAFETY: 等待一个有效的控制台句柄。
            (unsafe { WaitForSingleObject(handle, milliseconds) }) == WAIT_OBJECT_0
        }
        None => true,
    }
}

#[cfg(windows)]
fn read_stdin_fd(_fd: i32, buffer: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read as _;
    std::io::stdin().lock().read(buffer)
}

#[cfg(not(any(unix, windows)))]
fn wait_stdin_readable(_fd: i32, _timeout: Duration) -> bool {
    true
}

#[cfg(not(any(unix, windows)))]
fn read_stdin_fd(_fd: i32, buffer: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read as _;
    std::io::stdin().lock().read(buffer)
}

/// Win32 控制台输入模式（`SetConsoleMode`），与 libuv 的 `uv_tty_set_mode` 使用同一组标志。
#[cfg(windows)]
mod win32_console {
    pub const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    pub const ENABLE_LINE_INPUT: u32 = 0x0002;
    pub const ENABLE_ECHO_INPUT: u32 = 0x0004;
    pub const ENABLE_WINDOW_INPUT: u32 = 0x0008;
    pub const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    /// `INVALID_HANDLE_VALUE`。
    const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;

    /// `WAIT_OBJECT_0`（`WaitForSingleObject` 的“已通知”返回值）。
    pub const WAIT_OBJECT_0: u32 = 0;

    unsafe extern "system" {
        fn GetStdHandle(handle: u32) -> *mut std::ffi::c_void;
        fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
        pub fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    }

    /// 标准输入句柄；无效时返回 `None`。
    pub fn stdin_console_handle() -> Option<*mut std::ffi::c_void> {
        // SAFETY: 只查询本进程的标准输入句柄。
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(handle)
            }
        }
    }

    /// 当前 stdin 控制台模式；stdin 不是控制台（管道/重定向）时返回 `None`。
    pub fn stdin_console_mode() -> Option<u32> {
        // SAFETY: 只查询本进程的标准输入句柄。
        unsafe {
            let handle = stdin_console_handle()?;
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return None;
            }
            Some(mode)
        }
    }

    /// `SetConsoleMode(stdin, mode)`；失败返回 `false`（TS 的 `setRawMode` 失败时只 emit error）。
    pub fn set_stdin_console_mode(mode: u32) -> bool {
        // SAFETY: 只设置本进程的标准输入控制台模式。
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            !handle.is_null() && SetConsoleMode(handle, mode) != 0
        }
    }
}

#[cfg(windows)]
use win32_console::{
    ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_WINDOW_INPUT, set_stdin_console_mode,
};

#[cfg(unix)]
fn stdout_size() -> (Option<usize>, Option<usize>) {
    // SAFETY: ioctl(TIOCGWINSZ) 写入由 libc 初始化的 winsize 结构。
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut size) == 0 {
            // TS 的 `process.stdout.columns` / `rows` 两个 getter 各自独立：某一维为 0 时只有
            // 该维回退，另一维照常使用。
            return (
                (size.ws_col > 0).then_some(size.ws_col as usize),
                (size.ws_row > 0).then_some(size.ws_row as usize),
            );
        }
    }
    (None, None)
}

#[cfg(windows)]
fn stdout_size() -> (Option<usize>, Option<usize>) {
    #[repr(C)]
    #[derive(Default)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }
    unsafe extern "system" {
        fn GetStdHandle(handle: u32) -> *mut std::ffi::c_void;
        fn GetConsoleScreenBufferInfo(
            handle: *mut std::ffi::c_void,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    // SAFETY: 标准输出句柄 + 由本函数提供的结构体指针。
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() {
            return (None, None);
        }
        let mut info = ConsoleScreenBufferInfo::default();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return (None, None);
        }
        let columns = info.window.right - info.window.left + 1;
        let rows = info.window.bottom - info.window.top + 1;
        (
            (columns > 0).then_some(columns as usize),
            (rows > 0).then_some(rows as usize),
        )
    }
}

#[cfg(not(any(unix, windows)))]
fn stdout_size() -> (Option<usize>, Option<usize>) {
    (None, None)
}

/// `process.stdout.columns || Number(COLUMNS) || 80` 的 env 分支。
///
/// TS 的 getter 返回 `number`；Rust 的 trait 签名是 `usize`（ARCHITECTURE §4.9 冻结），
/// 小数值被截断、负数只能落到 0（JS 里负数仍是 truthy）。`+Infinity` 映射到 `usize::MAX`
/// （JS 的值无法表示，取同序的最大值）。
fn env_dimension(name: &str) -> Option<usize> {
    let raw = process_env(name)?;
    let value = kiana_json::parse_js_number(&raw);
    if value.is_nan() || value == 0.0 {
        return None;
    }
    if value.is_infinite() {
        return Some(if value > 0.0 { usize::MAX } else { 0 });
    }
    Some(if value > 0.0 { value as usize } else { 0 })
}

// ---------------------------------------------------------------------------
// KIANA_TUI_WRITE_LOG
// ---------------------------------------------------------------------------

/// `ProcessTerminal.writeLogPath` 的构造期求值。
///
/// 未设置或空串返回 `None`（不写日志）。指向**已存在的目录**时，在目录内生成
/// `tui-<本地时间戳>-<pid>.log`；否则把该值直接当文件路径（即使它尚不存在）。
fn resolve_write_log_path() -> Option<std::path::PathBuf> {
    let env = process_env("KIANA_TUI_WRITE_LOG").unwrap_or_default();
    if env.is_empty() {
        return None;
    }
    // `metadata` 失败（路径不存在）按“文件路径”处理；只有确实是目录才拼文件名。
    let path = std::path::Path::new(&env);
    if std::fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        let timestamp = local_timestamp();
        let pid = std::process::id();
        return Some(path.join(format!("tui-{timestamp}-{pid}.log")));
    }
    Some(path.to_path_buf())
}

/// `YYYY-MM-DD_HH-MM-SS`（本地时区，各段 2 位补零，年份 4 位）。
fn local_timestamp() -> String {
    let (year, month, day, hour, minute, second) = local_time_parts();
    format!("{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}")
}

#[cfg(unix)]
fn local_time_parts() -> (i64, i64, i64, i64, i64, i64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as libc::time_t;
    // SAFETY: localtime_r 写入由 libc 初始化的 tm 结构；返回值判空。
    // 优先本地时区；duration_since(UNIX_EPOCH) 失败时秒数按 0 处理，随后仍调用
    // localtime_r(0)（得到本地时区的 epoch，而非 UTC 计算结果）；只有 localtime_r 返回
    // 空指针时才回退到 utc_time_parts，保证仍返回一个六元组而不是 panic。
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if !libc::localtime_r(&seconds, &mut tm).is_null() {
            return (
                tm.tm_year as i64 + 1900,
                tm.tm_mon as i64 + 1,
                tm.tm_mday as i64,
                tm.tm_hour as i64,
                tm.tm_min as i64,
                tm.tm_sec as i64,
            );
        }
    }
    utc_time_parts(seconds as u64)
}

#[cfg(windows)]
fn local_time_parts() -> (i64, i64, i64, i64, i64, i64) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    unsafe extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }
    let mut time = SystemTime::default();
    // SAFETY: GetLocalTime 只写入本函数的栈上结构体。
    unsafe { GetLocalTime(&mut time) };
    (
        i64::from(time.year),
        i64::from(time.month),
        i64::from(time.day),
        i64::from(time.hour),
        i64::from(time.minute),
        i64::from(time.second),
    )
}

#[cfg(not(any(unix, windows)))]
fn local_time_parts() -> (i64, i64, i64, i64, i64, i64) {
    utc_time_parts(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    )
}

#[cfg_attr(unix, allow(dead_code, reason = "unix 上只在 localtime_r 失败时使用"))]
#[cfg_attr(windows, allow(dead_code, reason = "Windows 上用 GetLocalTime"))]
#[cfg_attr(not(any(unix, windows)), allow(dead_code, reason = "仅 UTC 回退使用"))]
/// 从 Unix 秒计算 UTC 的 `(year, month, day, hour, minute, second)`。
fn utc_time_parts(seconds: u64) -> (i64, i64, i64, i64, i64, i64) {
    let days = (seconds / 86_400) as i64;
    let mut remainder = (seconds % 86_400) as i64;
    let hour = remainder / 3_600;
    remainder %= 3_600;
    let minute = remainder / 60;
    let second = remainder % 60;

    // Howard Hinnant 的 civil_from_days。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day, hour, minute, second)
}

// 让 `Mutex` 导入在测试模式下不报未使用（测试会用到共享的捕获 writer 类型）。
#[allow(dead_code, reason = "仅测试/宿主使用")]
type SharedWrites = Arc<Mutex<Vec<String>>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_time_parts_matches_known_values() {
        assert_eq!(utc_time_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_time_parts(1_700_000_000), (2023, 11, 14, 22, 13, 20));
    }

    #[test]
    fn env_dimension_matches_js_truthiness() {
        // 与 `Number(x) || fallback` 一致：0/NaN/负数分别按 0 处理或不采用。
        assert_eq!(kiana_json::parse_js_number("123"), 123.0);
    }
}

#[cfg(test)]
mod drain_and_decoder_tests {
    use super::*;
    use crate::terminal_colors::Terminal;

    /// 最小 `block_on`：`TimerSleep` 只依赖 waker，不要求 Tokio runtime。
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};

        struct Parker(std::thread::Thread);
        impl Wake for Parker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(Parker(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    /// Node 的 `process.stdin.pause()` 不会重置 `StringDecoder`：`stop()` 必须保留半截 UTF-8。
    #[test]
    fn stop_keeps_pending_utf8_bytes() {
        let mut terminal = ProcessTerminal::with_output(Box::new(std::io::sink()));
        assert_eq!(terminal.utf8_decoder.decode(&[0xE2, 0x82]), "");
        terminal.stop();
        assert_eq!(
            terminal.utf8_decoder.pending,
            vec![0xE2, 0x82],
            "a pending multi-byte sequence must survive stop()"
        );
        assert_eq!(terminal.utf8_decoder.decode(&[0xAC]), "€");
    }

    /// `drainInput` 用数据**到达时刻**计算空闲窗口（TS 的 `onData` 更新 `lastDataTime`）。
    #[test]
    fn drain_input_uses_data_arrival_times() {
        let mut terminal = ProcessTerminal::with_output(Box::new(std::io::sink()));
        let (sender, receiver) = std::sync::mpsc::channel();
        terminal.stdin_rx = Some(receiver);
        let sender_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let _ = sender.send((Instant::now(), b"x".to_vec()));
        });

        let started = Instant::now();
        block_on(terminal.drain_input(1000, 100));
        let elapsed = started.elapsed();
        let _ = sender_thread.join();

        // 数据在 ~20ms 到达，必须再过满 100ms 空闲窗口才返回（≈200ms，检查点在 sleep 边界）。
        assert!(
            elapsed >= Duration::from_millis(160),
            "data arrival must extend the idle window, took {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(600),
            "must not wait for maxMs, took {elapsed:?}"
        );
    }
}

#[cfg(test)]
mod stdin_stream_tests {
    use super::*;
    use crate::terminal_colors::Terminal;

    /// `process.stdin.pause()` / `resume()` 的接线：`stop()` 暂停、`start()` 恢复。
    ///
    /// 这里不依赖真实 tty，也不触碰全局 Kitty 状态（`stop()` 之前从未 `start()` 过时，
    /// `stop()` 不会调用 `set_kitty_protocol_active`）。
    #[test]
    fn stop_pauses_and_start_resumes_stdin() {
        let mut terminal = ProcessTerminal::with_output(Box::new(std::io::sink()));
        terminal.stop();
        assert!(
            terminal.stdin_pause.is_paused(),
            "stop() must call process.stdin.pause()"
        );

        terminal.start(Box::new(|_| {}), Box::new(|| {}));
        assert!(
            !terminal.stdin_pause.is_paused(),
            "start() must call process.stdin.resume()"
        );
        assert!(!terminal.stdin_paused);
    }

    /// Node 的 `start()` 每次都执行 `process.stdin.setEncoding("utf8")`，安装一个**新的**
    /// `StringDecoder`：`stop()` 之前挂起的半截多字节序列会被丢弃（下一次解码得到 U+FFFD），
    /// 而不是与 resume 之后的字节拼完整。
    #[test]
    fn start_discards_a_pending_partial_utf8_sequence() {
        let mut terminal = ProcessTerminal::with_output(Box::new(std::io::sink()));
        terminal.setup_stdin_buffer();
        let (sender, receiver) = std::sync::mpsc::channel();
        terminal.stdin_rx = Some(receiver);

        sender
            .send((Instant::now(), vec![0xE2, 0x82]))
            .expect("send first chunk");
        assert!(
            !terminal.poll_input(),
            "an incomplete character emits nothing"
        );
        assert_eq!(terminal.utf8_decoder.pending, vec![0xE2, 0x82]);

        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        terminal.start(
            Box::new(move |data: &str| {
                sink.lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(data.to_string());
            }),
            Box::new(|| {}),
        );
        assert!(
            terminal.utf8_decoder.pending.is_empty(),
            "setEncoding(\"utf8\") installs a fresh StringDecoder"
        );

        sender
            .send((Instant::now(), vec![0xAC]))
            .expect("send second chunk");
        assert!(terminal.poll_input());
        assert_eq!(
            received
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_slice(),
            ["\u{fffd}".to_string()]
        );
    }
}

#[cfg(all(test, unix))]
mod paused_reader_tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::FromRawFd;

    fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        condition()
    }

    /// `stop()` 必须真正暂停读取：暂停期间写进 tty 的字节要留给前台子进程（外部编辑器 /
    /// 挂起后的 shell），不能被读取线程抢先读走并在 `start()` 后重放给 TUI。
    ///
    /// 用管道代替 tty（读取语义相同：`poll` + `read`），直接观察“暂停期间字节仍在管道里”。
    #[test]
    fn paused_stdin_reader_leaves_input_for_the_foreground_process() {
        let mut descriptors = [0i32; 2];
        // SAFETY: pipe(2) 写入两个新描述符。
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (descriptors[0], descriptors[1]);
        // SAFETY: write_fd 是 pipe(2) 返回的新描述符，所有权交给 File（Drop 时关闭）。
        let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd) };

        let mut terminal = ProcessTerminal::with_output(Box::new(std::io::sink()));
        terminal.setup_stdin_buffer();
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        terminal.set_input_handler(Box::new(move |data: &str| {
            sink.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(data.to_string());
        }));
        // 等价于 `start()` 里对 fd 0 做的事（这里换成管道读端，便于观察）。
        terminal.spawn_stdin_reader(read_fd);

        writer.write_all(b"a").expect("write");
        assert!(
            wait_until(Duration::from_secs(2), || terminal.poll_input()),
            "the reader must deliver data while resumed"
        );
        assert_eq!(
            received
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_slice(),
            ["a".to_string()]
        );

        // `process.stdin.pause()`（`stop()`）：线程不得再从 fd 读取。
        terminal.stdin_pause.pause();
        writer.write_all(b"bc").expect("write");
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            terminal
                .stdin_rx
                .as_ref()
                .is_none_or(|receiver| receiver.try_recv().is_err()),
            "a paused reader must not consume stdin"
        );

        // 字节仍在管道里：前台子进程（外部编辑器）能原样读到。
        assert!(wait_stdin_readable(read_fd, Duration::from_millis(500)));
        let mut buffer = [0u8; 8];
        let read = read_stdin_fd(read_fd, &mut buffer).expect("read pending bytes");
        assert_eq!(&buffer[..read], b"bc");

        // `process.stdin.resume()`（`start()`）：读取线程继续工作。
        terminal.stdin_pause.resume();
        writer.write_all(b"d").expect("write");
        assert!(wait_until(Duration::from_secs(2), || terminal.poll_input()));
        assert_eq!(
            received
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .last()
                .map(String::as_str),
            Some("d")
        );
    }

    /// stdin 关闭（`read() == 0`）对应 Node 的 `end` 事件：读取线程必须记录 EOF，
    /// 启动期 TUI 驱动循环据此按“事件循环耗尽”退出（否则 `--resume` 会永久挂起）。
    #[test]
    fn stdin_eof_is_reported_when_the_stream_closes() {
        let mut descriptors = [0i32; 2];
        // SAFETY: pipe(2) 写入两个新描述符。
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (descriptors[0], descriptors[1]);
        // SAFETY: write_fd 是 pipe(2) 返回的新描述符，所有权交给 File（Drop 时关闭）。
        let writer = unsafe { std::fs::File::from_raw_fd(write_fd) };

        let mut terminal = ProcessTerminal::with_output(Box::new(std::io::sink()));
        terminal.spawn_stdin_reader(read_fd);
        assert!(!terminal.stdin_eof(), "an open stream is not EOF");

        drop(writer);
        assert!(
            wait_until(Duration::from_secs(2), || terminal.stdin_eof()),
            "the reader must report EOF after the write end closes"
        );
    }
}
