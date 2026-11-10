//! utils/fs-watch.ts。
//!
//! TS: `packages/coding-agent/src/utils/fs-watch.ts`（30 行）。
//!
//! ```ts
//! export const FS_WATCH_RETRY_DELAY_MS = 5000;
//! export function closeWatcher(watcher): void;
//! export function watchWithErrorHandler(path, listener, onError): FSWatcher | null;
//! ```
//!
//! `fs.watch` → `notify` 8.2.1（`ARCHITECTURE.md` §4.12 / `spec/ca-cli.md` §2.21.9）：
//!
//! - `watch(path, listener)` 的**同步**抛错（路径不存在、权限等）→ 调用 `onError()` 并返回
//!   `null`；`RecommendedWatcher::new` 失败或 `watch(path)` 失败都走这条路径；
//! - `watcher.on("error", onError)` 的**异步**错误路径 → 事件回调收到 `Err` 时调用 `onError()`；
//! - `closeWatcher` 忽略 `close()` 的错误：`RecommendedWatcher` 的 `Drop` 即关闭，错误无从观察。
//!
//! 监听器签名对应 Node 的 `WatchListener<string>`：`(eventType, filename)`，`filename` 是
//! 被监视目录内发生变化的条目名（不是完整路径；平台不提供时为 `None`，对应 Node 的 `null`），
//! 下游（theme watcher / footer git watcher）按名字过滤或忽略。
//!
//! 偏差：Node 的 `eventType` 是 `"rename" | "change"`；这里用 [`WatchEventType`] 枚举 +
//! [`WatchEventType::as_str`] 保留同一字符串。`notify` 的 `EventKind` 归约为：
//! Create / Remove / Modify(Name) → `Rename`，其余 → `Change`。`Access`（打开/关闭文件）
//! 被丢弃：libuv 的监听掩码不订阅 IN_OPEN / IN_CLOSE_*，Node 不会为读取产生事件。
//!
//! ## 调用方
//!
//! - `kiana-theme` 的主题热重载：监听自定义主题目录，按事件里的条目名过滤出当前主题文件；
//! - `kiana-resources` 的 footer git 数据：监听 `HEAD` 所在目录、reftable 目录与
//!   `tables.list`，用 [`FS_WATCH_RETRY_DELAY_MS`] 做失败重试间隔；
//! - `kiana-extensions` 的 JS 宿主：把 `fs.watch` 暴露给扩展。
//!
//! 所有调用方都必须**持有返回的 watcher**：它一旦被 drop，监听立即停止（见
//! [`watch_with_error_handler`] 的文档）。

use std::path::Path;
use std::sync::{Arc, Mutex};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// `FS_WATCH_RETRY_DELAY_MS = 5000`（watcher 失败后的重试间隔）。
pub const FS_WATCH_RETRY_DELAY_MS: u64 = 5000;

/// Node `fs.watch` 的 `eventType`。
///
/// 与 `notify::EventKind` 的对应关系见模块文档：
/// `Create` / `Remove` / `Modify(Name)` 归为 `Rename`，其它（`Modify(Data)`、
/// `Modify(Metadata)` 等）归为 `Change`；`Access` 不入队。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchEventType {
    /// `"rename"`：创建、删除、重命名。
    Rename,
    /// `"change"`：内容或元数据变化。
    Change,
}

impl WatchEventType {
    /// 与 Node 相同的字符串：`"rename"` / `"change"`。
    pub fn as_str(self) -> &'static str {
        match self {
            WatchEventType::Rename => "rename",
            WatchEventType::Change => "change",
        }
    }
}

impl std::fmt::Display for WatchEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// TS: `watchWithErrorHandler` 的 `listener`（`WatchListener<string>`）：
/// `(eventType, filename)`；`filename` 为发生变化条目的文件名。
pub type WatchListener = Box<dyn FnMut(WatchEventType, Option<String>) + Send + 'static>;

/// TS: `watchWithErrorHandler` 的 `onError`。
pub type WatchErrorHandler = Box<dyn FnMut() + Send + 'static>;

/// TS: `closeWatcher(watcher)` —— 关闭 watcher，忽略关闭错误（`Drop` 即关闭）。
///
/// 传入 `None` 是空操作；重复关闭同一个 watcher 在 Rust 所有权下不可能发生
/// （watcher 按值传入并被消费）。
pub fn close_watcher(watcher: Option<RecommendedWatcher>) {
    drop(watcher);
}

/// 调用错误回调。回调被 `Mutex` 包住是因为创建 watcher 时与事件回调要共享同一个
/// `FnMut`；锁中毒（上一次回调 panic）时恢复内层数据继续调用，而不是再次 panic。
fn invoke_error<E: FnMut()>(slot: &Mutex<Option<E>>) {
    let mut guard = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(callback) = guard.as_mut() {
        callback();
    }
}

/// TS: `watchWithErrorHandler(path, listener, onError)` —— 非递归监听 `path`。
///
/// 创建/注册失败（对应 `fs.watch` 的同步异常）→ 调用 `onError()` 并返回 `None`。
/// 运行期的 watcher 错误 → 在事件回调里调用 `onError()`（watcher 保持存活，与 TS 相同）。
///
/// 返回值是**必须持有**的 watcher：`Drop` 即注销监听，提前丢弃会让后续事件静默消失
/// （`close_watcher_accepts_none_and_stops_delivering_events`）；[`close_watcher`] 就是
/// 显式丢弃。`listener` 与 `on_error` 都要求 `Send + 'static`，由 `notify` 在其事件分派
/// 路径上调用，因此不能借用调用方的栈上数据。
///
/// 只监听 `path` 这一层（`RecursiveMode::NonRecursive`），对目录内子目录的变化不会再
/// 递归上报。
pub fn watch_with_error_handler<L, E>(
    path: impl AsRef<Path>,
    listener: L,
    on_error: E,
) -> Option<RecommendedWatcher>
where
    L: FnMut(WatchEventType, Option<String>) + Send + 'static,
    E: FnMut() + Send + 'static,
{
    let error_slot = Arc::new(Mutex::new(Some(on_error)));
    let handler_error_slot = Arc::clone(&error_slot);
    let mut listener = listener;

    let event_handler = move |result: notify::Result<Event>| match result {
        Ok(event) => {
            // Node 的 `fs.watch` 直接建在 libuv 的 inotify 监听上，掩码只有 IN_MODIFY /
            // IN_CREATE / IN_DELETE / IN_ATTRIB / IN_MOVED_*；打开、关闭与读取文件不会
            // 产生任何事件。notify 额外订阅了 IN_OPEN / IN_CLOSE_WRITE，并把它们映射成
            // `EventKind::Access`；若继续把它们转发成 `"change"`，任何对监视文件的读取
            // （例如主题热重载自己重新读取文件）都会再次触发监听器，形成永不停止的事件
            // 循环。这里丢弃 Access，保持与 Node 一致的可见事件集合。
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            let event_type = match event.kind {
                EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Modify(notify::event::ModifyKind::Name(_)) => WatchEventType::Rename,
                _ => WatchEventType::Change,
            };
            let filename = event
                .paths
                .first()
                .and_then(|changed| changed.file_name())
                .map(|name| name.to_string_lossy().into_owned());
            listener(event_type, filename);
        }
        Err(_) => invoke_error(&handler_error_slot),
    };

    let mut watcher = match RecommendedWatcher::new(event_handler, Config::default()) {
        Ok(watcher) => watcher,
        Err(_) => {
            invoke_error(&error_slot);
            return None;
        }
    };

    if watcher
        .watch(path.as_ref(), RecursiveMode::NonRecursive)
        .is_err()
    {
        invoke_error(&error_slot);
        return None;
    }

    Some(watcher)
}
