//! 进程信号接缝（Unix）：复刻 Node 的 `process.on` / `process.off` 语义。
//!
//! `tokio::signal::unix` 注册的 libc 处理器**永不注销**（tokio 文档明说），因此无法复刻
//! TS 里 `shutdown()` 开头 `process.off` 的两个可观察语义：
//!
//! 1. 移除最后一个监听器后 Node 恢复信号的默认处置：`shutdown` 在 `dispose()` 期间收到
//!    第二个 SIGTERM/SIGHUP 会被默认动作直接终止进程（父进程看到 `signal=SIGTERM`、
//!    `code=null`），而不是继续走处理器；
//! 2. 处理器在 `run_*_mode` 返回后不再存在（嵌入式/进程内调用方的信号不再被吞掉）。
//!
//! 这里直接用 `sigaction` + self-pipe：信号处理器只做 async-signal-safe 的单字节 `write`，
//! 异步任务读管道后执行各自的关停动作。注册返回 [`Registration`]（保存注册前的处置与管道
//! 两端）；[`restore`] 等价于 `process.off`（恢复注册前的 `sigaction`）。
//!
//! 已知差异：TS 允许同一信号有多个 `process.on` 监听器，这里每个信号号只有一个写端槽位，
//! 进程内同一时刻只支持一个注册。print 与 RPC 模式不会在同一进程里并存。
//!
//! ## 数据流
//!
//! ```text
//! 内核 --信号--> wake()（信号处理器，仅 write 1 字节）--> pipe 写端
//!                                                          |
//! 异步任务 <--read_wake_byte()-- AsyncFd(pipe 读端) <-------+
//! ```
//!
//! 处理器不碰任何 Rust 侧非 async-signal-safe 的状态：只读静态数组
//! `WAKE_FDS` 拿到 fd，然后调用 `libc::write`。真正的关停逻辑（dispose、
//! `process::exit`）全部在异步任务里执行，见 `print_mode.rs` / `rpc_mode.rs` 的
//! `register_signal_handlers`。
//!
//! ## 生命周期与不变量
//!
//! - `register` 的顺序是“先建管道、先发布写端 fd、再安装 `sigaction`”。这样处理器一旦
//!   被装上，槽位里必定已是有效 fd，不会出现“处理器已生效但槽位还是 -1、唤醒被丢弃”的
//!   窗口。失败路径（`sigaction` 返回非 0）会把槽位重置为 -1。
//! - 返回的 `Registration` 拥有管道两端。调用方必须把 `read_end` 交给
//!   `AsyncFd`（要求非阻塞，`wake_pipe` 已设置），并在异步任务里**持有 `write_end`**；
//!   任务若提前关闭写端，读端会读到 EOF。
//! - `restore` 等价于 `process.off`：**先**断开槽位，**再**还原注册前的处置，此后到达的
//!   信号按默认动作终止进程。已经触发过的处理器不受影响（它的任务已离开等待，会走完关停）。
//! - 同一信号号在同一时刻只能有一个注册（槽位单值）；重复 `register` 会覆盖槽位，
//!   前一个 `Registration` 的 `previous` 仍然可以用于 `restore`，但两者交错会破坏语义。
//!   两个调用方（print / RPC）每次运行都只对每个信号注册一次。
//!
//! ## 安全前提（unsafe）
//!
//! - `wake` 作为 `sa_sigaction` 安装，必须满足 async-signal-safe：本函数只有一次
//!   `libc::write` 和原子读，符合要求；不得在其中加锁、分配或调 Rust 集合操作。
//! - 处理器读到的 fd 可能已被关闭/复用（槽位与关闭之间无同步），因此 `write` 的结果被
//!   忽略：最坏情况是这次唤醒丢失，而唤醒本来就只是“可能已触发”的通知。
//! - 管道写端是非阻塞的，写入失败（例如管道已满）时 `write` 的返回值同样被忽略；
//!   `read_wake_byte` 读到任意一个字节（甚至 EOF）就返回，因此多次信号合并成一次可观察
//!   唤醒是可接受的——这正是“等待被触发”的语义所需。
//! - 本文件只在 Unix 编译（`lib.rs` 里 `#[cfg(unix)] mod signal;`）；非 Unix 平台两种模式
//!   都不注册处理器。

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use std::sync::atomic::{AtomicI32, Ordering};

/// 本单元用到的信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalKind {
    /// SIGTERM。
    Term,
    /// SIGHUP（非 win32）。
    Hup,
}

impl SignalKind {
    /// 信号编号。
    pub(crate) fn number(self) -> libc::c_int {
        match self {
            SignalKind::Term => libc::SIGTERM,
            SignalKind::Hup => libc::SIGHUP,
        }
    }
}

/// SIGHUP(1)/SIGTERM(15) 都落在这个范围内；槽位按信号号索引。
const WAKE_FD_SLOTS: usize = 32;

/// 每个信号号一个写端 fd（-1 = 未注册）。信号处理器只能访问静态数据。
static WAKE_FDS: [AtomicI32; WAKE_FD_SLOTS] = [const { AtomicI32::new(-1) }; WAKE_FD_SLOTS];

/// 信号处理器本体：只做 async-signal-safe 的单字节 `write`。
extern "C" fn wake(signal: libc::c_int) {
    let slot = signal as usize;
    if slot >= WAKE_FD_SLOTS {
        return;
    }
    // Relaxed 足够：处理器只需要拿到一个“可能有效”的 fd；清理先把槽位置 -1，
    // 任务随后关闭写端（最坏情况：一次写落到已关闭/复用的 fd，被内核丢弃）。
    let fd = WAKE_FDS[slot].load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte = 1u8;
    unsafe {
        libc::write(fd, std::ptr::addr_of!(byte).cast(), 1);
    }
}

/// 一次注册的全部产物：清理所需的旧处置 + 唤醒管道的两端。
pub(crate) struct Registration {
    /// 信号编号。
    pub(crate) signal_number: libc::c_int,
    /// 注册前的 `sigaction`（`process.off` 时还原）。
    pub(crate) previous: libc::sigaction,
    /// 管道读端（交给 `AsyncFd`）。
    pub(crate) read_end: OwnedFd,
    /// 管道写端（任务持有；清理不关闭它，避免“关闭 -> EOF”被误判为一次信号）。
    pub(crate) write_end: OwnedFd,
}

/// `process.on(signal, handler)`；失败时返回 `None`（TS 的 `process.on` 不会失败，
/// 这里与 tokio 信号源的失败路径一致：跳过该信号）。
pub(crate) fn register(kind: SignalKind) -> Option<Registration> {
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
    // 先发布写端再安装处理器：安装与发布之间到达的信号不会丢（处理器读到 fd）。
    WAKE_FDS[slot].store(write_end.as_raw_fd(), Ordering::SeqCst);
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(signal_number, &action, &mut previous) } != 0 {
        WAKE_FDS[slot].store(-1, Ordering::SeqCst);
        return None;
    }
    Some(Registration {
        signal_number,
        previous,
        read_end,
        write_end,
    })
}

/// `process.off` 的 OS 部分：恢复注册前的处置。
///
/// 已触发的处理器不受影响——它的任务已经离开监听循环，会继续执行各自的关停动作
/// （与 Node 的 `process.off` 不取消正在运行的监听器回调一致）。
pub(crate) fn restore(signal_number: libc::c_int, previous: libc::sigaction) {
    let slot = signal_number as usize;
    if slot >= WAKE_FD_SLOTS {
        return;
    }
    // 先断开处理器与写端的关联，再还原处置：还原之后到达的信号走默认动作。
    WAKE_FDS[slot].store(-1, Ordering::SeqCst);
    unsafe {
        libc::sigaction(signal_number, &previous, std::ptr::null_mut());
    }
}

/// 等信号处理器写入一个字节（或 EOF/读错误）。返回即表示“处理器已触发”。
///
/// 一次 `read` 使用 8 字节缓冲（信号处理器每次只写 1 字节，因此一次调用通常只取到其中
/// 一部分）；只要有任意结果就返回，不做字节计数。返回类型是 `()`：调用方无法区分“真的
/// 收到了信号字节”“读到 EOF”与“读失败”——三者都意味着等待结束。`would_block` 表示
/// readiness 已不再成立，循环回到 `readable().await` 重新等待。
pub(crate) async fn read_wake_byte(reader: &tokio::io::unix::AsyncFd<OwnedFd>) {
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
            // 读到 ≥1 字节（或 EOF）都是“读端已就绪”的终态。
            Ok(Ok(_)) => return,
            Ok(Err(_)) => return,
            Err(_would_block) => continue,
        }
    }
}

fn wake_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `pipe` 成功时两个 fd 都归本函数所有。
    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    // 两端都必须非阻塞：tokio 的 `AsyncFd` 要求 fd 处于非阻塞模式（其类型文档明说），
    // 而写端由信号处理器写，也可能因管道满被内核拒绝。CLOEXEC 防止 fork+exec 泄漏。
    for fd in [&read_end, &write_end] {
        set_nonblocking_cloexec(fd.as_raw_fd())?;
    }
    Ok((read_end, write_end))
}

/// 给 fd 同时设置 `O_NONBLOCK` 与 `FD_CLOEXEC`（各用一次 `F_GETFL`/`F_SETFL` 与
/// `F_GETFD`/`F_SETFD`）。任何一步失败都返回 `last_os_error()`，调用方（`wake_pipe`）
/// 随之丢弃已创建的 `OwnedFd`（`?` 让它们析构并关闭）。
fn set_nonblocking_cloexec(fd: libc::c_int) -> std::io::Result<()> {
    let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if status < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let descriptor = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, descriptor | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
