//! `native/linux/src/{clipboard-worker.h,linux-platform-x11.c}` 的 Rust 等价物。
//!
//! 行为契约（`spec/tui-core.md` §2.16.3/§2.16.4）：
//!
//! - **一个私有工作线程**，忙时其它调用**立即返回不可用**（不排队、不阻塞事件循环）；
//! - 硬 deadline **3 秒**（Rust 侧是 `crate::terminal::sleep_for` 的线程 + waker 定时器，等价于
//!   `pthread_cond_timedwait`；**不使用** `tokio::time::timeout`——没有 Tokio time driver 时
//!   后者 poll 会 panic，见 `await_read` 的注释与测试
//!   `clipboard_futures_do_not_require_a_tokio_runtime`），
//!   transfer 层面的 2000 ms 错误优先上报；
//! - `MAX_CLIPBOARD_BYTES = 50 MiB`；
//! - 超时后到达的结果**丢弃**；
//! - 无 `DISPLAY` → `get_native_clipboard()` 返回 `None`（由父模块在**每次调用**时检查）。
//!
//! ## 实现映射
//!
//! | C（libxcb） | Rust |
//! |---|---|
//! | `pthread_cond_timedwait` + `worker_busy`/`waiting`/`finished` | `WorkerState` 的三个标志 + `tokio::sync::oneshot` |
//! | `xcb_poll_for_reply` / `xcb_poll_for_event` 的 deadline 循环 | `DeadlineStream`：把 2000 ms deadline 塞进 x11rb 的 `Stream::poll` |
//! | `xcb_connect` + `xcb_get_setup` | `parse_display` + `DefaultStream::connect` + `get_auth` + `RustConnection::connect_to_stream_with_auth_info` |
//! | `xcb_atom_t*` 强转（本机序） | `u32::from_ne_bytes`（x11rb 按连接字节序解析，与本机一致） |
//!
//! 三态语义与 `clipboard.h` 一致：`open`/连接失败 → `Unavailable`；selection 读取失败 →
//! `Err("Could not read X11 clipboard")`；没有收到属性（`data == NULL`，Rust 里 `data == None`）
//! → `Empty`；收到零长度属性 → 空值（`Latin1`/`Utf8`/`Buffer`），不是 `Empty`。
//!
//! ## 设计原因
//!
//! X11 的 selection 读取是一段同步握手：`ConvertSelection` → 等 `SelectionNotify` →
//! `GetProperty`，大内容还要多轮 INCR。TUI 事件循环不能被对端 owner（可能卡死）拖住，所以
//! 整段握手放在私有线程，调用方只拿一个 `oneshot` future。私有线程最多被等 3 秒；超时后
//! 结果丢弃，worker 要等线程真正结束才重新可用（`worker_busy` 语义）。2000 ms 的 X11 deadline
//! 与 3 秒的等待是两层独立预算：前者让“传输失败”先以 `Err("Could not read X11 clipboard")`
//! 上报，后者是最后兜底。
//!
//! `DeadlineStream` 把**同一个** 2000 ms deadline 叠在已经建立的 x11rb 连接上，因此 X11 协议
//! 握手（`connect_to_stream_with_auth_info` 的 setup 交换）、TARGETS 查询、
//! `SelectionNotify`/`PropertyNotify` 等待与 INCR 分块共享同一预算；超过之后任何
//! `Stream::poll` 都返回 `TimedOut`，x11rb 把它当连接错误，读取路径随之中止。底层 socket 的
//! 阻塞连接（`DefaultStream::connect`）发生在 `DeadlineStream` 构造之前，不会被该 deadline
//! 中断，只由 OS 的 connect 行为决定（它花掉的时间仍计入同一份预算，因为 deadline 的时间点
//! 在循环之前就已固定）；它若迟迟不返回，外层 3 秒等待只能让调用方先拿到 `Unavailable`，
//! 私有线程仍会卡在 connect 里、`busy` 保持置位。
//!
//! ## 执行流程
//!
//! 1. `get_text`/`get_image` 在**返回 future 之前**调用 `read_job` → `begin_read_with`：
//!    加锁检查 `ready`/`busy`，置 `busy`/`waiting`、递增 `epoch`，再 `spawn` 私有线程。
//!    因此“发起调用”本身就占住 worker；忙时直接得到 `Unavailable`。
//! 2. 私有线程执行 `read_clipboard`：`X11Clipboard::open`（连接 + 建窗 + intern atoms）→
//!    `preferred_target`（查 TARGETS，按表顺序选 target）→ `request_selection`（属性读取与
//!    INCR 累积）→ 归纳为 `ReadAttempt::{Unavailable, Value, Failed}`。
//! 3. 线程结束时若 `epoch` 未变：置 `finished`；若调用方已超时（`waiting == false`）则丢弃
//!    结果并清 `busy`，否则通过 `oneshot` 把结果交给等待者。
//! 4. `await_read` 让 `oneshot` 与 3 秒定时器竞争；之后再次校验 `epoch` 并复位
//!    `waiting`/`busy`。
//! 5. `text_value`/`image_value` 把 `ReadAttempt` 映射成三态或 `Err`。
//!
//! ## 注意事项
//!
//! - **单 worker、无队列**：`busy` 期间所有调用立即 `Unavailable`；这是 `clipboard-worker.h`
//!   的契约，不要改成排队或阻塞等待。
//! - **future 可能从未被 poll**：`ReadJob` 在 future 创建时就存在，Drop 时按 `epoch` 释放
//!   worker；没有它会让 `busy` 永久泄漏（见测试
//!   `dropping_an_unpolled_read_releases_the_worker`）。
//! - **线程 panic 会让 worker 永久忙碌**：panic 展开跳过 `begin_read_with` 线程闭包末尾的
//!   状态写回，`finished` 不置位、`busy` 一直为 `true`；当前 future 因 `oneshot` 发送端消失
//!   按 `Unavailable` 结束，之后该实例的每次读取也都立即 `Unavailable`（`is_ready()` 仍为
//!   `true`，因为 `ready` 标志没有被修改）。本模块没有重置/恢复路径；父模块 `native/mod.rs`
//!   的 `ClipboardWorker` 有同样的已知限制。
//! - **`epoch` 防止跨作业误清理**：旧作业的 Drop 或迟到线程只能修改属于自己 `epoch` 的状态。
//! - 锁中毒：`is_ready` 返回 `false`、`begin_read_with` 返回 `None`（都当作不可用）；线程收尾、
//!   `await_read` 与 `ReadJob::drop` 则用 `into_inner` 继续执行，不 panic。
//! - `MAX_CLIPBOARD_BYTES` 是**累积**上限（单块与 INCR 总量都受限）；超限走读取失败 `Err`，
//!   不是 `Empty`。
//! - 无 `DISPLAY` 的判定在父模块 `native::get_native_clipboard` 中**每次调用**执行；本模块
//!   `X11Clipboard::open` 内部再解析一次 `$DISPLAY`，直接构造本类型时同样适用。
//!
//! ## 阅读顺序
//!
//! `DeadlineStream`（超时如何注入 x11rb）→ `WorkerState`/`begin_read_with`/`ReadJob`/
//! `await_read`（线程与状态机）→ `X11Clipboard`（协议握手）→ `PropertyData`（属性校验与
//! INCR 合并）→ `text_value`/`image_value`（三态映射）→ 文件末尾的单元测试。

use std::io;
use std::os::unix::io::AsRawFd;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, WindowClass,
};
use x11rb::reexports::x11rb_protocol::parse_display::parse_display;
use x11rb::reexports::x11rb_protocol::xauth::get_auth;
use x11rb::rust_connection::{DefaultStream, PollMode, RustConnection, Stream};
use x11rb::utils::RawFdContainer;

use super::{ClipboardError, ClipboardFuture, ClipboardValue, NativeClipboard};

/// `MAX_CLIPBOARD_BYTES`：单次读取的**累积**上限（INCR 各分块之和也受限）。
const MAX_CLIPBOARD_BYTES: usize = 50 * 1024 * 1024;
/// `CLIPBOARD_TIMEOUT_MS`：一次读取的 X11 协议预算，X11 握手
/// （`connect_to_stream_with_auth_info`）、TARGETS 查询、回复等待与 INCR 分块共享它
/// （由 `DeadlineStream` 实施），不是每一步各自 2 秒。候选地址的阻塞
/// `DefaultStream::connect` 在 `DeadlineStream` 构造之前，不会被它中断，但阻塞耗时仍计入同一
/// 份预算（deadline 的时间点在循环之前固定）。
const CLIPBOARD_TIMEOUT_MS: u64 = 2000;

/// `clipboard-worker.h` 的 3 秒等待（“Allow the two-second transfer deadline to report its own
/// error first”）。
///
/// 与 `CLIPBOARD_TIMEOUT_MS` 是两层预算：2000 ms 的 X11 deadline 先让传输错误以 `Err` 上报，
/// 这里的 3 秒只兜底“线程完全卡死/连接不返回”的情况，超时结果是 `Unavailable`。
const WORKER_TIMEOUT: Duration = Duration::from_secs(3);
/// `XCB_ATOM_ATOM`：TARGETS 回复里元素的类型（atom 数组）。
const XCB_ATOM_ATOM: Atom = 4;
/// `XCB_ATOM_STRING`：ICCCM 定义的 `STRING` 文本类型，内容是 Latin-1。
const XCB_ATOM_STRING: Atom = 31;
/// `XCB_NONE`：X11 的无效 atom / “没有属性”哨兵。
const XCB_NONE: Atom = 0;

/// `text_types[]`。
///
/// **顺序即优先级**：`preferred_target` 按此顺序挑第一个被 owner 提供的 target，因此
/// `text/plain;charset=utf-8` 优先于 `UTF8_STRING`，`STRING` 最后。顺序必须与 C 源码一致
/// （`spec/tui-core.md` §2.16.4 特意强调过这一点）。
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];

/// `image_types[]`。顺序即优先级，规则与 `TEXT_TYPES` 相同。
const IMAGE_TYPES: [&str; 6] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
    "image/tiff",
];

// ---------------------------------------------------------------------------
// 带 deadline 的 x11rb stream
// ---------------------------------------------------------------------------

/// 在 x11rb 的 `Stream` 上叠加一个硬 deadline：`poll` 最多等到 `deadline`，超时返回
/// `TimedOut`（x11rb 会把它当作连接错误上报，等价于 C 里 `wait_for_input` 返回 false）。
///
/// 为什么需要它：`Stream::poll` 只承诺“阻塞到可读/可写或出错”，没有超时参数；x11rb 的连接
/// 握手、`get_property` 的回复等待和 `wait_for_event` 都会走到这里，所以在这一层加超时能
/// 同时覆盖所有阻塞点。deadline 在构造时固定，整个连接生命周期共享。
///
/// `read`/`write` 直接转发给内部的 `DefaultStream`：本类型只改变“等多久”，不改变字节搬运。
struct DeadlineStream {
    inner: DefaultStream,
    deadline: Instant,
}

impl Stream for DeadlineStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "X11 clipboard deadline exceeded",
                ));
            }
            let mut events: libc::c_short = 0;
            if mode.readable() {
                events |= libc::POLLIN;
            }
            if mode.writable() {
                events |= libc::POLLOUT;
            }
            let mut descriptor = libc::pollfd {
                fd: self.inner.as_raw_fd(),
                events,
                revents: 0,
            };
            let timeout = remaining.as_millis().max(1).min(i32::MAX as u128) as libc::c_int;
            // SAFETY: 单个 pollfd，指针与长度一致；fd 在 self 存活期间有效。
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
            if ready > 0 {
                // 不区分 revents：可读、可写与 POLLERR/POLLHUP 都交回上层，真正的错误由随后
                // 的 read/write 返回（与 x11rb 对 Stream 的用法一致）。
                return Ok(());
            }
            if ready == 0 {
                // 超时：回到循环顶部触发 deadline 检查。
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                // 信号打断不算超时，用剩余预算重试；否则会把 EINTR 误报成连接失败。
                continue;
            }
            return Err(error);
        }
    }

    fn read(&self, buf: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.inner.read(buf, fds)
    }

    fn write(&self, buf: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.inner.write(buf, fds)
    }
}

// ---------------------------------------------------------------------------
// 工作线程状态（clipboard-worker.h 的静态变量）
// ---------------------------------------------------------------------------

/// `worker_ready` / `worker_busy` / `worker_waiting` / `worker_finished` 的等价物。
///
/// 所有读写都在 `LinuxX11Clipboard::state` 的 `Mutex` 下；临界区只做标志位判断与赋值，
/// 不持锁执行 I/O（I/O 在私有线程里）。`is_ready`/`begin_read_with` 把锁中毒当作不可用；
/// 线程收尾、`await_read` 与 `ReadJob::drop` 则用 `into_inner` 继续，因为这里的状态只是几个
/// 布尔标志，不会出现半更新。
#[derive(Debug)]
struct WorkerState {
    /// `worker_ready`：工作线程可用。Rust 侧没有 `pthread_condattr_setclock` 失败路径
    /// （用 `Instant` + `crate::terminal::sleep_for` 的后台线程 + waker 定时器），构造后恒为
    /// `true`。
    ready: bool,
    /// `worker_busy`：私有线程正在读，其它调用立即返回 `Unavailable`。
    busy: bool,
    /// `worker_waiting`：调用方仍在等待（超时后置 `false`，迟到的结果被丢弃）。
    waiting: bool,
    /// `worker_finished`：私有线程已写完结果。
    finished: bool,
    /// 作业代号。C 里每个 job 由 `clipboard_execute` 独占，清理只可能属于当前 job；Rust 的
    /// future 可能被丢弃，旧作业的迟到清理（Drop/线程收尾）不能影响已经开始的新作业。
    /// 只有 `epoch` 与作业自身一致的清理才会修改 `busy`/`waiting`/`finished`。
    epoch: u64,
}

impl Default for WorkerState {
    fn default() -> Self {
        WorkerState {
            ready: true,
            busy: false,
            waiting: false,
            finished: false,
            epoch: 0,
        }
    }
}

/// `linux-platform-x11.node` 的等价物。
///
/// 一个实例代表**一个可复用的工作线程槽位**，不是一条 X11 连接：每次读取都由私有线程新建
/// 连接（`X11Clipboard::open`），线程结束即释放。本类型只持有共享状态
/// `Arc<Mutex<WorkerState>>`，可以跨线程共享；父模块用 `Arc` 把它缓存成进程级单例
/// （`native::linux_clipboard`）。
///
/// 构造不做任何 I/O，也不探测 `DISPLAY`。`is_ready()` 只有在锁中毒时才可能返回 `false`
/// （`ready` 标志本身没有置 false 的代码路径）。
pub struct LinuxX11Clipboard {
    state: Arc<Mutex<WorkerState>>,
}

impl Default for LinuxX11Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxX11Clipboard {
    /// `pthread_once(initialize_worker_condition)` 的等价物。
    ///
    /// 只初始化共享状态，不做 I/O、不探测 X server；构造后 `is_ready()` 为 `true`。
    pub fn new() -> Self {
        LinuxX11Clipboard {
            state: Arc::new(Mutex::new(WorkerState::default())),
        }
    }

    /// `worker_ready`：工作线程是否可用。锁中毒按“不可用”处理（不 panic，与三态语义一致）。
    pub fn is_ready(&self) -> bool {
        self.state.lock().map(|state| state.ready).unwrap_or(false)
    }

    /// `clipboard_execute` 的前半段：占住 worker，起私有线程执行一次读取。
    ///
    /// `None` 表示忙 / 不可用（TS 里 `job->format` 保持 `CLIPBOARD_UNAVAILABLE`）。
    ///
    /// 占位发生在**返回 future 之前**：TS 的 `getText()` 调用即执行 `queue_clipboard`，busy
    /// 判定在调用时刻完成；若把占位推迟到首次 poll，连续两次调用都会成功，破坏“忙时立即
    /// 不可用”的契约（见测试 `a_busy_worker_returns_unavailable_without_queueing`）。
    ///
    /// 线程 spawn 失败时回滚 `busy`/`waiting` 并返回 `None`，调用方据此得到 `Unavailable`。
    /// `epoch` 在锁内递增并随作业传递，使迟到的清理只能作用于本作业。
    fn begin_read_with<F>(&self, work: F) -> Option<(u64, oneshot::Receiver<ReadAttempt>)>
    where
        F: FnOnce() -> ReadAttempt + Send + 'static,
    {
        let epoch = {
            let mut state = self.state.lock().ok()?;
            if !state.ready || state.busy {
                return None;
            }
            state.epoch = state.epoch.wrapping_add(1);
            state.busy = true;
            state.waiting = true;
            state.finished = false;
            state.epoch
        };

        let shared = Arc::clone(&self.state);
        let (sender, receiver) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("kiana-tui-x11-clipboard".to_string())
            .spawn(move || {
                let attempt = work();
                let mut late = true;
                {
                    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                    if state.epoch == epoch {
                        state.finished = true;
                        // 调用方已超时：丢弃迟到结果并释放 worker。
                        late = !state.waiting;
                        if late {
                            state.busy = false;
                        }
                    }
                }
                if !late {
                    // 调用方丢弃了 future（等价于超时）：同样释放 worker，避免永久 busy。
                    if sender.send(attempt).is_err() {
                        let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                        if state.epoch == epoch {
                            state.busy = false;
                        }
                    }
                }
            });
        if spawned.is_err() {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.epoch == epoch {
                state.busy = false;
                state.waiting = false;
            }
            return None;
        }
        Some((epoch, receiver))
    }

    /// 一次读取的 future：`work` 在私有线程执行，最多等 3 秒，结果按三态映射。
    ///
    /// `work` 是 `Send + 'static`（要搬进线程）；`map` 是函数指针，在 future 被 poll 的线程上
    /// 执行，把 `ReadAttempt` 转成三态。`begin_read_with` 返回 `None` 时直接给一个已完成的
    /// `Unavailable` future，不创建线程。
    fn read_job<T, F>(
        &self,
        work: F,
        map: fn(ReadAttempt) -> Result<ClipboardValue<T>, ClipboardError>,
    ) -> ClipboardFuture<'_, T>
    where
        T: Send + 'static,
        F: FnOnce() -> ReadAttempt + Send + 'static,
    {
        match self.begin_read_with(work) {
            Some((epoch, receiver)) => {
                let state = Arc::clone(&self.state);
                // 守卫必须在 **future 创建时**就存在：调用方可能从未 poll 就丢弃 future，
                // 此时只有 Drop 能释放 worker（C 的 promise 总会 settle，不存在这种状态）。
                let job = ReadJob {
                    state: Arc::clone(&state),
                    epoch,
                };
                Box::pin(async move {
                    let _job = job;
                    map(await_read(&state, receiver, epoch).await)
                })
            }
            None => Box::pin(async { Ok(ClipboardValue::Unavailable) }),
        }
    }
}

impl NativeClipboard for LinuxX11Clipboard {
    /// 读取 X11 `CLIPBOARD` 的文本。
    ///
    /// 返回：worker 忙 / 无 `DISPLAY` / 连接失败 / 等待超时（3 秒）→ `Unavailable`；owner 没有
    /// 提供文本 target → `Empty`；读取失败 → `Err("Could not read X11 clipboard")`；成功时
    /// `STRING` 按 Latin-1 解码，其余按 UTF-8（非法序列替换为 U+FFFD）。
    fn get_text(&self) -> ClipboardFuture<'_, String> {
        self.read_job(|| read_clipboard(false), text_value)
    }

    /// 读取 X11 `CLIPBOARD` 的图片。三态语义与 `get_text` 相同，成功时返回原始字节
    /// （不做解码或格式转换）。
    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>> {
        self.read_job(|| read_clipboard(true), image_value)
    }
}

/// 一次读取的所有权守卫：future 被丢弃（未 poll 或等待中被丢弃）时释放 worker。
///
/// 没有它时，若调用方创建 future 后从不 poll 就丢弃，私有线程仍会 `send` 成功（接收端当时
/// 还活着），`waiting` 保持 `true` → `busy` 永远不被清除，之后所有读取都返回 `Unavailable`。
///
/// `epoch` 匹配时，Drop 总是清 `waiting`；只有 `finished` 为真时才同时清 `busy`——线程仍在
/// 运行时 `busy` 必须保持置位，否则会出现两个线程同时执行 X11 读取（见测试
/// `worker_wait_gives_up_after_three_seconds` 的断言）。`epoch` 不匹配（新作业已经开始）时
/// Drop 直接返回，不修改任何标志。
struct ReadJob {
    state: Arc<Mutex<WorkerState>>,
    epoch: u64,
}

impl Drop for ReadJob {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.epoch != self.epoch {
            return;
        }
        state.waiting = false;
        if state.finished {
            state.busy = false;
        }
    }
}

/// `clipboard_execute` 的后半段：最多等 3 秒；超时后结果丢弃、调用方拿到 `Unavailable`。
///
/// TS 的 promise 与 executor 无关；这里不用 `tokio::time::timeout`（没有 Tokio time driver
/// 时 poll 会 panic），改用 [`crate::terminal::sleep_for`] 的线程 + waker 定时器。
///
/// 两个事件源竞争，每次 poll 先看 `oneshot` 再看定时器：
/// - `oneshot` 就绪 → 采用结果；`Err` 表示发送端在 send 前消失（线程 panic），按
///   `Unavailable` 处理，future 仍然会 settle。但 panic 展开会跳过线程闭包末尾的状态写回：
///   `finished` 不会置位、`busy` 永久保持 `true`，该实例之后的读取全部立即 `Unavailable`
///   （`is_ready()` 仍返回 `true`），本模块没有重置路径（见模块文档“注意事项”）；
/// - 定时器就绪 → 用 `try_recv` 再取一次“刚好到达”的结果（对齐 C 中
///   `pthread_cond_timedwait` 返回 0 时取走结果的情形），取不到就是 `Unavailable`。
///
/// 无论哪条路径返回，都会在锁内按 `epoch` 复位 `waiting`；`finished` 为真才清 `busy`，
/// 保证私有线程还在跑时 worker 不会被第二次占用。
async fn await_read(
    shared: &Arc<Mutex<WorkerState>>,
    receiver: oneshot::Receiver<ReadAttempt>,
    epoch: u64,
) -> ReadAttempt {
    let mut receiver = receiver;
    let mut timer = crate::terminal::sleep_for(WORKER_TIMEOUT);
    let attempt = std::future::poll_fn(|context| {
        match Pin::new(&mut receiver).poll(context) {
            std::task::Poll::Ready(Ok(attempt)) => return std::task::Poll::Ready(attempt),
            std::task::Poll::Ready(Err(_)) => {
                return std::task::Poll::Ready(ReadAttempt::Unavailable);
            }
            std::task::Poll::Pending => {}
        }
        if Pin::new(&mut timer).poll(context).is_ready() {
            // TS：超时后即使结果刚好到达也不采用（`worker_finished` 为 false 时 job 保持空）；
            // 但若私有线程先完成，`pthread_cond_timedwait` 会返回 0 并取走结果。
            return std::task::Poll::Ready(match receiver.try_recv() {
                Ok(attempt) => attempt,
                Err(_) => ReadAttempt::Unavailable,
            });
        }
        std::task::Poll::Pending
    })
    .await;

    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
    if state.epoch == epoch {
        state.waiting = false;
        if state.finished {
            state.busy = false;
        }
    }
    drop(state);
    attempt
}

// ---------------------------------------------------------------------------
// X11 selection 读取
// ---------------------------------------------------------------------------

/// 读到的属性内容。
///
/// X11 属性带三类元数据：`format` 是每个元素的位数（只允许 8/16/32），`items` 是元素个数，
/// `length` 是这里累积的**字节数**；三者必须满足 `items * format / 8 == length` 才能跨块
/// 合并（见 [`PropertyData::append_property`]）。
///
/// `data`: `Some` 与 `None` 的含义不同（对应 C 的 `data == NULL`）：`None` 表示没有收到任何
/// 属性（映射为 `Empty`），`Some` 即使内容为空也表示“收到了属性”，会继续按 `type_` 判定
/// `Latin1`/`Utf8`/`Buffer`。
#[derive(Debug, Default)]
struct PropertyData {
    /// C 里 `data == NULL` 与“空但已分配”是两种状态：`None` 表示从未收到属性。
    data: Option<Vec<u8>>,
    length: usize,
    items: u32,
    format: u8,
    type_: Atom,
}

impl PropertyData {
    /// `append_bytes(result, bytes, length)`；超过 50 MiB 时返回 `false`。
    ///
    /// 上限按**累积后**的长度判断：本次 `bytes` 超过剩余额度就整块拒绝，不会部分写入。
    /// 即使 `bytes` 为空，也会把 `data` 从 `None` 变成 `Some(空)`（与 C 里 `realloc(0+1)`
    /// 总会得到非空指针一致），因此“零长度属性”不等于“没有属性”。
    fn append_bytes(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > MAX_CLIPBOARD_BYTES.saturating_sub(self.length) {
            return false;
        }
        let mut data = self.data.take().unwrap_or_default();
        data.extend_from_slice(bytes);
        self.length += bytes.len();
        self.data = Some(data);
        true
    }

    /// `append_property(result, chunk)`。
    ///
    /// 合并一个 INCR 分块；返回 `false` 表示**整次读取失败**（调用方不再使用已累积的数据）。
    /// 拒绝条件逐条对应 C 的实现，顺序即下面的检查顺序：
    /// 1. `format` 只能是 8/16/32（X11 属性的合法单位）；
    /// 2. `type == XCB_NONE` 的属性没有类型，无法参与合并；
    /// 3. `items * format / 8 == length`：分块自身的元数据必须自洽；
    /// 4. 非首块时 `type_`/`format` 必须与已累积的一致——INCR 分块属于同一个属性；
    /// 5. `items` 累加不得溢出 `u32`；
    /// 6. 累积字节数不得超过 50 MiB（由 [`PropertyData::append_bytes`] 判定）。
    ///
    /// 注意 `items == 0` 的终止块也要走完全部检查：元数据与之前不一致时，终止块同样判失败
    /// （测试 `validates_x11_incremental_property_metadata_before_accumulating_bytes` 的末尾）。
    fn append_property(&mut self, chunk: &PropertyData) -> bool {
        if !matches!(chunk.format, 8 | 16 | 32)
            || chunk.type_ == XCB_NONE
            || (chunk.items as usize) * (chunk.format as usize / 8) != chunk.length
            || (self.type_ != XCB_NONE
                && (self.type_ != chunk.type_ || self.format != chunk.format))
            || chunk.items > u32::MAX - self.items
        {
            return false;
        }
        if !self.append_bytes(chunk.data.as_deref().unwrap_or_default()) {
            return false;
        }
        self.type_ = chunk.type_;
        self.format = chunk.format;
        self.items += chunk.items;
        true
    }
}

/// 一次读取的最终格式（对应 `clipboard_job.format`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardFormat {
    /// `CLIPBOARD_EMPTY`（`undefined` 之外的另一态：`null`）。
    Empty,
    /// `CLIPBOARD_LATIN1`（X11 `STRING`）。
    Latin1,
    /// `CLIPBOARD_UTF8`。
    Utf8,
    /// `CLIPBOARD_BUFFER`（图片）。
    Buffer,
}

/// `read_clipboard()` 的结果。
#[derive(Debug)]
struct ReadResult {
    format: ClipboardFormat,
    data: Vec<u8>,
}

/// 私有线程的返回值：`Unavailable` 对应 `CLIPBOARD_UNAVAILABLE`，`Failed` 对应 `job->error`。
///
/// 3 秒等待超时**不在**这个枚举里：超时由 [`await_read`] 直接给出 `Unavailable`，迟到的
/// `Value`/`Failed` 被丢弃。两者在映射后都是 `undefined`，但到达路径不同。
#[derive(Debug)]
enum ReadAttempt {
    Unavailable,
    Value(ReadResult),
    Failed(ClipboardError),
}

/// `read_clipboard(job)`。
///
/// `image` 只影响 target 优先级（`IMAGE_TYPES` 还是 `TEXT_TYPES`）与格式归类，不影响连接流程。
///
/// 边界：`open` 失败（`$DISPLAY` 解析、连接、建窗、intern atom 任一失败）→ `Unavailable`；
/// 选择读取失败（含 2 秒 deadline 超时、超过 50 MiB、INCR 元数据不一致）→
/// `Failed("Could not read X11 clipboard")`；`contents.data == None`（没有 owner、owner 不提供
/// 所需 target）→ `Empty`；图片请求只要拿到属性就按 `Buffer` 处理。
fn read_clipboard(image: bool) -> ReadAttempt {
    let Some(clipboard) = X11Clipboard::open() else {
        return ReadAttempt::Unavailable;
    };
    let mut contents = PropertyData::default();
    if !clipboard.read_selection(image, &mut contents) {
        return ReadAttempt::Failed(ClipboardError::new("Could not read X11 clipboard"));
    }
    // 判定顺序有意义：先看有没有数据，再看是否为图片请求，最后才区分 X11 `STRING`
    // （ICCCM 里 `STRING` 是 Latin-1，其余文本 target 才是 UTF-8）。
    let format = match &contents.data {
        None => ClipboardFormat::Empty,
        Some(_) if image => ClipboardFormat::Buffer,
        Some(_) if contents.type_ == XCB_ATOM_STRING => ClipboardFormat::Latin1,
        Some(_) => ClipboardFormat::Utf8,
    };
    ReadAttempt::Value(ReadResult {
        format,
        data: contents.data.unwrap_or_default(),
    })
}

/// 文本 job 的三态映射：`null` → `Empty`，`STRING` → Latin-1，其余按 UTF-8。
///
/// `Failed` 映射为 `Err`（promise reject），与 `Unavailable`（`undefined`）严格区分；测试
/// `maps_failed_attempts_to_errors_and_unavailable_to_undefined` 锁定这一点。
///
/// Latin-1 分支把每个字节直接扩展成 `char`（U+0000..=U+00FF），不做 UTF-8 解码——X11 的
/// `STRING` 由 ICCCM 定义为 Latin-1，用 `from_utf8_lossy` 会破坏 `0x80..=0xFF`。
/// `Buffer` 落入 UTF-8 分支只是防御：文本请求不会产生它。
fn text_value(attempt: ReadAttempt) -> Result<ClipboardValue<String>, ClipboardError> {
    match attempt {
        ReadAttempt::Unavailable => Ok(ClipboardValue::Unavailable),
        ReadAttempt::Failed(error) => Err(error),
        ReadAttempt::Value(result) => Ok(match result.format {
            ClipboardFormat::Empty => ClipboardValue::Empty,
            ClipboardFormat::Latin1 => {
                ClipboardValue::Value(result.data.iter().map(|byte| char::from(*byte)).collect())
            }
            // `napi_create_string_utf8` 的非法序列 → U+FFFD。
            ClipboardFormat::Utf8 | ClipboardFormat::Buffer => {
                ClipboardValue::Value(String::from_utf8_lossy(&result.data).into_owned())
            }
        }),
    }
}

/// 图片 job：`Buffer` → `Uint8Array`，空 → `null`。
///
/// 只要收到了属性（`data == Some`）就原样返回字节，内容可以为空；`format` 归类
/// （`Buffer`/`Latin1`/`Utf8`）都接受，`Buffer` 是 `read_clipboard(true)` 的常规结果，另外两种
/// 是防御性覆盖。只有“完全没有属性”（`Empty`）才映射为 `null`。
fn image_value(attempt: ReadAttempt) -> Result<ClipboardValue<Vec<u8>>, ClipboardError> {
    match attempt {
        ReadAttempt::Unavailable => Ok(ClipboardValue::Unavailable),
        ReadAttempt::Failed(error) => Err(error),
        ReadAttempt::Value(result) => Ok(match result.format {
            ClipboardFormat::Empty => ClipboardValue::Empty,
            ClipboardFormat::Buffer | ClipboardFormat::Latin1 | ClipboardFormat::Utf8 => {
                ClipboardValue::Value(result.data)
            }
        }),
    }
}

/// `x11_clipboard`。
///
/// 一条 X11 连接 + 自建的事件窗口，只在私有线程内使用（本模块不把它跨线程共享），
/// 生命周期与一次 `read_clipboard` 相同，读完即随线程结束释放。
///
/// `property` 是接收 selection 数据的属性名（`KIANA_CLIPBOARD`）；`targets`/`incr` 是 intern
/// 后的 atom。结构体里的四个 atom（`clipboard`/`property`/`targets`/`incr`）在
/// [`X11Clipboard::open`] 时一次解析，任一失败即整体失败；目标类型表（`TEXT_TYPES`/
/// `IMAGE_TYPES`）的 atom 则在 [`X11Clipboard::preferred_target`] 中按需 `intern`（每次读取都
/// 重新 intern，文本回退还会再 intern `UTF8_STRING`）。
struct X11Clipboard {
    connection: RustConnection<DeadlineStream>,
    window: u32,
    clipboard: Atom,
    property: Atom,
    targets: Atom,
    incr: Atom,
}

impl X11Clipboard {
    /// `open_clipboard()`；任何失败都返回 `None`（调用方映射为 `Unavailable`）。
    ///
    /// 步骤（任一步失败都返回 `None`，不区分原因）：
    /// 1. `parse_display(None)` 解析 `$DISPLAY` 得到 screen 编号；未设置或格式非法即失败；
    /// 2. 遍历 `connect_instruction()` 给出的地址（Unix socket、TCP 等），对每个候选地址依次：
    ///    `DefaultStream::connect`（阻塞的 socket 连接）→ `get_auth` 取 MIT-MAGIC-COOKIE
    ///    （取不到就用空认证）→ 把 stream 包成 `DeadlineStream` → 用同一个 deadline 尝试
    ///    `RustConnection::connect_to_stream_with_auth_info`（X11 握手）。任一环节失败就继续
    ///    下一个地址；只有握手成功才保存连接并跳出循环，全部地址都失败才返回 `None`；
    /// 3. 在 root 上建一个 1x1 的输入窗口并订阅 `PropertyChange`——INCR 分块依赖
    ///    `PropertyNotify`；
    /// 4. intern `CLIPBOARD`/`KIANA_CLIPBOARD`/`TARGETS`/`INCR`，任一得到 `XCB_NONE` 即失败。
    ///
    /// `deadline` 在循环之前**一次性**设为 `now + 2000ms`，被循环里每一次成功建立的
    /// `DeadlineStream` 复用；它约束 X11 协议握手（`connect_to_stream_with_auth_info`）以及
    /// 此后在同一个连接上的读取步骤，多个候选地址共享同一份预算，前面地址耗时会吃掉后面的
    /// 额度。注意 `DefaultStream::connect` 发生在 `DeadlineStream` 构造之前，不会被该 deadline
    /// 中断，只由 OS 的 connect 行为决定；它的阻塞耗时仍计入同一份预算（deadline 的时间点在
    /// 循环之前固定）。它若长期阻塞，外层 3 秒等待只能让调用方先拿到 `Unavailable`，工作线程
    /// 仍会卡在 connect 里、`busy` 保持置位，直到 connect 返回、线程收尾才释放。
    fn open() -> Option<Self> {
        let deadline = Instant::now() + Duration::from_millis(CLIPBOARD_TIMEOUT_MS);
        let parsed = parse_display(None).ok()?;
        let screen = usize::from(parsed.screen);

        let mut connection = None;
        for address in parsed.connect_instruction() {
            let Ok((stream, (family, peer))) = DefaultStream::connect(&address) else {
                continue;
            };
            let (auth_name, auth_data) = get_auth(family, &peer, parsed.display)
                .ok()
                .flatten()
                .unwrap_or_default();
            let stream = DeadlineStream {
                inner: stream,
                deadline,
            };
            if let Ok(created) = RustConnection::connect_to_stream_with_auth_info(
                stream, screen, auth_name, auth_data,
            ) {
                connection = Some(created);
                break;
            }
        }
        let connection = connection?;

        let root = connection.setup().roots.get(screen)?.root;
        let window = connection.generate_id().ok()?;
        connection
            .create_window(
                x11rb::COPY_FROM_PARENT as u8,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .ok()?;

        let clipboard = intern_atom(&connection, "CLIPBOARD");
        let property = intern_atom(&connection, "KIANA_CLIPBOARD");
        let targets = intern_atom(&connection, "TARGETS");
        let incr = intern_atom(&connection, "INCR");
        if clipboard == XCB_NONE || property == XCB_NONE || targets == XCB_NONE || incr == XCB_NONE
        {
            return None;
        }

        Some(X11Clipboard {
            connection,
            window,
            clipboard,
            property,
            targets,
            incr,
        })
    }

    /// `read_property(clipboard, remove, result)`。
    ///
    /// 请求长度是 `MAX_CLIPBOARD_BYTES / 4`（`GetProperty` 的单位是 4 字节），意图一次取完
    /// 50 MiB 上限内的属性。以下条件**全部**满足才写入 `result`，否则返回 `false`：
    /// - `bytes_after == 0`：属性必须在一份回复里取完（INCR 的分块读取也走这里）；
    /// - `type_ != XCB_NONE`，且 `format` ∈ {8,16,32}；
    /// - 计算出的字节数 `value_len * (format/8)` 不超过 50 MiB，且不超过 `reply.length * 4`
    ///   （回复里的 `length` 以 4 字节为单位）；
    /// - `reply.value` 的实际长度不少于 `length`（防御被截断的回复）。
    ///
    /// 成功时**覆盖** `result` 的元数据（而不是累加），再用 `append_bytes` 追加数据。现有调用点
    /// 传入的都是空 `PropertyData`，真正的跨块合并由 INCR 循环里的 `append_property` 完成。
    ///
    /// 单位换算容易出错：`reply.length` 是 X11 的 4 字节单位，`value_len` 是元素个数，
    /// `format` 是每个元素的位数，三者不能直接比较。
    ///
    /// `remove` 只在 INCR 分块读取时为真：读取后属性即被删除。
    fn read_property(&self, remove: bool, result: &mut PropertyData) -> bool {
        let Ok(cookie) = self.connection.get_property(
            remove,
            self.window,
            self.property,
            AtomEnum::ANY,
            0,
            (MAX_CLIPBOARD_BYTES / 4) as u32,
        ) else {
            return false;
        };
        let Ok(reply) = cookie.reply() else {
            return false;
        };
        let valid_format = matches!(reply.format, 8 | 16 | 32);
        let length = u64::from(reply.value_len) * u64::from(reply.format / 8);
        if reply.bytes_after == 0
            && reply.type_ != XCB_NONE
            && valid_format
            && length <= MAX_CLIPBOARD_BYTES as u64
            && length <= u64::from(reply.length) * 4
            && reply.value.len() as u64 >= length
        {
            result.type_ = reply.type_;
            result.format = reply.format;
            result.items = reply.value_len;
            return result.append_bytes(&reply.value[..length as usize]);
        }
        false
    }

    /// `wait_for_event(clipboard, XCB_SELECTION_NOTIFY)`。
    ///
    /// 连接上可能混有其它窗口/其它请求的事件，不属于该类的事件直接跳过；`wait_for_event`
    /// 返回错误（包括 `DeadlineStream` 的 2000 ms 超时）时返回 `None`。
    fn wait_for_selection_notify(&self) -> Option<x11rb::protocol::xproto::SelectionNotifyEvent> {
        // C 里每次等待前都 `xcb_flush`；x11rb 的事件等待不会自己 flush。
        if self.connection.flush().is_err() {
            return None;
        }
        loop {
            match self.connection.wait_for_event() {
                Ok(Event::SelectionNotify(event)) => return Some(event),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    /// `wait_for_event(clipboard, XCB_PROPERTY_NOTIFY)`。
    ///
    /// 与 `wait_for_selection_notify` 相同：跳过无关事件，错误/超时返回 `None`。
    fn wait_for_property_notify(&self) -> Option<x11rb::protocol::xproto::PropertyNotifyEvent> {
        if self.connection.flush().is_err() {
            return None;
        }
        loop {
            match self.connection.wait_for_event() {
                Ok(Event::PropertyNotify(event)) => return Some(event),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    /// `request_selection(clipboard, target, result)`。
    ///
    /// 一次完整的选择转换，阶段如下：
    /// 1. 清空 `result`、删除旧属性（避免读到上一次请求的残留），再 `convert_selection`；
    /// 2. 循环等 `SelectionNotify`，只接受 `selection == CLIPBOARD` 且 `target` 匹配的事件；
    /// 3. `property == XCB_NONE` 表示没有 owner 或 owner 不提供该 target：返回 `true`，但
    ///    `result` 保持空（上层映射为 `Empty`）；
    /// 4. 否则 `GetProperty` 读一次；若类型是 `INCR`，丢弃这份内容并进入分块循环；
    /// 5. 分块循环只处理 `atom == property` 且 `state == NEW_VALUE` 的 `PropertyNotify`，
    ///    每块以 `remove=true` 读取并 `append_property`；`items == 0` 的零长度块表示结束。
    ///
    /// 任一等待/读取失败或分块元数据不一致 → `false`（上层转成
    /// `Err("Could not read X11 clipboard")`）。
    fn request_selection(&self, target: Atom, result: &mut PropertyData) -> bool {
        *result = PropertyData::default();
        // 先删掉旧属性：避免把上一次请求的残留当成这次的结果。
        let _ = self.connection.delete_property(self.window, self.property);
        let _ = self.connection.convert_selection(
            self.window,
            self.clipboard,
            target,
            self.property,
            x11rb::CURRENT_TIME,
        );

        loop {
            let Some(event) = self.wait_for_selection_notify() else {
                return false;
            };
            let matches = event.selection == self.clipboard && event.target == target;
            let received = event.property != XCB_NONE;
            if !matches {
                // 同一连接上可能有其它窗口/其它 selection 的事件，只认本次请求。
                continue;
            }
            if !received {
                // 没有 owner，或 owner 不提供该 target。
                return true;
            }
            if !self.read_property(false, result) {
                return false;
            }
            break;
        }

        if result.type_ != self.incr {
            return true;
        }

        // 首份回复只是 INCR 通知（真正的数据随后分块到达），丢弃它的内容重新累积。
        *result = PropertyData::default();
        let _ = self.connection.delete_property(self.window, self.property);

        loop {
            let Some(event) = self.wait_for_property_notify() else {
                return false;
            };
            if event.atom != self.property || event.state != Property::NEW_VALUE {
                // 只处理我们属性上的 NewValue；其它属性/删除事件跳过。
                continue;
            }

            let mut chunk = PropertyData::default();
            if !self.read_property(true, &mut chunk) {
                return false;
            }
            let appended = result.append_property(&chunk);
            let finished = chunk.items == 0;
            if !appended {
                return false;
            }
            if finished {
                return true;
            }
        }
    }

    /// `preferred_target(clipboard, image, target)`：返回要请求的 atom。
    ///
    /// `Some(XCB_NONE)` 表示 TS 里“返回 true 但 target 仍是 `XCB_NONE`”的分支（`read_selection`
    /// 会直接接受空结果）。
    ///
    /// 选择顺序：先把目标类型表全部 intern（任一失败 → `None`），再请求 `TARGETS`；随后按
    /// **表顺序**（不是 owner 返回的顺序）找第一个被提供的 atom。`TEXT_TYPES` 里
    /// `text/plain;charset=utf-8` 优先于 `UTF8_STRING`，`STRING` 排在最后。扫描前会校验
    /// TARGETS 回复：类型必须是 `ATOM`、format 为 32、`items == length / 4` 且 `length`
    /// 可被 4 整除。
    ///
    /// 回退规则：
    /// - 文本请求且 owner 拒绝 TARGETS（`type_ == XCB_NONE`）→ 直接试 `UTF8_STRING`（有些
    ///   owner 不实现 TARGETS 但提供文本）；
    /// - TARGETS 合法或为空 → `Some(XCB_NONE)`：确认“没有可用内容”，上层按 `Empty` 处理且
    ///   不发起读取；
    /// - 请求失败、TARGETS 元数据非法、intern 失败 → `None`（上层按读取失败处理）。
    fn preferred_target(&self, image: bool) -> Option<Atom> {
        let types: &[&str] = if image { &IMAGE_TYPES } else { &TEXT_TYPES };
        let mut wanted = Vec::with_capacity(types.len());
        for name in types {
            let atom = self.intern(name);
            if atom == XCB_NONE {
                return None;
            }
            wanted.push(atom);
        }

        let mut targets = PropertyData::default();
        let received = self.request_selection(self.targets, &mut targets);
        let valid = targets.type_ == XCB_ATOM_ATOM
            && targets.format == 32
            && u64::from(targets.items) == targets.length as u64 / 4
            && targets.length % 4 == 0;
        if received && valid {
            let offered = targets.data.as_deref().unwrap_or_default();
            for wanted_atom in &wanted {
                for index in 0..targets.items as usize {
                    let start = index * 4;
                    let Some(bytes) = offered.get(start..start + 4) else {
                        continue;
                    };
                    // xcb 的 `xcb_atom_t*` 强转：本机字节序。
                    if u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == *wanted_atom
                    {
                        return Some(*wanted_atom);
                    }
                }
            }
        }

        // 有些 owner 拒绝 TARGETS 但直接提供文本。
        if received && !image && targets.type_ == XCB_NONE {
            let atom = self.intern("UTF8_STRING");
            return if atom != XCB_NONE { Some(atom) } else { None };
        }
        if received && (valid || targets.type_ == XCB_NONE) {
            return Some(XCB_NONE);
        }
        None
    }

    /// `read_selection(clipboard, image, result)`。
    ///
    /// `preferred_target` 返回 `XCB_NONE` 时直接视为成功并保持空结果（`Empty`），不发起
    /// `ConvertSelection`。失败时把 `result` 清空，避免调用方把半截数据当结果。
    fn read_selection(&self, image: bool, result: &mut PropertyData) -> bool {
        let Some(target) = self.preferred_target(image) else {
            *result = PropertyData::default();
            return false;
        };
        let received = target == XCB_NONE || self.request_selection(target, result);
        if !received {
            *result = PropertyData::default();
        }
        received
    }

    /// `intern_atom(clipboard, name)`。
    fn intern(&self, name: &str) -> Atom {
        intern_atom(&self.connection, name)
    }
}

/// `intern_atom(clipboard, name)`：失败返回 `XCB_NONE`。
///
/// `only_if_exists = false`：atom 不存在时由 server 创建；读取路径只用已知名字，创建与否不
/// 影响语义。`XCB_NONE`（0）是 X11 的“无效 atom”，调用方据此判定失败。
fn intern_atom(connection: &RustConnection<DeadlineStream>, name: &str) -> Atom {
    let Ok(cookie) = connection.intern_atom(false, name.as_bytes()) else {
        return XCB_NONE;
    };
    cookie.reply().map(|reply| reply.atom).unwrap_or(XCB_NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `clipboard-x11-test.c` 的 `metadata` 模式：INCR 分块的 format/type/items/length 必须与首块一致才累积；不一致说明传输损坏，必须丢弃。
    #[test]
    fn validates_x11_incremental_property_metadata_before_accumulating_bytes() {
        let atom = XCB_ATOM_STRING;
        let mut chunk = PropertyData {
            data: Some(atom.to_ne_bytes().to_vec()),
            length: std::mem::size_of::<Atom>(),
            items: 1,
            format: 32,
            type_: XCB_ATOM_ATOM,
        };
        let mut result = PropertyData::default();
        assert!(result.append_property(&chunk));
        assert!(result.append_property(&chunk));
        assert_eq!(result.length, 2 * std::mem::size_of::<Atom>());
        assert_eq!(result.items, 2);

        // format/items/length 不一致。
        chunk.format = 8;
        chunk.items = std::mem::size_of::<Atom>() as u32;
        assert!(!result.append_property(&chunk));
        // type 不一致。
        chunk.format = 32;
        chunk.items = 1;
        chunk.type_ = XCB_ATOM_STRING;
        assert!(!result.append_property(&chunk));
        // items * format / 8 != length。
        chunk.type_ = XCB_ATOM_ATOM;
        chunk.length -= 1;
        assert!(!result.append_property(&chunk));
        assert_eq!(result.length, 2 * std::mem::size_of::<Atom>());
        assert_eq!(result.items, 2);

        // 终止块也必须与之前的元数据一致。
        chunk.length = 0;
        chunk.items = 0;
        chunk.format = 8;
        assert!(!result.append_property(&chunk));
        chunk.format = 32;
        assert!(result.append_property(&chunk));
    }

    /// `XCB_NONE` 类型与非法 format 一律拒绝（`chunk.type == XCB_NONE` 分支）。
    #[test]
    fn rejects_properties_without_a_type_or_with_an_invalid_format() {
        let mut result = PropertyData::default();
        let none = PropertyData {
            data: Some(vec![1, 2, 3, 4]),
            length: 4,
            items: 1,
            format: 32,
            type_: XCB_NONE,
        };
        assert!(!result.append_property(&none));
        let bad_format = PropertyData {
            data: Some(vec![1, 2, 3]),
            length: 3,
            items: 3,
            format: 24,
            type_: XCB_ATOM_STRING,
        };
        assert!(!result.append_property(&bad_format));
        assert!(result.data.is_none());
    }

    /// `MAX_CLIPBOARD_BYTES`（50 MiB）按累积长度判定，不分配超限缓冲。
    #[test]
    fn caps_accumulated_bytes_at_fifty_mebibytes() {
        let mut result = PropertyData::default();
        assert!(result.append_bytes(&[0u8; 4]));
        result.length = MAX_CLIPBOARD_BYTES - 4;
        assert!(!result.append_bytes(&[0u8; 5]));
        assert!(result.append_bytes(&[0u8; 4]));
        assert_eq!(result.length, MAX_CLIPBOARD_BYTES);
    }

    /// `text_value`/`image_value`：`Failed` 变成 `Err`（promise reject），不是 `Unavailable`。
    #[test]
    fn maps_failed_attempts_to_errors_and_unavailable_to_undefined() {
        assert_eq!(
            text_value(ReadAttempt::Unavailable),
            Ok(ClipboardValue::Unavailable)
        );
        assert_eq!(
            image_value(ReadAttempt::Unavailable),
            Ok(ClipboardValue::Unavailable)
        );
        assert_eq!(
            text_value(ReadAttempt::Failed(ClipboardError::new(
                "Could not read X11 clipboard"
            ))),
            Err(ClipboardError::new("Could not read X11 clipboard"))
        );
        assert_eq!(
            text_value(ReadAttempt::Value(ReadResult {
                format: ClipboardFormat::Empty,
                data: Vec::new(),
            })),
            Ok(ClipboardValue::Empty)
        );
    }

    /// X11 `STRING` 按 Latin-1 解码（与 `napi_create_string_latin1` 一致）。
    #[test]
    fn decodes_latin1_string_bytes() {
        assert_eq!(
            text_value(ReadAttempt::Value(ReadResult {
                format: ClipboardFormat::Latin1,
                data: vec![b'c', b'a', b'f', 0xe9, b' ', 0xa3, 0xff],
            })),
            Ok(ClipboardValue::Value("café £ÿ".to_string()))
        );
    }

    /// `clipboard-worker.h` 的 3 秒等待：超时 → `Unavailable`（不是错误），且 `waiting` 复位。
    #[tokio::test]
    async fn worker_wait_gives_up_after_three_seconds() {
        let shared = Arc::new(Mutex::new(WorkerState {
            ready: true,
            busy: true,
            waiting: true,
            finished: false,
            epoch: 1,
        }));
        let (sender, receiver) = oneshot::channel::<ReadAttempt>();
        let started = Instant::now();
        let attempt = await_read(&shared, receiver, 1).await;
        let elapsed = started.elapsed();

        assert!(matches!(attempt, ReadAttempt::Unavailable));
        assert!(
            elapsed >= WORKER_TIMEOUT - Duration::from_millis(50),
            "the wait must last the full 3 seconds, got {elapsed:?}"
        );
        assert!(
            elapsed < WORKER_TIMEOUT + Duration::from_secs(2),
            "the wait must be bounded, got {elapsed:?}"
        );
        let state = shared.lock().unwrap();
        assert!(!state.waiting);
        assert!(state.busy, "the worker is still running, so busy stays set");
        drop(sender); // 迟到的结果被丢弃。
    }

    /// 调用方创建 future 后从不 poll 就丢弃：私有线程完成时 `send` 成功（接收端当时还在），
    /// 若没有 Drop 清理，`waiting` 会一直为 `true`、`busy` 永不清除，后续读取全部变成
    /// `Unavailable`（C 里 promise 总会 settle，不存在这个状态）。
    #[test]
    fn dropping_an_unpolled_read_releases_the_worker() {
        let clipboard = LinuxX11Clipboard::new();
        let future = clipboard.read_job(|| ReadAttempt::Unavailable, text_value);

        // 等私有线程完成（结果已 send，但 future 从未 poll）。
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if clipboard
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .finished
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            clipboard
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .finished,
            "the private worker must have finished"
        );

        drop(future);

        let state = clipboard
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(
            !state.busy,
            "dropping an unpolled read must release the worker"
        );
        assert!(!state.waiting);
        drop(state);

        assert!(
            clipboard
                .begin_read_with(|| ReadAttempt::Unavailable)
                .is_some(),
            "a later read must not see a leaked busy worker"
        );
    }
}
