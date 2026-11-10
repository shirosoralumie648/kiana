//! `FileModelsStore` / `InMemoryCodingAgentModelsStore`。
//!
//! Locked JSON-backed storage for dynamically refreshed provider catalogs
//! （TS: `core/models-store.ts`）。
//!
//! `FileModelsStore` 复用 [`crate::auth::storage::FileAuthStorageBackend`] 的锁与 0600/0700
//! 语义（首次加锁会 `ensureParentDir()` + 以 `{}` 创建文件）；并发读按路径共享一次真实重载
//! （`getFileRevision` 相同则复用快照，读者计数归零时 abort 该次重载）。
//!
//! 投影差异（冻结的 [`ModelsStoreEntry`] 是结构体，TS 的条目是任意 JSON 对象）：
//! - `read` 按 `parse_entry_lenient` 投影：无法表达的已知字段暂存在 `extra` 里，
//!   `models` 中的未知类型模型只保留在 `raw_models`（写回时逐字恢复）；
//! - `write` 用同文件旧条目的键序写出（TS 的 `{...stored, checkedAt}` 语义，
//!   见 `reorder_like_previous`）。
//!
//! # 为什么需要这个文件
//!
//! 模型目录会随远端刷新变化，需要按 provider 整条替换并落到 `models-store.json`，同时让
//! 同一进程里指向同一路径的多个实例看到同一份快照。TS 的条目是任意 JSON 对象，Rust 侧
//! 的 [`ModelsStoreEntry`] 是冻结结构体，因此本文件额外承担“JSON ⇄ 结构体”的投影与键序
//! 恢复（见 `parse_entry_lenient` / `entry_to_value` / `reorder_like_previous`）。
//!
//! # 在系统中的位置
//!
//! - 构造方：`crate::models::runtime::ModelRuntime::create` 在调用方未注入 `models_store`
//!   时，按 modelsPath 是否为空二选一：非空用 `FileModelsStore`（路径默认取 models.json
//!   同目录的 `models-store.json`），为空用 `InMemoryCodingAgentModelsStore`。CLI 的
//!   auth 检查与部分测试会显式注入内存实现。
//! - 调用方：kiana-ai 的模型刷新/发布流程通过 `ModelsStore` trait 使用本类型——`read` 取
//!   上一条目录（`etag` 作为 `If-None-Match`、`lastModified` 判断远端新旧），`write` /
//!   `delete` 在发布阶段落盘。
//!
//! # FileModelsStore 的一次 read
//!
//! 1. 先检查调用方信号；已中止直接返回中止错误。
//! 2. 用 `get_file_revision` 取当前文件令牌，与共享读状态里的 `revision` 比较；相同则直接
//!    返回缓存文档（只做一次 metadata 查询，不读内容、不加锁）。文件不存在时
//!    `get_file_revision` 返回 `None`，缓存永远不命中，总是走第 3 步。
//! 3. 若该路径没有在途重载就启动一个（同一路径同一时刻至多一个），把自己计为一个读者，
//!    等待共享结果。调用方信号中止只放弃**本次等待**，不会取消其他读者仍在使用的重载。
//! 4. 最后一个离开的读者在槽位仍指向该重载（`Arc::ptr_eq`）时，把它从共享槽位移除并
//!    abort 它的信号；若重载已由任务结束清槽、新读者又安装了新重载，则只放弃自己的等待，
//!    既不移除槽位也不 abort 旧 controller。
//! 5. 拿到文档后按 TS 的顺序执行：取属性（顶层是 `null` 时抛 `TypeError`）→ 再检查一次
//!    中止 → 缺失/假值返回 `None` → 宽松投影。
//!
//! # 并发与缓存
//!
//! 共享读状态按**规范化后的路径**存在进程级表里（保存 `Weak` 引用，新建状态时顺带清理
//! 已失效项），因此两个 `FileModelsStore::new(同一路径)` 会共享缓存与在途重载。文件写入
//! 仍由 `FileAuthStorageBackend` 的文件锁串行化；缓存新鲜度只依赖 revision 令牌，进程外
//! 写入只要改变文件元数据就会在第 2 步被发现。`write`/`delete` 成功后把最新文档放进缓存
//! 但把 revision 置空，所以下一次 read 仍会重新加锁读取（缓存只在 revision 匹配时复用）。
//!
//! # 错误与边界
//!
//! - `AuthError` → `AiError` 的映射只区分中止（`name == "AbortError"` →
//!   `AiErrorKind::Aborted`）与其它错误（`Other`），`name`/`message` 原样保留。`StorageDocument`
//!   抛出的 `SyntaxError` / `TypeError` 因此仍能按 TS 的 `error.name` 分支。
//! - `write` 是整条替换，不做字段级合并；顶层文档不是对象时按 JS 属性语义处理（数组上
//!   的命名属性被静默丢弃、`null` 抛 `TypeError` 等，见 `StorageDocument::set_property`）。
//! - 无法投影的数据不会让 `read` 失败：见 `parse_entry_lenient` 的说明。
//! - 第一次加锁会由后端创建父目录与空文件 `{}`（权限语义见 `FileAuthStorageBackend`）；
//!   也就是说“读一个不存在的 store”会留下一个空文件。

// `kiana_ai::AiError` 的 Err 变体 >= 128 字节。ARCHITECTURE §4.1 固定了 `AiError` 的公开形状
// （`Display` == TS 的 `error.message`，字段不能装箱），kiana-ai 的 ModelsStore trait 也以同一
// 理由关闭该 lint，因此本文件跟随。
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::AuthOperationOptions;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::models::{ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use kiana_ai::utils::abort::{AbortReason, AbortScope, race_with_abort_signal};
use kiana_core::config::get_agent_dir;
use kiana_core::paths::{PathInputOptions, get_file_revision, normalize_path};
use tokio::sync::watch;

use crate::auth::storage::{
    AuthStorageBackend, FileAuthStorageBackend, LockResult, StorageDocument,
};

/// `FileModelsStore` 的默认路径：`join(getAgentDir(), "models-store.json")`。
///
/// `getAgentDir()` 每次调用都读环境变量（`KIANA_CODING_AGENT_DIR`，空串视为未设置），
/// 未设置时是 `<home>/.kiana/agent`。本函数自身不调用 `normalize_path`：`get_agent_dir`
/// 对非空环境变量会先做 `expand_tilde_path`（`~` 展开、`file://` 转换、Windows shell 路径），
/// 默认分支只做 `path.join` 的词法折叠；`FileModelsStore::new` 还会在此基础上再规范化一次。
pub fn default_models_store_path() -> String {
    get_agent_dir()
        .join("models-store.json")
        .to_string_lossy()
        .into_owned()
}

/// 取 `std::sync::Mutex` 锁；中毒时取回内部值继续。
///
/// 锁保护的是可重建的快照与槽位（缓存文档、在途重载），锁内只做单次表操作或克隆，
/// 不跨 await；因一次 panic 就永久拒绝访问会让之后所有读取都失败，所以忽略中毒标志。
fn lock_state<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `signal?.throwIfAborted()`（ModelsStore 的错误形态）。
///
/// 与 `kiana_ai::auth::types::throw_if_aborted` 语义相同，但返回本 trait 契约用的
/// [`AiError`]：有 reason 时 `name`/`message` 取自 reason，没有 reason 时用
/// `AbortReason::fallback_abort_error`（`AbortError` / "The operation was aborted"）。
/// read 路径的入口（以及内存实现的 `read`/`write`/`delete`）先调用它，保证“已中止”不会被
/// 缺失条目或假值条目的分支吞掉；文件后端的 `write`/`delete` 不经过本函数，其取消依赖
/// `AuthStorageBackend::with_lock_async` 的等价检查（进锁后、回调前与写盘前）。
fn throw_if_aborted(options: &ModelsStoreOperationOptions) -> Result<(), AiError> {
    let Some(signal) = &options.signal else {
        return Ok(());
    };
    if !signal.is_aborted() {
        return Ok(());
    }
    let reason = signal
        .reason()
        .unwrap_or_else(AbortReason::fallback_abort_error);
    Err(reason.to_error())
}

/// 后端锁错误 → `AiError`（`Display` 保持 `error.message`）。
///
/// 只按 `name == "AbortError"` 分类：中止必须能被 `AiError::is_aborted` 识别，其余错误
/// （文件锁失败、JSON 解析失败、JS 属性语义的 `TypeError` 等）一律归为 `Other` 以保留
/// 原始文本。`AuthError` 的 `code`/`error_code`/`details` 不会带过来，只保留 `name` 与
/// `message`。
fn to_ai_error(error: kiana_ai::auth::types::AuthError) -> AiError {
    let kind = if error.name == "AbortError" {
        AiErrorKind::Aborted
    } else {
        AiErrorKind::Other
    };
    AiError::new(kind, error.message).with_name(error.name)
}

/// 把存储的 JSON 条目投影为 [`ModelsStoreEntry`]（**不因无法表达的字段而失败**）。
///
/// TS 的 `read` 是 `entry ? structuredClone(entry) : undefined`：任意 JSON 都原样返回。
/// 冻结的 [`ModelsStoreEntry`] 只能表达 `models: AnyModel[]`、整数的 `lastModified`/
/// `checkedAt` 与字符串 `etag`。为尽量贴近 TS：
/// - 无法表达的**已知字段**（显式 `null`、非整数数字、非字符串 `etag`）暂存进
///   [`ModelsStoreEntry::extra`]：类型化字段为 `None`，序列化时逐字写回，因此 read → write
///   往返既不丢字段也不丢显式 `null`；调用方随后给类型化字段赋值时，
///   [`entry_to_value`] 会丢弃同名的暂存值。
/// - `models` 不是数组时按空数组处理（冻结结构无法表达，TS 也不消费这种条目）。
/// - 条目不是对象（TS 会原样返回标量/数组）→ `None`。
///
/// # 处理步骤（与代码分支一一对应）
///
/// 1. 只接受对象；其余 JSON 形状返回 `None`。
/// 2. 把“已知但无法被类型化字段表达”的值暂存并从待反序列化对象中删除：小数、超出 i64 的
///    数字与非字符串 `etag` 不移除会让整条条目反序列化失败；显式 `null` 本可被 serde 按
///    缺失处理接受，暂存它是为了写回时保留 `null` 的字节形状。
/// 3. 非 `null` 的 `models` 若不是数组则移除，交给 serde 得到空数组；`null` 由 serde
///    直接当作空数组接受。
/// 4. 反序列化失败（例如对象里还有其它类型错误）返回 `None`，不向上报错。
/// 5. 最后按原始对象的键序重建 `extra`：未知字段取自反序列化结果，暂存的已知字段按原位置
///    插回。因此 read → write 既不丢字段，也不把键序打乱。
fn parse_entry_lenient(value: &serde_json::Value) -> Option<ModelsStoreEntry> {
    let serde_json::Value::Object(map) = value else {
        return None;
    };
    let mut sanitized = map.clone();
    let mut stashed: IndexMap<String, serde_json::Value> = IndexMap::new();
    for key in ["lastModified", "checkedAt"] {
        match sanitized.get(key) {
            None => {}
            Some(serde_json::Value::Number(number)) if number.as_i64().is_some() => {}
            Some(other) => {
                stashed.insert(key.to_string(), other.clone());
                sanitized.shift_remove(key);
            }
        }
    }
    match sanitized.get("etag") {
        None | Some(serde_json::Value::String(_)) => {}
        Some(other) => {
            stashed.insert("etag".to_string(), other.clone());
            sanitized.shift_remove("etag");
        }
    }
    if sanitized
        .get("models")
        .is_some_and(|models| !models.is_array() && !models.is_null())
    {
        sanitized.shift_remove("models");
    }
    let mut entry: ModelsStoreEntry =
        serde_json::from_value(serde_json::Value::Object(sanitized)).ok()?;
    // 按原始键序重建 extra（未知字段 + 暂存的已知字段）。
    let mut extra: IndexMap<String, serde_json::Value> = IndexMap::new();
    for key in map.keys() {
        match key.as_str() {
            "models" => {}
            "lastModified" | "checkedAt" | "etag" => {
                if let Some(value) = stashed.shift_remove(key) {
                    extra.insert(key.clone(), value);
                }
            }
            _ => {
                if let Some(value) = entry.extra.shift_remove(key) {
                    extra.insert(key.clone(), value);
                }
            }
        }
    }
    entry.extra = extra;
    Some(entry)
}

/// [`ModelsStoreEntry`] → JSON（与冻结的 `Serialize` 相同，但类型化字段优先于 `extra`
/// 中的同名暂存值，避免显式 `null`/旧值覆盖调用方刚设置的值）。
///
/// 只清理三个可选字段（`lastModified`/`checkedAt`/`etag`）；`models` 不在清理范围内——
/// 手工把 `models` 放进 `extra` 会写出重复键（这一点由 [`ModelsStoreEntry::extra`] 的
/// 文档约定，读取路径不会产生这种条目）。序列化本身对结构体字段总是成功，返回
/// `Err` 仅用于防御性处理。
fn entry_to_value(entry: &ModelsStoreEntry) -> Result<serde_json::Value, AiError> {
    let mut sanitized = entry.clone();
    if sanitized.last_modified.is_some() {
        sanitized.extra.shift_remove("lastModified");
    }
    if sanitized.checked_at.is_some() {
        sanitized.extra.shift_remove("checkedAt");
    }
    if sanitized.etag.is_some() {
        sanitized.extra.shift_remove("etag");
    }
    serde_json::to_value(&sanitized)
        .map_err(|error| AiError::new(AiErrorKind::Other, error.to_string()))
}

/// 沿用上一个条目对象的键序（TS 的 `{...stored, checkedAt}` 保留 `stored` 的键序并把新键
/// 追加在末尾）。冻结的 `ModelsStoreEntry` 是结构体，丢掉了调用方对象的插入序；文件里已有的
/// 条目是唯一可恢复的顺序来源。新条目（文件里尚无该 provider）保持冻结序列化顺序
/// （`models, lastModified, checkedAt, etag, …extra`），与 TS 的常见字面量顺序一致。
///
/// 具体规则：`previous` 不是对象时原样返回；否则先按 `previous` 的键序写出“新值里也存在
/// 的键”，再把新值里剩余的键按新值自身顺序追加。旧条目独有的键不会复活——`write` 是
/// 整条替换。
fn reorder_like_previous(
    value: serde_json::Value,
    previous: Option<&serde_json::Value>,
) -> serde_json::Value {
    let Some(serde_json::Value::Object(previous_map)) = previous else {
        return value;
    };
    let serde_json::Value::Object(value_map) = value else {
        return value;
    };
    let mut ordered = serde_json::Map::new();
    for key in previous_map.keys() {
        if let Some(entry) = value_map.get(key) {
            ordered.insert(key.clone(), entry.clone());
        }
    }
    for (key, entry) in value_map {
        if !ordered.contains_key(&key) {
            ordered.insert(key, entry);
        }
    }
    serde_json::Value::Object(ordered)
}

/// 进程内模型目录存储。
///
/// TS: `models-store.ts#InMemoryCodingAgentModelsStore`
///
/// 进程内、非持久化：进程退出即丢失，每个 `new()` 得到独立的表，实例之间不共享任何状态。
/// 表由一把 `std::sync::Mutex` 保护，三个方法里都没有 await 点，锁不会跨 await 持有；
/// 锁中毒时用 `into_inner` 继续（表里只有整条条目的插入/删除/克隆）。`read` 返回克隆，
/// 调用方改写结果不会影响存储；`write` 整条替换该 provider；`delete` 对不存在的条目
/// 静默成功（幂等）。每个方法只在入口检查一次中止信号，检查通过后同步完成、不会因随后
/// 的 abort 回滚。
#[derive(Default)]
pub struct InMemoryCodingAgentModelsStore {
    entries: Mutex<IndexMap<String, ModelsStoreEntry>>,
}

impl InMemoryCodingAgentModelsStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ModelsStore for InMemoryCodingAgentModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        throw_if_aborted(&options)?;
        Ok(lock_state(&self.entries).get(provider_id).cloned())
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        throw_if_aborted(&options)?;
        lock_state(&self.entries).insert(provider_id.to_string(), entry.clone());
        Ok(())
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        throw_if_aborted(&options)?;
        lock_state(&self.entries).shift_remove(provider_id);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FileModelsStore
// ---------------------------------------------------------------------------

/// 一次在途的文件重载（同一路径同一时刻至多一个）。
///
/// `controller` 用于取消重载自身的加锁/读取；`result` 是 `watch` 通道，结果只发送一次
/// （`Some(结果)`），所有等待者克隆 receiver 读取同一个值；`readers` 记录正在等待的读者
/// 数，最后一个离开的读者在槽位仍指向它（`Arc::ptr_eq`）时把它清空并 abort 其 controller；
/// 否则（槽位已被重载任务清空或换了新重载）只放弃自己的等待，不动槽位。
struct ModelsFileReload {
    controller: AbortScope,
    result: watch::Receiver<Option<Result<StorageDocument, AiError>>>,
    readers: AtomicUsize,
}

/// 一个路径的缓存快照、快照对应的文件 revision 与在途重载槽位。
///
/// `revision == None` 表示“不知道这份快照对应哪个文件版本”（初始状态或刚刚写入），此时
/// `read_latest` 不会走缓存快路径。`reload` 由 `ensure_reload` 安装，由重载任务结束时
/// 或最后一个读者离开时清空（清空前用 `Arc::ptr_eq` 确认槽位仍是自己）。
#[derive(Default)]
struct ModelsReadState {
    data: StorageDocument,
    revision: Option<String>,
    reload: Option<Arc<ModelsFileReload>>,
}

/// 进程内按路径共享的读状态（TS 的 `sharedModelsFileReadState`，按路径索引）。
///
/// 值是 `Weak`，不会延长状态的寿命：没有 `FileModelsStore` 与其在途重载引用时状态自然
/// 回收。整张表由一把短临界区的 `std::sync::Mutex` 保护，锁内不做 IO。
static SHARED_MODELS_READ_STATES: LazyLock<Mutex<HashMap<String, Weak<Mutex<ModelsReadState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 取某路径的共享读状态；没有存活实例时新建。
///
/// 已有的弱引用能 upgrade 就复用（因此同一路径的不同 `FileModelsStore` 看到同一缓存）；
/// 否则先清理所有已失效的弱引用再插入新状态——清理只在新建时发生，避免每次读取都遍历
/// 整表。
fn shared_read_state(path: &str) -> Arc<Mutex<ModelsReadState>> {
    let mut states = lock_state(&SHARED_MODELS_READ_STATES);
    if let Some(existing) = states.get(path).and_then(Weak::upgrade) {
        return existing;
    }
    states.retain(|_, weak| weak.strong_count() > 0);
    let state = Arc::new(Mutex::new(ModelsReadState::default()));
    states.insert(path.to_string(), Arc::downgrade(&state));
    state
}

/// Locked JSON-backed storage for dynamically refreshed provider catalogs。
///
/// TS: `models-store.ts#FileModelsStore`
///
/// # 要点
///
/// - 路径在构造时规范化一次，之后同时用作 `FileAuthStorageBackend` 的路径与进程级共享读
///   状态的键；指向同一路径的多个实例共享缓存与在途重载。
/// - 读走“revision 快路径 + 共享重载”，写/删总是获取文件锁并整份读写（见模块文档）。
/// - 错误映射：`AbortError` → `AiErrorKind::Aborted`，其余 → `Other`，文本保持底层
///   message（文件锁、JSON 解析、JS 属性语义错误）。
/// - `Debug` 只打印路径，不打印共享缓存中的文档。
pub struct FileModelsStore {
    storage: AuthStorageBackend,
    path: String,
    read_state: Arc<Mutex<ModelsReadState>>,
}

// 只打印路径：共享缓存里的文档可能很大，且属于运行时目录内容，Debug 不把它带进日志。
impl std::fmt::Debug for FileModelsStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileModelsStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileModelsStore {
    /// `new FileModelsStore(path = join(getAgentDir(), "models-store.json"))`。
    ///
    /// 传入路径先经 `normalize_path`（默认选项只展开 `~`，不 trim、不去 `@` 前缀；Windows
    /// 上还会转换 shell 风格路径）规范化，得到的字符串既作为后端路径，也作为共享读状态的
    /// 键。构造不访问文件系统：目录与文件在第一次加锁时才创建。
    pub fn new(path: &str) -> Self {
        let path = normalize_path(path, &PathInputOptions::default());
        let storage = AuthStorageBackend::File(FileAuthStorageBackend::new(&path));
        let read_state = shared_read_state(&path);
        Self {
            storage,
            path,
            read_state,
        }
    }

    /// 默认路径版本。
    ///
    /// 等价于 `Self::new(&default_models_store_path())`；默认路径的解析规则见该函数。
    pub fn with_default_path() -> Self {
        Self::new(&default_models_store_path())
    }

    /// 文件路径。
    ///
    /// 返回构造时规范化后的路径，与共享读状态的键一致；不是调用方传入的原始字符串。
    pub fn path(&self) -> &str {
        &self.path
    }

    /// 用一份新文档覆盖共享缓存。
    ///
    /// `revision = None` 表示缓存对应的文件版本未知：`read_latest` 只在 revision 匹配时
    /// 才用缓存，所以写入后的下一次读会重新加锁读取，而不是信任这份刚写入的快照。
    fn update_read_state(&self, data: StorageDocument, revision: Option<String>) {
        let mut state = lock_state(&self.read_state);
        state.data = data;
        state.revision = revision;
    }

    /// 在文件锁内重新解析整份文档，并把结果与当前 revision 写进共享读状态。
    ///
    /// 返回的 future 是 `Send + 'static`：它会被 `ensure_reload` 移进独立的 tokio 任务。
    /// 加锁/读取使用传入的共享 `AbortScope`（不是某个调用方的信号），回调返回只读的
    /// `LockResult`，因此本路径不会写文件；但后端在首次加锁时可能创建父目录与空文件。
    fn reload_from_storage(
        &self,
        read_state: Arc<Mutex<ModelsReadState>>,
        options: ModelsStoreOperationOptions,
    ) -> impl std::future::Future<Output = Result<StorageDocument, AiError>> + Send + 'static {
        let storage = self.storage.clone();
        let path = self.path.clone();
        async move {
            storage
                .with_lock_async(
                    move |content| {
                        let read_state = Arc::clone(&read_state);
                        let path = path.clone();
                        Box::pin(async move {
                            let data =
                                StorageDocument::parse(content.as_deref()).map_err(to_ai_error)?;
                            let revision = get_file_revision(&path);
                            {
                                let mut state = lock_state(&read_state);
                                state.data = data.clone();
                                state.revision = revision;
                            }
                            Ok(LockResult::new(data))
                        })
                    },
                    &AuthOperationOptions {
                        signal: options.signal,
                    },
                )
                .await
                .map_err(to_ai_error)
        }
    }

    /// 取该路径的在途重载；没有则启动一个。
    ///
    /// 全程持有读状态锁，保证检查与安装是原子的（同一路径不会同时出现两个重载）。
    /// 重载使用新建的 `AbortScope`：单个调用方取消只影响它自己的等待，最后一个读者离开时
    /// 才通过该 scope 取消重载。
    fn ensure_reload(&self) -> Arc<ModelsFileReload> {
        let mut state = lock_state(&self.read_state);
        if let Some(reload) = &state.reload {
            return Arc::clone(reload);
        }

        let controller = AbortScope::new();
        let (sender, receiver) = watch::channel(None);
        let reload = Arc::new(ModelsFileReload {
            controller: controller.clone(),
            result: receiver,
            readers: AtomicUsize::new(0),
        });

        let task_reload = Arc::clone(&reload);
        let task_read_state = Arc::clone(&self.read_state);
        let future = self.reload_from_storage(
            Arc::clone(&self.read_state),
            ModelsStoreOperationOptions {
                signal: Some(controller),
            },
        );
        tokio::spawn(async move {
            let result = future.await;
            // 先广播结果，再尝试清槽位：等待者拿到值后不依赖槽位状态。
            let _ = sender.send(Some(result));
            let mut state = lock_state(&task_read_state);
            // 只清自己：若槽位已被更新的重载替换，不能把后来者清掉。
            if state
                .reload
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &task_reload))
            {
                state.reload = None;
            }
        });

        state.reload = Some(Arc::clone(&reload));
        reload
    }

    /// 等待共享重载的结果。
    ///
    /// 先同步 `borrow()` 一次：调用时结果若已就绪就直接返回（`watch::Receiver` 克隆总能
    /// 看到当前值），避免漏掉“重载已结束、通知发生在等待之前”的情况。发送端被丢弃视为
    /// 内部错误——正常路径下只有重载任务持有 sender，且它总会在结束时发送一次。
    async fn wait_reload(reload: Arc<ModelsFileReload>) -> Result<StorageDocument, AiError> {
        let mut receiver = reload.result.clone();
        loop {
            {
                let borrowed = receiver.borrow();
                if let Some(result) = borrowed.as_ref() {
                    return result.clone();
                }
            }
            if receiver.changed().await.is_err() {
                return Err(AiError::new(
                    kiana_ai::error::AiErrorKind::Other,
                    "Models store reload channel closed",
                ));
            }
        }
    }

    /// 取当前最新文档：revision 未变时用缓存，否则加入/发起一次共享重载。
    ///
    /// 读者计数在进入重载等待前加一、离开后减一；即使调用方的信号中止、等待提前返回，
    /// 也必须减计数（否则重载永远不会被回收）。计数归零只清空“仍是自己”的槽位，避免误杀
    /// 后来安装的新重载。
    async fn read_latest(
        &self,
        options: &ModelsStoreOperationOptions,
    ) -> Result<StorageDocument, AiError> {
        throw_if_aborted(options)?;
        // 快路径：revision 相同说明文件没变，直接克隆缓存文档。revision 为 None（文件不
        // 存在或元数据读取失败）时不信任缓存。
        let revision = get_file_revision(&self.path);
        if let Some(revision) = &revision {
            let state = lock_state(&self.read_state);
            if state.revision.as_deref() == Some(revision.as_str()) {
                return Ok(state.data.clone());
            }
        }

        // 加入或发起共享重载，并登记为读者。同一路径的并发读共享同一次文件读取。
        let reload = self.ensure_reload();
        reload.readers.fetch_add(1, Ordering::SeqCst);
        // 调用方信号只限制本次等待；中止后仍要走到下面的计数回收。
        let awaited = match options.signal.as_ref() {
            Some(signal) => {
                match race_with_abort_signal(Self::wait_reload(Arc::clone(&reload)), signal).await {
                    Ok(result) => result,
                    Err(error) => Err(error),
                }
            }
            None => Self::wait_reload(Arc::clone(&reload)).await,
        };

        // 最后一个读者负责把重载从槽位移除并 abort 它的信号（取消这次重载）。
        let remaining = reload.readers.fetch_sub(1, Ordering::SeqCst) - 1;
        if remaining == 0 {
            let mut state = lock_state(&self.read_state);
            if state
                .reload
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &reload))
            {
                state.reload = None;
                reload.controller.abort();
            }
        }
        awaited
    }

    /// 在文件锁内替换该 provider 的条目并在仍持锁时写回整份文档。
    ///
    /// 与 `read` 不同，这里不能信任共享缓存：文件可能已被其它进程改写，必须在锁内重新
    /// 解析。回调返回 `LockResult::with_next`，未被取消、锁未被判破坏时，后端才会在释放
    /// 锁之前写入新的整份内容；回调后若已取消或锁被破坏，`next` 不提交，文件保持原样并
    /// 返回错误（含 `AbortError`）。回调失败（解析错误、JS 属性赋值错误）时文件保持原样，
    /// 缓存也不更新。
    async fn write_entry(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        let latest: Arc<Mutex<Option<StorageDocument>>> = Arc::new(Mutex::new(None));
        let latest_written = Arc::clone(&latest);
        let provider_id = provider_id.to_string();
        self.storage
            .with_lock_async(
                move |content| {
                    let latest = Arc::clone(&latest_written);
                    let provider_id = provider_id.clone();
                    Box::pin(async move {
                        // 锁内重新解析，不能复用 read_latest 的缓存快照。
                        let mut current =
                            StorageDocument::parse(content.as_deref()).map_err(to_ai_error)?;
                        // TS: `current[providerId] = structuredClone(entry)`；键序来自 entry
                        // 对象本身。结构体无法携带插入序，用同文件的旧条目恢复（见
                        // `reorder_like_previous`）。
                        // 旧条目只用于恢复键序；用 `as_object()` 读取，避免对 null/标量
                        // 文档提前触发 “Cannot read properties of null” —— TS 的直接赋值
                        // 会在 `set_property` 里给出 “Cannot set properties of null”。
                        // 文档不是对象时 `previous` 为 None，随后由 `set_property` 给出与
                        // TS 一致的属性语义（数组静默丢弃命名属性、数字/字符串/null 抛
                        // TypeError）。
                        let previous = current
                            .as_object()
                            .and_then(|data| data.get(&provider_id))
                            .cloned();
                        let value = entry_to_value(&entry)?;
                        let value = reorder_like_previous(value, previous.as_ref());
                        current
                            .set_property(&provider_id, value)
                            .map_err(to_ai_error)?;
                        let next = current.to_json_string();
                        *lock_state(&latest) = Some(current);
                        Ok(LockResult::with_next((), next))
                    })
                },
                &AuthOperationOptions {
                    signal: options.signal,
                },
            )
            .await
            .map_err(to_ai_error)?;
        // 只有整段“加锁 → 读 → 回调 → 写回”成功后才更新共享缓存。
        if let Some(latest) = lock_state(&latest).clone() {
            self.update_read_state(latest, None);
        }
        Ok(())
    }
}

/// `read` 的「读取之后」段：先取属性（可能对 null 文档抛 TypeError），再做中止检查
/// （**在任何缺失/假值判定之前**，TS: models-store.ts:120-124），最后投影。
///
/// 这个顺序是语义的一部分：已中止的调用即使条目存在、或条目本身是假值，也必须报中止而
/// 不是成功/`None`；本文件的 `mod tests` 里两个单元测试分别锁定缺失条目与假值条目。
/// 顶层文档不是对象时 `get` 按 JS 属性语义工作：数组/字符串只认 `length` 与整数索引
/// （provider id 恰好是数字串时可能取到元素），数字/布尔返回 `None`，`null` 直接报错。
fn read_entry_from_document(
    data: &StorageDocument,
    provider_id: &str,
    options: &ModelsStoreOperationOptions,
) -> Result<Option<ModelsStoreEntry>, AiError> {
    let value = data.get(provider_id).map_err(to_ai_error)?;
    // 顺序固定：先检查信号，再把缺失/假值当作“没有条目”。
    throw_if_aborted(options)?;
    let Some(value) = value else {
        return Ok(None);
    };
    // TS: `return entry ? structuredClone(entry) : undefined`（假值按缺失处理）。
    // JSON 中没有 undefined：`null`/`false`/`0`/`""` 都按缺失处理，`{}`/`[]` 是真值。
    if !kiana_json::js_string::is_truthy(Some(&value)) {
        return Ok(None);
    }
    Ok(parse_entry_lenient(&value))
}

#[async_trait]
impl ModelsStore for FileModelsStore {
    async fn read(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<Option<ModelsStoreEntry>, AiError> {
        let data = self.read_latest(&options).await?;
        read_entry_from_document(&data, provider_id, &options)
    }

    async fn write(
        &self,
        provider_id: &str,
        entry: ModelsStoreEntry,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        self.write_entry(provider_id, entry, options).await
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: ModelsStoreOperationOptions,
    ) -> Result<(), AiError> {
        let latest: Arc<Mutex<Option<StorageDocument>>> = Arc::new(Mutex::new(None));
        let latest_written = Arc::clone(&latest);
        let provider_id = provider_id.to_string();
        self.storage
            .with_lock_async(
                move |content| {
                    let latest = Arc::clone(&latest_written);
                    let provider_id = provider_id.clone();
                    Box::pin(async move {
                        // 锁内重新解析；`delete` 对不存在的键静默成功，顶层 `null` 会抛
                        // TypeError（与 TS 的 `delete document[key]` 一致）。
                        let mut current =
                            StorageDocument::parse(content.as_deref()).map_err(to_ai_error)?;
                        current.delete(&provider_id).map_err(to_ai_error)?;
                        let next = current.to_json_string();
                        *lock_state(&latest) = Some(current);
                        Ok(LockResult::with_next((), next))
                    })
                },
                &AuthOperationOptions {
                    signal: options.signal,
                },
            )
            .await
            .map_err(to_ai_error)?;
        // 与 write 相同：锁内写回成功后才更新共享缓存，revision 置空。
        if let Some(latest) = lock_state(&latest).clone() {
            self.update_read_state(latest, None);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::storage::AuthStorageData;

    fn aborted_options() -> ModelsStoreOperationOptions {
        let signal = AbortScope::new();
        signal.abort();
        ModelsStoreOperationOptions {
            signal: Some(signal),
        }
    }

    /// `read` 的 `throwIfAborted` 在缺失/假值判定之前：缺失条目 + 已中止的信号必须报
    /// `AbortError`（TS: models-store.ts:120-124）。
    #[test]
    fn missing_entry_still_observes_an_aborted_signal() {
        let document = StorageDocument::from_data(AuthStorageData::new());
        let options = aborted_options();
        let error = read_entry_from_document(&document, "missing", &options).unwrap_err();
        assert_eq!(error.name, "AbortError");
        assert!(error.is_aborted());
    }

    /// 假值条目（`null`/`false`/`0`/`""`）同样先检查中止信号。
    #[test]
    fn falsy_entry_still_observes_an_aborted_signal() {
        let mut data = AuthStorageData::new();
        data.insert("p".to_string(), serde_json::Value::Null);
        let document = StorageDocument::from_data(data);
        let error = read_entry_from_document(&document, "p", &aborted_options()).unwrap_err();
        assert!(error.is_aborted());
    }
}
