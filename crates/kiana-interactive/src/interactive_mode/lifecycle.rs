//! `interactive-mode.ts` 的信号处理与生命周期（§2.50.7 / §2.50.19）。
//!
//! # 信号
//!
//! 复刻 Node 的 `process.on` / `process.off` 语义：`sigaction` + self-pipe（信号处理器只写
//! 一个字节，异步任务负责关停），退订时恢复注册前的处置。`tokio::signal` 的处理器永不
//! 注销，会让“移除最后一个监听器后恢复默认动作”这一可观察行为失真。
//!
//! # 退出
//!
//! TS 的 `process.exit()` 无法在库内建模成可返回值：这里直接调用 [`std::process::exit`]。
//! 信号/关闭路径的测试通过子进程运行（与 TS 的 fork 测试同构）。
//!
//! # 本文件负责的边界
//!
//! 实现 TS `interactive-mode.ts` 里与进程存活相关的成员：`registerSignalHandlers` /
//! `unregisterSignalHandlers` / `checkShutdownRequested` / `shutdown` /
//! `emergencyTerminalExit` / `uncaughtCrash` / `handleCtrlZ`，以及它们的平台注入点与
//! 测试接缝。共享状态只有 `InteractiveModeInner` 的三个字段：
//! `signal_cleanup_handlers`（每个信号一条恢复动作）、`shutdown_requested`（扩展请求
//! 关停，等当前轮次结束再处理）、`is_shutting_down`（关停只执行一次）。
//! TUI 的挂载/停止与扩展状态清理由其它子模块负责，这里只按 TS 的顺序调用它们。
//!
//! 调用方（均在本 crate 内）：
//!
//! * `InteractiveMode::init()` 调用 `register_signal_handlers()`；
//! * `InteractiveMode::stop_with()` 调用 `unregister_signal_handlers()`，但
//!   `is_shutting_down` 为真时跳过（见 `stop_with` 内的 #5724 注释）；
//! * 扩展 shutdown handler 把 `shutdown_requested` 置真后，`AgentSettled` 事件经
//!   `check_shutdown_requested()` 触发 `shutdown(false)`；
//! * `app.suspend` 键位经 `run_after_dispatch` 调用 `handle_ctrl_z()`；
//! * 导出/导入/分享与输入处理把 `io::Error` 交给 `uncaught_crash()`。
//!
//! # 从信号到关停的执行流程
//!
//! 1. `sigaction` 把 SIGTERM（非 Windows 还会注册 SIGHUP）指向 `wake`；该函数只做
//!    异步信号安全的一步：向 self-pipe 写 1 字节。
//! 2. 注册时 `tokio::task::spawn_local` 出一个任务，用 `AsyncFd` 等读端。任务属于 UI 的
//!    `LocalSet`，所以后续 `shutdown` 里的 `Rc`/`RefCell` 访问仍发生在单线程上。
//! 3. 读到字节后任务先调用 `kill_tracked_detached_children()`，再把 `Weak` 升级成
//!    `InteractiveMode` 并执行 `shutdown(true)`（信号路径）。
//! 4. `unregister_signal_handlers()` 恢复注册前的 `sigaction` 处置并清空恢复动作列表，
//!    因而“移除最后一个监听器后恢复默认动作”这一 TS 行为是可观察的——这正是没有使用
//!    `tokio::signal` 的原因（后者从不注销处理器）。
//!
//! # 约束与失败模式
//!
//! * `WAKE_FDS` 按**信号编号**索引且是进程级全局量：同一进程只应存在一个交互模式；
//!   `register_signal_handlers` 先注销旧注册，避免同一信号留下两个 handler。
//! * `wake` 忽略 `write` 的返回值：管道写满或读端已关闭时字节会丢失，但信号处理上下文
//!   里不能分配、加锁或打印，只能接受这一点。
//! * 关停/崩溃路径最终调用 `std::process::exit`，不展开栈、不运行 `Drop`。测试用
//!   **线程局部**钩子接管退出，因此钩子只在安装它的线程上生效（测试都跑在
//!   `current_thread` 运行时上）。
//! * panic hook 是进程级且只安装一次（`Once`）；`PANIC_HOOK_ACTIVE` 决定它是否处理当前
//!   panic。hook 自身重入或模式状态正被 `borrow_mut` 占用时退回默认 hook。

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use kiana_core::config::APP_NAME;

use super::{InteractiveMode, is_dead_terminal_error_code};

// ---------------------------------------------------------------------------
// sigaction + self-pipe
// ---------------------------------------------------------------------------

/// 本模块用到的信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalKind {
    Term,
    Hup,
}

impl SignalKind {
    fn number(self) -> libc::c_int {
        match self {
            SignalKind::Term => libc::SIGTERM,
            SignalKind::Hup => libc::SIGHUP,
        }
    }
}

/// `WAKE_FDS` 的槽位数，必须覆盖本模块用到的信号编号（SIGTERM=15、SIGHUP=1）。
const WAKE_FD_SLOTS: usize = 32;

/// 信号编号 → self-pipe 写端 fd；`-1` 表示该信号尚未注册。
///
/// 用静态数组而不是字典/`Mutex`，是因为 `wake` 运行在信号处理器上下文，只能访问
/// 异步信号安全的存储，不能加锁或分配内存。
static WAKE_FDS: [AtomicI32; WAKE_FD_SLOTS] = [const { AtomicI32::new(-1) }; WAKE_FD_SLOTS];

/// 信号处理器：向对应 self-pipe 写入 1 字节，唤醒等待中的异步任务。
///
/// 这里只允许异步信号安全的操作：`Relaxed` 原子读 + `libc::write`。返回值被有意忽略：
/// 管道满或读端已关闭时字节会丢失，但处理函数没有安全的办法报告或重试。
extern "C" fn wake(signal: libc::c_int) {
    let slot = signal as usize;
    if slot >= WAKE_FD_SLOTS {
        return;
    }
    let fd = WAKE_FDS[slot].load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte = 1u8;
    unsafe {
        libc::write(fd, std::ptr::addr_of!(byte).cast(), 1);
    }
}

/// 一次信号注册的产物：信号编号、注册前的处置（用于恢复）与 self-pipe 两端。
///
/// 读端交给监听任务，写端由任务持有以保证管道在任务存活期间不被关闭。
struct SignalRegistration {
    signal_number: libc::c_int,
    previous: libc::sigaction,
    read_end: OwnedFd,
    write_end: OwnedFd,
}

/// 注册一个信号的处理器并创建配套 self-pipe；失败返回 `None`。
///
/// 失败可能来自 `pipe`/`fcntl`（`wake_pipe`）或 `sigaction`；两种情况都不会留下半注册
/// 状态：`WAKE_FDS` 在 `sigaction` 失败时被重置为 `-1`，而前面的 `wake_pipe` 资源随
/// `Option` 提前返回被 `Drop` 关闭。
fn register_signal(kind: SignalKind) -> Option<SignalRegistration> {
    let signal_number = kind.number();
    let slot = signal_number as usize;
    if slot >= WAKE_FD_SLOTS {
        return None;
    }
    let (read_end, write_end) = wake_pipe().ok()?;
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = wake as *const () as usize;
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    // 必须先发布 fd 再装处理器，否则信号可能在槽位仍为 -1 时到达并被丢弃。
    WAKE_FDS[slot].store(write_end.as_raw_fd(), Ordering::SeqCst);
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(signal_number, &action, &mut previous) } != 0 {
        // 注册失败：处理器未生效，撤回 fd 的发布。
        WAKE_FDS[slot].store(-1, Ordering::SeqCst);
        return None;
    }
    Some(SignalRegistration {
        signal_number,
        previous,
        read_end,
        write_end,
    })
}

/// 撤销一个信号的注册：先让处理器找不到 fd，再恢复注册前的处置。
fn restore_signal(signal_number: libc::c_int, previous: libc::sigaction) {
    let slot = signal_number as usize;
    if slot >= WAKE_FD_SLOTS {
        return;
    }
    WAKE_FDS[slot].store(-1, Ordering::SeqCst);
    unsafe {
        libc::sigaction(signal_number, &previous, std::ptr::null_mut());
    }
}

/// 等待 self-pipe 至少可读一次，然后返回。
///
/// 不区分读到几个字节，也不重试到 EOF：一次可读就代表“收到过信号”，调用方随后直接进入
/// 关停，因此重复信号或合并写入都只加速同一次关停。`readable()` 或 `read` 报错（如 fd
/// 被关闭）同样直接返回，避免监听任务永久挂起。
async fn read_wake_byte(reader: &tokio::io::unix::AsyncFd<OwnedFd>) {
    let mut byte = [0u8; 8];
    loop {
        let mut ready = match reader.readable().await {
            Ok(ready) => ready,
            Err(_) => return,
        };
        let result = ready.try_io(|inner| {
            let written =
                unsafe { libc::read(inner.as_raw_fd(), byte.as_mut_ptr().cast(), byte.len()) };
            if written < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(written)
            }
        });
        match result {
            Ok(Ok(_)) => return,
            Ok(Err(_)) => return,
            Err(_would_block) => continue,
        }
    }
}

/// 创建 self-pipe；两端都设置 `O_NONBLOCK` 与 `FD_CLOEXEC`（见 [`set_nonblocking_cloexec`]）。
fn wake_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [&read_end, &write_end] {
        set_nonblocking_cloexec(fd.as_raw_fd())?;
    }
    Ok((read_end, write_end))
}

/// 给管道 fd 设置 `O_NONBLOCK` 与 `FD_CLOEXEC`（exec 后不把管道泄漏给子进程）。
///
/// 读端由 `AsyncFd` + `try_io` 轮询，必须非阻塞；写端非阻塞可保证信号处理器里的
/// `write` 在管道写满时立即返回而不是阻塞整个进程。
///
/// 两组失败的处理不对称：`F_GETFL`/`F_SETFL` 失败向上传播（非阻塞没设上，上述行为
/// 不成立），而 `F_GETFD`/`F_SETFD` 失败被忽略（cloexec 只是防泄漏的加固，失败时 fd
/// 的读写行为不变）。
fn set_nonblocking_cloexec(fd: libc::c_int) -> std::io::Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd_flags = libc::fcntl(fd, libc::F_GETFD);
        if fd_flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
        }
    }
    Ok(())
}

/// `isDeadTerminalError(error)`（Node 的 `error.code` 判定）。
pub(crate) fn is_dead_terminal_error(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .and_then(|code| {
            if code == libc::EIO
                || code == libc::EPIPE
                || code == libc::ENOTCONN
                || code == libc::ENOTTY
            {
                Some(())
            } else {
                None
            }
        })
        .is_some()
        || is_dead_terminal_error_code(&error.kind().to_string())
}

/// 进程退出的测试替身（TS 测试用 `vi.spyOn(process, "exit")`）。
type ExitHook = Box<dyn FnMut(i32)>;

thread_local! {
    static EXIT_HOOK: std::cell::RefCell<Option<ExitHook>> =
        const { std::cell::RefCell::new(None) };
}

/// 安装/清除进程退出钩子（`doc(hidden)`，仅供测试）。
#[doc(hidden)]
pub fn set_exit_hook_for_test(hook: Option<Box<dyn FnMut(i32)>>) {
    EXIT_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

/// `process.stdout.write` 钩子。
type StdoutHook = Box<dyn FnMut(&str)>;

thread_local! {
    /// `process.stdout.write` 的测试替身。
    static STDOUT_HOOK: std::cell::RefCell<Option<StdoutHook>> =
        const { std::cell::RefCell::new(None) };
}

/// 安装/清除 stdout 钩子（`doc(hidden)`，仅供测试）。
#[doc(hidden)]
pub fn set_stdout_hook_for_test(hook: Option<StdoutHook>) {
    STDOUT_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

/// `process.stdout.write`：测试钩子接管时走钩子，否则写真实 stdout。
fn write_stdout(text: &str) {
    let handled = STDOUT_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some(hook) => {
                hook(text);
                true
            }
            None => false,
        }
    });
    if !handled {
        print!("{text}");
    }
}

thread_local! {
    /// `process.stdout.isTTY` 的测试替身（`format_resume_command` 的分支）。
    static STDOUT_IS_TTY_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// 覆盖 `stdout.isTTY`（`doc(hidden)`，仅供测试）。
#[doc(hidden)]
pub fn set_stdout_is_tty_for_test(value: Option<bool>) {
    STDOUT_IS_TTY_OVERRIDE.with(|slot| slot.set(value));
}

/// `process.stdout.isTTY`：测试覆盖优先，否则问 libc 的 `isatty`。
fn stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    STDOUT_IS_TTY_OVERRIDE
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| std::io::stdout().is_terminal())
}

/// `process.exit(code)`；测试钩子存在时返回 `true`（调用方应提前返回）。
fn exit_process(code: i32) -> bool {
    EXIT_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(hook) = slot.as_mut() {
            hook(code);
            return true;
        }
        false
    })
}

// ---------------------------------------------------------------------------
// `process.prependListener("uncaughtException", uncaughtCrash)` 的 Rust 等价物
// ---------------------------------------------------------------------------

/// 进程级 panic hook 是否生效（`registerSignalHandlers` 安装 / `unregister` 移除）。
static PANIC_HOOK_ACTIVE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK_INSTALLED: Once = Once::new();

thread_local! {
    /// panic hook 递归保护：崩溃路径自身 panic 时交回默认 hook，避免进程 abort。
    static IN_PANIC_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 非 `Error` payload 的占位错误（Node 打印 `Box<dyn Any>` 等类似做法）。
#[derive(Debug)]
struct PanicMessageError(String);

impl std::fmt::Display for PanicMessageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PanicMessageError {}

/// Node `fs` 异常在未捕获崩溃路径里的载体。
///
/// `panic_any(io::Error)` 的 payload 不是 `Error` 的 `Display`：崩溃横幅会退化成
/// `Box<dyn Any>`。TS 里 `fs.mkdirSync`/`fs.writeFileSync` 抛出的是 Node 的 fs 异常，
/// `console.error(error)` 打印 `Error: <CODE>: <description>, <syscall> '<path>'`；
/// 这里把已经格式化成 Node 文案的消息与原始 `io::Error` 一起携带：`Display` 用于
/// 横幅/崩溃记录，`source()` 保留原始错误供 `isDeadTerminalError` 判定。
#[derive(Debug)]
pub(crate) struct NodeFsError {
    message: String,
    source: std::io::Error,
}

impl NodeFsError {
    /// 用 Node 的 `error.message` 文案与原始 `io::Error` 构造。
    pub(crate) fn new(message: String, source: std::io::Error) -> Self {
        NodeFsError { message, source }
    }

    /// 原始 `io::Error`。
    pub(crate) fn io_error(&self) -> &std::io::Error {
        &self.source
    }
}

impl std::fmt::Display for NodeFsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NodeFsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// `panic!` payload → 消息文本（与 `String(error)` 的直觉等价）。
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_string();
    }
    if let Some(error) = payload.downcast_ref::<NodeFsError>() {
        return error.to_string();
    }
    "Box<dyn Any>".to_string()
}

/// 安装 `uncaughtException` 等价钩子（幂等；进程内只装一次，用 `PANIC_HOOK_ACTIVE` 开关）。
pub(crate) fn install_uncaught_panic_hook() {
    PANIC_HOOK_ACTIVE.store(true, Ordering::SeqCst);
    PANIC_HOOK_INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // `catch_unwind_silent` / `catch_unwind_guarded` 覆盖的 panic 是 `try/catch`，
            // 不是未捕获异常（与 TS 的 uncaughtException 语义一致）。
            if !PANIC_HOOK_ACTIVE.load(Ordering::SeqCst)
                || crate::components::markdown_transform::panics_are_guarded()
            {
                previous(info);
                return;
            }
            if IN_PANIC_HOOK.with(|flag| flag.replace(true)) {
                previous(info);
                return;
            }
            let message = panic_payload_message(info.payload());
            let stack = format!(
                "Error: {message}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
            let Some(mut mode) = super::session::current_mode() else {
                IN_PANIC_HOOK.with(|flag| flag.set(false));
                previous(info);
                return;
            };
            // 正在可变借用模式状态（panic 发生在 borrow_mut 作用域内）：崩溃路径无法安全
            // 进入，交回默认 hook（tokio 会捕获任务 panic，等价的 TS 行为见报告 deviations）。
            if mode.inner.try_borrow_mut().is_err() {
                IN_PANIC_HOOK.with(|flag| flag.set(false));
                previous(info);
                return;
            }
            let fallback;
            let error: &(dyn std::error::Error + 'static) =
                if let Some(node_error) = info.payload().downcast_ref::<NodeFsError>() {
                    // `/debug` 的 fs 失败：Display 已是 Node 的 `error.message`，
                    // `uncaught_crash_code` 的 `to_string()` 与死终端判定都用它。
                    node_error
                } else if let Some(io_error) = info.payload().downcast_ref::<std::io::Error>() {
                    io_error
                } else {
                    fallback = PanicMessageError(message);
                    &fallback
                };
            let code = mode.uncaught_crash_code(error, Some(&stack));
            std::process::exit(code);
        }));
    });
}

/// `unregisterSignalHandlers()` 里移除 uncaughtException 监听。
pub(crate) fn disable_uncaught_panic_hook() {
    PANIC_HOOK_ACTIVE.store(false, Ordering::SeqCst);
}

impl InteractiveMode {
    /// `registerSignalHandlers()`。
    ///
    /// 注册前先调用 [`Self::unregister_signal_handlers`]，恢复上一个注册留下的 `sigaction`
    /// 处置并清空恢复动作列表。Unix 上随后对 SIGTERM / SIGHUP 各走一遍 [`register_signal`]：
    /// 清理动作进 `signal_cleanup_handlers`，监听任务用 `spawn_local` 挂到当前 `LocalSet`
    /// （`Rc`/`RefCell` 因此不用跨线程）。任一信号注册失败只是 `continue` 跳过该信号，
    /// 不阻塞其它信号与末尾的 panic hook 安装。
    pub(crate) fn register_signal_handlers(&mut self) {
        self.unregister_signal_handlers();

        #[cfg(unix)]
        {
            let mut kinds = vec![SignalKind::Term];
            kinds.push(SignalKind::Hup);
            for kind in kinds {
                let Some(registration) = register_signal(kind) else {
                    continue;
                };
                let read_end = registration.read_end;
                let write_end = registration.write_end;
                let signal_number = registration.signal_number;
                let previous = registration.previous;
                self.inner
                    .borrow_mut()
                    .signal_cleanup_handlers
                    .push(Box::new(move || {
                        restore_signal(signal_number, previous);
                    }));
                let weak = self.downgrade();
                tokio::task::spawn_local(async move {
                    let _write_end = write_end;
                    #[allow(
                        deprecated,
                        reason = "AsyncFd::new 仍是最直接的 OwnedFd 适配；register 需要 &mut 生命周期"
                    )]
                    let reader = match tokio::io::unix::AsyncFd::new(read_end) {
                        Ok(reader) => reader,
                        Err(_) => return,
                    };
                    read_wake_byte(&reader).await;
                    kiana_core::shell::kill_tracked_detached_children();
                    if let Some(inner) = weak.upgrade() {
                        let mut mode = InteractiveMode { inner };
                        mode.shutdown(true).await;
                    }
                });
            }
        }
        // Node 的 stdin/stdout/stderr `error` 监听在 Rust 里由 `io::Result` 返回码承担；
        // `uncaughtException` 用进程级 panic hook 复刻（终端已死时 panic 不落崩溃记录）。
        install_uncaught_panic_hook();
    }

    /// `unregisterSignalHandlers()`。
    ///
    /// 先一次性 `take` 出全部清理动作，再在借用结束之后逐个执行：动作是 `Box<dyn FnMut>`，
    /// 执行期间不应再持有 `inner` 的 `RefCell` 借用。
    pub(crate) fn unregister_signal_handlers(&mut self) {
        disable_uncaught_panic_hook();
        let handlers = {
            let mut inner = self.inner.borrow_mut();
            std::mem::take(&mut inner.signal_cleanup_handlers)
        };
        for mut cleanup in handlers {
            cleanup();
        }
    }

    /// `checkShutdownRequested()`。
    ///
    /// 由 `AgentSettled` 事件处理调用（`events` 子模块）。扩展的 shutdown handler 只置
    /// `shutdown_requested` 标志（见 `session` 子模块）；会话正忙时它不立即关停，而是
    /// 等本轮结束、事件循环走到这里再走非信号路径的 [`Self::shutdown`]。
    pub(crate) async fn check_shutdown_requested(&mut self) {
        if !self.inner.borrow().shutdown_requested {
            return;
        }
        self.shutdown(false).await;
    }

    /// `shutdown(options?)`。
    ///
    /// 幂等：`is_shutting_down` 已置真时直接返回，因此扩展 shutdown handler、
    /// `check_shutdown_requested` 与信号任务并发/先后触发也只会执行一次。两条路径的
    /// 顺序不同：信号路径按 TS 的 `session_shutdown`(dispose) → 终端 顺序执行（见函数内
    /// 原有注释），正常路径先 `stop()` 再 `dispose`，并额外打印 resume 命令。
    pub(crate) async fn shutdown(&mut self, from_signal: bool) {
        if self.inner.borrow().is_shutting_down {
            return;
        }
        self.inner.borrow_mut().is_shutting_down = true;

        if from_signal {
            // 信号路径：先 emit `session_shutdown`（dispose），再碰终端。
            let runtime = Rc::clone(&self.inner.borrow().runtime_host);
            runtime.dispose().await;
            self.disable_theme_auto_sync();
            drain_terminal_input(self).await;
            self.stop();
            if exit_process(0) {
                return;
            }
            std::process::exit(0);
        }

        self.disable_theme_auto_sync();
        drain_terminal_input(self).await;

        // 正常路径：停终端（写恢复序列）→ dispose 扩展 → 打印 resume 命令。
        self.stop();
        let runtime = Rc::clone(&self.inner.borrow().runtime_host);
        runtime.dispose().await;

        let session = self.session();
        if let Some(resume_command) = super::format_resume_command_with(&session, stdout_is_tty()) {
            let label = if stdout_supports_color() {
                "\x1b[2mTo resume this session:\x1b[22m".to_string()
            } else {
                "To resume this session:".to_string()
            };
            write_stdout(&format!("{label} {resume_command}\n"));
        }

        if exit_process(0) {
            return;
        }
        std::process::exit(0);
    }

    /// `emergencyTerminalExit()`：终端已死，不写任何恢复序列。
    pub(crate) fn emergency_terminal_exit(&mut self) -> ! {
        let code = self.emergency_terminal_exit_code();
        std::process::exit(code);
    }

    /// `emergencyTerminalExit()` 的可测试核心：只做副作用并返回退出码。
    ///
    /// 与正常崩溃流程的区别：终端已死，任何写终端的操作（TUI 恢复序列、扩展清理）都会
    /// 再次触发 EIO，因此只注销信号处理器、杀掉跟踪的子进程，然后以 129 退出。
    fn emergency_terminal_exit_code(&mut self) -> i32 {
        self.inner.borrow_mut().is_shutting_down = true;
        self.unregister_signal_handlers();
        kiana_core::shell::kill_tracked_detached_children();
        // 终端已死：不运行正常关停（TUI / 扩展清理会写恢复序列并再次触发 EIO）。
        129
    }

    /// `uncaughtCrash(error)`。
    ///
    /// 与 TS 的 `process.exit` 一样永不返回；但测试里 `set_exit_hook_for_test` 会接管退出，
    /// 此时返回给调用方（命令处理器随后结束自己的流程），以便断言崩溃路径。
    pub(crate) fn uncaught_crash(
        &mut self,
        error: &(dyn std::error::Error + 'static),
        stack: Option<&str>,
    ) {
        let code = self.uncaught_crash_code(error, stack);
        if exit_process(code) {
            return;
        }
        std::process::exit(code);
    }

    /// [`Self::uncaught_crash`] 的可测试核心：只做副作用并返回退出码。
    ///
    /// 错误分类决定三种去向：死终端错误 → [`Self::emergency_terminal_exit_code`]（退出码
    /// 129，不写终端、不记录 crash）；已在关停中 → 直接返回 1；其余 → 完整崩溃流程
    /// （停 TUI、打印横幅与栈、记录 crash）后返回 1。
    fn uncaught_crash_code(
        &mut self,
        error: &(dyn std::error::Error + 'static),
        stack: Option<&str>,
    ) -> i32 {
        // 终端已死不是 kiana 的崩溃：不尝试恢复终端，也不记录 crash（regression #5080）。
        let dead_terminal = error
            .downcast_ref::<std::io::Error>()
            .is_some_and(is_dead_terminal_error)
            || error
                .downcast_ref::<NodeFsError>()
                .is_some_and(|node_error| is_dead_terminal_error(node_error.io_error()));
        if dead_terminal {
            return self.emergency_terminal_exit_code();
        }
        if self.inner.borrow().is_shutting_down {
            return 1;
        }
        self.inner.borrow_mut().is_shutting_down = true;
        self.unregister_signal_handlers();
        kiana_core::shell::kill_tracked_detached_children();
        {
            let mut inner = self.inner.borrow_mut();
            inner.ui.stop(kiana_tui::tui::TuiStopOptions::default());
        }
        eprintln!("{APP_NAME} exiting due to uncaughtException:");
        // TS `console.error(error)`：Error 的输出包含栈。
        match stack {
            Some(stack) => eprintln!("{stack}"),
            None => eprintln!("{error}"),
        }
        let hint = self.get_crash_extension_hint(stack);
        if let Some(hint) = hint {
            eprintln!("\n{hint}");
        }
        if self.record_crash(
            kiana_resources::crash_log::CrashKind::UncaughtException,
            &error.to_string(),
            stack,
        ) {
            eprintln!("\n{}", self.crash_report_instructions());
        }
        1
    }

    /// `handleCtrlZ()`。
    pub(crate) fn handle_ctrl_z(&mut self) {
        #[cfg(unix)]
        {
            let mut platform = ProcessCtrlZPlatform;
            self.handle_ctrl_z_with_platform(&mut platform);
        }
        #[cfg(not(unix))]
        {
            let mut platform = UnsupportedCtrlZPlatform;
            self.handle_ctrl_z_with_platform(&mut platform);
        }
    }

    /// `handleCtrlZ` 的平台无关部分（测试注入 [`CtrlZPlatform`]）。
    ///
    /// 挂起流程：登记 SIGCONT 恢复回调 → 停 TUI（写恢复序列、回到正常屏幕）→ 对整个
    /// 进程组发 SIGTSTP。恢复回调由 `ProcessCtrlZPlatform::on_sigcont` 的轮询任务触发，
    /// 负责：清 keep-alive、移除 SIGINT 忽略、重启 TUI 并请求一帧重绘。`kill_sigtstp`
    /// 失败时回滚前两步登记的动作，再 panic（对应 Node 下 `process.kill` 抛异常）。
    fn handle_ctrl_z_with_platform(&mut self, platform: &mut dyn CtrlZPlatform) {
        if platform.is_windows() {
            self.show_status("Suspend to background is not supported on Windows");
            return;
        }

        // 挂起期间保持进程存活（Node 的事件循环句柄；Rust 下由恢复任务持有）。
        let clear_keep_alive = Rc::new(std::cell::RefCell::new(Some(platform.start_keep_alive())));
        // 挂起期间忽略 SIGINT，恢复时移除。
        let remove_sigint = Rc::new(std::cell::RefCell::new(Some(platform.ignore_sigint())));

        {
            let clear_keep_alive = Rc::clone(&clear_keep_alive);
            let remove_sigint = Rc::clone(&remove_sigint);
            let weak = self.downgrade();
            platform.on_sigcont(Box::new(move || {
                if let Some(mut clear) = clear_keep_alive.borrow_mut().take() {
                    clear();
                }
                if let Some(mut remove) = remove_sigint.borrow_mut().take() {
                    remove();
                }
                if let Some(inner) = weak.upgrade() {
                    let mode = InteractiveMode { inner };
                    mode.inner.borrow_mut().ui.start();
                    mode.inner.borrow_mut().ui.request_render(true);
                }
            }));
        }

        self.inner
            .borrow_mut()
            .ui
            .stop(kiana_tui::tui::TuiStopOptions::default());

        // 向整个进程组发送 SIGTSTP（pid=0）。
        if let Err(error) = platform.kill_sigtstp() {
            if let Some(mut clear) = clear_keep_alive.borrow_mut().take() {
                clear();
            }
            if let Some(mut remove) = remove_sigint.borrow_mut().take() {
                remove();
            }
            panic!("{error}");
        }
    }
}

// ---------------------------------------------------------------------------
// Ctrl+Z 平台注入点
// ---------------------------------------------------------------------------

/// `handleCtrlZ` 的平台/进程操作（TS 测试用 `vi.spyOn(process, ...)`）。
///
/// 生产实现见 [`ProcessCtrlZPlatform`]；测试用记录型实现断言 5080 系列测试观察的调用。
#[doc(hidden)]
pub trait CtrlZPlatform {
    /// `process.platform === "win32"`。
    fn is_windows(&self) -> bool;
    /// `setInterval(() => {}, 2**30)`；返回 `clearInterval` 动作。
    fn start_keep_alive(&mut self) -> Box<dyn FnMut()>;
    /// `process.on("SIGINT", noop)`；返回 `removeListener` 动作。
    fn ignore_sigint(&mut self) -> Box<dyn FnMut()>;
    /// `process.once("SIGCONT", handler)`。
    fn on_sigcont(&mut self, handler: Box<dyn FnMut() + 'static>);
    /// `process.kill(0, "SIGTSTP")`。
    fn kill_sigtstp(&mut self) -> std::io::Result<()>;
}

/// Windows / 非 Unix：只走“不支持挂起”的分支。
#[cfg(not(unix))]
struct UnsupportedCtrlZPlatform;

#[cfg(not(unix))]
impl CtrlZPlatform for UnsupportedCtrlZPlatform {
    fn is_windows(&self) -> bool {
        true
    }
    fn start_keep_alive(&mut self) -> Box<dyn FnMut()> {
        Box::new(|| {})
    }
    fn ignore_sigint(&mut self) -> Box<dyn FnMut()> {
        Box::new(|| {})
    }
    fn on_sigcont(&mut self, _handler: Box<dyn FnMut() + 'static>) {}
    fn kill_sigtstp(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Unix 生产实现：`libc::signal` + 轮询任务（Rust 没有 Node 的 `process.once`）。
#[cfg(unix)]
struct ProcessCtrlZPlatform;

#[cfg(unix)]
thread_local! {
    static PREV_SIGINT: std::cell::Cell<libc::sighandler_t> =
        const { std::cell::Cell::new(libc::SIG_DFL) };
}

#[cfg(unix)]
impl CtrlZPlatform for ProcessCtrlZPlatform {
    fn is_windows(&self) -> bool {
        false
    }

    fn start_keep_alive(&mut self) -> Box<dyn FnMut()> {
        // Node 需要 setInterval 防止事件循环退出；Rust 的恢复任务本身持有进程。
        Box::new(|| {})
    }

    fn ignore_sigint(&mut self) -> Box<dyn FnMut()> {
        let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
        PREV_SIGINT.with(|slot| slot.set(previous));
        Box::new(|| {
            let previous = PREV_SIGINT.with(std::cell::Cell::get);
            unsafe {
                libc::signal(libc::SIGINT, previous);
            }
        })
    }

    fn on_sigcont(&mut self, mut handler: Box<dyn FnMut() + 'static>) {
        use std::sync::atomic::AtomicBool;
        static RESUMED: AtomicBool = AtomicBool::new(false);
        extern "C" fn on_cont(_signal: libc::c_int) {
            RESUMED.store(true, Ordering::SeqCst);
        }
        RESUMED.store(false, Ordering::SeqCst);
        let previous =
            unsafe { libc::signal(libc::SIGCONT, on_cont as *const () as libc::sighandler_t) };
        tokio::task::spawn_local(async move {
            while !RESUMED.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            unsafe {
                libc::signal(libc::SIGCONT, previous);
            }
            handler();
        });
    }

    fn kill_sigtstp(&mut self) -> std::io::Result<()> {
        let result = unsafe { libc::kill(0, libc::SIGTSTP) };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// 在最多 `1000ms` 总时长 / `50ms` 空闲窗口内清空排队中的终端输入（参数传给
/// `Terminal::drain_input`，trait 定义在 `kiana_tui::terminal_colors`）。
///
/// 关停前调用一次，清空终端里尚未被消费的输入。期间持有终端的 `RefCell`
/// 直到 future 结束——单线程 UI 上没有并发借用者，见上面的
/// `clippy::await_holding_refcell_ref` 豁免。
#[allow(
    clippy::await_holding_refcell_ref,
    reason = "drain future borrows the terminal RefCell for its whole lifetime (单线程 UI 上没有并发借用者)"
)]
async fn drain_terminal_input(mode: &InteractiveMode) {
    let terminal = mode.inner.borrow().ui.terminal();
    let mut terminal = terminal.borrow_mut();
    let future = terminal.drain_input(1000, 50);
    future.await;
}

/// resume 命令标签是否用暗色渲染：跟随 `NO_COLOR` / `FORCE_COLOR=0`，且 stdout 必须是终端。
fn stdout_supports_color() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var("FORCE_COLOR").ok().as_deref() == Some("0") {
        return false;
    }
    std::io::stdout().is_terminal()
}

#[doc(hidden)]
impl InteractiveMode {
    /// `shutdown(options?)` 的测试接缝。
    pub async fn test_shutdown(&mut self, from_signal: bool) {
        self.shutdown(from_signal).await;
    }

    /// `handleCtrlZ()` 的平台注入版本（测试接缝）。
    pub fn test_handle_ctrl_z_with_platform(&mut self, platform: &mut dyn CtrlZPlatform) {
        self.handle_ctrl_z_with_platform(platform);
    }

    /// `uncaughtCrash(error)` 的退出码（测试接缝；不真正退出进程）。
    pub fn test_uncaught_crash_code(&mut self, error: &(dyn std::error::Error + 'static)) -> i32 {
        self.uncaught_crash_code(error, None)
    }

    /// `emergencyTerminalExit()` 的退出码（测试接缝）。
    pub fn test_emergency_terminal_exit_code(&mut self) -> i32 {
        self.emergency_terminal_exit_code()
    }

    /// 当前注册的信号/流清理处理器数量（测试接缝）。
    pub fn test_signal_cleanup_handler_count(&self) -> usize {
        self.inner.borrow().signal_cleanup_handlers.len()
    }

    /// `unregisterSignalHandlers()` 的测试接缝。
    pub fn test_unregister_signal_handlers(&mut self) {
        self.unregister_signal_handlers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/debug` 的 fs 失败经 panic hook 打印时必须给出 Node 的 `error.message`，而不是
    /// `Box<dyn Any>`（payload 不是 `String`/`&str` 时的旧占位符）。
    #[test]
    fn node_fs_panic_payload_renders_the_node_message() {
        let error = NodeFsError::new(
            "ENOSPC: no space left on device, write".to_string(),
            std::io::Error::from_raw_os_error(28),
        );
        let payload: Box<dyn std::any::Any + Send> = Box::new(error);
        assert_eq!(
            panic_payload_message(payload.as_ref()),
            "ENOSPC: no space left on device, write"
        );

        // 未知 payload 仍然使用占位符。
        let unknown: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(panic_payload_message(unknown.as_ref()), "Box<dyn Any>");
    }
}
