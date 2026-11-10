//! 每会话 durable JSONL 存储的共享打开/提交/扫描层。
//!
//! `background::durable` 的 task journal 与 `subagent::work_store` 的 Work 账本都按
//! “一个父会话一个 durable 存储目录”工作。目录命名、owner 标记、尾部恢复规避、
//! `SessionImpl` 复用这些细节只在这里实现一次；上层只提供自己的文档与变更类型。
//!
//! 约束与 `docs/adr/durable.md` 一致：本模块不是队列，不实现调度或重试；它只提供
//! durable `Session`/`Tx` 的事务入口。Work 账本和 background journal 各自使用独立的
//! 根目录（`<state_dir>/work` 与 `<state_dir>/background`），因此同一个会话在两个
//! 根下各有一个存储实例，互不覆盖。

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};

use kiana_chord::context::BACKGROUND_CONTEXT;
use kiana_durable::session::{SessionImpl, create_session};
use kiana_durable::storage::jsonl::{JsonlStorageOptions, open_node_jsonl_storage};
use kiana_durable::{DocTarget, JsonObject, Session as _, SessionChange};

/// 会话存储目录里的所有者标记文件名。
///
/// durable 根目录在所有会话、所有 kiana 进程之间共享。打开一个 JSONL 存储会做尾部
/// 恢复（截断未确认的行），必须避免两个进程同时打开同一个目录。
pub(crate) const OWNER_FILE: &str = "owner.json";
/// 跨进程目录锁文件（unix 上对它执行 `flock`）。
pub(crate) const OWNER_LOCK_FILE: &str = "owner.lock";
/// 所有者标记的大小上限：损坏/被篡改的文件不能让启动阶段无界读取。
const MAX_OWNER_FILE_BYTES: u64 = 1024;

/// 进程内已打开的 `SessionImpl`，以规范化后的存储目录为键。
///
/// durable JSONL 的尾部恢复要求同一目录在**任意时刻只有一个** `SessionImpl` 在写。
/// 进程内的多个 `DurableSessionStore`（background journal、Work 账本、宿主自建实例）
/// 必须共享同一实例；跨进程由目录里的 `owner.json` 与 `owner.lock` 拒绝接管。
static OPEN_SESSIONS: LazyLock<StdMutex<HashMap<PathBuf, Weak<SessionImpl>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// 串行化进程内打开存储的临界区。
///
/// 打开是 async 的，无法用 `StdMutex` 跨越 `await`；这个 async 锁保证两个并发
/// `slot_for` 不会各自创建一个 `SessionImpl`。不同目录也会串行，但打开只发生在
/// 首次访问，代价可以忽略。
static OPEN_SERIAL: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// owner 声明临时文件的序号（同一进程可并发打开不同目录）。
static OWNER_CLAIM_SEQ: AtomicU64 = AtomicU64::new(0);

fn registered_session(directory: &Path) -> Option<Arc<SessionImpl>> {
    let mut sessions = OPEN_SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match sessions.get(directory).and_then(Weak::upgrade) {
        Some(session) => Some(session),
        None => {
            sessions.remove(directory);
            None
        }
    }
}

fn register_session(directory: PathBuf, session: &Arc<SessionImpl>) {
    OPEN_SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(directory, Arc::downgrade(session));
}

/// 进程是否仍存活（`kill(pid, 0)` 只做存在性与权限检查，不发送信号）。
#[cfg(unix)]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    // `libc::pid_t` 是 i32；直接 `as` 会让越界 pid 回绕成负数，而 `kill(-n, 0)`
    // 检查的是进程组  n，可能把损坏的 owner 标记误判成“活着”。无法表示的 pid
    // 不能安全探测，按“不存活/可回收”处理。
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: 传 0 信号不会实际投递；全部参数都是标量。
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 非 unix 平台没有无损的 pid 存活探测。
///
/// 与 `background` / `isolation` 的同名 helper 一致，按“不存活”处理：恒为 `true`
/// 会让崩溃遗留的 `owner.json` 把目录永久锁死。该平台没有 `flock`，本就不具备
/// 跨进程互斥，只能接受后写者接管。
#[cfg(not(unix))]
pub(crate) fn process_is_alive(_pid: u32) -> bool {
    false
}

pub(crate) fn read_owner_pid(path: &Path) -> Option<u32> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_OWNER_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
}

fn owner_contents() -> String {
    let value = serde_json::json!({ "pid": std::process::id() });
    kiana_json::stringify(&kiana_json::normalize_numbers(value))
}

/// 目录锁的平台载体。
///
/// unix 上是 `owner.lock` 的打开文件描述：`flock` 与它绑定，进程崩溃时由内核释放，
/// 不需要心跳或超时。其它平台没有等价物，保留 `owner.json` 的 pid 协议。
#[cfg(unix)]
type DirectoryLock = std::fs::File;
#[cfg(not(unix))]
type DirectoryLock = ();

/// 进程内持有的目录租约（带引用计数）。
///
/// `owner.json` 是给外部/旧版本进程看的标记；真正防止两个进程同时打开同一 JSONL
/// 目录的是 `owner.lock` 上的 `flock`。同一进程的多个 store/槽位共享同一租约：
/// 引用计数归零时才删除 `owner.json` 并释放 `flock`。
static OWNER_LEASES: LazyLock<StdMutex<HashMap<PathBuf, OwnerLease>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

struct OwnerLease {
    refs: usize,
    /// 仅用于持有锁：字段 drop 时释放 `flock`，从不显式读取。
    _lock: Option<DirectoryLock>,
}

/// 目录租约的一个引用；最后一个引用 drop 时释放目录。
struct OwnerLeaseGuard {
    canonical: PathBuf,
}

impl Drop for OwnerLeaseGuard {
    fn drop(&mut self) {
        let mut leases = OWNER_LEASES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease) = leases.get_mut(&self.canonical) else {
            return;
        };
        if lease.refs > 1 {
            lease.refs -= 1;
            return;
        }
        let Some(lease) = leases.remove(&self.canonical) else {
            return;
        };
        // 先删除标记，再释放 `flock`：持锁期间没有别的 kiana 进程能接管这个目录，
        // 因此不会出现“标记删了一半又被别人发布”的窗口。只删本进程的标记。
        let owner_path = self.canonical.join(OWNER_FILE);
        if read_owner_pid(&owner_path) == Some(std::process::id()) {
            let _ = std::fs::remove_file(&owner_path);
        }
        // `flock` 必须在仍持有 `OWNER_LEASES` 时释放：否则同进程的并发 acquire 会
        // 看到“注册表里没有租约、但锁仍被占用”，把自己的目录误报成他人持有。
        drop(lease);
        drop(leases);
    }
}

/// 对目录执行非阻塞独占 `flock`。
///
/// 返回 `Ok(None)` 表示另一个进程持有目录；`Ok(Some(_))` 表示当前进程已经拿到锁，
/// 返回的句柄必须活到释放为止。
#[cfg(unix)]
fn try_lock_directory(directory: &Path) -> Result<Option<DirectoryLock>, String> {
    use std::os::unix::io::AsRawFd as _;

    let path = directory.join(OWNER_LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("opening durable store lock: {error}"))?;
    loop {
        // SAFETY: fd 属于仍然存活的 `file`；`flock` 只操作文件描述符。
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Some(file));
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            // 已被另一个进程持有：正常跳过，不是错误。
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
                return Ok(None);
            }
            Some(libc::EINTR) => continue,
            _ => return Err(format!("locking durable store directory: {error}")),
        }
    }
}

#[cfg(not(unix))]
fn try_lock_directory(_directory: &Path) -> Result<Option<DirectoryLock>, String> {
    // 非 unix 平台没有 `flock`，只能依赖 `owner.json` 的 pid 协议；该平台的
    // `process_is_alive` 恒为真，因此陈旧/损坏标记一律按“他人持有”跳过。
    Ok(Some(()))
}

/// `owner.json` 的归属判定。
enum OwnerMarker {
    /// 标记已经是当前进程的。
    Ours,
    /// 另一个仍存活的进程（仅在没有目录锁的平台上可判定）或无法分辨的损坏标记。
    LiveOther,
    /// 死进程留下的标记（或 unix 上持锁时可安全回收的损坏标记）。
    Reclaimable,
}

/// 发布 `owner.json` 的结果。
#[derive(PartialEq, Eq)]
enum MarkerOwnership {
    Published,
    ForeignLive,
}

/// 已关停的 store 不再接受新的打开。
fn store_is_closed() -> String {
    "durable session store is shut down".to_string()
}

/// `try_lock_directory` 是否提供真正的跨进程互斥。
///
/// unix 上是 `flock`；其它平台的实现只是占位，互斥仍依赖 `owner.json` 的 pid 协议。
const DIRECTORY_LOCK_IS_EXCLUSIVE: bool = cfg!(unix);

fn classify_owner_marker(owner_path: &Path) -> OwnerMarker {
    match read_owner_pid(owner_path) {
        Some(pid) if pid == std::process::id() => OwnerMarker::Ours,
        // 调用方已经持有跨进程锁：另一个“存活”的 pid 只可能是复用（目录不是它锁住
        // 的），必须按可回收处理，否则被复用的 pid 会把目录永久钉住。
        Some(_) if DIRECTORY_LOCK_IS_EXCLUSIVE => OwnerMarker::Reclaimable,
        Some(pid) if process_is_alive(pid) => OwnerMarker::LiveOther,
        Some(_) => OwnerMarker::Reclaimable,
        None => {
            if std::fs::symlink_metadata(owner_path).is_err() {
                // 标记不存在：可以发布。
                return OwnerMarker::Reclaimable;
            }
            // 标记存在但无法解析（空文件/被篡改）。持有跨进程锁时，删除与重发之间
            // 没有别的合作者能插手；其它平台没有锁，损坏标记可能是另一个进程正在
            // 发布的空文件，宁可跳过也不能回收。
            if DIRECTORY_LOCK_IS_EXCLUSIVE {
                OwnerMarker::Reclaimable
            } else {
                OwnerMarker::LiveOther
            }
        }
    }
}

/// 原子地取得目录租约。
///
/// 返回 `Ok(Some(guard))` 表示当前进程已持有（新声明或复用进程内租约）；
/// `Ok(None)` 表示另一个仍存活的进程持有；`Err` 表示无法完成声明。
///
/// 顺序是“先拿跨进程锁，再看/写 `owner.json`”：陈旧标记的删除与替换只发生在持锁
/// 期间，因此不存在“读到死 pid 后直接 unlink，两个进程同时抢到目录”的 TOCTOU。
fn acquire_owner_lease(directory: &Path) -> Result<Option<OwnerLeaseGuard>, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("creating durable session directory: {error}"))?;
    let canonical = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());

    let mut leases = OWNER_LEASES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lease) = leases.get_mut(&canonical) {
        lease.refs += 1;
        return Ok(Some(OwnerLeaseGuard { canonical }));
    }

    let Some(lock) = try_lock_directory(&canonical)? else {
        return Ok(None);
    };
    let owner_path = canonical.join(OWNER_FILE);
    match classify_owner_marker(&owner_path) {
        // 标记已经是本进程的（pid 复用或上次释放不完整）：持锁后可以直接沿用。
        OwnerMarker::Ours => {}
        // 只有没有跨进程锁的平台才会走到这里（或损坏标记的保守处理）。
        OwnerMarker::LiveOther => {
            drop(lock);
            return Ok(None);
        }
        OwnerMarker::Reclaimable => {
            if publish_owner_marker(&canonical, &owner_path)? == MarkerOwnership::ForeignLive {
                drop(lock);
                return Ok(None);
            }
        }
    }
    leases.insert(
        canonical.clone(),
        OwnerLease {
            refs: 1,
            _lock: Some(lock),
        },
    );
    Ok(Some(OwnerLeaseGuard { canonical }))
}

/// 把带 pid 的临时文件原子发布为 `owner.json`。
///
/// 用 `hard_link` 的 no-replace 语义：目标已存在时原子失败，不会出现“检查时无主、
/// 写入时已被他人接管”的窗口。即便不支持硬链接，调用方也已经持有目录锁，回退到
/// `create_new` 不会与其它新版本进程竞争。
fn publish_owner_marker(directory: &Path, owner_path: &Path) -> Result<MarkerOwnership, String> {
    let sequence = OWNER_CLAIM_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp_path = directory.join(format!(
        ".{OWNER_FILE}.{}.{sequence}.tmp",
        std::process::id()
    ));
    std::fs::write(&temp_path, owner_contents())
        .map_err(|error| format!("writing owner claim: {error}"))?;
    let result = publish_temp_owner_file(&temp_path, owner_path);
    let _ = std::fs::remove_file(&temp_path);
    result
}

fn publish_temp_owner_file(temp_path: &Path, owner_path: &Path) -> Result<MarkerOwnership, String> {
    for _ in 0..8 {
        match std::fs::hard_link(temp_path, owner_path) {
            Ok(()) => return Ok(MarkerOwnership::Published),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match classify_owner_marker(owner_path) {
                    OwnerMarker::Ours => return Ok(MarkerOwnership::Published),
                    OwnerMarker::LiveOther => return Ok(MarkerOwnership::ForeignLive),
                    OwnerMarker::Reclaimable => match std::fs::remove_file(owner_path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("reclaiming stale owner marker: {error}"));
                        }
                    },
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return publish_owner_file_with_create_new(temp_path, owner_path);
            }
            Err(error) => return Err(format!("claiming session storage owner: {error}")),
        }
    }
    Err("session storage owner claim raced too many times".to_string())
}

fn publish_owner_file_with_create_new(
    temp_path: &Path,
    owner_path: &Path,
) -> Result<MarkerOwnership, String> {
    let contents =
        std::fs::read(temp_path).map_err(|error| format!("reading owner claim: {error}"))?;
    for _ in 0..8 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(owner_path)
        {
            Ok(mut file) => {
                file.write_all(&contents)
                    .map_err(|error| format!("writing owner marker: {error}"))?;
                file.flush()
                    .map_err(|error| format!("flushing owner marker: {error}"))?;
                return Ok(MarkerOwnership::Published);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match classify_owner_marker(owner_path) {
                    OwnerMarker::Ours => return Ok(MarkerOwnership::Published),
                    OwnerMarker::LiveOther => return Ok(MarkerOwnership::ForeignLive),
                    OwnerMarker::Reclaimable => match std::fs::remove_file(owner_path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("reclaiming stale owner marker: {error}"));
                        }
                    },
                }
            }
            Err(error) => return Err(format!("claiming session storage owner: {error}")),
        }
    }
    Err("session storage owner claim raced too many times".to_string())
}

/// 打开会话存储的结果。
enum OpenOutcome {
    Opened(Arc<DurableSessionSlot>),
    /// 另一个仍存活的进程持有该目录；扫描路径应当跳过，显式打开应当报错。
    OwnedByOtherProcess,
}

/// 一个会话的 durable 存储。
pub(crate) struct DurableSessionSlot {
    session: Arc<SessionImpl>,
    /// 持有槽位期间保持目录租约；最后一个槽位 drop 时释放。
    _owner: OwnerLeaseGuard,
}

impl DurableSessionSlot {
    /// 读取文档快照。
    pub(crate) async fn snapshot(&self, doc: &DocTarget) -> Result<Option<JsonObject>, String> {
        self.session
            .snapshot(doc, &**BACKGROUND_CONTEXT)
            .await
            .map_err(|error| error.to_string())
    }
}

/// 共享的 per-session durable 存储。
pub(crate) struct DurableSessionStore {
    root: PathBuf,
    sessions: tokio::sync::Mutex<HashMap<String, Arc<DurableSessionSlot>>>,
    /// 干净关停后置位：拒绝新的打开，避免在途任务在槽位清空后重新取得目录租约。
    closed: AtomicBool,
}

impl DurableSessionStore {
    /// 在 `root` 下为每个会话创建独立的 durable JSONL 存储。
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }

    /// 根目录。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 尽最大努力同步释放缓存的会话槽位。
    ///
    /// 槽位持有目录租约（`owner.json` + `flock`）。干净关停必须让出它们，否则接管
    /// 同一会话的下一个进程会一直看到 “owned by another live process”。`sessions`
    /// 是异步锁，而 `shutdown` 位于同步的 `dispose` 路径：这里用 `try_lock`，只有
    /// 同一时刻仍有并发打开时才会失败（返回 `false`，租约最终由进程退出释放）。
    pub(crate) fn release_sessions_blocking(&self) -> bool {
        // 先置位再清空：`slot_for` 在插入前会在同一个 state 锁下检查它，因此要么
        // 看到 closed 并放弃，要么在 clear 之前插入、随后被 clear 掉。
        self.closed.store(true, Ordering::SeqCst);
        match self.sessions.try_lock() {
            Ok(mut state) => {
                state.clear();
                true
            }
            Err(_) => false,
        }
    }

    /// 会话存储目录（根目录下的稳定目录名）。
    pub(crate) fn session_dir(&self, session_id: &str) -> PathBuf {
        // 目录名同时保留可读前缀与稳定哈希，避免不同 session id 在清洗后相撞。
        let mut sanitized: String = session_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        if sanitized.len() > 64 {
            sanitized.truncate(64);
        }
        // FNV-1a：稳定、无额外依赖，仅用于目录名去重。
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in session_id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3_u64);
        }
        PathBuf::from(format!("{sanitized}-{hash:016x}"))
    }

    /// 会话存储目录的完整路径。
    pub(crate) fn session_path(&self, session_id: &str) -> PathBuf {
        self.root.join(self.session_dir(session_id))
    }

    /// 打开一个已经计算好路径的会话存储。
    async fn open_directory(&self, directory: &Path) -> Result<OpenOutcome, String> {
        // 同进程的并发打开必须串行；否则两个调用会各自 open 一次 JSONL。
        let _serial = OPEN_SERIAL.lock().await;
        // 目录锁是唯一的权威：先拿租约，再查进程内 registry。不能先看 `owner.json`
        // 的 pid —— 崩溃进程的 pid 被复用时，空闲的目录会被误判为“他人持有”。
        let Some(owner) = acquire_owner_lease(directory)? else {
            return Ok(OpenOutcome::OwnedByOtherProcess);
        };
        let canonical = owner.canonical.clone();
        if let Some(session) = registered_session(&canonical) {
            return Ok(OpenOutcome::Opened(Arc::new(DurableSessionSlot {
                session,
                _owner: owner,
            })));
        }

        // 不能对路径做 lossy 转换：`flock`/`owner.json` 使用原始 `Path`，lossy 后的
        // 字符串可能指向另一个目录（两个非 UTF-8 root 甚至可能映射到同一个 JSONL），
        // 造成“锁一个目录、写另一个目录”。拿不到 UTF-8 路径时直接失败。
        let Some(directory_string) = directory.to_str().map(str::to_owned) else {
            return Err(format!(
                "durable session directory is not valid UTF-8: {}",
                directory.display()
            ));
        };
        let storage = open_node_jsonl_storage(
            &directory_string,
            &**BACKGROUND_CONTEXT,
            JsonlStorageOptions { fsync: true },
        )
        .await
        .map_err(|error| format!("opening durable session storage: {error}"))?;
        let session = Arc::new(create_session(Arc::new(storage)));
        register_session(canonical, &session);
        Ok(OpenOutcome::Opened(Arc::new(DurableSessionSlot {
            session,
            _owner: owner,
        })))
    }

    /// 扫描时打开一个已有目录；由另一个存活进程持有时返回 `None`（跳过）。
    async fn open_existing_directory(
        &self,
        directory: &Path,
    ) -> Result<Option<Arc<DurableSessionSlot>>, String> {
        match self.open_directory(directory).await? {
            OpenOutcome::Opened(slot) => Ok(Some(slot)),
            OpenOutcome::OwnedByOtherProcess => Ok(None),
        }
    }

    async fn open_session(&self, session_id: &str) -> Result<Arc<DurableSessionSlot>, String> {
        match self.open_directory(&self.session_path(session_id)).await? {
            OpenOutcome::Opened(slot) => Ok(slot),
            OpenOutcome::OwnedByOtherProcess => Err(format!(
                "durable session storage for {session_id} is owned by another live process"
            )),
        }
    }

    /// 在单实例的 sessions 表里按 session id 或目录名查找已打开的槽位。
    fn cached_slot(
        &self,
        state: &HashMap<String, Arc<DurableSessionSlot>>,
        name: &str,
    ) -> Option<Arc<DurableSessionSlot>> {
        state
            .iter()
            .find(|(key, _)| {
                key.as_str() == name || self.session_dir(key).to_string_lossy() == name
            })
            .map(|(_, slot)| Arc::clone(slot))
    }

    /// 取会话存储；必要时打开。
    pub(crate) async fn slot_for(
        &self,
        session_id: &str,
    ) -> Result<Arc<DurableSessionSlot>, String> {
        let directory_name = self.session_dir(session_id);
        let directory_key = directory_name.to_string_lossy().into_owned();
        {
            let state = self.sessions.lock().await;
            if self.closed.load(Ordering::SeqCst) {
                return Err(store_is_closed());
            }
            if let Some(slot) = state.get(session_id) {
                return Ok(Arc::clone(slot));
            }
            // `all_sessions`（重启 sweep）可能先按目录名登记过同一个存储；复用同一实例，
            // 避免两个 SessionImpl 同时写一个 JSONL 目录。
            if let Some(slot) = state.get(&directory_key) {
                return Ok(Arc::clone(slot));
            }
        }
        // 不能在持有 state 锁时 `await` 打开存储；先释放锁，再重新检查（双检）。
        let opened = self.open_session(session_id).await?;
        let mut state = self.sessions.lock().await;
        // 关停与打开并发时，`opened` 不能把租约带回已经清空的 store。
        if self.closed.load(Ordering::SeqCst) {
            return Err(store_is_closed());
        }
        if let Some(slot) = state.get(session_id) {
            return Ok(Arc::clone(slot));
        }
        if let Some(slot) = state.get(&directory_key) {
            return Ok(Arc::clone(slot));
        }
        Ok(state.entry(session_id.to_owned()).or_insert(opened).clone())
    }

    /// 打开根目录下所有已有的会话存储。
    ///
    /// 跳过由另一个仍存活的 kiana 进程持有的存储：`JsonlStorage::open` 会做尾部恢复，
    /// 与对方并发执行会破坏它的写入；等它退出后的下一次 sweep 再接管。
    ///
    /// 单个目录打开失败（损坏的 JSONL、权限问题）只跳过该目录，不让一个坏会话阻断
    /// 其它会话的 journal 操作；只有**全部**目录都打不开时才把错误返回给调用方。
    /// 扫描结果不写入 `self.sessions`：只读 sweep 不应把目录租约持有到进程结束。
    pub(crate) async fn all_sessions(
        &self,
    ) -> Result<Vec<(String, Arc<DurableSessionSlot>)>, String> {
        self.all_sessions_metered()
            .await
            .map(|(sessions, _skipped)| sessions)
    }

    /// 与 [`Self::all_sessions`] 相同，但额外返回被跳过的目录数。
    ///
    /// 被另一个存活进程持有、或损坏/权限不足而打不开的目录都会被跳过。需要把扫描
    /// 结果当作**完整账本**使用的调用方（例如子代理工作区的 keep-set）必须先检查
    /// 这个计数：部分扫描不能证明“某个 pid 不再被任何可恢复 Work 引用”。
    pub(crate) async fn all_sessions_metered(
        &self,
    ) -> Result<(Vec<(String, Arc<DurableSessionSlot>)>, usize), String> {
        // 扫描同样受 `closed` 门禁：清理之后在途的扫描不能重新获取 owner.json/flock
        // 租约，否则关停中的 store 会再次持有目录。
        if self.closed.load(Ordering::SeqCst) {
            return Err(store_is_closed());
        }
        let (names, mut skipped) = scan_session_directories(&self.root)?;
        let mut sessions = Vec::new();
        let mut first_error: Option<String> = None;
        for name in names {
            if self.closed.load(Ordering::SeqCst) {
                return Err(store_is_closed());
            }
            // `slot_for` 以 session id 为键，但目录名是清洗后的形态；这里直接用目录名作为
            // 打开用的 key 会重复哈希，因此直接构造路径打开。
            let directory = self.root.join(&name);
            let slot = {
                let state = self.sessions.lock().await;
                self.cached_slot(&state, &name)
            };
            let slot = match slot {
                Some(slot) => slot,
                None => match self.open_existing_directory(&directory).await {
                    Ok(Some(slot)) => slot,
                    Ok(None) => {
                        // 另一个存活进程持有目录租约；这次扫描看不到它的账本。
                        skipped += 1;
                        continue;
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                        skipped += 1;
                        continue;
                    }
                },
            };
            // 打开期间 store 被关停：刚拿到的租约交还，不能通过扫描结果带回调用方。
            if self.closed.load(Ordering::SeqCst) {
                return Err(store_is_closed());
            }
            sessions.push((name, slot));
        }
        if sessions.is_empty()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        Ok((sessions, skipped))
    }

    /// 在一个 durable 事务里提交变更。
    pub(crate) async fn commit(
        &self,
        session_id: &str,
        change: &dyn SessionChange,
    ) -> Result<Box<dyn std::any::Any + Send>, String> {
        let slot = self.slot_for(session_id).await?;
        slot.session
            .commit(change, &**BACKGROUND_CONTEXT, Default::default())
            .await
            .map_err(|error| error.to_string())
    }

    /// 读取一个会话的文档快照；会话存储不存在时返回 `None`，不会创建目录。
    pub(crate) async fn snapshot(
        &self,
        session_id: &str,
        doc: &DocTarget,
    ) -> Result<Option<JsonObject>, String> {
        if !self.session_path(session_id).exists() {
            return Ok(None);
        }
        let slot = self.slot_for(session_id).await?;
        slot.snapshot(doc).await
    }
}

/// 扫描 durable 根目录下的会话目录名。
///
/// 返回 `(names, skipped)`：`skipped` 统计“看起来可能是会话、但这次无法纳入扫描”
/// 的条目。调用方把扫描结果当作完整账本使用（例如子代理工作区的 keep-set），
/// 因此这里不能像 `read_dir` 的便捷迭代那样静默丢弃：
///
/// - 目录项读取失败、类型探测失败：磁盘/权限问题可能藏着一个会话目录；
/// - 非 UTF-8 目录名：无法无损表示，打开路径也可能分叉；
/// - 符号链接：不跟随。被克隆仓库里的 `sessions` 链接可能指向状态目录之外，
///   跟随会让扫描与清理越界。
///
/// 普通文件不是会话存储，直接忽略，不计入 `skipped`。
fn scan_session_directories(root: &Path) -> Result<(Vec<String>, usize), String> {
    let mut names = Vec::new();
    let mut skipped = 0usize;
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), 0));
        }
        Err(error) => {
            return Err(format!("reading durable store root: {error}"));
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if file_type.is_symlink() {
            // 不跟随：会话目录必须是状态目录下的真实目录。链接可能指向目录外，
            // 跟随会让 keep-set 扫描与工作区清理越出 durable root。
            skipped += 1;
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        match entry.file_name().into_string() {
            Ok(name) => names.push(name),
            Err(_) => skipped += 1,
        }
    }
    Ok((names, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_process_stores_share_one_session_impl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let first = DurableSessionStore::new(&root);
        let second = DurableSessionStore::new(&root);

        let left = first.slot_for("session-a").await.expect("first slot");
        let right = second.slot_for("session-a").await.expect("second slot");
        assert!(
            Arc::ptr_eq(&left.session, &right.session),
            "two stores in one process must share the JSONL SessionImpl"
        );

        let scanned = second.all_sessions().await.expect("scan");
        assert_eq!(scanned.len(), 1);
        assert!(Arc::ptr_eq(&scanned[0].1.session, &left.session));
    }

    #[tokio::test]
    async fn concurrent_slot_for_is_single_flight() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableSessionStore::new(temp.path().join("root"));
        let (left, right) = tokio::join!(store.slot_for("session-a"), store.slot_for("session-a"));
        let left = left.expect("left slot");
        let right = right.expect("right slot");
        assert!(Arc::ptr_eq(&left.session, &right.session));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_pids_are_not_interpreted_as_process_groups() {
        assert!(!process_is_alive(0));
        assert!(!process_is_alive(u32::MAX));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn slot_for_reclaims_an_invalid_owner_pid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableSessionStore::new(temp.path().join("root"));
        let directory = store.session_path("session-a");
        std::fs::create_dir_all(&directory).expect("directory");
        std::fs::write(
            directory.join(OWNER_FILE),
            format!(r#"{{"pid":{}}}"#, u32::MAX),
        )
        .expect("owner");

        let slot = store
            .slot_for("session-a")
            .await
            .expect("invalid pid must be reclaimable");
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            Some(std::process::id()),
            "the claim must be visible while a slot is alive"
        );
        drop(slot);
        // `slot_for` 的缓存仍持有租约；drop store 后最后一个引用消失。
        drop(store);
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            None,
            "dropping the last slot must release the owner marker"
        );
        let leftovers = std::fs::read_dir(&directory)
            .expect("directory")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "owner claim temp file must be removed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn owner_marker_lives_as_long_as_any_slot_reference() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let first = DurableSessionStore::new(&root);
        let second = DurableSessionStore::new(&root);
        let directory = first.session_path("session-a");

        let left = first.slot_for("session-a").await.expect("first slot");
        let right = second.slot_for("session-a").await.expect("second slot");
        assert!(Arc::ptr_eq(&left.session, &right.session));
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            Some(std::process::id())
        );

        // 槽位对象留在各自 store 的缓存里；释放租约要等最后一个 store 被 drop。
        drop(left);
        drop(right);
        drop(first);
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            Some(std::process::id()),
            "another store in the same process still holds the lease"
        );
        drop(second);
        assert_eq!(read_owner_pid(&directory.join(OWNER_FILE)), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scanning_does_not_pin_directories_for_the_process_lifetime() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableSessionStore::new(temp.path().join("root"));
        let directory = store.session_path("session-a");
        std::fs::create_dir_all(&directory).expect("directory");

        let scanned = store.all_sessions().await.expect("scan");
        assert_eq!(scanned.len(), 1);
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            Some(std::process::id())
        );
        drop(scanned);
        assert_eq!(
            read_owner_pid(&directory.join(OWNER_FILE)),
            None,
            "a read-only scan must not keep the lease after it returns"
        );
    }

    #[tokio::test]
    async fn scan_skips_a_directory_that_cannot_be_opened() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableSessionStore::new(temp.path().join("root"));
        let good = store.session_path("session-good");
        std::fs::create_dir_all(&good).expect("good directory");
        let bad = store.session_path("session-bad");
        // `main.jsonl` 是 JsonlStorage 打开的文件；把它做成目录会让 open 失败，
        // 用来模拟一个损坏的会话存储。
        std::fs::create_dir_all(bad.join("main.jsonl")).expect("bad directory");

        let scanned = store.all_sessions().await.expect("scan must survive");
        assert_eq!(scanned.len(), 1);
        assert_eq!(
            scanned[0].0,
            store.session_dir("session-good").to_string_lossy()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_released_store_rejects_new_opens() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableSessionStore::new(temp.path().join("root"));
        let slot = store.slot_for("session-a").await.expect("slot");
        drop(slot);

        assert!(store.release_sessions_blocking());
        let error = match store.slot_for("session-a").await {
            Ok(_) => panic!("a released store must not re-acquire the lease"),
            Err(error) => error,
        };
        assert!(error.contains("shut down"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn slot_for_rejects_a_directory_held_by_a_flock_holder() {
        use std::os::unix::io::AsRawFd as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableSessionStore::new(temp.path().join("root"));
        let directory = store.session_path("session-a");
        std::fs::create_dir_all(&directory).expect("directory");
        // 模拟另一个存活进程：真正持有 `owner.lock` 的 `flock`。同一进程里第二次
        // open 得到独立的 open file description，锁会真实互斥。
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(OWNER_LOCK_FILE))
            .expect("lock file");
        // SAFETY: 传标量；`lock` 在测试期间保持打开。
        let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert!(locked, "the test must hold the directory lock");

        let error = match store.slot_for("session-a").await {
            Ok(_) => panic!("another live process owns the store"),
            Err(error) => error,
        };
        assert!(error.contains("another live process"), "{error}");

        // 释放后可以接管：`owner.json` 的 pid 不再是权威。
        drop(lock);
        let slot = store
            .slot_for("session-a")
            .await
            .expect("a released directory must be reclaimable");
        drop(slot);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_utf8_state_dirs_are_rejected_instead_of_lossily_opened() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(std::ffi::OsString::from_vec(vec![
            b'r', 0xff, b'o', b'o', b't',
        ]));
        let store = DurableSessionStore::new(&root);
        let error = match store.slot_for("session-a").await {
            Ok(_) => panic!("a non-UTF-8 durable root must not be lossily opened"),
            Err(error) => error,
        };
        assert!(error.contains("not valid UTF-8"), "{error}");
    }

    /// 回归（round-16 finding #4）：无法纳入扫描的目录项必须计入 `skipped`，
    /// 否则上层会把部分账本当完整账本，sweep 误删其它会话可恢复的 worktree。
    #[test]
    fn scan_session_directories_counts_unrepresentable_entries() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        std::fs::create_dir_all(root.join("session-a")).expect("session dir");
        std::fs::create_dir_all(temp.path().join("target")).expect("target");
        std::os::unix::fs::symlink(temp.path().join("target"), root.join("linked"))
            .expect("symlink");
        std::fs::create_dir(root.join(std::ffi::OsString::from_vec(vec![
            b's', b'e', b's', b's', b'i', b'o', b'n', 0xff,
        ])))
        .expect("non-utf8 dir");
        std::fs::write(root.join("README"), "not a session").expect("regular file");

        let (mut names, skipped) = scan_session_directories(&root).expect("scan");
        names.sort();
        assert_eq!(
            names,
            vec!["session-a".to_string()],
            "a symlinked session directory must not be followed"
        );
        assert_eq!(
            skipped, 2,
            "non-UTF-8 names and symlinked directories must both be counted"
        );
    }

    #[tokio::test]
    async fn all_sessions_metered_stops_after_release() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let store = DurableSessionStore::new(&root);
        store.slot_for("session-a").await.expect("slot");
        assert!(store.release_sessions_blocking());

        // 清理之后在途的扫描不能重新拿目录租约。
        assert!(store.all_sessions_metered().await.is_err());
        assert!(store.all_sessions().await.is_err());
    }
}
