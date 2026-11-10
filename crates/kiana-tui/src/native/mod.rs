//! 原生平台层：`native/{napi.h,clipboard.h}`、`native/{linux,darwin,win32}` 与
//! `src/{native-module-path.ts,native-platform.ts,native-modifiers.ts}` 的 Rust 等价物。
//!
//! TS 侧通过 N-API 动态加载 `.node` 预编译产物；Rust 是单一二进制，不需要模块加载器，
//! 平台后端改为 `#[cfg]` + 可选 feature 编译进 crate（决策见 `ARCHITECTURE.md` §8 第 17 条与
//! `spec/GAPS.md` G1.5 的方案 (a)）。
//!
//! 三态语义（**必须保留**，来自 `native/clipboard.h` 的 `complete_clipboard_work`）：
//!
//! | TS 值 | Rust | 含义 |
//! |---|---|---|
//! | `undefined` | [`ClipboardValue::Unavailable`] | 平台不支持 / helper 未加载 |
//! | `null` | [`ClipboardValue::Empty`] | 支持但剪贴板为空 |
//! | 值 | [`ClipboardValue::Value`] | 读到内容 |
//! | promise reject | `Err(`[`ClipboardError`]`)` | 传输失败（`"Clipboard operation cancelled"` 等逐字消息） |
//!
//! 缓存的语义：缓存的是**后端是否可用**，不是 `DISPLAY` 的可用性——Linux 的
//! `DISPLAY` 每次调用重新检查（`spec/tui-core.md` §2.17.2 的测试 4）。
//!
//! # 设计原因
//!
//! TS 用 N-API 在运行期加载 `.node`，因此需要一个“加载失败 → `undefined`”的降级面；Rust
//! 把后端编译进二进制后，这个降级面必须由 `#[cfg]` 与 feature 显式构造出来：没有编译后端的
//! 目标上，[`get_native_platform_helper`] 与 [`get_native_clipboard`] 返回 `None`，修饰键
//! 查询恒为 `false`。调用方（例如 `kiana-core` 的剪贴板层）据此走“平台没有原生剪贴板”的
//! 分支，而不是把 `Unavailable` 当成“剪贴板为空”。
//!
//! # 一次调用经过哪些层
//!
//! 以 darwin/win32 的 `get_text` 为例：
//!
//! 1. [`get_native_clipboard`] 返回进程级缓存的 `Arc<dyn NativeClipboard>`；
//! 2. 方法内部由 `ClipboardWorker` 先占位（忙则立即得到 [`ClipboardValue::Unavailable`]），
//!    再在私有线程上执行平台调用；
//! 3. 返回的 future 同时轮询结果通道与 3 秒定时器，超时结果为 `Unavailable`。
//!
//! Linux 的 `x11` 后端不经过 `ClipboardWorker`：它有一份同构的 worker 实现（见 `linux_x11`
//! 模块文档），三态语义与超时行为相同。
//!
//! # 注意事项
//!
//! - [`NativeClipboard`] 的方法返回的 future 不依赖 Tokio runtime：定时器是
//!   `crate::terminal::sleep_for`（后台线程 + waker），因此可以在普通线程里用 noop waker
//!   轮询。不要把这些 future 改成 `tokio::time::timeout`——后者在没有 Tokio time driver 时
//!   一 poll 就 panic。
//! - 忙时得到的是 `Ok(Unavailable)` 而不是 `Err`：TS 的 `worker_busy` 让 promise resolve 成
//!   `undefined`，只有真正的传输失败才 reject。
//! - 调用方法本身就会占住 worker（占位在返回 future 之前完成），拿到 future 后不 poll 也一样；
//!   future 被丢弃时由内部的作业守卫释放。
//! - `ClipboardWorker` 的工作线程若在写回状态前 panic，`busy` 不会被清除，该实例之后的调用
//!   都会立即得到 `Unavailable`；本文件没有重置它的兜底路径。

use std::sync::{Arc, OnceLock};

/// `ModifierKey`（`native-platform.ts`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifierKey {
    Shift,
    Command,
    Control,
    Option,
}

impl ModifierKey {
    /// TS 的字符串拼写（`"shift"`/`"command"`/`"control"`/`"option"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            ModifierKey::Shift => "shift",
            ModifierKey::Command => "command",
            ModifierKey::Control => "control",
            ModifierKey::Option => "option",
        }
    }
}

/// 剪贴板读取的三态结果（见模块文档的表）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ClipboardValue<T> {
    /// TS `undefined`：后端不可用。
    #[default]
    Unavailable,
    /// TS `null`：后端可用但剪贴板没有该类型的内容。
    Empty,
    /// TS 的值。
    Value(T),
}

impl<T> ClipboardValue<T> {
    /// TS 的 `?? null` 之后的视图：`Unavailable` 与 `Empty` 都是 `None`。
    pub fn into_option(self) -> Option<T> {
        match self {
            ClipboardValue::Value(v) => Some(v),
            ClipboardValue::Unavailable | ClipboardValue::Empty => None,
        }
    }
}

/// 剪贴板操作失败。`Display` 逐字等于 TS `reject(new Error(message))` 的 message。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ClipboardError {
    /// 完整错误文案；构造后不再加工。
    pub message: String,
}

impl ClipboardError {
    /// 用调用方给出的完整文案构造；错误文案是跨实现契约，调用方不要自行加前缀。
    pub fn new(message: impl Into<String>) -> Self {
        ClipboardError {
            message: message.into(),
        }
    }

    /// `complete_clipboard_work` 里 `status != 0` 的逐字文案。
    pub fn cancelled() -> Self {
        Self::new("Clipboard operation cancelled")
    }

    /// `queue_clipboard` 创建 N-API 值失败时的逐字文案。
    pub fn could_not_create_value() -> Self {
        Self::new("Could not create clipboard value")
    }
}

/// 盒装 future（TUI 单线程也允许跨线程调用，故 `+ Send`）。
pub type ClipboardFuture<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<ClipboardValue<T>, ClipboardError>> + Send + 'a>,
>;

/// `NativeClipboard`（`native-platform.ts`）：`getText`/`getImage` 必备，
/// `getFilePaths`/`setText` 仅部分平台导出。
pub trait NativeClipboard: Send + Sync {
    /// `getText(): Promise<string | null | undefined>`。
    fn get_text(&self) -> ClipboardFuture<'_, String>;

    /// `getImage(): Promise<Uint8Array | null | undefined>`。
    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>>;

    /// `getFilePaths?(): Promise<string[] | null | undefined>`。默认不支持（TS 只在 darwin 导出）。
    fn get_file_paths(&self) -> Option<ClipboardFuture<'_, Vec<String>>> {
        None
    }

    /// `setText?(text): Promise<void>`。默认不支持（TS 只在 darwin/win32 导出）。
    fn set_text(&self, _text: String) -> Option<ClipboardFuture<'_, ()>> {
        None
    }

    /// `enableVirtualTerminalInput?(): boolean`（Win32）。默认 `false`。
    fn enable_virtual_terminal_input(&self) -> bool {
        false
    }

    /// `isModifierPressed?(name): boolean`。默认 `false`。
    fn is_modifier_pressed(&self, _key: ModifierKey) -> bool {
        false
    }
}

/// 共享句柄（`getNativeClipboard()` 的返回类型）。
pub type SharedNativeClipboard = Arc<dyn NativeClipboard>;

/// 后端实例的缓存：缓存的是“后端是否可用”，不是 `DISPLAY` 的可用性。
///
/// `OnceLock` 只初始化一次：第一次得到 `None` 后，本进程内不会再次尝试构造。平台后端由编译期
/// 目标与 feature 决定，运行期不会改变；Linux 不走这条路径（它有自己的缓存与每次调用的
/// `DISPLAY` 检查）。
fn cached_helper() -> &'static OnceLock<Option<SharedNativeClipboard>> {
    static HELPER: OnceLock<Option<SharedNativeClipboard>> = OnceLock::new();
    &HELPER
}

/// `getNativePlatformHelper()`：darwin/win32 的 helper（Linux 恒 `None`）。
///
/// TS 在非 darwin/win32 上直接返回 `undefined`；Rust 没有对应后端时同样返回 `None`。
pub fn get_native_platform_helper() -> Option<SharedNativeClipboard> {
    cached_helper().get_or_init(platform_helper).clone()
}

/// `getNativeClipboard()`：Linux 额外要求 `DISPLAY`（**每次调用都重新检查**），
/// 其它平台等价于 [`get_native_platform_helper`]。
///
/// 测试 3 的恒等断言（`getNativeClipboard() === getNativePlatformHelper()`）在 Rust 侧对应
/// `Arc::ptr_eq`：两者返回同一个缓存实例。
pub fn get_native_clipboard() -> Option<SharedNativeClipboard> {
    if cfg!(target_os = "linux") {
        return linux_clipboard();
    }
    get_native_platform_helper()
}

/// `isNativeModifierPressed(key)`（`native-modifiers.ts`）：后端缺失或抛错都返回 `false`。
pub fn is_native_modifier_pressed(key: ModifierKey) -> bool {
    match get_native_platform_helper() {
        Some(helper) => helper.is_modifier_pressed(key),
        None => false,
    }
}

/// `enableVirtualTerminalInput?.()`（`terminal.ts:429`）。
pub fn enable_virtual_terminal_input() -> bool {
    get_native_platform_helper()
        .map(|helper| helper.enable_virtual_terminal_input())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 平台后端选择
// ---------------------------------------------------------------------------
//
// TS 通过 N-API 在运行时加载 `native/<platform>/prebuilds/<platform>-<arch>/*.node`；
// Rust 在编译期选择（feature + `#[cfg(target_os)]`），因此不再需要
// `getNativeModuleCandidates` 的“包安装位置 → moduleDir/.. → moduleDir → exe 目录”搜索链。
// 保留它只是为了对齐 `native-module-path.test.ts` 的**可观察契约**（见 [`module_path`]），
// 真正的后端不经过它。

// 下面三个同名 `platform_helper` 由 `#[cfg]` 互斥选择，同一目标只编译其中一个。darwin/win32
// 版本内部的 `OnceLock` 保证 `Arc::new` 只执行一次；外层 `cached_helper` 还会缓存整个 `Option`。

/// 非 Linux 的 helper（`getNativePlatformHelper()`）：darwin/win32 有后端，其余平台 `None`。
#[cfg(all(target_os = "macos", feature = "darwin"))]
fn platform_helper() -> Option<SharedNativeClipboard> {
    static HELPER: OnceLock<SharedNativeClipboard> = OnceLock::new();
    Some(
        HELPER
            .get_or_init(|| Arc::new(darwin::DarwinClipboard::new()))
            .clone(),
    )
}

#[cfg(all(target_os = "windows", feature = "win32"))]
fn platform_helper() -> Option<SharedNativeClipboard> {
    static HELPER: OnceLock<SharedNativeClipboard> = OnceLock::new();
    Some(
        HELPER
            .get_or_init(|| Arc::new(win32::Win32Clipboard::new()))
            .clone(),
    )
}

#[cfg(not(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
)))]
fn platform_helper() -> Option<SharedNativeClipboard> {
    None
}

/// Linux 的剪贴板（`getNativeClipboard()` 的 Linux 分支）：每次调用都重新检查 `DISPLAY`。
///
/// TS 用真值判断（`if (!process.env.DISPLAY) return undefined`），所以**空字符串**也算没有
/// `DISPLAY`（`spec/tui-core.md` §4.1）。
///
/// 检查发生在取缓存**之前**：`DISPLAY` 被删除后立即回到 `None`，重新设置后返回的还是同一个
/// 已缓存实例（`tests/native_platform.rs` 的 `linux_loads_x11_lazily_and_rechecks_display`
/// 锁定这两点）。本函数只读环境变量，不连接 X server；连接失败体现在单次操作的结果上。
#[cfg(all(target_os = "linux", feature = "x11"))]
fn linux_clipboard() -> Option<SharedNativeClipboard> {
    let display = std::env::var_os("DISPLAY")?;
    if display.is_empty() {
        return None;
    }
    static HELPER: OnceLock<SharedNativeClipboard> = OnceLock::new();
    Some(
        HELPER
            .get_or_init(|| Arc::new(linux_x11::LinuxX11Clipboard::new()))
            .clone(),
    )
}

#[cfg(not(all(target_os = "linux", feature = "x11")))]
fn linux_clipboard() -> Option<SharedNativeClipboard> {
    None
}

// 平台后端模块：只有“目标系统 + 对应 feature”同时成立时才会编译，文件级说明在各文件开头
// 的 `//!` 里。未编译时上面的 `platform_helper`/`linux_clipboard` 走返回 `None` 的版本。
#[cfg(all(target_os = "macos", feature = "darwin"))]
pub mod darwin;
#[cfg(all(target_os = "linux", feature = "x11"))]
pub mod linux_x11;
#[cfg(all(target_os = "windows", feature = "win32"))]
pub mod win32;

// ---------------------------------------------------------------------------
// clipboard.h 的私有工作线程语义（darwin/win32）
// ---------------------------------------------------------------------------
//
// 这里只放 darwin/win32 共用的 worker；Linux 的 x11 后端有自己的一份实现（读取时要保留
// 连接与 INCR 状态），两者语义相同但状态字段不同。

/// `clipboard.h` 的 `queue_clipboard` + `clipboard-worker.h` 的 `clipboard_execute`：
/// 平台操作在 **libuv 线程池**（这里：私有线程）执行，TUI/事件循环线程只等待 future。
///
/// - 忙时其它调用立即返回 `Unavailable`（`worker_busy`）；
/// - 最多等 3 秒（`pthread_cond_timedwait` 的 deadline），超时后的结果丢弃；
/// - 调用方丢弃 future（Rust 特有：JS 的 promise 总会 settle）时同样释放 worker。
///
/// 一个实例就是一个“单槽位”：同一时刻最多执行一个平台操作，没有队列。它由 `DarwinClipboard`
/// 或 `Win32Clipboard` 持有；父模块再把该后端缓存成进程级单例，因此并发调用会竞争同一槽位。
/// 状态由 `Mutex<ClipboardWorkerState>` 保护，临界区只做标志位读写，平台 I/O 在锁外执行。
#[cfg(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
))]
pub(crate) struct ClipboardWorker {
    state: Arc<std::sync::Mutex<ClipboardWorkerState>>,
}

/// `worker_busy`/`worker_waiting`/`worker_finished` 与作业代号。
///
/// `busy` 是单槽位标志：为 `true` 时不会再启动新线程；`waiting` 表示等待方 future 还没
/// 放弃（超时或丢弃后为 `false`）；`finished` 表示工作线程已经执行完 `work` 并置位。操作
/// 结果通过 `oneshot` 送回等待方，不存放于共享状态。`epoch` 是防御性代号：Drop 与线程收尾
/// 在修改标志前先核对自己仍属于当前作业，避免迟到的清理误伤后续作业（与 `linux_x11` 的
/// 同名机制一致）。
#[cfg(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
))]
#[derive(Default)]
struct ClipboardWorkerState {
    /// `worker_busy`：私有线程正在执行，其它调用立即 `Unavailable`。
    busy: bool,
    /// `worker_waiting`：调用方仍在等待（超时/丢弃后置 `false`，迟到的结果被丢弃）。
    waiting: bool,
    /// `worker_finished`：私有线程已写完结果。
    finished: bool,
    /// 作业代号：旧作业的迟到清理不得影响已经开始的新作业（见 linux_x11 的同名机制）。
    epoch: u64,
}

/// `clipboard-worker.h` 的 3 秒等待（“Allow the two-second transfer deadline to report its own
/// error first”，transfer 自身的 2 秒 deadline 在各平台实现内部）。
#[cfg(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
))]
const CLIPBOARD_WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 作业守卫：future 被丢弃（未 poll 或等待中）时释放 worker。
///
/// 它被返回的 future 捕获，因此正常完成、超时后丢弃、甚至从未 poll 就被丢弃都会走到 `Drop`。
/// 释放规则是“线程说了算”：
///
/// - 线程已经写完结果（`finished == true`）：立即清 `busy`；
/// - 线程还在跑：只清 `waiting`，让线程结束时看到“无人等待”并自行清 `busy`。这样同一时刻
///   永远不会有两个平台调用。
#[cfg(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
))]
struct ClipboardJob {
    /// 与 worker 共享的状态。
    state: Arc<std::sync::Mutex<ClipboardWorkerState>>,
    /// 创建该作业时从状态里取到的代号；与当前值不符的清理一律忽略。
    epoch: u64,
}

#[cfg(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
))]
impl Drop for ClipboardJob {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // 锁中毒也继续：状态只有布尔标志和一个计数器，不存在半更新。
        if state.epoch != self.epoch {
            // 旧作业的守卫不得改动新作业的状态。
            return;
        }
        // 先声明“无人等待”；线程若仍在跑，会在结束时看到这一点并清 `busy`。
        state.waiting = false;
        if state.finished {
            // 线程已结束（结果已写回或被判定迟到），worker 立即可以接下一个作业。
            state.busy = false;
        }
    }
}

#[cfg(any(
    all(target_os = "macos", feature = "darwin"),
    all(target_os = "windows", feature = "win32")
))]
impl ClipboardWorker {
    /// 创建一个空闲 worker：只分配共享状态，不启动线程（线程按作业临时创建）。
    pub(crate) fn new() -> Self {
        ClipboardWorker {
            state: Arc::new(std::sync::Mutex::new(ClipboardWorkerState::default())),
        }
    }

    /// `clipboard_execute(job)`：占住 worker 并在私有线程执行 `work`，最多等 3 秒。
    ///
    /// # 流程
    ///
    /// 1. 加锁检查 `busy`：忙则返回一个立即完成于 [`ClipboardValue::Unavailable`] 的 future，
    ///    不排队、不等待。
    /// 2. 递增 `epoch`，置 `busy`/`waiting`，在锁外启动名为 `kiana-tui-clipboard` 的线程。
    /// 3. 线程执行 `work`；`epoch` 未变时置 `finished`。调用方仍在等待就通过 `oneshot` 送回
    ///    结果，否则丢弃结果并清 `busy`。
    /// 4. 返回的 future 同时轮询 `oneshot` 与 3 秒定时器：
    ///    - 结果先到：原样返回；
    ///    - 定时器先到：`Ok(Unavailable)`，线程之后的结果被丢弃（线程仍要跑完，worker 才空闲）；
    ///    - 发送端在 send 之前消失（线程 panic）：`Ok(Unavailable)`。
    ///
    /// # 保证与边界
    ///
    /// - **占位发生在返回 future 之前**：只调用不 poll 也会占住 worker；future 被丢弃时由
    ///   `ClipboardJob` 的 `Drop` 复位。
    /// - 超时只是让等待方拿到 `Unavailable`，不会中断平台调用；worker 在线程真正结束前不接新活。
    /// - 锁中毒一律 `into_inner` 后继续，不 panic（状态不存在半更新）。
    /// - `work` 若在更新共享状态之前 panic，`busy` 不会被清除（见模块文档的注意事项）。
    pub(crate) fn run<T, F>(&self, work: F) -> ClipboardFuture<'_, T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<ClipboardValue<T>, ClipboardError> + Send + 'static,
    {
        // 占位必须在返回 future 之前完成：若推迟到首次 poll，连续两次调用都会成功，
        // 破坏“忙时立即不可用”的契约。
        let epoch = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.busy {
                // `job->format` 保持 `CLIPBOARD_UNAVAILABLE` → promise resolves undefined。
                return Box::pin(async { Ok(ClipboardValue::Unavailable) });
            }
            state.epoch = state.epoch.wrapping_add(1);
            state.busy = true;
            state.waiting = true;
            state.finished = false;
            state.epoch
        };

        // 平台调用在工作线程里执行；锁只在占位与收尾阶段短暂持有，不跨线程持有。
        let shared = Arc::clone(&self.state);
        let (sender, receiver) =
            tokio::sync::oneshot::channel::<Result<ClipboardValue<T>, ClipboardError>>();
        let spawned = std::thread::Builder::new()
            .name("kiana-tui-clipboard".to_string())
            .spawn(move || {
                let result = work();
                let mut late = true;
                {
                    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                    if state.epoch == epoch {
                        state.finished = true;
                        // 调用方已超时/丢弃 future：丢弃迟到结果并释放 worker。
                        late = !state.waiting;
                        if late {
                            state.busy = false;
                        }
                    }
                }
                if !late && sender.send(result).is_err() {
                    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                    if state.epoch == epoch {
                        state.busy = false;
                    }
                }
            });
        // 线程创建失败：回滚占位并返回 `Unavailable`（等价 TS 的 queue 失败）。
        if spawned.is_err() {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.epoch == epoch {
                state.busy = false;
                state.waiting = false;
            }
            return Box::pin(async { Ok(ClipboardValue::Unavailable) });
        }

        let job = ClipboardJob {
            state: Arc::clone(&self.state),
            epoch,
        };
        Box::pin(async move {
            let _job = job;
            let mut receiver = receiver;
            // 结果通道与 3 秒定时器择一先完成。定时器是 `crate::terminal::TimerSleep`
            // （后台线程 + waker），不依赖 Tokio runtime，因此这个 future 可在任意 executor
            // 上推进，也可以在普通线程里用 noop waker 轮询。
            let mut timer = crate::terminal::sleep_for(CLIPBOARD_WORKER_TIMEOUT);
            std::future::poll_fn(|context| {
                match std::pin::Pin::new(&mut receiver).poll(context) {
                    std::task::Poll::Ready(Ok(result)) => return std::task::Poll::Ready(result),
                    // 线程在 send 前失败（panic）：promise 不会 settle，按不可用处理。
                    std::task::Poll::Ready(Err(_)) => {
                        return std::task::Poll::Ready(Ok(ClipboardValue::Unavailable));
                    }
                    std::task::Poll::Pending => {}
                }
                if std::pin::Pin::new(&mut timer).poll(context).is_ready() {
                    // 超时后结果丢弃（`worker_finished` 为 false 时 job 保持空）。
                    return std::task::Poll::Ready(Ok(ClipboardValue::Unavailable));
                }
                std::task::Poll::Pending
            })
            .await
        })
    }
}

pub mod module_path;

pub use module_path::get_native_module_candidates;
