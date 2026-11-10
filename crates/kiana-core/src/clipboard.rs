//! utils/{clipboard,clipboard-command,open-browser}.ts。
//!
//! TS 源：`packages/coding-agent/src/utils/{clipboard,clipboard-command,open-browser}.ts`。
//!
//! # 对应关系
//!
//! | TS | Rust |
//! |---|---|
//! | `clipboard-command.ts#runClipboardCommand` | [`run_clipboard_command`] |
//! | `clipboard.ts#readClipboardText` / `readClipboardFilePaths` / `copyToClipboard` | [`read_clipboard_text`] / [`read_clipboard_file_paths`] / [`copy_to_clipboard`] |
//! | `open-browser.ts#openBrowser` | [`open_browser`] |
//!
//! # 平台与环境是参数
//!
//! TS 测试用 `vi.mock` 替换 `node:os.platform`、`process.env`、`clipboard-command` 与
//! `@earendil-works/kiana-tui`。Rust 侧把同样的接缝做成**注入点**（[`ClipboardTestIo`]，`#[doc(hidden)]`）：
//! 平台字符串、环境变量快照、原生剪贴板句柄、子进程执行器与 stdout 写入器都可由调用方提供。
//! 公开函数用进程的真实状态构造默认的 [`ClipboardTestIo`]，对外行为保持稳定：
//!
//! - [`read_clipboard_text`] / [`read_clipboard_file_paths`] / [`copy_to_clipboard`] 的公开签名不带参数；
//! - [`read_clipboard_text_with`] 等 `_with` 变体接受注入的 IO，供 `crates/kiana-core/tests/clipboard.rs` 使用。
//!
//! # 原生剪贴板
//!
//! 原生剪贴板走 `kiana_tui::native::get_native_clipboard()`（`ARCHITECTURE.md` §8 第 17 条）：
//! 三态 [`ClipboardValue`] 的 `Unavailable`（TS `undefined`）/ `Empty`（`null`）/
//! `Value` 必须保留，`Unavailable` 与 `Empty` 的区分决定失败文案与回退路径。
//!
//! # OSC 52
//!
//! 序列逐字 `\x1b]52;c;{base64}\x07`；base64 编码长度 > 100000 视为超大，**不写出任何字节**并返回
//! `false`（TS 的 `MAX_OSC52_ENCODED_LENGTH` 是模块私有常量，这里同样不导出；边界由本文件的
//! 模块内测试覆盖）。
//!
//! # 回退顺序与状态推进
//!
//! 读路径在第一条成功处停止；写路径的兜底链也在第一条成功处停止，但远程会话会在成功后额外
//! 补发一次 OSC 52（见下文第 2 条）。顺序本身是可观察契约：
//! `crates/kiana-core/tests/clipboard.rs` 断言命令链的命令名、参数、`timeout_ms` 与调用
//! 次数；`tests/clipboard_command.rs` 只覆盖单条 `run_clipboard_command`（二进制输出、空输出
//! 成功、stdin/stdout、超时、缓冲上限与进程组隔离），不断言回退命令链。读取顺序会改变读到的
//! 内容；写入顺序会改变由哪条通路负责写入及成败，写入的文本本身不变。
//!
//! 1. 读取 [`read_clipboard_text`]：Termux 命令 → Wayland 命令 → X11 命令（`xclip` 先、
//!    `xsel` 后）→ 原生后端。每条命令超时 5000 ms；任一条以 0 退出即返回，即使输出为空也
//!    不再回退（#7248：空的 Wayland 内容不能回落到陈旧的 X11 剪贴板）。原生读取的拒绝被
//!    吞掉，只有非空文本才返回 `Some`。
//! 2. 写入 [`copy_to_clipboard`]：非 Linux 的原生 `set_text` → 平台命令链 → WSL 专项
//!    （`WT_SESSION` 时先试 OSC 52，超限或失败再走 PowerShell）→ 远程会话或
//!    「未成功且 headless」时的 OSC 52。远程会话在原生/命令写入成功后**仍**会发一次
//!    OSC 52，因为结果要到达客户端终端（#10114、#10826、#10835）；没有远程标记的本地
//!    会话不会为了兜底而发（Linux 的 headless 会话除外），因为 OSC 52 无法验证是否真的
//!    到达终端（#9618）。
//! 3. 打开浏览器 [`open_browser`]：同步 spawn 平台启动器后立即返回，不等待、不报错。
//!
//! # 失败模式
//!
//! - 子进程不存在、非 0 退出、超时、输出超限都统一折算成 `None` / `false`，不产生错误
//!   类型；诊断信息被丢弃是有意为之——剪贴板是尽力而为的辅助功能，调用方不应因它中断
//!   主流程。
//! - 写入失败最终由 [`ClipboardUnavailableError`] 给出按环境定制的可操作提示；
//!   [`read_clipboard_text`] 则把一切失败压成 `None`。
//! - 「没有文件路径」与「原生读取失败」在公开版 [`read_clipboard_file_paths`] 中不可区分；
//!   需要区分的调用方（`kiana-interactive` 的粘贴处理）使用注入版，后者用 `Err` 保留拒绝。
//!
//! # 注意事项
//!
//! - 注入项全部带 `#[doc(hidden)]`：它们服务于 `crates/kiana-core/tests/*` 与
//!   `kiana-interactive` 的粘贴输入（`interactive_mode/input.rs` 用
//!   `ClipboardTestIo::from_process()` 构造读取源），不是稳定 API。
//! - WSL 的 PowerShell 通路会写一个 `mode(0o600)` 临时文件再删除；文件名含随机 UUID，
//!   每次调用独立。用文件而不是 stdin，是因为 PowerShell 的 stdin 会按控制台代码页解码。
//! - 剪贴板读写相关的公开函数是 `async fn`，内部使用 `tokio::process` 与
//!   `tokio::time::timeout`；`open_browser` 是同步函数，用 `std::process` spawn 后立即返回。

use std::ffi::OsString;
use std::future::Future;
use std::io::Write as _;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use kiana_tui::native::{
    ClipboardError, ClipboardValue, SharedNativeClipboard, get_native_clipboard,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, ChildStdout, Command};

/// 环境变量快照（`NodeJS.ProcessEnv` 的注入视图）。
///
/// 与 [`crate::changelog::is_wsl`] 的 `env` 参数同形：`Some(entries)` 时只读 `entries`，
/// `None` 时读真实进程环境。空值（`""`）在 JS 里是 falsy，所有调用点都按「存在且非空」判断。
///
/// 注入的 `entries` 由 `env_truthy` / `env_equals` 线性扫描：重复键只要任一满足条件
/// 就视为真（真实进程环境通常没有重复键；这只影响测试/嵌入方提供的快照）。
pub type EnvMap = [(OsString, OsString)];

/// `MAX_OSC52_ENCODED_LENGTH`（TS 模块私有常量）。
const MAX_OSC52_ENCODED_LENGTH: usize = 100_000;

/// `runClipboardCommand` 的默认超时（毫秒）。
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 3_000;

/// `runClipboardCommand` 的默认输出上限（50 MB）。
pub(crate) const DEFAULT_MAX_BUFFER_BYTES: usize = 50 * 1024 * 1024;

/// `copyToClipboard` 抛出的错误（`Clipboard unavailable...`）。
///
/// `Display` 逐字等于 TS `new Error(message).message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardUnavailableError {
    message: String,
}

impl ClipboardUnavailableError {
    /// 用给定的完整文案构造（文案由本模块内部提供，测试也可断言）。
    pub fn new(message: impl Into<String>) -> Self {
        ClipboardUnavailableError {
            message: message.into(),
        }
    }

    /// 完整文案。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ClipboardUnavailableError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClipboardUnavailableError {}

/// `runClipboardCommand` 的选项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardCommandOptions {
    /// 写入 stdin 的文本。为 `Some` 时 stdout 不再收集（TS 把 stdout 设为 `ignore`）。
    pub input: Option<String>,
    /// 超时毫秒数，默认 3000。
    pub timeout_ms: Option<u64>,
    /// stdout 上限字节数，默认 50 MB。超过即视为失败。
    pub max_buffer_bytes: Option<usize>,
}

/// `runClipboardCommand(command, args, options?)`：执行剪贴板辅助命令。
///
/// 返回值语义（当前稳定契约，改动需同步测试）：
/// - `Some(bytes)`：命令以 0 退出。**空 `Vec` 是成功**（`Buffer.alloc(0)`）；
/// - `None`：命令不存在、非 0 退出、超时（发送 `SIGKILL`）或输出超过 `max_buffer_bytes`
///   （同样立即发送 `SIGKILL`，不等进程退出或超时，对应 TS `abort()`）。
///
/// spawn 配置逐字：
/// `stdio: ["pipe", input === undefined ? "pipe" : "ignore", "ignore"]`——stdin **永远是 pipe**
/// （有 input 时仍要能写进去），stdout 仅在**没有** input 时才 pipe；POSIX `detached`
/// （`process_group(0)`，否则 Ctrl+Z 会停掉剪贴板所有者，#11179）；Windows `windowsHide`。
pub async fn run_clipboard_command(
    command: &str,
    args: &[String],
    options: Option<ClipboardCommandOptions>,
) -> Option<Vec<u8>> {
    let options = options.unwrap_or_default();
    let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let max_buffer_bytes = options.max_buffer_bytes.unwrap_or(DEFAULT_MAX_BUFFER_BYTES);
    let input = options.input;

    let mut process = Command::new(command);
    process.args(args);
    process.stdin(Stdio::piped());
    process.stdout(if input.is_none() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    process.stderr(Stdio::null());
    #[cfg(unix)]
    process.process_group(0);
    #[cfg(windows)]
    process.creation_flags(0x0800_0000); // CREATE_NO_WINDOW（`windowsHide: true`）

    let mut child = match process.spawn() {
        Ok(child) => child,
        // TS 的 `error` 事件 → finish(undefined)。
        Err(_) => return None,
    };
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();

    let write_input = async move {
        if let Some(mut stdin) = stdin {
            if let Some(input) = input {
                // stdin 的 error 事件被忽略：writer 可能提前退出。
                let _ = stdin.write_all(input.as_bytes()).await;
            }
            // `child.stdin.end(input)`（input 为 undefined 时也是 end）。
            let _ = stdin.shutdown().await;
        }
    };
    // 超时是「写 stdin + 读 stdout + 等子进程退出」三者共享的整体预算：两个 future 用
    // `join!` 在同一个任务里并发推进（没有 spawn），任何一方卡住都会在 timeout 处一起放弃。
    let outcome = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        let ((), output) = tokio::join!(
            write_input,
            read_command_output(&mut child, stdout, max_buffer_bytes)
        );
        output
    })
    .await;

    match outcome {
        Err(_) => {
            // 超时：kill("SIGKILL")。
            let _ = child.kill().await;
            None
        }
        Ok(CommandOutput::Overflow) => {
            // 输出超过 maxBufferBytes：TS 的 abort() 已在 data 事件里 kill 过；这里只负责回收。
            let _ = child.kill().await;
            None
        }
        // `code === 0` 才是成功；被信号杀死时 Node 的 code 是 null。
        Ok(CommandOutput::Exited {
            status: Some(status),
            bytes,
        }) => status.success().then_some(bytes),
        Ok(CommandOutput::Exited { status: None, .. }) => None,
    }
}

/// [`read_command_output`] 的结果。
enum CommandOutput {
    /// 子进程已退出。`status: None` 表示等待失败（TS 的 `close` 没有 `code`）。
    Exited {
        status: Option<std::process::ExitStatus>,
        bytes: Vec<u8>,
    },
    /// 输出超过 `max_buffer_bytes`：对应 TS 的 `abort()`——立即 `SIGKILL`，结果为 `undefined`。
    Overflow,
}

/// 读取 stdout 并等待子进程退出；超过上限时**立即**杀掉子进程。
///
/// TS 在 `stdout` 的 `data` 事件里同步调用 `abort()`：`SIGKILL` + 销毁流 + 立刻 resolve
/// `undefined`，不会等子进程自己退出，也不会等超时。这里用 `select!` 同时等待输出与退出，
/// 超限分支先 `start_kill()` 再返回，因此耗时与 TS 一样是毫秒级。
///
/// 子进程先退出时继续把管道读干净再返回：Node 的 `close` 事件在进程退出**且**所有 stdio
/// 关闭后才触发，此时数据已经全部被 data 事件收集。
async fn read_command_output(
    child: &mut Child,
    stdout: Option<ChildStdout>,
    max_buffer_bytes: usize,
) -> CommandOutput {
    let Some(mut stdout) = stdout else {
        // stdout 为 `ignore`：成功时是空 Buffer。
        return CommandOutput::Exited {
            status: child.wait().await.ok(),
            bytes: Vec::new(),
        };
    };

    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    // `None`：进程还在运行；`Some(status)`：`wait` 已经完成。
    let mut status: Option<Option<std::process::ExitStatus>> = None;
    let mut eof = false;

    // 循环不变量：`status` 一旦为 `Some` 就不再调用 `wait()`，之后的迭代只把管道读干净；
    // `eof` 只在 `read` 返回 0 或读失败时置位，置位后终止读取、按 `status` 决定出口。
    loop {
        if eof {
            // 管道已关闭：进程尚未 `wait` 过就先补一次，再回到顶部走返回分支。
            return match status {
                Some(status) => CommandOutput::Exited {
                    status,
                    bytes: collected,
                },
                None => {
                    status = Some(child.wait().await.ok());
                    continue;
                }
            };
        }

        let read_result = if status.is_some() {
            // 进程已退出：只把管道里剩余的输出读干净。
            stdout.read(&mut chunk).await
        } else {
            // 进程尚未退出：读与 wait 竞争。wait 先就绪时记录状态，下一轮转为纯排空管道
            // （Node 的 close 事件同样要等进程退出**且**所有 stdio 关闭）。
            tokio::select! {
                read = stdout.read(&mut chunk) => read,
                exit = child.wait() => {
                    status = Some(exit.ok());
                    continue;
                }
            }
        };

        match read_result {
            Ok(0) => eof = true,
            Ok(read) => {
                // 恰好等于上限仍算成功：判定是严格大于。
                if collected.len() + read > max_buffer_bytes {
                    // TS 的 abort()：SIGKILL + 销毁流 + finish(undefined)，不等进程退出。
                    if status.is_none() {
                        // 仅当进程仍在运行时才需要杀；已 wait 过（status 为 `Some`）就跳过。
                        let _ = child.start_kill();
                    }
                    return CommandOutput::Overflow;
                }
                collected.extend_from_slice(&chunk[..read]);
            }
            // 读取失败按 EOF 处理（等价于流被销毁，没有更多数据）。
            Err(_) => eof = true,
        }
    }
}

/// 默认的命令执行器：直接调用 [`run_clipboard_command`]。
pub(crate) fn default_command_runner() -> ClipboardCommandRunner {
    Arc::new(|request: ClipboardCommandRequest| {
        Box::pin(async move {
            run_clipboard_command(
                &request.command,
                &request.args,
                Some(ClipboardCommandOptions {
                    input: request.input,
                    timeout_ms: Some(request.timeout_ms),
                    max_buffer_bytes: Some(request.max_buffer_bytes),
                }),
            )
            .await
        })
    })
}

/// 一次剪贴板命令请求（[`ClipboardCommandRunner`] 的入参）。
///
/// 与 TS 的 `runClipboardCommand(command, args, options)` 一一对应，但所有默认值都已解析：
/// `timeout_ms` / `max_buffer_bytes` 总是具体数值。
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommandRequest {
    /// 可执行文件名。
    pub command: String,
    /// 参数列表。
    pub args: Vec<String>,
    /// 写入 stdin 的文本（`None` 时仍会关闭 stdin）。
    pub input: Option<String>,
    /// 超时毫秒数。
    pub timeout_ms: u64,
    /// stdout 上限字节数。
    pub max_buffer_bytes: usize,
}

/// 可注入的命令执行器（TS 测试里的 `vi.mock("../src/utils/clipboard-command.ts")`）。
#[doc(hidden)]
#[allow(
    clippy::type_complexity,
    reason = "注入点的形状由测试策略决定：请求 → 可发送的 future，等价 TS 的 mock 函数"
)]
pub type ClipboardCommandRunner = Arc<
    dyn Fn(ClipboardCommandRequest) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + Send>>
        + Send
        + Sync,
>;

/// 测试/嵌入注入点：平台、环境、原生剪贴板、命令执行器与 stdout 写入器。
///
/// `#[doc(hidden)]`：不是稳定 API，只为让平台分支可以被确定性地测试（TS 用 `vi.mock` 达到
/// 同样目的）。`env: None` 表示读真实进程环境；`Some(vec![])` 表示空环境。
#[doc(hidden)]
pub struct ClipboardTestIo {
    /// `process.platform`：`"linux"` / `"darwin"` / `"win32"` / `"android"` / `"freebsd"` /
    /// `"sunos"` 等 Node 平台名（见 [`process_platform`]）。
    pub platform: String,
    /// 环境变量快照；`None` 表示读真实进程环境。
    pub env: Option<Vec<(OsString, OsString)>>,
    /// `getNativeClipboard()` 的结果；`None` 等价 TS 的 `undefined`。
    pub native: Option<SharedNativeClipboard>,
    /// 命令执行器。
    pub run_command: ClipboardCommandRunner,
    /// `process.stdout.write`：接收完整的 OSC 52 序列。
    pub write_stdout: Arc<dyn Fn(&str) + Send + Sync>,
}

impl ClipboardTestIo {
    /// 用进程的真实状态构造（公开函数内部使用）。
    #[doc(hidden)]
    pub fn from_process() -> Self {
        ClipboardTestIo {
            platform: process_platform().to_string(),
            env: None,
            native: get_native_clipboard(),
            run_command: default_command_runner(),
            write_stdout: Arc::new(|text: &str| {
                let mut stdout = std::io::stdout();
                let _ = stdout.write_all(text.as_bytes());
                // TS 的 `process.stdout.write` 立即把字节交给终端/管道；Rust 的 stdout 是
                // 行缓冲的，而 OSC 52 序列（`\x1b]52;c;...\x07`）没有换行，不 flush 就会滞留
                // 在进程缓冲里，直到之后某次换行写出或进程退出。
                let _ = stdout.flush();
            }),
        }
    }

    /// 环境变量视图（`None` 表示真实进程环境）。
    fn env_map(&self) -> Option<&EnvMap> {
        self.env.as_deref()
    }
}

/// `process.platform` 的等价物（`node:os.platform()`）。
///
/// Termux 报告 `"android"`（#10391），macOS 是 `"darwin"`，Windows 是 `"win32"`。
/// 其余平台按 Node 的编译期常量原样报告：只有真正的 Linux 才走 Linux 分支
/// （例：headless 的 FreeBSD 在 TS 里是 `Clipboard unavailable`，不会被当成 Linux 发 OSC 52）。
pub(crate) fn process_platform() -> &'static str {
    platform_for_os(std::env::consts::OS)
}

/// [`std::env::consts::OS`]（即 `target_os`）→ `NodeJS.Platform`。
///
/// Node 的 `process.platform` 是 `src/node.cc` 里的编译期宏：`__APPLE__`（含 iOS 等）
/// → `"darwin"`，`_WIN32` → `"win32"`，`__sun` → `"sunos"`，其余平台名与 `target_os` 一致。
fn platform_for_os(os: &str) -> &'static str {
    match os {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => "darwin",
        "windows" => "win32",
        "android" => "android",
        "linux" => "linux",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        "dragonfly" => "dragonfly",
        "solaris" | "illumos" => "sunos",
        "aix" => "aix",
        "haiku" => "haiku",
        // Node 没有这些目标（fuchsia/redox/…）：沿用旧行为，避免 panic。
        _ => "linux",
    }
}

/// `Boolean(env[key])`（存在且非空）。
pub(crate) fn env_truthy(env: Option<&EnvMap>, key: &str) -> bool {
    match env {
        Some(entries) => entries
            .iter()
            .any(|(name, value)| name.as_os_str() == key && !value.is_empty()),
        None => std::env::var_os(key)
            .map(|value| !value.is_empty())
            .unwrap_or(false),
    }
}

/// `env[key] === value`。
pub(crate) fn env_equals(env: Option<&EnvMap>, key: &str, value: &str) -> bool {
    match env {
        Some(entries) => entries
            .iter()
            .any(|(name, candidate)| name.as_os_str() == key && candidate.as_os_str() == value),
        None => std::env::var_os(key)
            .map(|candidate| candidate == value)
            .unwrap_or(false),
    }
}

/// `isRemoteSession(env)`：远程会话也会发 OSC 52，让客户端终端收到复制结果。
///
/// `KIANA_CLIPBOARD_OSC52=1` 覆盖没有 SSH 标记的远程客户端（#10114、#10835、#10826）。
fn is_remote_session(env: Option<&EnvMap>) -> bool {
    env_truthy(env, "SSH_CONNECTION")
        || env_truthy(env, "SSH_CLIENT")
        || env_truthy(env, "MOSH_CONNECTION")
        || env_equals(env, "KIANA_CLIPBOARD_OSC52", "1")
}

/// `Buffer.from(text).toString("base64")` 后的长度检查；超限返回 `None`。
///
/// 上限比较的是 **base64 字符串长度**（ASCII，因此字节数 = 字符数 = UTF-16 码元数），
/// 不是原文长度：原文先按 UTF-8 编码再编码，非 ASCII 文本能塞进的字符数更少。
fn encode_osc52(text: &str) -> Option<String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if encoded.len() > MAX_OSC52_ENCODED_LENGTH {
        None
    } else {
        Some(encoded)
    }
}

/// `emitOsc52(text)`（私有）：编码、检查上限并写入注入的写入器。
///
/// TS 的 `emitOsc52` 直接写 `process.stdout`；Rust 把「写哪里」参数化（默认写入器见
/// [`ClipboardTestIo::from_process`]），编码与超限语义保持不变。
fn emit_osc52_with(text: &str, write: &dyn Fn(&str)) -> bool {
    match encode_osc52(text) {
        Some(encoded) => {
            write(&format!("\x1b]52;c;{encoded}\x07"));
            true
        }
        None => false,
    }
}

/// `emitOsc52` 的可断言版本：把序列写进任意 [`std::io::Write`]，返回是否写出。
///
/// `#[doc(hidden)]`：`emitOsc52` 在 TS 里是模块私有函数；测试需要断言序列与超限行为
/// （`MAX_OSC52_ENCODED_LENGTH` 不导出），因此提供这个注入变体。
#[doc(hidden)]
pub fn emit_osc52_to(text: &str, out: &mut dyn std::io::Write) -> bool {
    match encode_osc52(text) {
        Some(encoded) => {
            let _ = write!(out, "\x1b]52;c;{encoded}\x07");
            true
        }
        None => false,
    }
}

/// `copyViaWindowsClipboard(text)`（私有）：WSL 通过 interop 写 Windows 剪贴板。
///
/// PowerShell 从文件读取文本：`clip.exe` / PowerShell 的 stdin 按控制台代码页解码，
/// 非 ASCII UTF-8 会被破坏。
///
/// 流程：写 `mode(0o600)` 临时文件 → `wslpath -w` 转 Windows 路径（超时 1000 ms）→
/// `powershell.exe -NoProfile -Command "Set-Clipboard ..."`（超时 5000 ms）→ 无条件删除
/// 临时文件。任一步失败返回 `false`；调用方仅在远程会话或未成功且 headless 时补发 OSC 52，
/// 其余情况直接进入错误文案选择。文件名带随机 UUID，每次调用都会生成新路径。
async fn copy_via_windows_clipboard(io: &ClipboardTestIo, text: &str) -> bool {
    let tmp_file = node_tmpdir().join(format!("kiana-wsl-clip-{}.txt", uuid::Uuid::new_v4()));
    let result = copy_via_windows_clipboard_inner(io, text, &tmp_file).await;
    // finally：文件可能根本没创建。
    let _ = std::fs::remove_file(&tmp_file);
    result
}

/// [`copy_via_windows_clipboard`] 的主体（拆成两层，让外层能无条件删除临时文件）。
///
/// `wslpath` 失败或输出 trim 后为空 → `false`。PowerShell 一步只看返回的 `Option`：
/// 默认执行器已把非 0 退出折成 `None`，所以 `Some` 即视为写入成功（对应 TS 的「结果不是
/// undefined」）。`win_path` 中的单引号按 PowerShell 规则翻倍（`''`）后嵌入 `-Command`，
/// 防止路径里的引号截断脚本。
async fn copy_via_windows_clipboard_inner(
    io: &ClipboardTestIo,
    text: &str,
    tmp_file: &std::path::Path,
) -> bool {
    if write_private_file(tmp_file, text.as_bytes()).is_err() {
        return false;
    }

    let Some(win_path_bytes) = run_io_command(
        io,
        "wslpath",
        vec!["-w".to_string(), tmp_file.to_string_lossy().into_owned()],
        None,
        1_000,
    )
    .await
    else {
        return false;
    };
    let win_path = String::from_utf8_lossy(&win_path_bytes).trim().to_string();
    if win_path.is_empty() {
        return false;
    }

    let script = format!(
        "Set-Clipboard -Value ([System.IO.File]::ReadAllText('{}', [System.Text.Encoding]::UTF8))",
        win_path.replace('\'', "''")
    );
    run_io_command(
        io,
        "powershell.exe",
        vec!["-NoProfile".to_string(), "-Command".to_string(), script],
        None,
        5_000,
    )
    .await
    .is_some()
}

/// `readClipboardText()`：读取系统剪贴板纯文本。
///
/// 命令链按固定顺序回退（当前稳定契约）：`TERMUX_VERSION` → `termux-clipboard-get`；Linux 且 `WAYLAND_DISPLAY`
/// → `wl-paste --no-newline --type text`；Linux 且 `DISPLAY` → `xclip` / `xsel`；每个超时 5000。
/// 命令成功（**包括空输出**）即停止回退：空文本返回 `None` 但不再读原生剪贴板（#7248）。
/// 最后回退原生剪贴板；原生读取失败被吞掉（TS 的 `catch`）。
pub async fn read_clipboard_text() -> Option<String> {
    read_clipboard_text_with(&ClipboardTestIo::from_process()).await
}

/// [`read_clipboard_text`] 的注入版本（测试用）。
///
/// 候选命令由 `io.platform` 与 `io.env` 决定：`env` 为 `Some` 时完全不读真实进程环境。
/// 返回映射：某条命令成功但输出为空 → `None`，且**不**再尝试后续命令或原生后端；所有命令
/// 都失败后才读原生后端，原生返回 `Value("")` / `Empty` / `Unavailable` 或拒绝 → `None`。
#[doc(hidden)]
pub async fn read_clipboard_text_with(io: &ClipboardTestIo) -> Option<String> {
    let env = io.env_map();
    let mut commands: Vec<(&str, Vec<String>)> = Vec::new();
    // Termux 报告的 platform 是 "android"，不是 "linux"（#10391）。
    if env_truthy(env, "TERMUX_VERSION") {
        commands.push(("termux-clipboard-get", Vec::new()));
    }
    // 除 Termux 外，Linux 桌面按环境变量追加命令；Wayland 与 X11 同时存在时按
    // wl-paste → xclip → xsel 的顺序尝试，每个超时 5000 ms。
    if io.platform == "linux" {
        if env_truthy(env, "WAYLAND_DISPLAY") {
            commands.push((
                "wl-paste",
                vec![
                    "--no-newline".to_string(),
                    "--type".to_string(),
                    "text".to_string(),
                ],
            ));
        }
        if env_truthy(env, "DISPLAY") {
            commands.push((
                "xclip",
                vec![
                    "-selection".to_string(),
                    "clipboard".to_string(),
                    "-out".to_string(),
                ],
            ));
            commands.push((
                "xsel",
                vec!["--clipboard".to_string(), "--output".to_string()],
            ));
        }
    }
    for (command, args) in commands {
        let bytes = run_io_command(io, command, args, None, 5_000).await;
        if let Some(bytes) = bytes {
            let text = decode_utf8_lossy(&bytes);
            return if text.is_empty() { None } else { Some(text) };
        }
    }
    let Some(clipboard) = &io.native else {
        return None;
    };
    match clipboard.get_text().await {
        Ok(ClipboardValue::Value(text)) if !text.is_empty() => Some(text),
        Ok(_) | Err(_) => None,
    }
}

/// `readClipboardFilePaths()`：读取文件管理器复制的文件路径（macOS Finder 等）。
///
/// TS: `clipboard.ts#readClipboardFilePaths` —— `getNativeClipboard()?.getFilePaths?.()`；没有后端、
/// 后端不导出 `getFilePaths`、`null`/`undefined`、空数组都返回 `null`，但**不捕获**原生拒绝
/// （`handleClipboardPaste` 的 `catch` 会把它显示成 `Failed to paste from clipboard: <message>`
/// 并中止粘贴，不再回落到图片/文本）。
///
/// Rust 的公开签名按计划冻结为 `Option<Vec<String>>`：`None` 对应 TS 的 `null`，原生拒绝同样只能
/// 压成 `None`（与「没有路径」不可区分）。完整的 TS 语义保留在注入版
/// [`read_clipboard_file_paths_with`]（拒绝以 `Err` 返回），需要区分两者的调用方应使用它。
pub async fn read_clipboard_file_paths() -> Option<Vec<String>> {
    read_clipboard_file_paths_with(&ClipboardTestIo::from_process())
        .await
        .unwrap_or(None)
}

/// [`read_clipboard_file_paths`] 的注入版本（测试用）。
///
/// 与公开版的唯一区别：原生 `getFilePaths()` 的拒绝以 `Err(ClipboardError)` 返回（等价 TS 的
/// Promise rejection），而不是压成 `None`。其余映射如下：没有原生后端 / 后端不导出
/// `getFilePaths` / `null` / `undefined` / 空数组 → `Ok(None)`；非空数组 → `Ok(Some(paths))`。
#[doc(hidden)]
pub async fn read_clipboard_file_paths_with(
    io: &ClipboardTestIo,
) -> Result<Option<Vec<String>>, ClipboardError> {
    let Some(clipboard) = io.native.as_ref() else {
        return Ok(None);
    };
    let Some(future) = clipboard.get_file_paths() else {
        return Ok(None);
    };
    let paths = future.await?;
    match paths {
        ClipboardValue::Value(paths) if !paths.is_empty() => Ok(Some(paths)),
        _ => Ok(None),
    }
}

/// `copyToClipboard(text)`：把文本写进系统剪贴板，必要时发 OSC 52。
///
/// 顺序逐字照 §2.21.29：
/// 1. 非 Linux 先试原生 `setText`（异常吞掉）；
/// 2. 未成功则命令链（darwin `pbcopy`、win32 `clip`、其它 `termux-clipboard-set` / `wl-copy` /
///    `xclip` / `xsel`，`input: text`，timeout 5000，首个成功即停）；
/// 3. Linux+WSL 且仍未成功：`WT_SESSION` 时先 OSC 52，否则走 `copyViaWindowsClipboard`；
/// 4. headless（Linux 且无 `DISPLAY`/`WAYLAND_DISPLAY`/`TERMUX_VERSION`）或远程会话 → OSC 52；
/// 5. 失败文案见 [`ClipboardUnavailableError`]。
pub async fn copy_to_clipboard(text: &str) -> Result<(), ClipboardUnavailableError> {
    copy_to_clipboard_with(text, &ClipboardTestIo::from_process()).await
}

/// [`copy_to_clipboard`] 的注入版本（测试用）。
///
/// 内部用三个标志推进状态：`copied`（有通路报告成功）、`osc52_emitted`（已经写出过 OSC 52，
/// 防止 WSL 分支与末尾的远程/headless 分支重复写同一个序列）、`oversized`（OSC 52 因超限
/// 被拒，用于最后选择「超限」文案而不是「没有可用工具」文案）。
///
/// 错误文案不包含子进程的失败原因：命令层把所有失败压成 `None`，这里只能根据平台与环境
/// 推断该提示用户安装什么。
#[doc(hidden)]
pub async fn copy_to_clipboard_with(
    text: &str,
    io: &ClipboardTestIo,
) -> Result<(), ClipboardUnavailableError> {
    let platform = io.platform.as_str();
    let env = io.env_map();
    let mut copied = false;

    // 阶段 1：非 Linux 先试原生写入。
    // 直接写入先于 OSC 52，避免终端与原生写入器竞态。
    // Linux 工具在调用返回后仍保留剪贴板 selection 的所有权。
    // 原生拒绝 → 试平台命令；`set_text` 返回 `None`（后端不支持写入）同样落到命令链。
    if platform != "linux"
        && let Some(clipboard) = &io.native
        && let Some(set_text) = clipboard.set_text(text.to_string())
        && set_text.await.is_ok()
    {
        copied = true;
    }

    // 阶段 2：平台命令链；每条命令把文本写进 stdin，超时 5000 ms，首个成功即 break。
    if !copied {
        let mut commands: Vec<(&str, Vec<String>)> = Vec::new();
        if platform == "darwin" {
            commands.push(("pbcopy", Vec::new()));
        } else if platform == "win32" {
            commands.push(("clip", Vec::new()));
        } else {
            if env_truthy(env, "TERMUX_VERSION") {
                commands.push(("termux-clipboard-set", Vec::new()));
            }
            if env_truthy(env, "WAYLAND_DISPLAY") {
                commands.push(("wl-copy", Vec::new()));
            }
            if env_truthy(env, "DISPLAY") {
                commands.push((
                    "xclip",
                    vec!["-selection".to_string(), "clipboard".to_string()],
                ));
                commands.push((
                    "xsel",
                    vec!["--clipboard".to_string(), "--input".to_string()],
                ));
            }
        }
        for (command, args) in commands {
            if run_io_command(io, command, args, Some(text.to_string()), 5_000)
                .await
                .is_some()
            {
                copied = true;
                break;
            }
        }
    }

    // 阶段 3：WSL 且尚未成功时写 Windows 剪贴板。
    let mut osc52_emitted = false;
    if !copied && platform == "linux" && crate::changelog::is_wsl(env) {
        // Windows Terminal 支持 OSC 52；比 PowerShell 往返更快。
        if env_truthy(env, "WT_SESSION") {
            osc52_emitted = emit_osc52_with(text, &*io.write_stdout);
        }
        // OSC 52 成功就不再走 PowerShell；超限（返回 false）时才回退。
        copied = osc52_emitted || copy_via_windows_clipboard(io, text).await;
    }

    // 阶段 4：最后的 OSC 52 通路。`!osc52_emitted` 防止与阶段 3 重复写序列。
    // OSC 52 无法验证，因此有 display 的桌面会话报告失败（#9618）。没有 display 时终端是唯一
    // 的剪贴板通路（容器、没有 WSLg 的 WSL），远程会话则总是发 OSC 52 以到达客户端剪贴板。
    let headless = platform == "linux"
        && !env_truthy(env, "DISPLAY")
        && !env_truthy(env, "WAYLAND_DISPLAY")
        && !env_truthy(env, "TERMUX_VERSION");
    let mut oversized = false;
    if !osc52_emitted && (is_remote_session(env) || (!copied && headless)) {
        if emit_osc52_with(text, &*io.write_stdout) {
            copied = true;
        } else {
            oversized = true;
        }
    }

    // 阶段 5：成功直接返回；否则按「最具体 → 最一般」的顺序选文案：超限 → Termux →
    // Wayland → X11 → 通用。顺序是当前稳定契约，测试按整句断言。
    if copied {
        return Ok(());
    }
    if oversized {
        return Err(ClipboardUnavailableError::new(
            "Clipboard unavailable: text exceeds the OSC 52 size limit",
        ));
    }
    if env_truthy(env, "TERMUX_VERSION") {
        return Err(ClipboardUnavailableError::new(
            "Clipboard unavailable: install the Termux:API app and `termux-api` package",
        ));
    }
    if platform == "linux" {
        if env_truthy(env, "WAYLAND_DISPLAY") {
            return Err(ClipboardUnavailableError::new(
                "Clipboard unavailable: install `wl-clipboard` (`wl-copy`) or check Wayland access",
            ));
        }
        if env_truthy(env, "DISPLAY") {
            return Err(ClipboardUnavailableError::new(
                "Clipboard unavailable: install `xclip` or `xsel`, or check X11 access",
            ));
        }
    }
    Err(ClipboardUnavailableError::new("Clipboard unavailable"))
}

/// `openBrowser(target)`：在平台默认处理器中打开 URL/文件。
///
/// 绝不经过 shell：Windows 不能用 `cmd /c start`（cmd.exe 会重新解析 `&`/`|`/`^`，让
/// 攻击者控制的 URL 可注入）。启动失败（例如没有 `xdg-open`）被忽略：调用方仍会把目标
/// 呈现给用户，浏览器启动是尽力而为。
pub fn open_browser(target: &str) {
    open_browser_with(target, process_platform(), &launch_detached);
}

/// [`open_browser`] 的注入版本：平台与启动器由调用方提供（测试用）。
///
/// 平台 → 启动器：`darwin` → `open <target>`；`win32` →
/// `rundll32 url.dll,FileProtocolHandler <target>`；其余 → `xdg-open <target>`。
/// `target` 始终作为单个 argv 元素传递，不经过 shell 解析（测试断言命令名与完整参数表）。
#[doc(hidden)]
pub fn open_browser_with(target: &str, platform: &str, launch: &dyn Fn(&str, &[String])) {
    let (command, args): (&str, Vec<String>) = if platform == "darwin" {
        ("open", vec![target.to_string()])
    } else if platform == "win32" {
        (
            "rundll32",
            vec![
                "url.dll,FileProtocolHandler".to_string(),
                target.to_string(),
            ],
        )
    } else {
        ("xdg-open", vec![target.to_string()])
    };
    launch(command, &args);
}

/// `spawn(cmd, args, { stdio: "ignore", detached: true }).on("error", () => {}).unref()`。
///
/// 启动失败（例如目标机器没有 `xdg-open`）静默忽略，函数立即返回；POSIX 下子进程进入新的
/// 进程组（父进程退出不会波及它），Windows 下 `CREATE_NO_WINDOW`。成功时用一个后台线程
/// `wait()` 回收，避免僵尸进程；该线程只做等待，不参与任何剪贴板逻辑。
fn launch_detached(command: &str, args: &[String]) {
    let mut process = std::process::Command::new(command);
    process.args(args);
    process.stdin(Stdio::null());
    process.stdout(Stdio::null());
    process.stderr(Stdio::null());
    #[cfg(unix)]
    {
        // `detached: true`：新进程组，父进程退出不会波及。
        use std::os::unix::process::CommandExt as _;
        process.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        process.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    // `error` 事件被忽略；`unref()` 后不等待。这里用一个后台线程回收，避免僵尸进程。
    if let Ok(mut child) = process.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Node `os.tmpdir()`（Node 26 `lib/os.js` / libuv `uv_os_tmpdir`）。
pub(crate) fn node_tmpdir() -> std::path::PathBuf {
    node_tmpdir_from(&|key| std::env::var_os(key))
}

/// [`node_tmpdir`] 的环境注入版本（`None`/空值按 JS 的 falsy 处理）。
///
/// 非 Windows 的取值与 Node 26 的 `getTempDir()`（libuv `uv_os_tmpdir`）一致：TMPDIR、TMP、TEMP
/// 依次取第一个非空值，**去掉恰好一个结尾 `/`**（`len > 1` 时才去，所以 `/` 保持 `/`，
/// `//` → `/`，`/tmp/foo//` → `/tmp/foo/`），都缺失时用 `/tmp`。
pub(crate) fn node_tmpdir_from(get: &dyn Fn(&str) -> Option<OsString>) -> std::path::PathBuf {
    // 非 Windows：libuv 依次检查 TMPDIR、TMP、TEMP（空值跳过），否则 `/tmp`。
    #[cfg(not(windows))]
    {
        for key in ["TMPDIR", "TMP", "TEMP"] {
            if let Some(value) = get(key)
                && !value.is_empty()
            {
                return std::path::PathBuf::from(trim_one_trailing_separator(value));
            }
        }
        std::path::PathBuf::from("/tmp")
    }
    #[cfg(windows)]
    {
        windows_tmpdir(get)
    }
}

/// Node 26 `os.tmpdir()` 的 POSIX 尾部处理：`len > 1 && value[len - 1] === '/'` 时去掉一个字符。
///
/// JS 的 `length` 是 UTF-16 码元数，但结尾一定是单字节的 `/`，唯一的「长度为 1 且以 `/` 结尾」
/// 的值就是根目录 `/`，所以按字节判断与 JS 等价（`/` 不动、`//` → `/`、`/tmp/x/` → `/tmp/x`）。
#[cfg(not(windows))]
fn trim_one_trailing_separator(value: OsString) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let mut bytes = value.into_vec();
        if bytes.len() > 1 && bytes.last() == Some(&b'/') {
            bytes.pop();
        }
        OsString::from_vec(bytes)
    }
    #[cfg(not(unix))]
    {
        let mut text = value.to_string_lossy().into_owned();
        if text.len() > 1 && text.ends_with('/') {
            text.pop();
        }
        OsString::from(text)
    }
}

/// Windows 的 `os.tmpdir()`：`process.env.TEMP || process.env.TMP ||
/// (process.env.SystemRoot || process.env.windir) + '\\temp'`，再去掉一个尾部反斜杠
/// （盘符根 `C:\` 除外）。JS 的 `||` 把空字符串当假值；两个变量都缺失时字符串拼接得到
/// 字面量 `undefined\temp`（与 Node `os.tmpdir()` 的回退结果一致）。
#[cfg(windows)]
fn windows_tmpdir(get: &dyn Fn(&str) -> Option<OsString>) -> std::path::PathBuf {
    let truthy = |key: &str| get(key).filter(|value| !value.is_empty());
    let path = truthy("TEMP").or_else(|| truthy("TMP")).unwrap_or_else(|| {
        let root = truthy("SystemRoot")
            .or_else(|| truthy("windir"))
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "undefined".to_string());
        OsString::from(format!("{root}\\temp"))
    });
    let path = path.to_string_lossy().into_owned();
    let trimmed = path.strip_suffix('\\').filter(|rest| {
        // `path.length > 1 && path[path.length - 1] === '\\' && path[path.length - 2] !== ':'`
        !rest.is_empty() && !rest.ends_with(':')
    });
    std::path::PathBuf::from(trimmed.unwrap_or(&path))
}

/// `writeFileSync(path, text, { encoding: "utf8", mode: 0o600 })`。
///
/// 写入前先 `truncate`：若文件意外已存在，内容会被整体替换而不是追加。调用方
/// （`copy_via_windows_clipboard`）传的是带 UUID 的新路径，正常情况下走创建分支。
fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

/// `Buffer#toString("utf8")` 的等价物（非法字节 → U+FFFD）。
fn decode_utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `runClipboardCommand(command, args, { input, timeoutMs })`（注入版）。
///
/// 缓冲上限固定为 `DEFAULT_MAX_BUFFER_BYTES`（所有调用点都不覆盖它），`timeout_ms` 与
/// `input` 由调用方给出。请求经 `ClipboardTestIo::run_command` 转发；生产环境下它就是
/// `default_command_runner`，最终调用 [`run_clipboard_command`]。
pub(crate) async fn run_io_command(
    io: &ClipboardTestIo,
    command: &str,
    args: Vec<String>,
    input: Option<String>,
    timeout_ms: u64,
) -> Option<Vec<u8>> {
    (io.run_command)(ClipboardCommandRequest {
        command: command.to_string(),
        args,
        input,
        timeout_ms,
        max_buffer_bytes: DEFAULT_MAX_BUFFER_BYTES,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MAX_OSC52_ENCODED_LENGTH` 是私有常量；这里钉死边界：
    /// base64 长度 100000 可以写出，100004 拒绝且不写任何字节。
    #[test]
    fn osc52_limit_is_private_and_exact() {
        // 75000 字节 → base64 恰好 100000 字符。
        let at_limit = "x".repeat(75_000);
        let mut out: Vec<u8> = Vec::new();
        assert!(emit_osc52_to(&at_limit, &mut out));
        let sequence = String::from_utf8(out).expect("OSC 52 is ASCII");
        assert!(sequence.starts_with("\x1b]52;c;"));
        assert!(sequence.ends_with('\x07'));
        assert_eq!(sequence.len(), "\x1b]52;c;".len() + 100_000 + 1);

        // 75001 字节 → 100004 字符，超限：不写任何字节。
        let over_limit = "x".repeat(75_001);
        let mut out: Vec<u8> = Vec::new();
        assert!(!emit_osc52_to(&over_limit, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn osc52_encodes_utf8_bytes() {
        let mut out: Vec<u8> = Vec::new();
        assert!(emit_osc52_to("héllo", &mut out));
        let expected = format!(
            "\x1b]52;c;{}\x07",
            base64::engine::general_purpose::STANDARD.encode("héllo".as_bytes())
        );
        assert_eq!(String::from_utf8(out).expect("ASCII"), expected);
    }

    /// `process.platform` 的取值必须与 Node 一致（只有真正的 linux 才走 Linux 分支）。
    #[test]
    fn process_platform_matches_node_platform_names() {
        let cases = [
            ("linux", "linux"),
            ("macos", "darwin"),
            ("ios", "darwin"),
            ("windows", "win32"),
            ("android", "android"),
            ("freebsd", "freebsd"),
            ("openbsd", "openbsd"),
            ("netbsd", "netbsd"),
            ("dragonfly", "dragonfly"),
            ("solaris", "sunos"),
            ("illumos", "sunos"),
            ("aix", "aix"),
            ("haiku", "haiku"),
        ];
        for (os, expected) in cases {
            assert_eq!(platform_for_os(os), expected, "{os}");
        }
        // 本机（编译期 `target_os`）走同一张映射表。
        assert_eq!(process_platform(), platform_for_os(std::env::consts::OS));
    }

    /// Node 26 `os.tmpdir()`：非 Windows 依次 TMPDIR / TMP / TEMP（空值跳过）并去掉一个结尾 `/`，
    /// 否则 `/tmp`。
    #[cfg(not(windows))]
    #[test]
    fn node_tmpdir_checks_tmpdir_tmp_temp_then_slash_tmp() {
        use std::path::PathBuf;

        fn node_tmpdir_with(pairs: &[(&str, &str)]) -> PathBuf {
            let get = |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            };
            node_tmpdir_from(&get)
        }

        assert_eq!(node_tmpdir_with(&[]), PathBuf::from("/tmp"));
        assert_eq!(
            node_tmpdir_with(&[("TEMP", "/temp-probe")]),
            PathBuf::from("/temp-probe")
        );
        assert_eq!(
            node_tmpdir_with(&[("TMP", "/tmp-probe"), ("TEMP", "/temp-probe")]),
            PathBuf::from("/tmp-probe")
        );
        // 空值是 falsy：继续看下一个变量。
        assert_eq!(
            node_tmpdir_with(&[("TMPDIR", ""), ("TMP", "/tmp-probe")]),
            PathBuf::from("/tmp-probe")
        );
        assert_eq!(
            node_tmpdir_with(&[("TMPDIR", "/tmpdir-probe"), ("TMP", "/tmp-probe")]),
            PathBuf::from("/tmpdir-probe")
        );
        // `TEMPDIR` 不是 Node 的变量。
        assert_eq!(
            node_tmpdir_with(&[("TEMPDIR", "/tempdir-probe")]),
            PathBuf::from("/tmp")
        );
        // Node 26 `getTempDir()`：去掉**恰好一个**结尾 `/`（`len > 1` 时才去）。
        // 以 `node -e "console.log(require('os').tmpdir())"` 的实测值为准。
        assert_eq!(
            node_tmpdir_with(&[("TMPDIR", "/tmp/foo/")]),
            PathBuf::from("/tmp/foo")
        );
        assert_eq!(
            node_tmpdir_with(&[("TMPDIR", "/tmp/foo//")]),
            PathBuf::from("/tmp/foo/")
        );
        assert_eq!(
            node_tmpdir_with(&[("TMPDIR", "/tmp/foo///")]),
            PathBuf::from("/tmp/foo//")
        );
        // 根目录长度是 1：不去（`/` 仍是 `/`）；`//` 去一个变成 `/`。
        assert_eq!(node_tmpdir_with(&[("TMPDIR", "/")]), PathBuf::from("/"));
        assert_eq!(node_tmpdir_with(&[("TMPDIR", "//")]), PathBuf::from("/"));
        // 同样的尾部处理也作用于 TMP / TEMP 的值。
        assert_eq!(
            node_tmpdir_with(&[("TMP", "/tmp-probe/")]),
            PathBuf::from("/tmp-probe")
        );
        assert_eq!(
            node_tmpdir_with(&[("TEMP", "/temp-probe//")]),
            PathBuf::from("/temp-probe/")
        );
    }

    /// Node 26 `os.tmpdir()` 的 Windows 分支：`TEMP || TMP || (SystemRoot || windir) + '\\temp'`。
    #[cfg(windows)]
    #[test]
    fn node_tmpdir_checks_temp_tmp_then_system_root() {
        use std::path::PathBuf;

        fn node_tmpdir_with(pairs: &[(&str, &str)]) -> PathBuf {
            let get = |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            };
            node_tmpdir_from(&get)
        }

        // TEMP 优先于 TMP（Node 的 JS 实现），TMPDIR 不参与。
        assert_eq!(
            node_tmpdir_with(&[
                ("TMPDIR", "C:\\ignored"),
                ("TMP", "C:\\tmp-probe"),
                ("TEMP", "C:\\temp-probe"),
            ]),
            PathBuf::from("C:\\temp-probe")
        );
        assert_eq!(
            node_tmpdir_with(&[("TMP", "C:\\tmp-probe"), ("SystemRoot", "C:\\Windows"),]),
            PathBuf::from("C:\\tmp-probe")
        );
        assert_eq!(
            node_tmpdir_with(&[("SystemRoot", "C:\\Windows")]),
            PathBuf::from("C:\\Windows\\temp")
        );
        // 尾部反斜杠被去掉；盘符根保持不变。
        assert_eq!(
            node_tmpdir_with(&[("TEMP", "C:\\Temp\\")]),
            PathBuf::from("C:\\Temp")
        );
        assert_eq!(node_tmpdir_with(&[("TEMP", "C:\\")]), PathBuf::from("C:\\"));
    }
}
