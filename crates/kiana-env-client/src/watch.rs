//! watch.ts：daemon 里的文件观察器。
//!
//! 连接丢失后，等连接可以再次启动时重新打开，并报告 `overflow`（断线期间的变化没有看到）。
//! 事件通过连接读线程送达，因此回调必须可在任意线程运行（[`WatchCallback`] 已要求 `Send + Sync`）。
//!
//! 与 TS 的差异：TS 的 `#request = request.then(...)` 在 Promise 上完成；Rust 用
//! `tokio::spawn` 驱动请求，并把完成信号存成 `Shared` future 供 `close()` 等待。
//!
//! # 这个文件解决什么问题
//! Durable 的 `FileWatcher` 需要远端实现：daemon 在文件旁边运行 `NodeFileWatcher`，本模块
//! 负责发 `watch` 请求、把事件翻译成 [`WatchChange`]，并在连接丢失后重连。
//! `RemoteExecutionEnv::watch` 先解析所有 target 的路径，再调用 [`RemoteWatcher::open`]。
//!
//! # 生命周期
//! ```text
//! open ──ready──> Watching ──连接丢失──> Reconnecting ──重新 ready──> Watching
//!   │                 │                        │
//!   │                 └── daemon error 事件 ───┴──> Closed
//!   └── 请求失败 ──> open 返回 Err
//! close() ─────────────────────────────────────────> Closed
//! ```
//! - Opening：`start_watch` 已发出请求，等 `ready` 事件；
//! - Watching：观察器已建立，`change`/`error` 事件到达；
//! - Reconnecting：连接丢失后按 1 s 起步、翻倍到 30 s 的退避重试；成功后先投递一次
//!   `overflow`（断线期间的变化没有看到）；
//! - Closed：`close()` 或终端性错误；之后不再开始新的投递，重复 `close()` 直接返回。
//!
//! # 主要流程
//! 1. `open` 构造 `WatcherState`，调用 `start_watch`；
//! 2. `start_watch` 组装 body、发 `watch` 请求，在 `on_event` 里处理 `ready`/`change`/`error`；
//! 3. `ready` 事件结算 open，并把 daemon 实际选择的模式写进 `mode`；
//! 4. 请求异常结束时按原因分流：未 ready → open 失败；连接丢失 → `reconnect`；其它 → 投递
//!    `WatchChange::Error` 并关闭；
//! 5. `close()` 置 closed、唤醒退避、abort 当前 controller、等待请求完成。
//!
//! # 并发与注意事项
//! - `WatcherState` 由 `Arc` 共享给请求任务、事件回调和重连循环；`registration` 互斥把
//!   “close 置 closed”与“start_watch 安装 controller/request”配成一对，避免 close 之后还
//!   留下新的未中止请求。
//! - 事件回调在连接的读任务里同步执行；`deliver` 先查 `closed`，并用 `catch_unwind` 吞掉
//!   回调 panic，否则 panic 会从读任务里展开，读循环随之中断。
//! - 没有 `Drop`：丢弃 [`RemoteWatcher`] 不会停止 daemon 里的观察器，调用方必须显式
//!   `close().await`。
//!
//! TS: packages/env/src/watch.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use kiana_chord::context::{AbortSignal, Context};
use kiana_durable::env::{
    FileError, FileErrorCode, WatchCallback, WatchChange, WatchMode, WatchTarget,
};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::SharedResult;
use crate::connection::{Connection, Json, RemoteError, RequestOptions, is_connection_lost};
use crate::errors::{EnvError, to_file_error};
use crate::js_string_of;

const RECONNECT_FIRST_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 30_000;

/// 取锁；中毒（持锁线程 panic）时取回内部值继续，不把 panic 传播给后续 `close`/重连。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// daemon 如何观察，类似 `NodeExecutionEnv` 的 `NodeWatchOptions`。
///
/// 三个字段都是 `Option`：`None` 时不出现在请求 JSON 里，由 daemon 决定默认值
/// （`mode`：Windows 或不可靠文件系统 → polling，否则 native；`pollIntervalMs`：2000；
/// `maxDirectories`：10 000，见 `kiana-env` 的 `watch.rs`）。
///
/// TS: packages/env/src/watch.ts#RemoteWatchOptions
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemoteWatchOptions {
    /// 强制模式；默认 Windows 与不报告远端变化的文件系统用轮询。
    ///
    /// daemon 在实际安装 watcher 失败时可能把 native 降级为 polling，并通过一个带
    /// `mode: "polling"` 的 `overflow` 事件告知；[`RemoteWatcher::mode`] 反映实际模式。
    pub mode: Option<WatchMode>,
    /// 轮询模式下两次快照的间隔；默认 2000 ms。
    ///
    /// 以 `js_number` 原样发送；daemon 只接受非负整数，其余（小数、负数、非有限值）回落到
    /// 默认值。
    pub poll_interval_ms: Option<f64>,
    /// 一个观察器最多覆盖的目录数；默认 10,000。
    ///
    /// 预算是全部 target 共享的全局计数，不是每个 target 各算一次。打开阶段超限是请求错误
    /// （`open` 失败）；运行中重扫超限则收到一次 `WatchChange::Error` 并结束（daemon 的
    /// `watch.rs`）。
    pub max_directories: Option<usize>,
}

/// `watch` 请求完成时结算的共享 future：`close()` 克隆一份并等它。
type RequestDone = SharedResult<()>;

/// 观察器的全部共享状态：请求任务、事件回调、重连循环和 `RemoteWatcher` 都通过它通信。
///
/// 锁的临界区都很短：`registration` 只包住“检查 closed + 安装/取走 controller 与 request”，
/// 回调的调用与 `await` 都在锁外。
struct WatcherState {
    connection: Arc<Connection>,
    targets: Vec<WatchTarget>,
    on_change: WatchCallback,
    options: RemoteWatchOptions,
    mode: Mutex<WatchMode>,
    closed: AtomicBool,
    /// `close()` 与 `start_watch` 的安装共用：TS 的 `#start` 在 Promise executor 里同步装好
    /// `#controller`/`#request`，与 `#reconnect` 的 `#closed` 检查处于同一个同步回合。
    registration: Mutex<()>,
    controller: Mutex<Option<AbortSignal>>,
    request: Mutex<Option<RequestDone>>,
    wake: Mutex<Option<oneshot::Sender<()>>>,
}

impl WatcherState {
    /// 把变化交给回调；已关闭时丢弃，回调 panic 不向外传播。
    ///
    /// 事件在连接的读任务里同步派发（`connection.rs` 的 `read_stdout` → `dispatch`），所以这里
    /// 用 `catch_unwind` 保住读循环：一次业务回调的 panic 不应该让整条连接停止处理帧。
    fn deliver(&self, change: WatchChange) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let callback = Arc::clone(&self.on_change);
        // A throwing callback must not stop watching.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || callback(change)));
    }
}

/// daemon 里的观察器（文件旁边运行 Durable 的 `NodeFileWatcher`）。
///
/// # 所有权与生命周期
/// `RemoteWatcher` 只持有 `Arc<WatcherState>`；请求任务与重连任务也持有它，所以丢弃 watcher
/// 本身不会停止观察（没有 `Drop`）。调用方必须 `close().await`；同一个 watcher 的 `close`
/// 可以重复调用。
///
/// TS: packages/env/src/watch.ts#RemoteWatcher
pub struct RemoteWatcher {
    state: Arc<WatcherState>,
}

impl std::fmt::Debug for RemoteWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteWatcher")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl RemoteWatcher {
    /// 观察 `targets`（已解析的路径）；无法建立覆盖时像 `NodeFileWatcher.open` 一样失败。
    ///
    /// 成功时只在收到 daemon 的 `ready` 事件后返回 `Ok`；请求在 `ready` 之前失败（daemon
    /// 以错误回复拒绝，例如 `EINVAL`）时返回 `Err`，对应 TS 的 `!ready → reject`。`targets`
    /// 必须是已经解析过的绝对路径（`RemoteExecutionEnv::watch` 负责），本函数不再做路径解析。
    /// 只有请求成功结束却没有 `ready` 时才与 TS 一样永久挂起（只有不遵守协议的 daemon 会触发）。
    pub async fn open(
        connection: Arc<Connection>,
        targets: Vec<WatchTarget>,
        on_change: WatchCallback,
        options: RemoteWatchOptions,
    ) -> Result<RemoteWatcher, RemoteError> {
        let state = Arc::new(WatcherState {
            connection,
            targets,
            on_change,
            options,
            // 先放 Native 占位；`ready` 事件会用 daemon 报告的实际模式覆盖它。
            mode: Mutex::new(WatchMode::Native),
            closed: AtomicBool::new(false),
            registration: Mutex::new(()),
            controller: Mutex::new(None),
            request: Mutex::new(None),
            wake: Mutex::new(None),
        });
        match start_watch(Arc::clone(&state)).await? {
            StartWatch::Installed => Ok(RemoteWatcher { state }),
            // `open` 的调用者还没有 watcher，close() 不可能抢先；保守地按已关闭处理。
            StartWatch::Closed => Err(crate::connection::closed_error()),
        }
    }

    /// 观察模式。
    ///
    /// 由最近一次 `ready` 或 `change` 事件更新：`ready` 直接给 daemon 实际选择的模式；
    /// 普通 `change` 事件不带模式，只有 native→polling 降级时的 `overflow` 事件会带
    /// `mode: "polling"`。
    pub fn mode(&self) -> WatchMode {
        *lock(&self.state.mode)
    }

    /// 停止观察；幂等，返回后不再有 `on_change` 调用。
    ///
    /// 步骤：在 `registration` 锁内置 `closed`（与 `start_watch` 的安装互斥）→ 取走唤醒端、
    /// 当前 controller 与请求 → 唤醒重连退避 → abort（连接写 `CANCEL` 帧）→ 等请求结算。
    /// 请求结算由 `watch` 请求的 RESULT/ERROR 驱动，因此正常 daemon 下 `close` 不会无限等待。
    pub async fn close(&self) {
        let (wake, controller, request) = {
            // 与 `start_watch` 的安装互斥：close() 要么在安装之前（start_watch 看到 closed、
            // 不装），要么在安装之后（拿到并中止新的 controller/request）。
            let _registration = lock(&self.state.registration);
            if self.state.closed.swap(true, Ordering::SeqCst) {
                return;
            }
            (
                lock(&self.state.wake).take(),
                lock(&self.state.controller).take(),
                lock(&self.state.request).clone(),
            )
        };
        if let Some(wake) = wake {
            let _ = wake.send(());
        }
        if let Some(controller) = controller {
            controller.abort_without_reason();
        }
        if let Some(request) = request {
            request.wait().await;
        }
    }
}

#[async_trait]
impl kiana_durable::env::FileWatcher for RemoteWatcher {
    fn mode(&self) -> WatchMode {
        RemoteWatcher::mode(self)
    }

    async fn close(&self, _context: &dyn Context) {
        RemoteWatcher::close(self).await;
    }
}

/// 组装 `watch` 请求体：targets，以及可选的 mode/pollIntervalMs/maxDirectories。
///
/// 每个 target 的 `recursive` 与 `exclude` 总是会被发送：缺省的 `exclude` 写成
/// `{ hidden: false, names: [] }`（TS 的 `target.exclude?.hidden === true` /
/// `[...(names ?? [])]`）。顶层的三个选项则相反：`None` 时不写字段，由 daemon 取默认值。
fn watch_body(state: &WatcherState) -> Json {
    let targets = state
        .targets
        .iter()
        .map(|target| {
            let mut exclude = Json::new();
            exclude.insert(
                "hidden".to_owned(),
                Value::Bool(
                    target.exclude.as_ref().and_then(|exclude| exclude.hidden) == Some(true),
                ),
            );
            let names = target
                .exclude
                .as_ref()
                .and_then(|exclude| exclude.names.clone())
                .unwrap_or_default();
            exclude.insert(
                "names".to_owned(),
                Value::Array(names.into_iter().map(Value::String).collect()),
            );
            let mut json = Json::new();
            json.insert("path".to_owned(), Value::String(target.path.clone()));
            json.insert(
                "recursive".to_owned(),
                Value::Bool(target.recursive == Some(true)),
            );
            json.insert("exclude".to_owned(), Value::Object(exclude));
            Value::Object(json)
        })
        .collect();
    let mut body = Json::new();
    body.insert("targets".to_owned(), Value::Array(targets));
    // 顶层选项只在 Some 时出现，让 daemon 决定默认值。
    if let Some(mode) = state.options.mode {
        body.insert(
            "mode".to_owned(),
            Value::String(
                match mode {
                    WatchMode::Native => "native",
                    WatchMode::Polling => "polling",
                }
                .to_owned(),
            ),
        );
    }
    if let Some(interval) = state.options.poll_interval_ms {
        body.insert("pollIntervalMs".to_owned(), crate::js_number(interval));
    }
    if let Some(max_directories) = state.options.max_directories {
        body.insert("maxDirectories".to_owned(), Value::from(max_directories));
    }
    body
}

/// `start_watch` 的结果。
enum StartWatch {
    /// controller 与 request 已安装，请求已开始。
    Installed,
    /// `close()` 在安装之前抢先。TS 里这等价于 `#reconnect` 的 `#closed` 检查先返回：不再发
    /// watch 请求，也不留下未中止的 controller。
    Closed,
}

/// 测试专用钩子（`#[cfg(test)]`）：在 `start_watch` 安装 controller/request 之前调用。
#[cfg(test)]
mod test_hooks {
    use super::*;

    /// 钩子类型（`Send` 足够：只在 `start_watch` 的任务里调用一次）。
    pub(crate) type Hook = Box<dyn Fn() + Send>;

    /// 只触发一次。
    pub(crate) static BEFORE_REGISTER: Mutex<Option<Hook>> = Mutex::new(None);
}

/// 打开 daemon 的观察器；它报告覆盖范围后 `Ok`。
///
/// 具体是收到 `ready` 事件后返回 `Installed`。安装顺序是这里最需要小心的地方：
/// 1. 先创建 controller、oneshot 与请求 future（此时还没有发出任何帧）；
/// 2. 在 `registration` 锁内检查 `closed`，再把 controller/request 放进 state；
/// 3. 之后才 `tokio::spawn` 驱动请求并等 `ready`。
/// 这样 `close()` 只有两种可能：在安装前（看到 closed，什么都不装）或安装后（拿到并中止
/// 新的 controller）。测试 `close_during_reconnect_installs_no_watch_request` 用钩子把
/// `close()` 恰好塞进“检查与安装之间”来锁定这一点。
///
/// 请求在 `ready` 之前失败时，任务把错误发给 `started_sender`，`started_receiver` 收到
/// `Ok(Err(error))`，调用方（`open`）返回该错误，不会挂起。只有请求成功结束却没有 `ready`
/// 时，发送端才随任务一起被丢弃，`started_receiver` 收到 `Err(RecvError)`，调用方永久挂起，
/// 与 TS 的 promise 永不结算一致。
async fn start_watch(state: Arc<WatcherState>) -> Result<StartWatch, RemoteError> {
    let controller = AbortSignal::new();
    let request_controller = controller.clone();
    let (started_sender, started_receiver) = oneshot::channel::<Result<(), RemoteError>>();
    let started_sender = Arc::new(Mutex::new(Some(started_sender)));
    let ready = Arc::new(AtomicBool::new(false));

    let body = watch_body(&state);
    let event_state = Arc::clone(&state);
    let event_ready = Arc::clone(&ready);
    let event_started = Arc::clone(&started_sender);
    let request_state = Arc::clone(&state);
    let request: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), RemoteError>> + Send>,
    > = Box::pin(async move {
        request_state
            .connection
            .request(
                "watch",
                body,
                RequestOptions {
                    signal: Some(request_controller),
                    on_event: Some(Arc::new(move |event: Json, _payload: Vec<u8>| match event
                        .get("kind")
                        .and_then(Value::as_str)
                    {
                        Some("ready") => {
                            // 只有 `ready` 能结算 open；把 daemon 报告的实际模式写进 state，
                            // 非 "polling" 一律按 native。
                            event_ready.store(true, Ordering::SeqCst);
                            *lock(&event_state.mode) =
                                if event.get("mode").and_then(Value::as_str) == Some("polling") {
                                    WatchMode::Polling
                                } else {
                                    WatchMode::Native
                                };
                            if let Some(sender) = lock(&event_started).take() {
                                let _ = sender.send(Ok(()));
                            }
                        }
                        Some("change") => {
                            // 普通 `change` 事件不带模式；只有 native→polling 降级时 daemon
                            // 才发一个带 mode 的 overflow 事件。
                            if event.get("mode").and_then(Value::as_str) == Some("polling") {
                                *lock(&event_state.mode) = WatchMode::Polling;
                            }
                            if event.get("overflow") == Some(&Value::Bool(true)) {
                                event_state.deliver(WatchChange::Overflow);
                            } else {
                                let paths = event
                                    .get("paths")
                                    .and_then(Value::as_array)
                                    .map(|paths| {
                                        paths
                                            .iter()
                                            .filter_map(|path| path.as_str().map(str::to_owned))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                event_state.deliver(WatchChange::Paths(paths));
                            }
                        }
                        Some("error") => {
                            // daemon 的 error 事件是终态（之后不再有事件）：投递错误并置
                            // closed，让后续投递被 deliver 丢弃；这里不 abort 请求。
                            let code = if event.get("code").and_then(Value::as_str)
                                == Some("permission_denied")
                            {
                                FileErrorCode::PermissionDenied
                            } else {
                                FileErrorCode::Invalid
                            };
                            event_state.deliver(WatchChange::Error(FileError::new(
                                code,
                                js_string_of(event.get("message")),
                            )));
                            event_state.closed.store(true, Ordering::SeqCst);
                        }
                        _ => {}
                    })),
                    ..RequestOptions::default()
                },
            )
            .await
            .map(|_reply| ())
    });

    #[cfg(test)]
    {
        // 测试钩子：让 close() 正好落在 reconnect 的 closed 检查与本次安装之间。
        let hook = lock(&test_hooks::BEFORE_REGISTER).take();
        if let Some(hook) = hook {
            hook();
        }
    }

    let (done_sender, done) = SharedResult::<()>::new();
    {
        // 与 close() 互斥：安装时再查一次 closed（TS 的 #start executor 与 #reconnect 的
        // #closed 检查在同一个同步回合）。close() 抢先时像 TS 一样什么都不做。
        let _registration = lock(&state.registration);
        if state.closed.load(Ordering::SeqCst) {
            return Ok(StartWatch::Closed);
        }
        *lock(&state.controller) = Some(controller);
        *lock(&state.request) = Some(done);
    }

    let run_state = Arc::clone(&state);
    tokio::spawn(async move {
        if let Err(error) = request.await {
            if !ready.load(Ordering::SeqCst) {
                // 还没 ready：open() 必须失败（对应 TS 的 `!ready → reject`）。
                if let Some(sender) = lock(&started_sender).take() {
                    let _ = sender.send(Err(error));
                }
            } else if run_state.closed.load(Ordering::SeqCst) {
                // close() 或 daemon 的 error 事件已经关闭：不再重连，也不重复投递错误。
            } else if is_connection_lost(&error) {
                // 连接丢失是唯一可恢复的错误：交给重连循环。
                reconnect(Arc::clone(&run_state)).await;
            } else {
                // daemon 明确拒绝、协议错误等：投递一次 Error 后停止。
                run_state.deliver(WatchChange::Error(to_file_error(
                    EnvError::Remote(error),
                    None,
                )));
                run_state.closed.store(true, Ordering::SeqCst);
            }
        }
        // 无论请求成功、失败还是被 close() 中止，都要让 close() 的等待结束。
        done_sender.send(());
    });

    match started_receiver.await {
        Ok(result) => result.map(|()| StartWatch::Installed),
        // The request settled without a `ready` event. TS's `#start` resolves only from the `ready`
        // event, so its promise never settles then; mirror that instead of reporting a fake open
        // (only reachable with a daemon that does not follow the protocol).
        Err(_) => std::future::pending::<Result<StartWatch, RemoteError>>().await,
    }
}

/// 重连循环。返回装箱的 `Send` future 以打断 `start_watch` ↔ `reconnect` 的 `Send` 推断环。
///
/// 退避从 1 s 开始，每次连接丢失翻倍，封顶 30 s；第一轮也要先等 1 s（与 TS 的
/// `setTimeout` 一致）。等待期间 `state.wake` 里放一个 oneshot，`close()` 用它提前唤醒；
/// 醒来后先清空槽位再检查 closed。成功重新打开后先投递一次 `overflow` 再返回：断线期间发生
/// 的变化没有被观察到。只有连接丢失会重试，其它错误投递 [`WatchChange::Error`] 并置 closed。
fn reconnect(
    state: Arc<WatcherState>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let mut delay = RECONNECT_FIRST_MS;
        while !state.closed.load(Ordering::SeqCst) {
            // 注册唤醒端：close() 取走它并 send，就能打断这次退避。
            let (sender, receiver) = oneshot::channel::<()>();
            *lock(&state.wake) = Some(sender);
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(delay)) => {}
                _ = receiver => {}
            }
            *lock(&state.wake) = None;
            // close() 可能在任何时刻置 closed；醒来后再查一次，避免多开一个观察器。
            if state.closed.load(Ordering::SeqCst) {
                return;
            }
            match start_watch(Arc::clone(&state)).await {
                Ok(StartWatch::Installed) => {
                    // Changes while disconnected were not seen.
                    state.deliver(WatchChange::Overflow);
                    return;
                }
                // close() 在安装之前抢先：与 TS 的 `#closed` 检查一样直接结束。
                Ok(StartWatch::Closed) => return,
                Err(error) => {
                    if is_connection_lost(&error) {
                        // 仍然连不上：指数退避后重试。
                        delay = (delay * 2).min(RECONNECT_MAX_MS);
                        continue;
                    }
                    // 不可恢复的错误：投递后停止。
                    state.deliver(WatchChange::Error(to_file_error(
                        EnvError::Remote(error),
                        None,
                    )));
                    state.closed.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归（review finding）：`close()` 落在 `#reconnect` 的 closed 检查与 `#start` 的
    /// controller/request 安装之间时，不能留下未中止的 watch 请求（TS 里两步在同一个同步
    /// 回合，close() 总能拿到当前 controller 并 await 当前 request）。
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_during_reconnect_installs_no_watch_request() {
        use std::sync::mpsc;

        let state = Arc::new(WatcherState {
            connection: Arc::new(Connection::new(vec![
                "/definitely-not-a-real-kiana-env-command".to_owned(),
            ])),
            targets: vec![WatchTarget {
                path: "/watch".to_owned(),
                recursive: None,
                exclude: None,
            }],
            on_change: Arc::new(|_change: WatchChange| {}),
            options: RemoteWatchOptions::default(),
            mode: Mutex::new(WatchMode::Native),
            closed: AtomicBool::new(false),
            registration: Mutex::new(()),
            controller: Mutex::new(None),
            request: Mutex::new(None),
            wake: Mutex::new(None),
        });

        let (reached_sender, reached) = mpsc::channel::<()>();
        let (release_sender, release) = mpsc::channel::<()>();
        *lock(&test_hooks::BEFORE_REGISTER) = Some(Box::new(move || {
            let _ = reached_sender.send(());
            let _ = release.recv();
        }));

        let reconnecting = tokio::spawn(reconnect(Arc::clone(&state)));
        // reconnect 先等 1 s，再调 start_watch；钩子随后在安装之前停下。
        reached
            .recv_timeout(Duration::from_secs(10))
            .expect("start_watch reaches the window before installing");
        let watcher = RemoteWatcher {
            state: Arc::clone(&state),
        };
        watcher.close().await;
        release_sender.send(()).expect("release start_watch");
        tokio::time::timeout(Duration::from_secs(10), reconnecting)
            .await
            .expect("reconnect finishes")
            .expect("reconnect does not panic");

        assert!(state.closed.load(Ordering::SeqCst));
        assert!(
            lock(&state.controller).is_none(),
            "close() 之后不能留下新的 controller"
        );
        assert!(
            lock(&state.request).is_none(),
            "close() 之后不能留下新的 watch 请求"
        );
    }
}
