//! `ProjectTrustStore` / `trust.json` / `TRUST_REQUIRING_*`。
//!
//! TS: `core/trust-manager.ts`
//!
//! `trust.json` 只保存 `true | false | null`；键按 **JS 默认字符串排序**（UTF-16 码元比较，
//! 不是 Rust 的字节/字符序）写出，格式为 `JSON.stringify(sorted, null, 2) + "\n"`（**有**末尾
//! 换行，与 settings/auth 不同）。锁：对 trust 目录加锁，但锁文件是 `<trust.json>.lock`
//! （`lockfilePath: ${path}.lock`）。
//!
//! # 这个文件解决什么问题
//!
//! 运行时（`kiana-coding-agent` 的启动信任解析、`kiana-builtin-extensions` 的 MCP 命令）需要
//! 知道某个项目目录“是否被用户信任”，以决定能否加载项目本地的配置与扩展。本文件把用户的
//! 决定持久化到 agent 目录下的 `trust.json`，并提供三种查询：沿目录树向上继承的最近决定、
//! 决定所在目录、以及 UI 用的候选选项。真正的判定顺序在
//! [`crate::trust::project_trust::resolve_project_trusted`]；没有本文件，决定无法跨进程记住，
//! 每次启动都要重新询问。
//!
//! # 数据结构
//!
//! `trust.json` 是**扁平**的 JSON 对象：键是目录路径，值是 `true`、`false` 或 `null`，没有
//! 嵌套。本实现写入的键一律先经 `normalize_cwd` 规范化；读取时不校验键的形状，外部写入的
//! 任意字符串键会原样保留。父子目录关系不存储，由查询时逐级 `dirname` 推导
//! （`find_nearest_trust_entry`）。
//!
//! `null` 的存储语义（查询时与键不存在等价）：
//! - 读取时 null 与键不存在都表示此层没有决定，查询继续向上（`set(child, None)` 删除键后
//!   同样回落到父目录的决定，见 `tests/trust_manager.rs`）；
//! - 读改写时 `null` 键原样写回；只有 `set_many` 收到 `decision: None` 才会真正删除键
//!   （`IndexMap::shift_remove`）。
//!
//! # 读写流程与并发
//!
//! `get_entry` 与 `set_many` 共用 1–3 的 gate/建目录/文件锁流程：
//! 1. 取进程内 `gate`（同一 `ProjectTrustStore` 及其 `clone` 共享；`std::sync::Mutex` 中毒后
//!    取回内部值继续）；
//! 2. `create_dir_all(dirname(trust_path))` —— 因此**只读的 `get` 也会创建 agent 目录**；
//! 3. 以 `<trust.json>.lock` 为锁目录加同步文件锁（[`crate::auth::lock`]）：最多 10 次、
//!    每次间隔 20 ms，只重试 ELOCKED；超过窗口后错误文案是
//!    `Lock file is already being held`；
//! 4. `get_entry` 随后只读取文件；`set_many` 才继续读文件 → 修改内存表 → 整份重写。
//!
//! 跨进程互斥只靠这个目录锁。两个**独立构造**的 store 各有自己的 `gate`，同进程内也会走
//! 文件锁；竞争超过重试窗口时不会继续等待，而是返回错误。
//!
//! # 失败模式
//!
//! 所有错误统一为 [`TrustError`]（`new Error(message)` 的等价物，`Display` 即 `message`）：
//! - 读取：I/O 错误包裹成 `Failed to read trust store <path>: <Node 风格 errno 文案>`；
//! - 解析：使用 V8 兼容的 JSON 错误文案；根不是对象、或值不是 `true`/`false`/`null` 时返回
//!   `Invalid trust store ...`；
//! - 写入：`mkdir` / `open` / `write` 的 Node 风格文案**不**加 `Failed to write ...` 前缀
//!   （错误文案是当前稳定契约，测试逐字断言）；
//! - 写入非原子：`File::create` 先截断再 `write_all`，中途失败可能留下部分内容。
//!
//! 字节格式（排序、缩进、末尾换行）由 `tests/trust_manager.rs` 的黄金 fixture 锁定；修改
//! 渲染逻辑会让 fixture 失败，并破坏与 TS 的字节级一致。

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use kiana_core::config::CONFIG_DIR_NAME;
use kiana_core::paths::{PathInputOptions, canonicalize_path, resolve_path};
use kiana_core::text::strip_bom;
use serde_json::{Map, Value};

use crate::auth::lock;

/// `ProjectTrustDecision = boolean | null`。
pub type ProjectTrustDecision = Option<bool>;

/// `{ path, decision }`（最近的有效决定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustStoreEntry {
    /// 决定所在目录（规范化后的绝对路径）。
    pub path: String,
    /// `true` / `false`。
    pub decision: bool,
}

/// `{ path, decision }`（一次写入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustUpdate {
    /// 目录（写入前 `normalizeCwd`）。
    pub path: String,
    /// `true` / `false` / `null`（null → 删除该键）。
    pub decision: ProjectTrustDecision,
}

/// 信任选择项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustOption {
    /// 展示标签（逐字）。
    pub label: String,
    /// 是否信任。
    pub trusted: bool,
    /// 选择后写入的更新。
    pub updates: Vec<ProjectTrustUpdate>,
    /// 用于 UI 反查「已保存决定」的路径。
    pub saved_path: Option<String>,
}

/// `type TrustFile = Record<string, boolean | null | undefined>`（`None` = `null`）。
type TrustFile = IndexMap<String, Option<bool>>;

/// 需要项目信任才会加载的项目资源（`TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES`，顺序即定义）。
pub const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES: [&str; 8] = [
    "settings.json",
    "mcp.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

/// trust.json 读写错误（`new Error(message)` 等价物；`Display` == TS `error.message`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustError {
    /// 逐字错误文案。
    pub message: String,
}

impl TrustError {
    /// `new Error(message)`。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TrustError {}

fn lock_state<T>(slot: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `path.dirname`（根 → 根；空 → `"."`）。
pub(crate) fn dirname(path: &str) -> String {
    match Path::new(path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        Some(_) => ".".to_string(),
        None => path.to_string(),
    }
}

/// `path.join(parts...)`（词法拼接，保留绝对路径）。
pub(crate) fn join_path(parts: &[&str]) -> String {
    let mut buffer = PathBuf::new();
    for part in parts {
        buffer.push(part);
    }
    buffer.to_string_lossy().into_owned()
}

fn path_options() -> PathInputOptions {
    PathInputOptions::default()
}

/// `normalizeCwd(cwd)`：`canonicalizePath(resolvePath(cwd))`。
///
/// `canonicalizePath` 无法做 realpath 时（例如目录不存在）回退为 `resolvePath` 的词法结果，
/// 因此不存在的目录也能得到稳定的键。
pub fn normalize_cwd(cwd: &str) -> String {
    canonicalize_path(&resolve_path(cwd, &current_dir_string(), &path_options()))
}

/// 从 `cwd` 开始逐级向上，返回第一个值为 `true` / `false` 的条目。
///
/// `null` 条目会被跳过（继续向上）；循环在 `dirname` 返回自身（文件系统根）时结束。
fn find_nearest_trust_entry(data: &TrustFile, cwd: &str) -> Option<ProjectTrustStoreEntry> {
    let mut current_dir = normalize_cwd(cwd);
    loop {
        if let Some(Some(decision)) = data.get(&current_dir) {
            return Some(ProjectTrustStoreEntry {
                path: current_dir,
                decision: *decision,
            });
        }
        let parent_dir = dirname(&current_dir);
        if parent_dir == current_dir {
            return None;
        }
        current_dir = parent_dir;
    }
}

/// `getProjectTrustParentPath(cwd)`。
pub fn get_project_trust_parent_path(cwd: &str) -> Option<String> {
    let trust_path = normalize_cwd(cwd);
    let parent_dir = dirname(&trust_path);
    if parent_dir == trust_path {
        None
    } else {
        Some(parent_dir)
    }
}

/// `getProjectTrustOptions(cwd, { includeSessionOnly })`：标签与顺序逐字。
///
/// 顺序固定：
/// 1. `Trust`；
/// 2. `Trust parent folder (<parent>)`（仅当存在父目录）；
/// 3. `Trust (this session only)`（仅 `include_session_only`）；
/// 4. `Do not trust`；
/// 5. `Do not trust (this session only)`（仅 `include_session_only`）。
///
/// 标签会参与 UI 的逐字匹配，不能改写。父目录选项的 `updates` 是两步：先把父目录置为
/// `true`，再删除 cwd 自己的键；这样保存后 cwd 继承父目录的决定，而不是在 cwd 上写死。
/// 两个会话级选项的 `updates` 为空、`saved_path` 为 `None`，因此不会写盘。
pub fn get_project_trust_options(cwd: &str, include_session_only: bool) -> Vec<ProjectTrustOption> {
    let trust_path = normalize_cwd(cwd);
    let mut options = vec![ProjectTrustOption {
        label: "Trust".to_string(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(true),
        }],
        saved_path: Some(trust_path.clone()),
    }];

    if let Some(parent_path) = get_project_trust_parent_path(cwd) {
        options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent_path.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: trust_path.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent_path),
        });
    }

    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Trust (this session only)".to_string(),
            trusted: true,
            updates: Vec::new(),
            saved_path: None,
        });
    }

    options.push(ProjectTrustOption {
        label: "Do not trust".to_string(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(false),
        }],
        saved_path: Some(trust_path.clone()),
    });

    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Do not trust (this session only)".to_string(),
            trusted: false,
            updates: Vec::new(),
            saved_path: None,
        });
    }

    options
}

/// `readTrustFile(path)`。
///
/// 文件不存在 → 空表。存在时先把字节按 UTF-8 有损解码，再 `strip_bom`，然后交给
/// `kiana_json::js_parse`（错误文案与 V8 的 `JSON.parse` 对齐，测试逐字断言）。根必须是
/// 对象，值只能是 `true` / `false` / `null`，其余形状返回 [`TrustError`]。
fn read_trust_file(path: &str) -> Result<TrustFile, TrustError> {
    if !Path::new(path).exists() {
        return Ok(TrustFile::new());
    }
    let result_path = Path::new(path);
    let bytes = fs::read(result_path).map_err(|error| {
        TrustError::new(format!(
            "Failed to read trust store {path}: {}",
            crate::auth::storage::node_io_error(
                crate::auth::storage::read_syscall(&error),
                result_path,
                &error,
            )
        ))
    })?;
    let content = String::from_utf8_lossy(&bytes);
    let parsed = kiana_json::js_parse::parse(&strip_bom(&content)).map_err(|error| {
        TrustError::new(format!(
            "Failed to read trust store {path}: {}",
            error.message
        ))
    })?;
    let Value::Object(map) = parsed else {
        return Err(TrustError::new(format!(
            "Invalid trust store {path}: expected an object"
        )));
    };
    let mut data = TrustFile::new();
    for (key, value) in map {
        let decision = match value {
            Value::Bool(value) => Some(value),
            Value::Null => None,
            _ => {
                return Err(TrustError::new(format!(
                    "Invalid trust store {path}: value for {} must be true, false, or null",
                    kiana_json::js_object::stringify(&Value::String(key))
                )));
            }
        };
        data.insert(key, decision);
    }
    Ok(data)
}

/// `Object.keys(data).sort()`：JS 默认字符串排序 = UTF-16 码元比较。
///
/// 逐对比较 `u16`，不是 Unicode 码点序：增补平面字符（如 emoji）的代理对会排在
/// U+E000..=U+FFFF 的 BMP 字符之前，`tests/fixtures/trust/trust-json-cases.json` 锁定该顺序。
fn sort_keys_js(keys: &mut [String]) {
    keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
}

/// `writeTrustFile` 的逐字渲染：`Object.keys(data).sort()`（UTF-16）+ 只保留 true/false/null +
/// `JSON.stringify(sorted, null, 2) + "\n"`。
///
/// 渲染分两步：先收集键并按 UTF-16 排序，再按排序后的顺序构建 JSON 对象（`IndexMap` 自身的
/// 插入顺序在这里不生效）。`None` 渲染为 JSON `null`，不会省略键；末尾无条件追加 `\n`。
fn render_trust_file(data: &TrustFile) -> String {
    let mut keys: Vec<String> = data.keys().cloned().collect();
    sort_keys_js(&mut keys);
    let mut sorted = Map::new();
    for key in keys {
        if let Some(decision) = data.get(&key).copied().flatten() {
            sorted.insert(key, Value::Bool(decision));
        } else {
            sorted.insert(key, Value::Null);
        }
    }
    let mut content = kiana_json::js_object::stringify_pretty(&Value::Object(sorted), 2);
    content.push('\n');
    content
}

/// **test hook, not public API**：按 `writeTrustFile` 的字节规则渲染（黄金测试用）。
#[doc(hidden)]
pub fn render_trust_store_json(entries: &[(String, ProjectTrustDecision)]) -> String {
    let mut data = TrustFile::new();
    for (key, decision) in entries {
        data.insert(key.clone(), *decision);
    }
    render_trust_file(&data)
}

/// `writeTrustFile(path, data)`：排序渲染后写盘（`mkdirSync(dirname)` 递归）。
///
/// 顺序：`create_dir_all(dirname)` → `File::create`（截断）→ `write_all`。三个阶段的错误
/// 分别映射为 Node 风格的 `mkdir` / `open` / `write` 文案；`write` 阶段不带路径（见
/// `node_io_error`）。写入不是原子替换，中途失败可能留下截断或部分写入的文件。
fn write_trust_file(path: &str, data: &TrustFile) -> Result<(), TrustError> {
    use std::io::Write;

    let content = render_trust_file(data);
    let directory = dirname(path);
    fs::create_dir_all(&directory)
        .map_err(|error| TrustError::new(node_io_mkdir(&directory, &error)))?;
    let file_path = Path::new(path);
    let mut file = fs::File::create(file_path)
        .map_err(|error| TrustError::new(node_io_open(file_path, &error)))?;
    file.write_all(content.as_bytes())
        .map_err(|error| TrustError::new(node_io_write(file_path, &error)))
}

/// Node `errno` 文案（`mkdir` 带 path）。
fn node_io_mkdir(path: &str, error: &std::io::Error) -> String {
    crate::auth::storage::node_io_error("mkdir", Path::new(path), error)
}

/// Node `errno` 文案（`writeFileSync` 的打开阶段：`open` 带 path）。
fn node_io_open(path: &Path, error: &std::io::Error) -> String {
    crate::auth::storage::node_io_error("open", path, error)
}

/// Node `errno` 文案（`write` 阶段不带 path）。
fn node_io_write(path: &Path, error: &std::io::Error) -> String {
    crate::auth::storage::node_io_error("write", path, error)
}

/// `acquireTrustLockSync(path)`：对目录加锁、锁文件为 `${path}.lock`、10×20 ms 仅重试 ELOCKED。
///
/// 锁目标目录是 `dirname(path)`，锁目录固定为 `${path}.lock`（proper-lockfile 的
/// `lockfilePath`）。目录锁要求父目录已存在，所以先 `create_dir_all`。`guard` 在 `f` 返回后
/// drop 释放，`f` 返回 `Err` 时同样释放；进程内 `gate` 在文件锁之后才释放。
fn with_trust_file_lock<T>(
    path: &str,
    f: impl FnOnce() -> Result<T, TrustError>,
) -> Result<T, TrustError> {
    let trust_dir = dirname(path);
    fs::create_dir_all(&trust_dir)
        .map_err(|error| TrustError::new(node_io_mkdir(&trust_dir, &error)))?;
    let lockfile_path = format!("{path}.lock");
    let guard =
        lock::acquire_lock_sync_with_retry_at(Path::new(&trust_dir), Path::new(&lockfile_path))
            .map_err(|error| TrustError::new(error.to_string()))?;
    let result = f();
    drop(guard);
    result
}

/// cwd 是否含有必须由项目信任门控的项目本地资源。
///
/// 规则（TS 逐字）：`<cwd>/.kiana` 下任一 `TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES`；或
/// cwd 及其祖先中的 `.agents/skills`。`$HOME/.agents/skills` 是用户级资源，永远不算。
///
/// 两个检查的搜索范围不同，不能合并：
/// - `<cwd>/.kiana` 下的资源只检查**当前目录**这一层，父目录的 `.kiana` 不影响结果；
/// - `.agents/skills` 从 `cwd` 逐级查到根，但字符串上等于 `$HOME/.agents/skills` 的那一级
///   被排除。`$HOME` 未设置或为空时回退 `dirs::home_dir()`。
///
/// 命中判断使用 `Path::exists()`。
pub fn has_trust_requiring_project_resources(cwd: &str) -> bool {
    let base = current_dir_string();
    let home = std::env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let home_dir = canonicalize_path(&resolve_path(&home, &base, &path_options()));
    let user_agents_skills_dir = join_path(&[&home_dir, ".agents", "skills"]);
    let mut current_dir = canonicalize_path(&resolve_path(cwd, &base, &path_options()));

    let config_dir = join_path(&[&current_dir, CONFIG_DIR_NAME]);
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES
        .iter()
        .any(|entry| Path::new(&join_path(&[&config_dir, entry])).exists())
    {
        return true;
    }

    loop {
        let agents_skills_dir = join_path(&[&current_dir, ".agents", "skills"]);
        if agents_skills_dir != user_agents_skills_dir && Path::new(&agents_skills_dir).exists() {
            return true;
        }
        let parent_dir = dirname(&current_dir);
        if parent_dir == current_dir {
            return false;
        }
        current_dir = parent_dir;
    }
}

/// `ProjectTrustStore`：`<agent-dir>/trust.json` 的读写门面。
///
/// 所有方法都是同步的（内部使用 `std::fs` 与同步目录锁），没有 async 包装；`clone` 得到的
/// 副本与原件共享同一个进程内 `gate`，但**分别构造**的实例不共享。
///
/// TS: `trust-manager.ts#ProjectTrustStore`
#[derive(Debug, Clone)]
pub struct ProjectTrustStore {
    trust_path: String,
    /// 串行化同一进程内的读改写（跨进程由文件锁保证）。
    gate: std::sync::Arc<std::sync::Mutex<()>>,
}

impl ProjectTrustStore {
    /// `new ProjectTrustStore(agentDir)`：`join(resolvePath(agentDir), "trust.json")`。
    ///
    /// `resolvePath` 以进程当前目录为基准，按默认选项展开 `~`；这里不做 canonicalize，
    /// 因此 `trust_path` 只是词法拼接的结果。
    pub fn new(agent_dir: &str) -> Self {
        let resolved = resolve_path(agent_dir, &current_dir_string(), &path_options());
        Self {
            trust_path: join_path(&[&resolved, "trust.json"]),
            gate: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// `trust.json` 的路径。
    pub fn trust_path(&self) -> &str {
        &self.trust_path
    }

    /// `get(cwd)`：最近的有效决定，没有则 `None`（TS 的 `null`）。
    ///
    /// 从 `cwd` 起逐级向上直到根，`null` 项被跳过。`None` 只表示“没有任何决定”，不表示
    /// 不信任（不信任是 `Some(false)`）。读文件、解析或加锁失败都以 [`TrustError`] 返回。
    pub fn get(&self, cwd: &str) -> Result<ProjectTrustDecision, TrustError> {
        Ok(self.get_entry(cwd)?.map(|entry| entry.decision))
    }

    /// `getEntry(cwd)`：`{ path, decision }` 或 `None`。
    ///
    /// 返回的 `path` 是决定实际所在的目录，可能不等于 `cwd`（继承自父目录时是祖先路径），
    /// UI 用它显示 “inherited from ...”。
    pub fn get_entry(&self, cwd: &str) -> Result<Option<ProjectTrustStoreEntry>, TrustError> {
        let _guard = lock_state(&self.gate);
        with_trust_file_lock(&self.trust_path, || {
            let data = read_trust_file(&self.trust_path)?;
            Ok(find_nearest_trust_entry(&data, cwd))
        })
    }

    /// `set(cwd, decision)`。
    ///
    /// 单条更新的便捷入口，等价于一次 [`ProjectTrustStore::set_many`]；`decision` 为 `None`
    /// 时删除 `cwd` 的键，查询随即回落到父目录的决定。
    pub fn set(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<(), TrustError> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_string(),
            decision,
        }])
    }

    /// `setMany(decisions)`：加锁读改写（null → 删除键）。
    ///
    /// 按数组顺序应用，重复路径以最后一条为准；每条 `path` 写入前都经过 [`normalize_cwd`]。
    /// 空切片也会加锁并把读到的内容重新渲染写回（原有的 `null` 键会保留）。
    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), TrustError> {
        let _guard = lock_state(&self.gate);
        with_trust_file_lock(&self.trust_path, || {
            let mut data = read_trust_file(&self.trust_path)?;
            for update in decisions {
                let key = normalize_cwd(&update.path);
                match update.decision {
                    None => {
                        data.shift_remove(&key);
                    }
                    Some(decision) => {
                        data.insert(key, Some(decision));
                    }
                }
            }
            write_trust_file(&self.trust_path, &data)
        })
    }
}
