//! `core/output-guard.ts`：stdout 接管与“原始 stdout”写通道。
//!
//! TS: `packages/coding-agent/src/core/output-guard.ts`；契约见 `spec/ca-session.md` §2.11 与
//! `spec/ca-rpc-print.md` §2.8。
//!
//! ## 为什么 Rust 版是“输出抽象”而不是 patch `process.stdout.write`
//!
//! TS 把 `process.stdout.write` 替换成一个写 stderr 的包装，于是接管期间任何 `console.log`
//! 都进 stderr，而 `writeRawStdout`（协议字节）绕开包装直接写真实 fd 1。Rust 里 `println!`
//! 直接写 fd 1，无法被库替换，因此本模块提供等价的两条通道：
//!
//! - [`write_stdout`]：CLI 的普通输出。**未接管**时写真实 stdout；**已接管**时写 stderr。
//!   需要接管保护的普通输出应走它；但生产代码里目前只有 `kiana-coding-agent` 的
//!   `Output::write_stdout` 在直写 fd 失败时回退调用它（`cli/setup.rs`），
//!   `kiana-rpc-print` 只使用 raw 通道（`write_raw_stdout` / 背压 / flush / 接管），
//!   从不调用本函数。
//! - [`write_raw_stdout`]：协议字节。无论是否接管都写真实 stdout（fd 1），并串行化。
//!
//! 直接调用 `println!` / `std::io::stdout()` 的代码**不会**被重定向——这是与 TS 的固有差异
//! （TS 的接管依赖运行时替换 `process.stdout.write`），模块文档记录在此。
//!
//! ## 语义（逐条对齐 TS）
//!
//! - [`take_over_stdout`] 幂等：已接管直接返回；[`restore_stdout`] 幂等。
//! - [`write_raw_stdout`]：空串**直接丢弃**（不排入写队列）；非空串追加到串行链尾，调用方不等待
//!   （“发后不管”）。链上任何错误都会结束进程（TS 是 `process.exit(1)`；这里先
//!   `tracing::error!` 记录再 `exit(1)`）。
//! - 写入遇到 `ENOBUFS` / `EAGAIN` / `EWOULDBLOCK`（Rust 的 `WouldBlock` 或对应 errno）时每
//!   10ms 重试，无限重试；其它错误上抛给链尾处理。
//! - [`wait_for_raw_stdout_backpressure`] 复刻 TS 的“自旋直到链尾稳定”：先取链尾快照、排入一个
//!   屏障并等待它，然后重读链尾；若等待期间有新写入追加，就继续下一轮。快照与入队在同一把
//!   互斥锁内完成，“追加写入 + 推进链尾”也在同一把锁内，因此每次写入要么完全排在屏障之前
//!   （屏障会等到它）、要么完全在快照之后（下一轮会等到它）——这正是 TS 里
//!   `rawStdoutWriteTail = rawStdoutWriteTail.then(...)` 单步赋值的线性化语义。
//! - [`flush_raw_stdout`] 先等背压，再写一个空串并 flush（TS 的 `writeRawStdoutChunk("")` 用于
//!   驱动底层流的一次写回调；Rust 里显式 flush 等价）。
//!
//! ## 实现
//!
//! 一个惰性启动的后台线程拥有真实的 stdout：所有原始写入经 `mpsc` 队列串行执行，保证跨调用的
//! 字节顺序（TS 用 promise 链保证同一件事）。因为写线程是普通 OS 线程，本模块不要求调用方处于
//! tokio 运行时；只有两个 `await` 函数需要运行时（它们等待 `tokio::sync::oneshot` 确认）。
//!
//! 进程退出前必须调用 [`flush_raw_stdout`]（`kiana-rpc-print` 的 print/RPC 模式都会调用），否则
//! 队列里未执行的字节可能随进程退出丢失——TS 里 Node 会在正常退出时冲刷 stdout 流，Rust 没有
//! 等价的进程级钩子。这是本模块与 TS 的第二处固有差异。
//!
//! ## 调用方与退出纪律
//!
//! - `kiana-rpc-print`：`GlobalRawStdout` 把协议写入、背压、冲刷、接管全部委托到本模块；
//!   正常退出路径在退出前 `await` [`flush_raw_stdout`]；信号退出路径按 TS 契约跳过 flush
//!   （print 的 SIGTERM / SIGHUP、RPC 的 SIGTERM），见 `print_mode.rs` / `rpc_mode.rs`。
//! - `kiana-coding-agent`：`app.rs` 对“非交互且不是纯元数据命令”的模式调用
//!   [`take_over_stdout`]，print 模式结束后 [`restore_stdout`]；`cli/setup.rs` 的
//!   `Output::write_raw_stdout` / `flush` 也走本模块。
//! - `kiana-resources/src/package/exec.rs`：spawn 子进程前用 [`is_stdout_taken_over`] 决定
//!   子进程输出的落点（接管时 Unix 分支把 stdout/stderr 都接到 fd 2）。
//!
//! ## 顺序与并发
//!
//! - [`write_raw_stdout`] 与**未接管时**的 [`write_stdout`] 共用同一个写线程，进程内全局
//!   串行；同一线程内的调用顺序就是落盘顺序（`tests/output_guard.rs` 的 Phase 2 锁定每个写
//!   线程的内部顺序）。接管时 [`write_stdout`] 直接写 stderr，不经过写线程。
//! - `RAW_STDOUT_TAIL` 的“取快照 + 入队屏障”与“入队写入 + 递增”都在同一把互斥锁内完成，
//!   这是 [`wait_for_raw_stdout_backpressure`] 不会漏掉等待期间新写入的依据。
//! - [`take_over_stdout`] / [`restore_stdout`] 各由两个独立操作组成（原子标志与 TUI 钩子），
//!   二者不是原子的：另一个线程可能在切换窗口里看到中间状态。并发调用方需自行避免依赖
//!   该窗口的行为。
//! - 写线程只在所有 `Sender` 被丢弃后退出；本模块的静态 `Sender` 一直存活，因此线程生命周期
//!   等于进程生命周期（除非线程创建失败，见 `RAW_STDOUT` 的初始化）。

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

/// 背压重试间隔（TS: `RAW_STDOUT_RETRY_DELAY_MS = 10`）。
const RAW_STDOUT_RETRY_DELAY: Duration = Duration::from_millis(10);

/// 是否处于接管状态（TS 的模块级 `stdoutTakeoverState !== undefined`）。
static STDOUT_TAKEN_OVER: AtomicBool = AtomicBool::new(false);

/// 串行链尾的“身份”（TS 的模块级 `rawStdoutWriteTail`）。
///
/// TS 每次 `writeRawStdout` 都用单步赋值把链尾替换成新的 promise；`waitForRawStdoutBackpressure`
/// 比较 await 前后的引用身份。Rust 里用一个计数器表示链尾身份：每次非空写入在**同一个临界区**
/// 里入队并递增。读取快照与排入屏障也在同一个临界区，因此每次写入要么线性化在快照之前（屏障
/// 会等到它），要么在快照之后（下一轮自旋会等到它）。
static RAW_STDOUT_TAIL: Mutex<u64> = Mutex::new(0);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 锁内只做入队与计数；毒化只可能来自 OOM 级别的 panic，恢复内层数据。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 写线程的任务。
enum Job {
    /// 把一段 UTF-8 文本写到真实 stdout。
    Write(String),
    /// 写一个空串并 flush（TS 的 `flushRawStdout` 末尾），然后确认。
    Flush(tokio::sync::oneshot::Sender<()>),
    /// 纯屏障：此前排队的写入都完成后确认。
    Barrier(tokio::sync::oneshot::Sender<()>),
}

/// 原始 stdout 单例的写端：只保存 `Job` 队列的发送端，接收端归后台写线程所有。
///
/// 静态 `RAW_STDOUT` 由 `LazyLock` 持有：首次使用时创建通道并启动写线程。该结构没有其它
/// 状态；写入顺序完全由通道的 FIFO 与唯一接收者保证。
struct RawStdout {
    sender: Sender<Job>,
}

static RAW_STDOUT: LazyLock<RawStdout> = LazyLock::new(|| {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("kiana-raw-stdout".to_string())
        .spawn(move || writer_loop(receiver))
        // 线程创建失败意味着进程已无法正常工作；与 TS 的 `process.exit(1)` 路径一致。
        .expect("failed to spawn the raw stdout writer thread");
    RawStdout { sender }
});

/// `io::Error` 是否是可重试的背压错误：`WouldBlock`（EAGAIN/EWOULDBLOCK）或 Unix 的 `ENOBUFS`。
fn is_backpressure(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        if let Some(code) = error.raw_os_error() {
            // EAGAIN == EWOULDBLOCK 在 Linux/macOS 上同值，但两者都列出以对齐 TS 的错误码集合。
            if code == libc::ENOBUFS || code == libc::EAGAIN || code == libc::EWOULDBLOCK {
                return true;
            }
        }
    }
    false
}

/// 把全部字节写到真实 stdout，背压时每 10ms 重试。
///
/// 循环处理部分写（`write` 返回本次写出的字节数）；返回 0 视为 `WriteZero` 错误（与
/// `write_all` 的语义一致）。背压错误不终止循环，sleep 后从**剩余**字节继续。整个循环
/// 持有 `stdout` 的锁，因此单次调用写入的字节不会被进程内其它 `io::stdout()` 用户穿插。
fn write_all_to_stdout(mut data: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    while !data.is_empty() {
        match handle.write(data) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write the whole buffer to stdout",
                ));
            }
            Ok(written) => data = &data[written..],
            Err(error) if is_backpressure(&error) => {
                std::thread::sleep(RAW_STDOUT_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    handle.flush()
}

/// 写线程主循环：串行执行队列里的任务，错误时结束进程（TS 的 `.catch(() => process.exit(1))`）。
///
/// `mpsc` 保证 FIFO，因此任务按入队顺序执行；`recv` 只有在所有发送端被丢弃后才返回
/// `Err`，循环随之退出、线程结束（本模块的静态发送端不会消失，见 `RAW_STDOUT`）。
fn writer_loop(receiver: Receiver<Job>) {
    while let Ok(job) = receiver.recv() {
        match job {
            Job::Write(text) => {
                if let Err(error) = write_all_to_stdout(text.as_bytes()) {
                    fatal_stdout_error(&error);
                }
            }
            Job::Flush(acknowledge) => {
                if let Err(error) = write_all_to_stdout(b"").and_then(|()| io::stdout().flush()) {
                    fatal_stdout_error(&error);
                }
                let _ = acknowledge.send(());
            }
            Job::Barrier(acknowledge) => {
                let _ = acknowledge.send(());
            }
        }
    }
}

/// 记录并结束进程。TS 在链的 `catch` 里直接 `process.exit(1)`（不打印）；这里保留一条 tracing
/// 记录以便诊断，但同样以退出码 1 结束。
fn fatal_stdout_error(error: &io::Error) -> ! {
    tracing::error!("Failed to write to stdout: {error}");
    std::process::exit(1);
}

/// 排入一个原始写入（不等待完成）。
///
/// `send` 失败只可能发生在写线程已经退出（例如它 panic 后接收端被丢弃）；此时继续执行只会
/// 静默丢字节，所以按写错误处理：记录并 `exit(1)`。
fn enqueue(job: Job) {
    if RAW_STDOUT.sender.send(job).is_err() {
        fatal_stdout_error(&io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the raw stdout writer thread is gone",
        ));
    }
}

/// 接管 stdout：之后 [`write_stdout`] 写 stderr，[`write_raw_stdout`] 仍写真实 stdout。
/// 幂等。TS: `output-guard.ts#takeOverStdout`。
///
/// 同时给 [`kiana_tui::terminal::ProcessTerminal`] 安装写入钩子：TS 的接管替换的是
/// `process.stdout.write`，TUI 的字节因此也进 stderr；Rust 用钩子复刻同一效果。
///
/// 实现顺序是“先置原子标志，再安装钩子”。两步不是原子的，因此另一个线程在这两步之间
/// 可能看到“标志已接管、但 TUI 仍直接写 fd 1”的短暂状态（标志已置、钩子未装）；恢复时
/// 顺序相同——同样先清标志、再摘钩子，中间态是“标志已清、钩子未摘”，此时钩子查到标志
/// 已为 false，TUI 字节会写真实 stdout。本函数不关闭写线程，也不排空已有队列。
pub fn take_over_stdout() {
    STDOUT_TAKEN_OVER.store(true, Ordering::SeqCst);
    kiana_tui::terminal::set_stdout_write_hook(Some(write_tui_stdout_bytes));
}

/// 恢复 stdout（未接管时无操作，幂等）。TS: `output-guard.ts#restoreStdout`。
///
/// 只清除接管标志并移除 TUI 钩子；写线程与队列保持原样，已排队的原始字节仍会写出。
/// 与 [`take_over_stdout`] 一样，标志与钩子的切换不是原子操作，且顺序相同：先清标志、
/// 再摘钩子。中间态是“标志已清、钩子未摘”；此时钩子调用 [`is_stdout_taken_over`] 已返回
/// `false`，TUI 字节会写真实 stdout，随后才移除钩子。
pub fn restore_stdout() {
    STDOUT_TAKEN_OVER.store(false, Ordering::SeqCst);
    kiana_tui::terminal::set_stdout_write_hook(None);
}

/// TUI 输出通道（`ProcessTerminal::new()` 的 stdout 写入）：接管时写 stderr，否则写真实
/// stdout。TS 里接管把 `process.stdout.write` 换成 `rawStderrWrite`，未接管时是原始流；
/// 这里直接同步写，保持 TUI 帧的原子性（`ProcessTerminal::write_raw` 紧接着 flush）。
///
/// `ProcessTerminal` 只在 [`take_over_stdout`] 之后经过本函数（恢复时钩子被移除），因此
/// 未接管时的分支只用于防御。
///
/// 写入与 flush 的错误都被忽略：这个函数是 TUI 的字节下沉点，没有可回报错误的调用方
/// （TS 的 `process.stdout.write` 调用方同样不检查错误）。整段字节在一次调用里写完后立即
/// flush，调用返回时字节已交给操作系统。
pub fn write_tui_stdout_bytes(bytes: &[u8]) {
    if is_stdout_taken_over() {
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        let _ = handle.write_all(bytes);
        let _ = handle.flush();
    } else {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        let _ = handle.write_all(bytes);
        let _ = handle.flush();
    }
}

/// stdout 是否处于接管状态。TS: `output-guard.ts#isStdoutTakenOver`。
pub fn is_stdout_taken_over() -> bool {
    STDOUT_TAKEN_OVER.load(Ordering::SeqCst)
}

/// 普通输出通道：接管时写 stderr，否则写真实 stdout（**未接管时**与 [`write_raw_stdout`]
/// 共用串行链，保证两类输出之间的字节顺序）。
///
/// TS 里 `process.stdout.write` 的返回布尔背压信号与接管后写 stderr 的错误传播在这里都没有
/// 对应物：未接管时写入失败会按 [`write_raw_stdout`] 的规则结束进程；接管后直接写 stderr，
/// 错误被忽略（与 [`write_tui_stdout_bytes`] 的理由相同：没有可回报错误的调用方）。
/// 空串忽略（与 TS 的空写入一致：不产生字节）。
pub fn write_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    if is_stdout_taken_over() {
        // TS 接管时把 chunk 交给 rawStderrWrite；Rust 的 stderr 不缓冲。
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        let _ = handle.write_all(text.as_bytes());
        let _ = handle.flush();
        return;
    }
    write_raw_stdout(text);
}

/// 写协议字节到真实 stdout（绕过接管），不等待完成；空串直接丢弃。
/// TS: `output-guard.ts#writeRawStdout`。
///
/// 入队后立即返回，字节由后台写线程异步写出；返回时**不能**假设对端已经收到。需要确认
/// 已写出时 `await` [`wait_for_raw_stdout_backpressure`]，退出前 `await`
/// [`flush_raw_stdout`]。写线程遇到不可重试的 I/O 错误会直接 `exit(1)`。
pub fn write_raw_stdout(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut tail = lock(&RAW_STDOUT_TAIL);
    enqueue(Job::Write(text.to_string()));
    *tail += 1;
}

/// 自旋直到串行链尾稳定：等待调用时已排队的写入全部完成，且等待期间若有新写入追加（链尾身份
/// 变化）则继续等待。
/// TS: `output-guard.ts#waitForRawStdoutBackpressure`。
///
/// 每轮都会在链尾插入一个不写字节的屏障并等待它完成；调用方必须处于能驱动 async 的运行时
/// （等待的是 `tokio::sync::oneshot`）。若其它线程持续追加写入，本函数会一直自旋（与 TS 的
/// `while (tail !== rawStdoutWriteTail)` 一致），没有超时。
pub async fn wait_for_raw_stdout_backpressure() {
    loop {
        let (generation, receiver) = {
            let tail = lock(&RAW_STDOUT_TAIL);
            let generation = *tail;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            enqueue(Job::Barrier(sender));
            (generation, receiver)
        };
        if receiver.await.is_err() {
            fatal_stdout_error(&io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the raw stdout writer thread is gone",
            ));
        }
        if *lock(&RAW_STDOUT_TAIL) == generation {
            return;
        }
    }
}

/// 先等背压，再写一个空串并 flush，确保缓冲区里的字节被推出去。
/// TS: `output-guard.ts#flushRawStdout`。
///
/// 返回只表示字节已经交给操作系统（`Stdout` 句柄已 flush），不保证对端已读取、也不做
/// `fsync`。返回时，本函数入队 `Flush` 任务之前的所有写入都已完成；此后才追加的写入不在
/// 本次等待范围内。因此它是“进程退出前最后一件事”的推荐调用点。
pub async fn flush_raw_stdout() {
    wait_for_raw_stdout_backpressure().await;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    enqueue(Job::Flush(sender));
    if receiver.await.is_err() {
        fatal_stdout_error(&io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the raw stdout writer thread is gone",
        ));
    }
}
